#![allow(clippy::type_complexity)]

//! Rocket integration for `inertia_rs`.

use super::{Inertia, Location, Redirect, RequestContext, VARY, X_INERTIA, X_INERTIA_LOCATION};
use rocket::fairing::{Fairing, Info, Kind};
use rocket::http::{self, uri::Reference, Method};
use rocket::request::{FromRequest, Outcome, Request};
use rocket::response::{self, Redirect as RocketRedirect, Responder, Response};
use rocket::serde::json::Json;
use rocket::Data;
use rocket::{error, get, routes, uri};
use serde::Serialize;
use std::sync::Arc;
use tracing::trace;

const BASE_ROUTE: &str = "/inertia-rs";

fn request_context(request: &Request<'_>) -> RequestContext {
    RequestContext::from_header_fn(|name| request.headers().get_one(name))
}

/// Parsed Inertia headers for use as a Rocket request guard.
///
/// This guard always succeeds. It lets route handlers branch on whether a
/// request came from the Inertia client without manually reading raw headers.
pub struct InertiaHeaders {
    context: RequestContext,
}

impl InertiaHeaders {
    /// Returns `true` when the request includes the `X-Inertia` header.
    pub fn is_inertia(&self) -> bool {
        self.context.is_inertia()
    }

    /// Returns the request's `X-Inertia-Version` header value.
    pub fn version(&self) -> Option<&str> {
        self.context.version()
    }

    /// Returns the parsed request context.
    pub fn context(&self) -> &RequestContext {
        &self.context
    }
}

#[rocket::async_trait]
impl<'r> FromRequest<'r> for InertiaHeaders {
    type Error = std::convert::Infallible;

    async fn from_request(request: &'r Request<'_>) -> Outcome<Self, Self::Error> {
        Outcome::Success(Self {
            context: request_context(request),
        })
    }
}

#[derive(Serialize)]
/// Context passed to the application HTML response renderer.
pub struct HtmlResponseContext {
    data_page: String,
}

impl HtmlResponseContext {
    /// Returns the JSON-serialized Inertia page object.
    pub fn data_page(&self) -> &str {
        &self.data_page
    }
}

fn escape_json_for_html_script(json: &str) -> String {
    json.chars()
        .fold(String::with_capacity(json.len()), |mut escaped, c| {
            match c {
                '<' => escaped.push_str("\\u003C"),
                '>' => escaped.push_str("\\u003E"),
                '&' => escaped.push_str("\\u0026"),
                '\u{2028}' => escaped.push_str("\\u2028"),
                '\u{2029}' => escaped.push_str("\\u2029"),
                _ => escaped.push(c),
            }

            escaped
        })
}

fn serialize_page_for_html<T: Serialize>(page: &T) -> Result<String, http::Status> {
    serde_json::to_string(page)
        .map(|json| escape_json_for_html_script(&json))
        .map_err(|_e| http::Status::InternalServerError)
}

#[derive(Clone)]
struct InertiaVersion(String);

fn add_vary_header<'r>(response: Response<'r>) -> Response<'r> {
    Response::build_from(response)
        .raw_header_adjoin(VARY, X_INERTIA)
        .finalize()
}

fn validated_uri_reference(url: String) -> Result<String, http::Status> {
    let uri: Reference<'static> = url.try_into().map_err(|_e| {
        error!("Invalid URI used for redirect.");
        http::Status::InternalServerError
    })?;

    Ok(uri.to_string())
}

fn is_write_method(method: Method) -> bool {
    matches!(
        method,
        Method::Post | Method::Put | Method::Patch | Method::Delete
    )
}

impl<'r, 'o: 'r, R: Serialize> Responder<'r, 'o> for Inertia<R> {
    #[inline(always)]
    fn respond_to(self, request: &'r Request<'_>) -> response::Result<'o> {
        let url = self
            .url()
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| request.uri().to_string());
        let version = request
            .local_cache(|| None::<InertiaVersion>)
            .clone()
            .map(|version| version.0);
        let context = if request.method() == Method::Get {
            request_context(request)
        } else {
            request_context(request).without_partial_reload()
        };
        let inertia_response = self
            .into_page(url, version, &context)
            .map_err(|_e| http::Status::InternalServerError)?;

        if context.is_inertia() {
            Response::build()
                .merge(Json(inertia_response).respond_to(request)?)
                .raw_header(X_INERTIA, "true")
                .raw_header_adjoin(VARY, X_INERTIA)
                .ok()
        } else {
            let ctx = HtmlResponseContext {
                data_page: serialize_page_for_html(&inertia_response)?,
            };

            match request.rocket().state::<ResponderFn>() {
                Some(f) => f.0(request, &ctx).map(add_vary_header),
                None => {
                    error!("Responder not found");
                    http::Status::InternalServerError.respond_to(request)
                }
            }
        }
    }
}

impl<'r, 'o: 'r> Responder<'r, 'o> for Location {
    #[inline(always)]
    fn respond_to(self, request: &'r Request<'_>) -> response::Result<'o> {
        let url = validated_uri_reference(self.url)?;

        if request_context(request).is_inertia() {
            Response::build()
                .status(http::Status::Conflict)
                .raw_header(X_INERTIA_LOCATION, url)
                .raw_header_adjoin(VARY, X_INERTIA)
                .ok()
        } else if is_write_method(request.method()) {
            RocketRedirect::to(url)
                .respond_to(request)
                .map(add_vary_header)
        } else {
            RocketRedirect::found(url)
                .respond_to(request)
                .map(add_vary_header)
        }
    }
}

impl<'r, 'o: 'r> Responder<'r, 'o> for Redirect {
    #[inline(always)]
    fn respond_to(self, request: &'r Request<'_>) -> response::Result<'o> {
        let url = validated_uri_reference(self.url)?;

        if is_write_method(request.method()) {
            RocketRedirect::to(url).respond_to(request)
        } else {
            RocketRedirect::found(url).respond_to(request)
        }
    }
}

/// Rocket fairing that handles Inertia asset versioning and HTML rendering.
///
/// The fairing stores the current asset version for page responses, handles
/// stale Inertia `GET` requests by returning `409 Conflict`, and registers the
/// HTML response callback used for first-page loads.
pub struct VersionFairing<'resp> {
    version: String,
    html_response:
        Arc<dyn Fn(&Request<'_>, &HtmlResponseContext) -> response::Result<'resp> + Send + Sync>,
}

impl<'resp> VersionFairing<'resp> {
    /// Creates a fairing with a static asset `version` and HTML renderer.
    pub fn new<'a, 'b, F, V: Into<String>>(version: V, html_response: F) -> Self
    where
        F: Fn(&Request<'_>, &HtmlResponseContext) -> response::Result<'resp>
            + Send
            + Sync
            + 'static,
    {
        Self {
            version: version.into(),
            html_response: Arc::new(html_response),
        }
    }
}

struct VersionConflictResponse(String);

impl<'r, 'o: 'r> Responder<'r, 'o> for VersionConflictResponse {
    #[inline(always)]
    fn respond_to(self, _request: &'r Request<'_>) -> response::Result<'o> {
        Response::build()
            .status(http::Status::Conflict)
            .raw_header(X_INERTIA_LOCATION, self.0)
            .raw_header_adjoin(VARY, X_INERTIA)
            .ok()
    }
}

fn is_local_location(location: &str) -> bool {
    location.starts_with('/')
        && !location.starts_with("//")
        && !location.starts_with("/\\")
        && !location.chars().any(|c| c.is_ascii_control())
}

#[get("/version-conflict?<location>")]
fn version_conflict(location: String) -> Result<VersionConflictResponse, http::Status> {
    if is_local_location(&location) {
        Ok(VersionConflictResponse(location))
    } else {
        Err(http::Status::BadRequest)
    }
}

struct ResponderFn<'resp>(
    Arc<dyn Fn(&Request<'_>, &HtmlResponseContext) -> response::Result<'resp> + Send + Sync>,
);

#[rocket::async_trait]
impl Fairing for VersionFairing<'static> {
    fn info(&self) -> Info {
        Info {
            name: "Inertia Asset Versioning",
            kind: Kind::Ignite | Kind::Request,
        }
    }

    async fn on_ignite(&self, rocket: rocket::Rocket<rocket::Build>) -> rocket::fairing::Result {
        Ok(rocket
            .mount(BASE_ROUTE, routes![version_conflict])
            .manage(ResponderFn(self.html_response.clone())))
    }

    async fn on_request(&self, request: &mut Request<'_>, _: &mut Data<'_>) {
        request.local_cache(|| Some(InertiaVersion(self.version.clone())));

        let context = request_context(request);

        if request.method() == Method::Get && context.is_inertia() {
            let request_version = context.version();

            trace!(
                "request version {:?} / asset version {}",
                request_version,
                &self.version
            );

            if request_version != Some(self.version.as_str()) {
                let uri = uri!(
                    "/inertia-rs",
                    version_conflict(location = request.uri().to_string())
                );

                trace!("\tredirecting to {}", &uri.to_string());

                request.set_uri(uri);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ScrollProps, X_INERTIA_EXCEPT_ONCE_PROPS, X_INERTIA_INFINITE_SCROLL_MERGE_INTENT,
        X_INERTIA_PARTIAL_COMPONENT, X_INERTIA_PARTIAL_DATA, X_INERTIA_PARTIAL_EXCEPT,
        X_INERTIA_RESET, X_INERTIA_VERSION,
    };
    use rocket::{
        delete,
        http::{Header, Status},
        local::blocking::Client,
        patch, post, put,
    };

    #[derive(Serialize)]
    struct Props {
        n: i32,
    }

    #[derive(Serialize)]
    struct TextProps {
        text: String,
    }

    #[derive(Serialize)]
    struct AdvancedProps {
        users: Vec<&'static str>,
        stats: i32,
        plans: Vec<&'static str>,
        notifications: Vec<&'static str>,
    }

    #[get("/foo")]
    fn foo() -> Inertia<Props> {
        Inertia::response("foo", Props { n: 42 })
    }

    #[get("/url")]
    fn url_override() -> Inertia<Props> {
        Inertia::response("foo", Props { n: 42 }).with_url("/custom-url")
    }

    #[get("/unsafe")]
    fn unsafe_props() -> Inertia<TextProps> {
        Inertia::response(
            "unsafe",
            TextProps {
                text: "</script><script>alert(1)</script>&\u{2028}\u{2029}".into(),
            },
        )
    }

    #[get("/advanced")]
    fn advanced() -> Inertia<AdvancedProps> {
        Inertia::response(
            "advanced",
            AdvancedProps {
                users: vec!["Ada", "Grace"],
                stats: 42,
                plans: vec!["basic"],
                notifications: vec!["welcome"],
            },
        )
        .encrypt_history()
        .clear_history()
        .preserve_fragment()
        .always("users")
        .merge("users")
        .prepend("notifications")
        .defer("stats")
        .once("plans")
        .share("users")
    }

    #[rocket::post("/write")]
    fn write() -> Inertia<AdvancedProps> {
        Inertia::response(
            "write",
            AdvancedProps {
                users: vec!["Ada", "Grace"],
                stats: 42,
                plans: vec!["basic"],
                notifications: vec!["welcome"],
            },
        )
        .once("plans")
    }

    #[get("/scrolling")]
    fn scrolling() -> Inertia<serde_json::Value> {
        Inertia::page("scrolling")
            .scroll("posts", ScrollProps::new("page", 1).next_page(2))
            .props(serde_json::json!({
                "posts": {
                    "data": [1, 2]
                }
            }))
    }

    #[get("/external")]
    fn external() -> Location {
        Inertia::location("https://example.com/outside")
    }

    #[post("/external")]
    fn external_post() -> Location {
        Inertia::location("https://example.com/outside")
    }

    #[get("/bad-external")]
    fn bad_external() -> Location {
        Inertia::location("/bad\nlocation")
    }

    #[get("/go")]
    fn get_redirect() -> Redirect {
        Inertia::redirect("/target")
    }

    #[get("/bad-go")]
    fn bad_redirect() -> Redirect {
        Inertia::redirect("/bad\nlocation")
    }

    #[post("/go")]
    fn post_redirect() -> Redirect {
        Inertia::redirect("/target")
    }

    #[put("/go")]
    fn put_redirect() -> Redirect {
        Inertia::redirect("/target")
    }

    #[patch("/go")]
    fn patch_redirect() -> Redirect {
        Inertia::redirect("/target")
    }

    #[delete("/go")]
    fn delete_redirect() -> Redirect {
        Inertia::redirect("/target")
    }

    #[get("/headers")]
    fn headers(headers: InertiaHeaders) -> String {
        format!(
            "{}:{}",
            headers.is_inertia(),
            headers.version().unwrap_or("none")
        )
    }

    #[get("/context")]
    fn context(headers: InertiaHeaders) -> String {
        format!(
            "{}:{}",
            headers.context().partial_component().unwrap_or("none"),
            headers.context().partial_data().join("|")
        )
    }

    const CURRENT_VERSION: &str = "1";

    fn rocket() -> rocket::Rocket<rocket::Build> {
        rocket::build()
            .mount(
                "/",
                routes![
                    foo,
                    url_override,
                    unsafe_props,
                    advanced,
                    write,
                    scrolling,
                    external,
                    external_post,
                    bad_external,
                    get_redirect,
                    bad_redirect,
                    post_redirect,
                    put_redirect,
                    patch_redirect,
                    delete_redirect,
                    headers
                ],
            )
            .attach(VersionFairing::new(CURRENT_VERSION, |request, ctx| {
                serde_json::to_string(ctx).unwrap().respond_to(request)
            }))
    }

    fn rocket_without_fairing() -> rocket::Rocket<rocket::Build> {
        rocket::build().mount("/", routes![foo, headers])
    }

    #[test]
    fn html_response_sent() {
        let client = Client::tracked(rocket()).unwrap();

        // no X-Inertia header should fall back to the response closure
        let req = client.get("/foo");

        let resp = req.dispatch();
        let headers = resp.headers();

        assert_eq!(resp.status(), Status::Ok);
        assert_eq!(
            headers.get_one("Content-Type"),
            Some("text/plain; charset=utf-8")
        );
        assert_eq!(headers.get_one(X_INERTIA), None);
        assert_eq!(headers.get_one(VARY), Some(X_INERTIA));
    }

    #[test]
    fn json_sent_versions_eq() {
        let client = Client::tracked(rocket()).unwrap();

        let req = client
            .get("/foo")
            .header(Header::new(X_INERTIA, "true"))
            .header(Header::new(X_INERTIA_VERSION, CURRENT_VERSION));

        let resp = req.dispatch();
        let headers = resp.headers();

        assert_eq!(resp.status(), Status::Ok);
        assert_eq!(headers.get_one("Content-Type"), Some("application/json"));
        assert_eq!(headers.get_one(X_INERTIA), Some("true"));
        assert_eq!(headers.get_one(VARY), Some(X_INERTIA));

        let body = resp.into_string().unwrap();
        let page: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(page["version"], CURRENT_VERSION);
    }

    #[test]
    fn json_response_without_fairing_omits_version() {
        let client = Client::tracked(rocket_without_fairing()).unwrap();

        let resp = client
            .get("/foo")
            .header(Header::new(X_INERTIA, "true"))
            .dispatch();
        let body = resp.into_string().unwrap();
        let page: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert!(page.get("version").is_none());
    }

    #[test]
    fn html_response_includes_query_string_and_version() {
        let client = Client::tracked(rocket()).unwrap();

        let resp = client.get("/foo?bar=baz").dispatch();
        let body = resp.into_string().unwrap();
        let ctx: serde_json::Value = serde_json::from_str(&body).unwrap();
        let data_page = ctx["data_page"].as_str().unwrap();
        let page: serde_json::Value = serde_json::from_str(data_page).unwrap();

        assert_eq!(page["url"], "/foo?bar=baz");
        assert_eq!(page["version"], CURRENT_VERSION);
    }

    #[test]
    fn html_response_escapes_json_for_script_context() {
        let client = Client::tracked(rocket()).unwrap();

        let resp = client.get("/unsafe").dispatch();
        let body = resp.into_string().unwrap();
        let ctx: serde_json::Value = serde_json::from_str(&body).unwrap();
        let data_page = ctx["data_page"].as_str().unwrap();

        assert!(!data_page.contains("</script>"));
        assert!(data_page.contains("\\u003C/script\\u003E"));
        assert!(data_page.contains("\\u003Cscript\\u003E"));
        assert!(data_page.contains("\\u0026"));
        assert!(data_page.contains("\\u2028"));
        assert!(data_page.contains("\\u2029"));

        let page: serde_json::Value = serde_json::from_str(data_page).unwrap();
        assert_eq!(
            page["props"]["text"],
            "</script><script>alert(1)</script>&\u{2028}\u{2029}"
        );
    }

    #[test]
    fn html_response_embeds_v3_metadata() {
        let client = Client::tracked(rocket()).unwrap();

        let resp = client.get("/advanced").dispatch();
        let body = resp.into_string().unwrap();
        let ctx: serde_json::Value = serde_json::from_str(&body).unwrap();
        let data_page = ctx["data_page"].as_str().unwrap();
        let page: serde_json::Value = serde_json::from_str(data_page).unwrap();

        assert_eq!(page["encryptHistory"], true);
        assert_eq!(page["mergeProps"], serde_json::json!(["users"]));
        assert_eq!(
            page["deferredProps"],
            serde_json::json!({ "default": ["stats"] })
        );
        assert_eq!(page["props"]["users"], serde_json::json!(["Ada", "Grace"]));
        assert!(page["props"].get("stats").is_none());
    }

    #[test]
    fn json_response_includes_query_string() {
        let client = Client::tracked(rocket()).unwrap();

        let resp = client
            .get("/foo?bar=baz")
            .header(Header::new(X_INERTIA, "true"))
            .header(Header::new(X_INERTIA_VERSION, CURRENT_VERSION))
            .dispatch();
        let body = resp.into_string().unwrap();
        let page: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(page["url"], "/foo?bar=baz");
    }

    #[test]
    fn with_url_overrides_request_uri() {
        let client = Client::tracked(rocket()).unwrap();

        let resp = client
            .get("/url?bar=baz")
            .header(Header::new(X_INERTIA, "true"))
            .header(Header::new(X_INERTIA_VERSION, CURRENT_VERSION))
            .dispatch();
        let body = resp.into_string().unwrap();
        let page: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(page["url"], "/custom-url");
    }

    #[test]
    fn version_conflict_location_includes_query_string() {
        let client = Client::tracked(rocket()).unwrap();

        let resp = client
            .get("/foo?bar=baz")
            .header(Header::new(X_INERTIA, "true"))
            .header(Header::new(X_INERTIA_VERSION, "OUTDATED_VERSION"))
            .dispatch();

        assert_eq!(resp.status(), Status::Conflict);
        assert_eq!(
            resp.headers().get_one(X_INERTIA_LOCATION),
            Some("/foo?bar=baz")
        );
        assert_eq!(resp.headers().get_one(VARY), Some(X_INERTIA));
    }

    #[test]
    fn external_location_response_uses_see_other_for_direct_write_requests() {
        let client = Client::tracked(rocket()).unwrap();

        let resp = client.post("/external").dispatch();

        assert_eq!(resp.status(), Status::SeeOther);
        assert_eq!(
            resp.headers().get_one("Location"),
            Some("https://example.com/outside")
        );
        assert_eq!(resp.headers().get_one(VARY), Some(X_INERTIA));
    }

    #[test]
    fn version_conflict_rejects_external_location() {
        let client = Client::tracked(rocket()).unwrap();

        let resp = client
            .get("/inertia-rs/version-conflict?location=https://example.com")
            .dispatch();

        assert_eq!(resp.status(), Status::BadRequest);
        assert_eq!(resp.headers().get_one(X_INERTIA_LOCATION), None);
    }

    #[test]
    fn version_conflict_rejects_protocol_relative_location() {
        let client = Client::tracked(rocket()).unwrap();

        let resp = client
            .get("/inertia-rs/version-conflict?location=//example.com")
            .dispatch();

        assert_eq!(resp.status(), Status::BadRequest);
        assert_eq!(resp.headers().get_one(X_INERTIA_LOCATION), None);
    }

    #[test]
    fn version_conflict_rejects_backslash_location() {
        let client = Client::tracked(rocket()).unwrap();

        let resp = client
            .get("/inertia-rs/version-conflict?location=/%5Cexample.com")
            .dispatch();

        assert_eq!(resp.status(), Status::BadRequest);
        assert_eq!(resp.headers().get_one(X_INERTIA_LOCATION), None);
    }

    #[test]
    fn version_conflict_rejects_control_character_location() {
        let client = Client::tracked(rocket()).unwrap();

        let resp = client
            .get("/inertia-rs/version-conflict?location=/%0Aexample.com")
            .dispatch();

        assert_eq!(resp.status(), Status::BadRequest);
        assert_eq!(resp.headers().get_one(X_INERTIA_LOCATION), None);
    }

    #[test]
    fn inertia_headers_guard_reads_headers() {
        let client = Client::tracked(rocket_without_fairing()).unwrap();

        let resp = client
            .get("/headers")
            .header(Header::new(X_INERTIA, "true"))
            .header(Header::new(X_INERTIA_VERSION, CURRENT_VERSION))
            .dispatch();

        assert_eq!(resp.into_string().unwrap(), "true:1");
    }

    #[test]
    fn inertia_headers_guard_exposes_request_context() {
        let client = Client::tracked(rocket::build().mount("/", routes![context])).unwrap();

        let resp = client
            .get("/context")
            .header(Header::new(X_INERTIA, "true"))
            .header(Header::new(X_INERTIA_PARTIAL_COMPONENT, "advanced"))
            .header(Header::new(X_INERTIA_PARTIAL_DATA, "users,stats"))
            .dispatch();

        assert_eq!(resp.into_string().unwrap(), "advanced:users|stats");
    }

    #[test]
    fn inertia_headers_guard_handles_regular_requests() {
        let client = Client::tracked(rocket_without_fairing()).unwrap();

        let resp = client.get("/headers").dispatch();

        assert_eq!(resp.into_string().unwrap(), "false:none");
    }

    #[test]
    fn rocket_json_response_serializes_v3_metadata_and_omits_deferred_props() {
        let client = Client::tracked(rocket()).unwrap();

        let resp = client
            .get("/advanced")
            .header(Header::new(X_INERTIA, "true"))
            .header(Header::new(X_INERTIA_VERSION, CURRENT_VERSION))
            .dispatch();

        assert_eq!(resp.status(), Status::Ok);
        assert_eq!(resp.headers().get_one(X_INERTIA), Some("true"));
        assert_eq!(resp.headers().get_one(VARY), Some(X_INERTIA));

        let body = resp.into_string().unwrap();
        let page: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(page["encryptHistory"], true);
        assert_eq!(page["clearHistory"], true);
        assert_eq!(page["preserveFragment"], true);
        assert_eq!(page["mergeProps"], serde_json::json!(["users"]));
        assert_eq!(page["prependProps"], serde_json::json!(["notifications"]));
        assert_eq!(
            page["deferredProps"],
            serde_json::json!({ "default": ["stats"] })
        );
        assert_eq!(page["sharedProps"], serde_json::json!(["users"]));
        assert_eq!(
            page["onceProps"]["plans"],
            serde_json::json!({ "prop": "plans", "expiresAt": null })
        );
        assert_eq!(page["props"]["errors"], serde_json::json!({}));
        assert_eq!(page["props"]["users"], serde_json::json!(["Ada", "Grace"]));
        assert!(page["props"].get("stats").is_none());
    }

    #[test]
    fn rocket_partial_reload_includes_only_requested_props() {
        let client = Client::tracked(rocket()).unwrap();

        let resp = client
            .get("/advanced")
            .header(Header::new(X_INERTIA, "true"))
            .header(Header::new(X_INERTIA_VERSION, CURRENT_VERSION))
            .header(Header::new(X_INERTIA_PARTIAL_COMPONENT, "advanced"))
            .header(Header::new(X_INERTIA_PARTIAL_DATA, "stats"))
            .dispatch();

        assert_eq!(resp.status(), Status::Ok);

        let body = resp.into_string().unwrap();
        let page: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(
            page["props"],
            serde_json::json!({
                "errors": {},
                "stats": 42,
                "users": ["Ada", "Grace"]
            })
        );
    }

    #[test]
    fn rocket_partial_except_takes_precedence_over_partial_data() {
        let client = Client::tracked(rocket()).unwrap();

        let resp = client
            .get("/advanced")
            .header(Header::new(X_INERTIA, "true"))
            .header(Header::new(X_INERTIA_VERSION, CURRENT_VERSION))
            .header(Header::new(X_INERTIA_PARTIAL_COMPONENT, "advanced"))
            .header(Header::new(X_INERTIA_PARTIAL_DATA, "stats"))
            .header(Header::new(X_INERTIA_PARTIAL_EXCEPT, "notifications"))
            .dispatch();

        assert_eq!(resp.status(), Status::Ok);

        let body = resp.into_string().unwrap();
        let page: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(page["props"]["errors"], serde_json::json!({}));
        assert_eq!(page["props"]["users"], serde_json::json!(["Ada", "Grace"]));
        assert_eq!(page["props"]["plans"], serde_json::json!(["basic"]));
        assert!(page["props"].get("notifications").is_none());
        assert_eq!(page["props"]["stats"], 42);
    }

    #[test]
    fn rocket_partial_reload_ignores_component_mismatch() {
        let client = Client::tracked(rocket()).unwrap();

        let resp = client
            .get("/advanced")
            .header(Header::new(X_INERTIA, "true"))
            .header(Header::new(X_INERTIA_VERSION, CURRENT_VERSION))
            .header(Header::new(X_INERTIA_PARTIAL_COMPONENT, "different"))
            .header(Header::new(X_INERTIA_PARTIAL_DATA, "stats"))
            .dispatch();

        assert_eq!(resp.status(), Status::Ok);

        let body = resp.into_string().unwrap();
        let page: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(page["props"]["errors"], serde_json::json!({}));
        assert_eq!(page["props"]["users"], serde_json::json!(["Ada", "Grace"]));
        assert_eq!(page["props"]["plans"], serde_json::json!(["basic"]));
        assert!(page["props"].get("stats").is_none());
    }

    #[test]
    fn rocket_non_get_response_ignores_partial_reload_filtering() {
        let client = Client::tracked(rocket()).unwrap();

        let resp = client
            .post("/write")
            .header(Header::new(X_INERTIA, "true"))
            .header(Header::new(X_INERTIA_PARTIAL_COMPONENT, "write"))
            .header(Header::new(X_INERTIA_PARTIAL_DATA, "users"))
            .dispatch();

        assert_eq!(resp.status(), Status::Ok);

        let body = resp.into_string().unwrap();
        let page: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(page["props"]["errors"], serde_json::json!({}));
        assert_eq!(page["props"]["users"], serde_json::json!(["Ada", "Grace"]));
        assert_eq!(page["props"]["stats"], 42);
        assert_eq!(page["props"]["plans"], serde_json::json!(["basic"]));
        assert_eq!(
            page["props"]["notifications"],
            serde_json::json!(["welcome"])
        );
    }

    #[test]
    fn rocket_non_get_response_preserves_once_prop_exclusions() {
        let client = Client::tracked(rocket()).unwrap();

        let resp = client
            .post("/write")
            .header(Header::new(X_INERTIA, "true"))
            .header(Header::new(X_INERTIA_PARTIAL_COMPONENT, "write"))
            .header(Header::new(X_INERTIA_PARTIAL_DATA, "users"))
            .header(Header::new(X_INERTIA_EXCEPT_ONCE_PROPS, "plans"))
            .dispatch();

        assert_eq!(resp.status(), Status::Ok);

        let body = resp.into_string().unwrap();
        let page: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(page["props"]["errors"], serde_json::json!({}));
        assert_eq!(page["props"]["users"], serde_json::json!(["Ada", "Grace"]));
        assert_eq!(page["props"]["stats"], 42);
        assert!(page["props"].get("plans").is_none());
        assert_eq!(
            page["props"]["notifications"],
            serde_json::json!(["welcome"])
        );
        assert_eq!(
            page["onceProps"]["plans"],
            serde_json::json!({ "prop": "plans", "expiresAt": null })
        );
    }

    #[test]
    fn rocket_reset_omits_merge_and_scroll_metadata_for_reset_props() {
        let client = Client::tracked(rocket()).unwrap();

        let resp = client
            .get("/scrolling")
            .header(Header::new(X_INERTIA, "true"))
            .header(Header::new(X_INERTIA_VERSION, CURRENT_VERSION))
            .header(Header::new(X_INERTIA_PARTIAL_COMPONENT, "scrolling"))
            .header(Header::new(X_INERTIA_PARTIAL_DATA, "posts"))
            .header(Header::new(X_INERTIA_RESET, "posts"))
            .dispatch();

        assert_eq!(resp.status(), Status::Ok);

        let body = resp.into_string().unwrap();
        let page: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(page["props"]["posts"]["data"], serde_json::json!([1, 2]));
        assert!(page.get("mergeProps").is_none());
        assert!(page.get("prependProps").is_none());
        assert!(page.get("scrollProps").is_none());
    }

    #[test]
    fn rocket_infinite_scroll_prepend_intent_sets_prepend_metadata() {
        let client = Client::tracked(rocket()).unwrap();

        let resp = client
            .get("/scrolling")
            .header(Header::new(X_INERTIA, "true"))
            .header(Header::new(X_INERTIA_VERSION, CURRENT_VERSION))
            .header(Header::new(X_INERTIA_PARTIAL_COMPONENT, "scrolling"))
            .header(Header::new(X_INERTIA_PARTIAL_DATA, "posts"))
            .header(Header::new(
                X_INERTIA_INFINITE_SCROLL_MERGE_INTENT,
                "prepend",
            ))
            .dispatch();

        assert_eq!(resp.status(), Status::Ok);

        let body = resp.into_string().unwrap();
        let page: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(page["prependProps"], serde_json::json!(["posts.data"]));
        assert!(page.get("mergeProps").is_none());
        assert_eq!(page["scrollProps"]["posts"]["nextPage"], 2);
    }

    #[test]
    fn rocket_once_props_already_on_client_are_omitted() {
        let client = Client::tracked(rocket()).unwrap();

        let resp = client
            .get("/advanced")
            .header(Header::new(X_INERTIA, "true"))
            .header(Header::new(X_INERTIA_VERSION, CURRENT_VERSION))
            .header(Header::new(X_INERTIA_EXCEPT_ONCE_PROPS, "plans"))
            .dispatch();

        assert_eq!(resp.status(), Status::Ok);

        let body = resp.into_string().unwrap();
        let page: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert!(page["props"].get("plans").is_none());
        assert_eq!(
            page["onceProps"]["plans"],
            serde_json::json!({ "prop": "plans", "expiresAt": null })
        );
    }

    #[test]
    fn external_location_response_uses_inertia_conflict_header_for_inertia_requests() {
        let client = Client::tracked(rocket()).unwrap();

        let resp = client
            .get("/external")
            .header(Header::new(X_INERTIA, "true"))
            .header(Header::new(X_INERTIA_VERSION, CURRENT_VERSION))
            .dispatch();

        assert_eq!(resp.status(), Status::Conflict);
        assert_eq!(
            resp.headers().get_one(X_INERTIA_LOCATION),
            Some("https://example.com/outside")
        );
        assert_eq!(resp.headers().get_one(VARY), Some(X_INERTIA));
    }

    #[test]
    fn external_location_response_falls_back_to_normal_redirect_for_direct_requests() {
        let client = Client::tracked(rocket()).unwrap();

        let resp = client.get("/external").dispatch();

        assert_eq!(resp.status(), Status::Found);
        assert_eq!(
            resp.headers().get_one("Location"),
            Some("https://example.com/outside")
        );
        assert_eq!(resp.headers().get_one(VARY), Some(X_INERTIA));
    }

    #[test]
    fn stale_version_location_route_conflicts_before_falling_back_to_direct_redirect() {
        let client = Client::tracked(rocket()).unwrap();

        let resp = client
            .get("/external")
            .header(Header::new(X_INERTIA, "true"))
            .header(Header::new(X_INERTIA_VERSION, "stale"))
            .dispatch();

        assert_eq!(resp.status(), Status::Conflict);
        assert_eq!(
            resp.headers().get_one(X_INERTIA_LOCATION),
            Some("/external")
        );
    }

    #[test]
    fn external_location_response_rejects_invalid_uri_references() {
        let client = Client::tracked(rocket()).unwrap();

        let resp = client
            .get("/bad-external")
            .header(Header::new(X_INERTIA, "true"))
            .header(Header::new(X_INERTIA_VERSION, CURRENT_VERSION))
            .dispatch();

        assert_eq!(resp.status(), Status::InternalServerError);
        assert_eq!(resp.headers().get_one(X_INERTIA_LOCATION), None);
    }

    #[test]
    fn get_redirect_uses_found_status() {
        let client = Client::tracked(rocket()).unwrap();

        let resp = client.get("/go").dispatch();

        assert_eq!(resp.status(), Status::Found);
        assert_eq!(resp.headers().get_one("Location"), Some("/target"));
    }

    #[test]
    fn write_redirects_use_see_other_status() {
        let client = Client::tracked(rocket()).unwrap();

        let resp = client.post("/go").dispatch();
        assert_eq!(resp.status(), Status::SeeOther);
        assert_eq!(resp.headers().get_one("Location"), Some("/target"));

        let resp = client.put("/go").dispatch();
        assert_eq!(resp.status(), Status::SeeOther);
        assert_eq!(resp.headers().get_one("Location"), Some("/target"));

        let resp = client.patch("/go").dispatch();
        assert_eq!(resp.status(), Status::SeeOther);
        assert_eq!(resp.headers().get_one("Location"), Some("/target"));

        let resp = client.delete("/go").dispatch();
        assert_eq!(resp.status(), Status::SeeOther);
        assert_eq!(resp.headers().get_one("Location"), Some("/target"));
    }

    #[test]
    fn redirect_response_rejects_invalid_uri_references() {
        let client = Client::tracked(rocket()).unwrap();

        let resp = client.get("/bad-go").dispatch();

        assert_eq!(resp.status(), Status::InternalServerError);
        assert_eq!(resp.headers().get_one("Location"), None);
    }

    #[test]
    fn json_sent_versions_different() {
        let client = Client::tracked(rocket()).unwrap();

        let req = client
            .get("/foo")
            .header(Header::new(X_INERTIA, "true"))
            .header(Header::new(X_INERTIA_VERSION, "OUTDATED_VERSION"));

        let resp = req.dispatch();

        assert_eq!(resp.status(), Status::Conflict);
    }

    #[test]
    fn json_sent_version_absent() {
        let client = Client::tracked(rocket()).unwrap();

        let req = client.get("/foo").header(Header::new(X_INERTIA, "true"));

        let resp = req.dispatch();

        assert_eq!(resp.status(), Status::Conflict);
    }

    #[test]
    fn not_found_response() {
        let client = Client::tracked(rocket()).unwrap();

        let req = client
            .get("/not/a/real/path")
            .header(Header::new(X_INERTIA, "true"))
            .header(Header::new(X_INERTIA_VERSION, CURRENT_VERSION));

        let resp = req.dispatch();

        assert_eq!(resp.status(), Status::NotFound);
    }
}
