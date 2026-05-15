#![allow(clippy::type_complexity)]

//! Rocket integration for `inertia_rs`.

use super::{Inertia, VARY, X_INERTIA, X_INERTIA_LOCATION, X_INERTIA_VERSION};
use rocket::fairing::{Fairing, Info, Kind};
use rocket::http::{self, Method};
use rocket::request::{FromRequest, Outcome, Request};
use rocket::response::{self, Responder, Response};
use rocket::serde::json::Json;
use rocket::Data;
use rocket::{error, get, routes, uri};
use serde::Serialize;
use std::sync::Arc;
use tracing::trace;

#[derive(Serialize)]
struct InertiaResponse<T> {
    component: String,
    props: T,
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<InertiaVersion>,
}

const BASE_ROUTE: &str = "/inertia-rs";

trait InertiaRequest {
    fn inertia_request(&self) -> bool;

    fn inertia_version(&self) -> Option<String>;
}

impl<'a> InertiaRequest for Request<'a> {
    fn inertia_request(&self) -> bool {
        self.headers().get_one(X_INERTIA).is_some()
    }

    fn inertia_version(&self) -> Option<String> {
        self.headers().get_one(X_INERTIA_VERSION).map(|s| s.into())
    }
}

/// Parsed Inertia headers for use as a Rocket request guard.
///
/// This guard always succeeds. It lets route handlers branch on whether a
/// request came from the Inertia client without manually reading raw headers.
pub struct InertiaHeaders {
    is_inertia: bool,
    version: Option<String>,
}

impl InertiaHeaders {
    /// Returns `true` when the request includes the `X-Inertia` header.
    pub fn is_inertia(&self) -> bool {
        self.is_inertia
    }

    /// Returns the request's `X-Inertia-Version` header value.
    pub fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }
}

#[rocket::async_trait]
impl<'r> FromRequest<'r> for InertiaHeaders {
    type Error = std::convert::Infallible;

    async fn from_request(request: &'r Request<'_>) -> Outcome<Self, Self::Error> {
        Outcome::Success(Self {
            is_inertia: request.inertia_request(),
            version: request.inertia_version(),
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

#[derive(Serialize, Clone)]
struct InertiaVersion(String);

impl AsRef<str> for InertiaVersion {
    fn as_ref(&self) -> &str {
        self.0.as_ref()
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

fn add_vary_header<'r>(response: Response<'r>) -> Response<'r> {
    Response::build_from(response)
        .raw_header_adjoin(VARY, X_INERTIA)
        .finalize()
}

impl<'r, 'o: 'r, R: Serialize> Responder<'r, 'o> for Inertia<R> {
    #[inline(always)]
    fn respond_to(self, request: &'r Request<'_>) -> response::Result<'o> {
        let url = self.url.unwrap_or_else(|| request.uri().to_string());
        let version = request.local_cache(|| None);

        let inertia_response = InertiaResponse {
            component: self.component,
            props: self.props,
            url,
            version: version.clone(),
        };

        if request.inertia_request() {
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
    location.starts_with('/') && !location.starts_with("//")
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
        let current_version = InertiaVersion(self.version.clone());
        request.local_cache(|| Some(current_version));

        if request.method() == Method::Get && request.inertia_request() {
            let request_version = request.inertia_version();

            trace!(
                "request version {:?} / asset version {}",
                &request_version,
                &self.version
            );

            if request_version.as_ref() != Some(&self.version) {
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
    use rocket::{
        http::{Header, Status},
        local::blocking::Client,
    };

    #[derive(Serialize)]
    struct Props {
        n: i32,
    }

    #[derive(Serialize)]
    struct TextProps {
        text: String,
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

    #[get("/headers")]
    fn headers(headers: InertiaHeaders) -> String {
        format!(
            "{}:{}",
            headers.is_inertia(),
            headers.version().unwrap_or("none")
        )
    }

    const CURRENT_VERSION: &str = "1";

    fn rocket() -> rocket::Rocket<rocket::Build> {
        rocket::build()
            .mount("/", routes![foo, url_override, unsafe_props, headers])
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
    fn inertia_headers_guard_handles_regular_requests() {
        let client = Client::tracked(rocket_without_fairing()).unwrap();

        let resp = client.get("/headers").dispatch();

        assert_eq!(resp.into_string().unwrap(), "false:none");
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
