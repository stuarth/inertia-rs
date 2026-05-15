#![allow(clippy::type_complexity)]

//! Rocket integration for `inertia_rs`.

use super::{
    html_response_context, Inertia, Location, Redirect, RequestContext, VARY, X_INERTIA,
    X_INERTIA_LOCATION,
};
use rocket::fairing::{Fairing, Info, Kind};
use rocket::http::{self, uri::Reference, Method};
use rocket::request::{FromRequest, Outcome, Request};
use rocket::response::{self, Redirect as RocketRedirect, Responder, Response};
use rocket::serde::json::Json;
use rocket::Data;
use rocket::{error, get, routes, uri};
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
use tracing::trace;

pub use super::HtmlResponseContext;

const BASE_ROUTE: &str = "/inertia-rs";

type SharedPropProvider = Arc<
    dyn for<'request, 'rocket> Fn(&'request Request<'rocket>) -> Result<Value, serde_json::Error>
        + Send
        + Sync,
>;
type VersionProvider = Arc<dyn Fn() -> String + Send + Sync>;

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

/// Shared Inertia props resolved for every Rocket page response.
///
/// Register this as Rocket managed state with [`rocket::Rocket::manage`].
/// Shared props are shallow-merged into page props; route props win on key
/// collisions. Providers run once per page response and may inspect the
/// current request. Dotted keys, such as `auth.user`, are expanded into
/// nested props.
///
/// Shared props are merged after partial-reload filtering, so they remain
/// present on partial responses even when omitted from `only` or `except`
/// reload options.
///
/// ```rust
/// use inertia_rs::rocket::SharedProps;
///
/// let shared_props = SharedProps::new()
///     .value("appName", "My App")
///     .prop("auth.csrfToken", |request| {
///         request.headers().get_one("X-CSRF").map(ToOwned::to_owned)
///     });
/// ```
#[derive(Clone, Default)]
pub struct SharedProps {
    providers: Vec<(String, SharedPropProvider)>,
}

impl SharedProps {
    /// Creates an empty shared prop registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a fixed serializable shared prop value.
    pub fn value<K, T>(self, key: K, value: T) -> Self
    where
        K: Into<String>,
        T: Clone + Send + Sync + Serialize + 'static,
    {
        self.prop(key, move |_request| value.clone())
    }

    /// Registers a request-aware shared prop provider.
    ///
    /// The provider should return an owned serializable value. For request
    /// data such as headers or cookies, clone the value before returning it.
    pub fn prop<K, F, T>(mut self, key: K, provider: F) -> Self
    where
        K: Into<String>,
        F: for<'request, 'rocket> Fn(&'request Request<'rocket>) -> T + Send + Sync + 'static,
        T: Serialize,
    {
        let provider =
            Arc::new(move |request: &Request<'_>| serde_json::to_value(provider(request)));

        self.providers.push((key.into(), provider));
        self
    }

    /// Returns `true` when no shared props have been registered.
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    fn resolve(&self, request: &Request<'_>) -> Result<Vec<(String, Value)>, serde_json::Error> {
        self.providers
            .iter()
            .map(|(key, provider)| provider(request).map(|value| (key.clone(), value)))
            .collect()
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

#[derive(Clone)]
struct InertiaVersion(String);

struct VersionProviderFn(VersionProvider);

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
        let version_provider = request
            .rocket()
            .state::<VersionProviderFn>()
            .map(|provider| provider.0.clone());
        let version = request
            .local_cache(|| version_provider.map(|provider| InertiaVersion(provider())))
            .clone()
            .map(|version| version.0);
        let context = if request.method() == Method::Get {
            request_context(request)
        } else {
            request_context(request).without_partial_reload()
        };
        let mut inertia_response = self
            .into_page(url, version, &context)
            .map_err(|_e| http::Status::InternalServerError)?;

        if let Some(shared_props) = request.rocket().state::<SharedProps>() {
            inertia_response = inertia_response.with_shared_props(
                shared_props
                    .resolve(request)
                    .map_err(|_e| http::Status::InternalServerError)?,
            );
        }

        if context.is_inertia() {
            Response::build()
                .merge(Json(inertia_response).respond_to(request)?)
                .raw_header(X_INERTIA, "true")
                .raw_header_adjoin(VARY, X_INERTIA)
                .ok()
        } else {
            let ctx = html_response_context(&inertia_response)
                .map_err(|_e| http::Status::InternalServerError)?;

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
    version_provider: VersionProvider,
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
        let version = version.into();

        Self::dynamic(move || version.clone(), html_response)
    }

    /// Creates a fairing with a dynamic asset-version provider and HTML renderer.
    ///
    /// The provider is called when a page response or Inertia version check
    /// needs the current version. Its value is included in successful page
    /// objects and compared with `X-Inertia-Version` for Inertia `GET`
    /// requests. Keep the provider fast and non-blocking; if the version is
    /// loaded from disk or another external source, cache it in application
    /// state and read the cached value here.
    pub fn dynamic<F, H, V>(version_provider: F, html_response: H) -> Self
    where
        F: Fn() -> V + Send + Sync + 'static,
        H: Fn(&Request<'_>, &HtmlResponseContext) -> response::Result<'resp>
            + Send
            + Sync
            + 'static,
        V: Into<String>,
    {
        Self {
            version_provider: Arc::new(move || version_provider().into()),
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
            .manage(VersionProviderFn(self.version_provider.clone()))
            .manage(ResponderFn(self.html_response.clone())))
    }

    async fn on_request(&self, request: &mut Request<'_>, _: &mut Data<'_>) {
        let context = request_context(request);

        if request.method() == Method::Get && context.is_inertia() {
            let version = request
                .local_cache(|| Some(InertiaVersion((self.version_provider)())))
                .clone()
                .expect("version fairing caches a version for Inertia GET requests")
                .0;
            let request_version = context.version();

            trace!(
                "request version {:?} / asset version {}",
                request_version,
                &version
            );

            if request_version != Some(version.as_str()) {
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
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
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

    #[get("/route-auth")]
    fn route_auth() -> Inertia<serde_json::Value> {
        Inertia::response(
            "route-auth",
            serde_json::json!({
                "auth": {
                    "user": {
                        "name": "Route"
                    }
                }
            }),
        )
    }

    #[get("/empty")]
    fn empty() -> Inertia<()> {
        Inertia::response("empty", ())
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
                    route_auth,
                    empty,
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

    fn rocket_with_shared_props() -> rocket::Rocket<rocket::Build> {
        rocket().manage(
            SharedProps::new()
                .value("appName", "Demo")
                .value("n", 99)
                .value("auth.user", serde_json::json!({ "name": "Ada" }))
                .prop("csrfToken", |request| {
                    request.headers().get_one("X-CSRF").map(ToOwned::to_owned)
                }),
        )
    }

    fn rocket_with_dynamic_version(version: Arc<AtomicUsize>) -> rocket::Rocket<rocket::Build> {
        rocket::build()
            .mount("/", routes![foo])
            .attach(VersionFairing::dynamic(
                move || format!("dynamic-{}", version.load(Ordering::SeqCst)),
                |request, ctx| serde_json::to_string(ctx).unwrap().respond_to(request),
            ))
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
    fn dynamic_version_is_resolved_for_page_responses() {
        let version = Arc::new(AtomicUsize::new(1));
        let client = Client::tracked(rocket_with_dynamic_version(version.clone())).unwrap();

        version.store(2, Ordering::SeqCst);

        let resp = client
            .get("/foo")
            .header(Header::new(X_INERTIA, "true"))
            .header(Header::new(X_INERTIA_VERSION, "dynamic-2"))
            .dispatch();
        let body = resp.into_string().unwrap();
        let page: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(page["version"], "dynamic-2");
    }

    #[test]
    fn dynamic_version_is_used_for_conflict_checks() {
        let version = Arc::new(AtomicUsize::new(1));
        let client = Client::tracked(rocket_with_dynamic_version(version.clone())).unwrap();

        version.store(3, Ordering::SeqCst);

        let resp = client
            .get("/foo")
            .header(Header::new(X_INERTIA, "true"))
            .header(Header::new(X_INERTIA_VERSION, "dynamic-2"))
            .dispatch();

        assert_eq!(resp.status(), Status::Conflict);
        assert_eq!(resp.headers().get_one(X_INERTIA_LOCATION), Some("/foo"));
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
    fn shared_props_are_merged_into_html_responses() {
        let client = Client::tracked(rocket_with_shared_props()).unwrap();

        let resp = client
            .get("/foo")
            .header(Header::new("X-CSRF", "token-html"))
            .dispatch();
        let body = resp.into_string().unwrap();
        let ctx: serde_json::Value = serde_json::from_str(&body).unwrap();
        let data_page = ctx["data_page"].as_str().unwrap();
        let page: serde_json::Value = serde_json::from_str(data_page).unwrap();

        assert_eq!(page["props"]["appName"], "Demo");
        assert_eq!(page["props"]["auth"]["user"]["name"], "Ada");
        assert_eq!(page["props"]["csrfToken"], "token-html");
        assert_eq!(page["props"]["n"], 42);
        assert_eq!(
            page["sharedProps"],
            serde_json::json!(["appName", "auth", "csrfToken"])
        );
    }

    #[test]
    fn shared_props_are_merged_into_json_responses() {
        let client = Client::tracked(rocket_with_shared_props()).unwrap();

        let resp = client
            .get("/foo")
            .header(Header::new(X_INERTIA, "true"))
            .header(Header::new(X_INERTIA_VERSION, CURRENT_VERSION))
            .header(Header::new("X-CSRF", "token-json"))
            .dispatch();

        assert_eq!(resp.status(), Status::Ok);

        let body = resp.into_string().unwrap();
        let page: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(page["props"]["appName"], "Demo");
        assert_eq!(page["props"]["auth"]["user"]["name"], "Ada");
        assert_eq!(page["props"]["csrfToken"], "token-json");
        assert_eq!(page["props"]["n"], 42);
        assert_eq!(
            page["sharedProps"],
            serde_json::json!(["appName", "auth", "csrfToken"])
        );
    }

    #[test]
    fn shared_props_turn_empty_props_into_an_object() {
        let client = Client::tracked(rocket_with_shared_props()).unwrap();

        let resp = client
            .get("/empty")
            .header(Header::new(X_INERTIA, "true"))
            .header(Header::new(X_INERTIA_VERSION, CURRENT_VERSION))
            .dispatch();

        assert_eq!(resp.status(), Status::Ok);

        let body = resp.into_string().unwrap();
        let page: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(
            page["props"],
            serde_json::json!({
                "errors": {},
                "appName": "Demo",
                "n": 99,
                "auth": {
                    "user": {
                        "name": "Ada"
                    }
                },
                "csrfToken": null
            })
        );
        assert_eq!(
            page["sharedProps"],
            serde_json::json!(["appName", "n", "auth", "csrfToken"])
        );
    }

    #[test]
    fn shared_dotted_props_do_not_merge_into_route_owned_roots() {
        let client = Client::tracked(rocket_with_shared_props()).unwrap();

        let resp = client
            .get("/route-auth")
            .header(Header::new(X_INERTIA, "true"))
            .header(Header::new(X_INERTIA_VERSION, CURRENT_VERSION))
            .dispatch();

        assert_eq!(resp.status(), Status::Ok);

        let body = resp.into_string().unwrap();
        let page: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(page["props"]["auth"]["user"]["name"], "Route");
        assert_eq!(page["props"]["appName"], "Demo");
        assert_eq!(page["props"]["n"], 99);
        assert_eq!(page["props"]["csrfToken"], serde_json::Value::Null);
        assert_eq!(
            page["sharedProps"],
            serde_json::json!(["appName", "n", "csrfToken"])
        );
    }

    #[test]
    fn shared_dotted_props_do_not_replace_filtered_route_owned_roots() {
        let client = Client::tracked(rocket_with_shared_props()).unwrap();

        let resp = client
            .get("/route-auth")
            .header(Header::new(X_INERTIA, "true"))
            .header(Header::new(X_INERTIA_VERSION, CURRENT_VERSION))
            .header(Header::new(X_INERTIA_PARTIAL_COMPONENT, "route-auth"))
            .header(Header::new(X_INERTIA_PARTIAL_DATA, "missing"))
            .dispatch();

        assert_eq!(resp.status(), Status::Ok);

        let body = resp.into_string().unwrap();
        let page: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert!(page["props"].get("auth").is_none());
        assert_eq!(page["props"]["appName"], "Demo");
        assert_eq!(page["props"]["n"], 99);
        assert_eq!(page["props"]["csrfToken"], serde_json::Value::Null);
        assert_eq!(
            page["sharedProps"],
            serde_json::json!(["appName", "n", "csrfToken"])
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
    fn rocket_partial_reload_includes_shared_props() {
        let client = Client::tracked(rocket_with_shared_props()).unwrap();

        let resp = client
            .get("/advanced")
            .header(Header::new(X_INERTIA, "true"))
            .header(Header::new(X_INERTIA_VERSION, CURRENT_VERSION))
            .header(Header::new(X_INERTIA_PARTIAL_COMPONENT, "advanced"))
            .header(Header::new(X_INERTIA_PARTIAL_DATA, "stats"))
            .header(Header::new("X-CSRF", "token-partial"))
            .dispatch();

        assert_eq!(resp.status(), Status::Ok);

        let body = resp.into_string().unwrap();
        let page: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(page["props"]["stats"], 42);
        assert_eq!(page["props"]["users"], serde_json::json!(["Ada", "Grace"]));
        assert_eq!(page["props"]["appName"], "Demo");
        assert_eq!(page["props"]["auth"]["user"]["name"], "Ada");
        assert_eq!(page["props"]["csrfToken"], "token-partial");
        assert_eq!(page["props"]["n"], 99);
        assert_eq!(
            page["sharedProps"],
            serde_json::json!(["users", "appName", "n", "auth", "csrfToken"])
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
