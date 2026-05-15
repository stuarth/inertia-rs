//! Axum integration for `inertia_rs`.

use super::{
    html_response_context, Inertia, IntoPageProps, Location, Page, Redirect, RequestContext, VARY,
    X_INERTIA, X_INERTIA_LOCATION,
};
use ::axum::extract::{FromRequestParts, OriginalUri};
use ::axum::http::header::{InvalidHeaderValue, LOCATION};
use ::axum::http::request::Parts;
use ::axum::http::uri::Uri;
use ::axum::http::{Extensions, HeaderMap, HeaderValue, Method, Request, StatusCode};
use ::axum::response::{IntoResponse, Response};
use ::axum::Json;
use fluent_uri::{ParseError, UriRef};
use serde::Serialize;
use serde_json::Value;
use std::convert::Infallible;
use std::error::Error;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tower::{Layer, Service};
use tracing::error;

pub use super::HtmlResponseContext;

type SharedPropProvider =
    Arc<dyn Fn(&InertiaRequest) -> Result<Option<Value>, serde_json::Error> + Send + Sync>;
type VersionProvider = Arc<dyn Fn() -> String + Send + Sync>;
type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

fn header<'headers>(headers: &'headers HeaderMap, name: &str) -> Option<&'headers str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

fn request_context(headers: &HeaderMap) -> RequestContext {
    RequestContext::from_header_fn(|name| header(headers, name))
}

fn add_vary_header(response: &mut Response) {
    response
        .headers_mut()
        .append(VARY, HeaderValue::from_static(X_INERTIA));
}

fn is_write_method(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

fn validate_uri_reference(url: &str) -> Result<(), InertiaError> {
    UriRef::parse(url)
        .map(|_uri| ())
        .map_err(InertiaError::invalid_uri)
}

fn location_header(url: &str) -> Result<HeaderValue, InertiaError> {
    validate_uri_reference(url)?;
    HeaderValue::from_str(url).map_err(InertiaError::invalid_header)
}

fn local_uri(uri: &Uri) -> String {
    uri.path_and_query()
        .map(|path_and_query| path_and_query.as_str().to_owned())
        .unwrap_or_else(|| "/".to_owned())
}

fn original_uri_from_extensions<B>(request: &Request<B>) -> String {
    request
        .extensions()
        .get::<OriginalUri>()
        .map(|original_uri| local_uri(&original_uri.0))
        .unwrap_or_else(|| local_uri(request.uri()))
}

fn redirect_response(status: StatusCode, url: &str) -> Result<Response, InertiaError> {
    let mut response = status.into_response();
    response
        .headers_mut()
        .insert(LOCATION, location_header(url)?);
    add_vary_header(&mut response);
    Ok(response)
}

fn conflict_response(url: &str) -> Result<Response, InertiaError> {
    let mut response = StatusCode::CONFLICT.into_response();
    response
        .headers_mut()
        .insert(X_INERTIA_LOCATION, location_header(url)?);
    add_vary_header(&mut response);
    Ok(response)
}

/// Error returned while building Axum Inertia responses.
#[derive(Debug)]
pub enum InertiaError {
    /// The page object could not be serialized.
    Serialization(serde_json::Error),
    /// A response header value could not be constructed.
    InvalidHeader(InvalidHeaderValue),
    /// A redirect or location URL was not a valid URI reference.
    InvalidUri(ParseError),
}

impl InertiaError {
    fn invalid_header(error: InvalidHeaderValue) -> Self {
        Self::InvalidHeader(error)
    }

    fn invalid_uri(error: ParseError) -> Self {
        Self::InvalidUri(error)
    }
}

impl fmt::Display for InertiaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Serialization(error) => write!(f, "failed to serialize Inertia page: {error}"),
            Self::InvalidHeader(error) => write!(f, "invalid Inertia response header: {error}"),
            Self::InvalidUri(error) => write!(f, "invalid Inertia URI reference: {error}"),
        }
    }
}

impl Error for InertiaError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Serialization(error) => Some(error),
            Self::InvalidHeader(error) => Some(error),
            Self::InvalidUri(error) => Some(error),
        }
    }
}

impl From<serde_json::Error> for InertiaError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialization(error)
    }
}

impl IntoResponse for InertiaError {
    fn into_response(self) -> Response {
        error!(error = %self, "failed to build Axum Inertia response");

        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to build Inertia response",
        )
            .into_response()
    }
}

fn internal_error_response(error: InertiaError) -> Response {
    error.into_response()
}

/// Current asset version inserted by [`VersionLayer`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InertiaVersion(String);

impl InertiaVersion {
    /// Creates an asset version value for request extensions.
    pub fn new<V: Into<String>>(version: V) -> Self {
        Self(version.into())
    }

    /// Returns the asset version string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Shared Inertia props resolved for every Axum page response.
///
/// Register this as an Axum extension layer with [`axum::Extension`]. Shared
/// props are shallow-merged into page props; route props win on key collisions.
/// Providers run once per page response and may inspect the extracted
/// [`InertiaRequest`]. Dotted keys, such as `auth.user`, are expanded into
/// nested props.
///
/// Shared props are merged after partial-reload filtering, so they remain
/// present on partial responses even when omitted from `only` or `except`
/// reload options.
///
/// ```rust,no_run
/// use axum::{Extension, Router};
/// use inertia_rs::axum::{SharedProps, VersionLayer};
///
/// let shared_props = SharedProps::new()
///     .value("appName", "My App")
///     .prop_optional("auth.csrfToken", |request| {
///         request.extension::<String>().cloned()
///     });
///
/// let app: Router<()> = Router::new()
///     .layer(Extension(shared_props))
///     .layer(VersionLayer::new("asset-version-1"));
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
    /// The provider should return an owned serializable value. For values read
    /// from request extensions, clone the value before returning it.
    pub fn prop<K, F, T>(mut self, key: K, provider: F) -> Self
    where
        K: Into<String>,
        F: Fn(&InertiaRequest) -> T + Send + Sync + 'static,
        T: Serialize,
    {
        let provider = Arc::new(move |request: &InertiaRequest| {
            serde_json::to_value(provider(request)).map(Some)
        });

        self.providers.push((key.into(), provider));
        self
    }

    /// Registers a request-aware shared prop provider that can skip its key.
    ///
    /// Returning `None` omits the shared prop instead of serializing it as
    /// JSON `null`.
    pub fn prop_optional<K, F, T>(mut self, key: K, provider: F) -> Self
    where
        K: Into<String>,
        F: Fn(&InertiaRequest) -> Option<T> + Send + Sync + 'static,
        T: Serialize,
    {
        let provider = Arc::new(move |request: &InertiaRequest| {
            provider(request).map(serde_json::to_value).transpose()
        });

        self.providers.push((key.into(), provider));
        self
    }

    /// Returns `true` when no shared props have been registered.
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    fn resolve(
        &self,
        request: &InertiaRequest,
        page: &Page<Value>,
    ) -> Result<Vec<(String, Value)>, serde_json::Error> {
        self.providers
            .iter()
            .filter(|(key, _provider)| !page.owns_prop_root(key))
            .filter_map(|(key, provider)| match provider(request) {
                Ok(Some(value)) => Some(Ok((key.clone(), value))),
                Ok(None) => None,
                Err(error) => Some(Err(error)),
            })
            .collect()
    }
}

/// Axum request extractor for Inertia protocol state.
///
/// Use this extractor in handlers that return Inertia page responses. Pair it
/// with [`VersionLayer`] when page objects should include an asset version and
/// stale Inertia visits should receive a `409 Conflict` response.
#[derive(Clone)]
pub struct InertiaRequest {
    context: RequestContext,
    extensions: Option<Arc<Extensions>>,
    method: Method,
    shared_props: Option<SharedProps>,
    uri: String,
    version: Option<String>,
}

impl fmt::Debug for InertiaRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InertiaRequest")
            .field("context", &self.context)
            .field("method", &self.method)
            .field("uri", &self.uri)
            .field("version", &self.version)
            .field("has_shared_props", &self.shared_props.is_some())
            .finish()
    }
}

impl InertiaRequest {
    /// Returns `true` when the request includes the `X-Inertia` header.
    pub fn is_inertia(&self) -> bool {
        self.context.is_inertia()
    }

    /// Returns the parsed request context.
    pub fn context(&self) -> &RequestContext {
        &self.context
    }

    /// Returns the request URI used as the default page-object URL.
    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// Returns the current asset version installed by [`VersionLayer`].
    pub fn asset_version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    /// Returns a request extension value inserted by an Axum layer.
    ///
    /// `InertiaRequest` captures extensions at extraction time. Values inserted
    /// by earlier layers are available here; values inserted by later
    /// extractors depend on handler argument order. The extension snapshot is
    /// retained for shared-prop providers, so this returns `None` when no
    /// [`SharedProps`] extension is registered.
    pub fn extension<T>(&self) -> Option<&T>
    where
        T: Send + Sync + 'static,
    {
        self.extensions
            .as_ref()
            .and_then(|extensions| extensions.get::<T>())
    }

    /// Converts an [`Inertia`] value into an Axum response.
    ///
    /// Inertia requests receive a JSON page object with `X-Inertia: true`.
    /// Direct browser requests are rendered through `html_response`.
    pub fn render<T, F, R>(
        &self,
        inertia: Inertia<T>,
        html_response: F,
    ) -> Result<Response, InertiaError>
    where
        T: IntoPageProps,
        F: FnOnce(HtmlResponseContext) -> R,
        R: IntoResponse,
    {
        let context = if self.method == Method::GET {
            self.context.clone()
        } else {
            self.context.clone().without_partial_reload()
        };
        let url = inertia
            .url()
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| self.uri.clone());
        let mut page = inertia.into_page(url, self.version.clone(), &context)?;

        if let Some(shared_props) = &self.shared_props {
            if !shared_props.is_empty() {
                let mut shared_request = self.clone();
                shared_request.context = context.clone();
                let resolved_shared_props = shared_props.resolve(&shared_request, &page)?;

                if !resolved_shared_props.is_empty() {
                    page = page.with_shared_props(resolved_shared_props);
                }
            }
        }

        if context.is_inertia() {
            let mut response = Json(page).into_response();
            response
                .headers_mut()
                .insert(X_INERTIA, HeaderValue::from_static("true"));
            add_vary_header(&mut response);
            Ok(response)
        } else {
            let context = html_response_context(&page)?;
            let mut response = html_response(context).into_response();
            add_vary_header(&mut response);
            Ok(response)
        }
    }

    /// Converts an external Inertia location visit into an Axum response.
    ///
    /// Inertia requests receive `409 Conflict` with `X-Inertia-Location`.
    /// Direct browser requests fall back to a method-aware redirect.
    pub fn location(&self, location: Location) -> Result<Response, InertiaError> {
        if self.context.is_inertia() {
            conflict_response(location.url())
        } else if is_write_method(&self.method) {
            redirect_response(StatusCode::SEE_OTHER, location.url())
        } else {
            redirect_response(StatusCode::FOUND, location.url())
        }
    }

    /// Converts a method-aware redirect into an Axum response.
    pub fn redirect(&self, redirect: Redirect) -> Result<Response, InertiaError> {
        if is_write_method(&self.method) {
            redirect_response(StatusCode::SEE_OTHER, redirect.url())
        } else {
            redirect_response(StatusCode::FOUND, redirect.url())
        }
    }
}

impl<S> FromRequestParts<S> for InertiaRequest
where
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let context = request_context(&parts.headers);
        let version = parts
            .extensions
            .get::<InertiaVersion>()
            .map(|version| version.0.clone());
        let shared_props = parts.extensions.get::<SharedProps>().cloned();
        let extensions = shared_props.as_ref().map(|_shared_props| {
            let mut extensions = parts.extensions.clone();
            extensions.remove::<SharedProps>();
            Arc::new(extensions)
        });

        Ok(Self {
            context,
            extensions,
            method: parts.method.clone(),
            shared_props,
            uri: parts
                .extensions
                .get::<OriginalUri>()
                .map(|original_uri| local_uri(&original_uri.0))
                .unwrap_or_else(|| local_uri(&parts.uri)),
            version,
        })
    }
}

/// Tower layer that installs Inertia asset version handling for Axum apps.
///
/// The layer inserts the current version into request extensions so
/// [`InertiaRequest::render`] can include it in page objects. For Inertia `GET`
/// requests whose `X-Inertia-Version` is missing or stale, it returns
/// `409 Conflict` with `X-Inertia-Location` before the route handler runs.
#[derive(Clone)]
pub struct VersionLayer {
    version_provider: VersionProvider,
}

impl VersionLayer {
    /// Creates a layer with a static asset `version`.
    pub fn new<V: Into<String>>(version: V) -> Self {
        let version = version.into();

        Self::dynamic(move || version.clone())
    }

    /// Creates a layer with a dynamic asset-version provider.
    ///
    /// Keep the provider fast and non-blocking. If the version is loaded from
    /// disk or a manifest, cache it in application state and read the cached
    /// value here.
    pub fn dynamic<F, V>(version_provider: F) -> Self
    where
        F: Fn() -> V + Send + Sync + 'static,
        V: Into<String>,
    {
        Self {
            version_provider: Arc::new(move || version_provider().into()),
        }
    }
}

impl<S> Layer<S> for VersionLayer {
    type Service = VersionService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        VersionService {
            inner,
            version_provider: self.version_provider.clone(),
        }
    }
}

/// Service produced by [`VersionLayer`].
#[derive(Clone)]
pub struct VersionService<S> {
    inner: S,
    version_provider: VersionProvider,
}

impl<S, B> Service<Request<B>> for VersionService<S>
where
    S: Service<Request<B>, Response = Response> + Send + 'static,
    S::Future: Send + 'static,
    B: Send + 'static,
{
    type Response = Response;
    type Error = S::Error;
    type Future = BoxFuture<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut request: Request<B>) -> Self::Future {
        let version = (self.version_provider)();
        let request_version = header(request.headers(), super::X_INERTIA_VERSION);

        if request.method() == Method::GET
            && header(request.headers(), X_INERTIA).is_some()
            && request_version != Some(version.as_str())
        {
            let response = conflict_response(&original_uri_from_extensions(&request))
                .unwrap_or_else(internal_error_response);

            return Box::pin(async move { Ok(response) });
        }

        request
            .extensions_mut()
            .insert(InertiaVersion::new(version));

        let future = self.inner.call(request);
        Box::pin(future)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        InertiaProps, X_INERTIA_EXCEPT_ONCE_PROPS, X_INERTIA_PARTIAL_COMPONENT,
        X_INERTIA_PARTIAL_DATA, X_INERTIA_VERSION,
    };
    use ::axum::body::Body;
    use ::axum::http::header::CONTENT_TYPE;
    use ::axum::http::{Request, StatusCode};
    use ::axum::response::Html;
    use ::axum::routing::get;
    use ::axum::Extension;
    use ::axum::Router;
    use serde::Serialize;
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use tower::ServiceExt;

    #[derive(Serialize)]
    struct Props {
        n: i32,
        plans: Vec<&'static str>,
        stats: i32,
        notifications: Vec<&'static str>,
    }

    #[derive(Serialize)]
    struct TextProps {
        text: String,
    }

    #[derive(Clone)]
    struct User {
        name: &'static str,
    }

    #[derive(Clone)]
    struct CsrfToken(String);

    async fn page(request: InertiaRequest) -> Result<Response, InertiaError> {
        request.render(
            Inertia::response(
                "foo",
                Props {
                    n: 42,
                    plans: vec!["basic"],
                    stats: 7,
                    notifications: vec!["welcome"],
                },
            )
            .once("plans")
            .defer("stats"),
            |context| {
                Html(format!(
                    "<!doctype html><script data-page=\"app\" type=\"application/json\">{}</script>",
                    context.data_page()
                ))
            },
        )
    }

    async fn custom_url(request: InertiaRequest) -> Result<Response, InertiaError> {
        request.render(
            Inertia::response(
                "custom",
                Props {
                    n: 42,
                    plans: vec!["basic"],
                    stats: 7,
                    notifications: vec!["welcome"],
                },
            )
            .with_url("/custom-url"),
            |context| Html(context.data_page().to_owned()),
        )
    }

    async fn route_auth(request: InertiaRequest) -> Result<Response, InertiaError> {
        request.render(
            Inertia::response(
                "route-auth",
                json!({
                    "auth": {
                        "user": {
                            "name": "Route"
                        }
                    }
                }),
            ),
            |context| Html(context.data_page().to_owned()),
        )
    }

    async fn builder_page(request: InertiaRequest) -> Result<Response, InertiaError> {
        request.render(
            Inertia::page("builder")
                .merge("notifications")
                .props(Props {
                    n: 42,
                    plans: vec!["basic"],
                    stats: 7,
                    notifications: vec!["welcome"],
                }),
            |context| Html(context.data_page().to_owned()),
        )
    }

    async fn lazy_page(request: InertiaRequest) -> Result<Response, InertiaError> {
        request.render(
            Inertia::response(
                "lazy",
                InertiaProps::new()
                    .value("users", json!(["Ada", "Grace"]))
                    .defer("stats", || 7)
                    .optional("audit", || json!(["created"])),
            ),
            |context| Html(context.data_page().to_owned()),
        )
    }

    async fn unsafe_page(request: InertiaRequest) -> Result<Response, InertiaError> {
        request.render(
            Inertia::response(
                "unsafe",
                TextProps {
                    text: "</script><script>alert(1)</script>&\u{2028}\u{2029}".into(),
                },
            ),
            |context| {
                Html(format!(
                    "<!doctype html><script data-page=\"app\" type=\"application/json\">{}</script>",
                    context.data_page()
                ))
            },
        )
    }

    async fn external(request: InertiaRequest) -> Result<Response, InertiaError> {
        request.location(Inertia::location("https://example.com/outside"))
    }

    async fn relative_location(request: InertiaRequest) -> Result<Response, InertiaError> {
        request.location(Inertia::location("../outside?from=axum#fragment"))
    }

    async fn bad_location(request: InertiaRequest) -> Result<Response, InertiaError> {
        request.location(Inertia::location("bad location"))
    }

    async fn redirect(request: InertiaRequest) -> Result<Response, InertiaError> {
        request.redirect(Inertia::redirect("/target"))
    }

    async fn relative_redirect(request: InertiaRequest) -> Result<Response, InertiaError> {
        request.redirect(Inertia::redirect("?next=target#fragment"))
    }

    async fn bad_redirect(request: InertiaRequest) -> Result<Response, InertiaError> {
        request.redirect(Inertia::redirect("bad location"))
    }

    fn app() -> Router {
        Router::new()
            .route("/foo", get(page).post(page))
            .route("/custom-url", get(custom_url))
            .route("/builder", get(builder_page))
            .route("/route-auth", get(route_auth))
            .route("/lazy", get(lazy_page))
            .route("/unsafe", get(unsafe_page))
            .route("/external", get(external).post(external))
            .route("/relative-external", get(relative_location))
            .route("/bad-location", get(bad_location))
            .route("/go", get(redirect).post(redirect))
            .route("/relative-go", get(relative_redirect))
            .route("/bad-go", get(bad_redirect))
            .layer(VersionLayer::new("1"))
    }

    fn app_with_shared_props() -> Router {
        let shared_props = SharedProps::new()
            .value("appName", "Demo")
            .value("n", 99)
            .prop("auth.user", |request| {
                request
                    .extension::<User>()
                    .map(|user| json!({ "name": user.name }))
            })
            .prop("csrfToken", |request| {
                request
                    .extension::<CsrfToken>()
                    .map(|token| token.0.clone())
            });

        app_with_shared_props_registry(shared_props)
            .layer(Extension(User { name: "Ada" }))
            .layer(Extension(CsrfToken("token-shared".into())))
    }

    fn app_with_shared_props_registry(shared_props: SharedProps) -> Router {
        app().layer(Extension(shared_props))
    }

    fn app_without_layer() -> Router {
        Router::new().route("/foo", get(page))
    }

    fn dynamic_app(version: Arc<AtomicUsize>) -> Router {
        Router::new()
            .route("/foo", get(page))
            .layer(VersionLayer::dynamic(move || {
                format!("dynamic-{}", version.load(Ordering::SeqCst))
            }))
    }

    fn nested_app() -> Router {
        Router::new().nest(
            "/nested",
            Router::new()
                .route("/foo", get(page))
                .layer(VersionLayer::new("1")),
        )
    }

    async fn body_json(response: Response) -> serde_json::Value {
        let bytes = ::axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();

        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn html_response_includes_escaped_page_and_version() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/foo?bar=baz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(VARY)
                .and_then(|value| value.to_str().ok()),
            Some(X_INERTIA)
        );

        let body = ::axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = std::str::from_utf8(&body).unwrap();

        assert!(body.contains("\"url\":\"/foo?bar=baz\""));
        assert!(body.contains("\"version\":\"1\""));
    }

    #[tokio::test]
    async fn html_response_escapes_json_for_script_context() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/unsafe")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = ::axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = std::str::from_utf8(&body).unwrap();

        assert!(!body.contains("</script><script>"));
        assert!(body.contains("\\u003C/script\\u003E"));
        assert!(body.contains("\\u0026"));
        assert!(body.contains("\\u2028\\u2029"));
    }

    #[tokio::test]
    async fn inertia_json_response_includes_headers_url_and_version() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/foo?bar=baz")
                    .header(X_INERTIA, "true")
                    .header(X_INERTIA_VERSION, "1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
        assert_eq!(
            response
                .headers()
                .get(X_INERTIA)
                .and_then(|value| value.to_str().ok()),
            Some("true")
        );
        assert_eq!(
            response
                .headers()
                .get(VARY)
                .and_then(|value| value.to_str().ok()),
            Some(X_INERTIA)
        );

        let page = body_json(response).await;

        assert_eq!(page["component"], "foo");
        assert_eq!(page["url"], "/foo?bar=baz");
        assert_eq!(page["version"], "1");
        assert_eq!(page["props"]["n"], 42);
    }

    #[tokio::test]
    async fn nested_routes_use_original_uri_for_page_urls() {
        let response = nested_app()
            .oneshot(
                Request::builder()
                    .uri("/nested/foo?bar=baz")
                    .header(X_INERTIA, "true")
                    .header(X_INERTIA_VERSION, "1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let page = body_json(response).await;

        assert_eq!(page["url"], "/nested/foo?bar=baz");
    }

    #[tokio::test]
    async fn nested_absolute_form_requests_use_local_page_urls() {
        let response = nested_app()
            .oneshot(
                Request::builder()
                    .uri("http://example.test/nested/foo?bar=baz")
                    .header(X_INERTIA, "true")
                    .header(X_INERTIA_VERSION, "1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let page = body_json(response).await;

        assert_eq!(page["url"], "/nested/foo?bar=baz");
    }

    #[tokio::test]
    async fn render_respects_explicit_url_override() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/custom-url?ignored=true")
                    .header(X_INERTIA, "true")
                    .header(X_INERTIA_VERSION, "1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let page = body_json(response).await;

        assert_eq!(page["url"], "/custom-url");
    }

    #[tokio::test]
    async fn render_supports_advanced_builder_pages() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/builder")
                    .header(X_INERTIA, "true")
                    .header(X_INERTIA_VERSION, "1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let page = body_json(response).await;

        assert_eq!(page["component"], "builder");
        assert_eq!(page["mergeProps"], json!(["notifications"]));
    }

    #[tokio::test]
    async fn render_supports_lazy_props() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/lazy")
                    .header(X_INERTIA, "true")
                    .header(X_INERTIA_VERSION, "1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let page = body_json(response).await;

        assert_eq!(page["props"]["users"], json!(["Ada", "Grace"]));
        assert!(page["props"].get("stats").is_none());
        assert!(page["props"].get("audit").is_none());
        assert_eq!(page["deferredProps"], json!({ "default": ["stats"] }));

        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/lazy")
                    .header(X_INERTIA, "true")
                    .header(X_INERTIA_VERSION, "1")
                    .header(X_INERTIA_PARTIAL_COMPONENT, "lazy")
                    .header(X_INERTIA_PARTIAL_DATA, "stats,audit")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let page = body_json(response).await;

        assert_eq!(page["props"]["stats"], 7);
        assert_eq!(page["props"]["audit"], json!(["created"]));
        assert!(page["props"].get("users").is_none());
        assert!(page.get("deferredProps").is_none());
    }

    #[tokio::test]
    async fn shared_props_are_merged_into_html_responses() {
        let response = app_with_shared_props()
            .oneshot(Request::builder().uri("/foo").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = ::axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        let data_page = body
            .split("<script data-page=\"app\" type=\"application/json\">")
            .nth(1)
            .and_then(|tail| tail.split("</script>").next())
            .unwrap();
        let page: serde_json::Value = serde_json::from_str(data_page).unwrap();

        assert_eq!(page["props"]["appName"], "Demo");
        assert_eq!(page["props"]["auth"]["user"]["name"], "Ada");
        assert_eq!(page["props"]["csrfToken"], "token-shared");
        assert_eq!(page["props"]["n"], 42);
        assert_eq!(page["sharedProps"], json!(["appName", "auth", "csrfToken"]));
    }

    #[tokio::test]
    async fn shared_props_are_merged_into_json_responses() {
        let response = app_with_shared_props()
            .oneshot(
                Request::builder()
                    .uri("/foo")
                    .header(X_INERTIA, "true")
                    .header(X_INERTIA_VERSION, "1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let page = body_json(response).await;

        assert_eq!(page["props"]["appName"], "Demo");
        assert_eq!(page["props"]["auth"]["user"]["name"], "Ada");
        assert_eq!(page["props"]["csrfToken"], "token-shared");
        assert_eq!(page["props"]["n"], 42);
        assert_eq!(page["sharedProps"], json!(["appName", "auth", "csrfToken"]));
    }

    #[tokio::test]
    async fn partial_reloads_include_shared_props_but_preserve_route_owned_roots() {
        let response = app_with_shared_props()
            .oneshot(
                Request::builder()
                    .uri("/foo")
                    .header(X_INERTIA, "true")
                    .header(X_INERTIA_VERSION, "1")
                    .header(X_INERTIA_PARTIAL_COMPONENT, "foo")
                    .header(X_INERTIA_PARTIAL_DATA, "stats")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let page = body_json(response).await;

        assert_eq!(page["props"]["stats"], 7);
        assert!(page["props"].get("n").is_none());
        assert_eq!(page["props"]["appName"], "Demo");
        assert_eq!(page["props"]["auth"]["user"]["name"], "Ada");
        assert_eq!(page["props"]["csrfToken"], "token-shared");
        assert_eq!(page["sharedProps"], json!(["appName", "auth", "csrfToken"]));
    }

    #[tokio::test]
    async fn skipped_colliding_shared_props_are_not_resolved() {
        let calls = Arc::new(AtomicUsize::new(0));
        let shared_props = SharedProps::new()
            .value("appName", "Demo")
            .prop("auth.user", {
                let calls = Arc::clone(&calls);
                move |_request| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    json!({ "name": "Shared" })
                }
            });
        let response = app_with_shared_props_registry(shared_props)
            .oneshot(
                Request::builder()
                    .uri("/route-auth")
                    .header(X_INERTIA, "true")
                    .header(X_INERTIA_VERSION, "1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let page = body_json(response).await;

        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(page["props"]["auth"]["user"]["name"], "Route");
        assert_eq!(page["props"]["appName"], "Demo");
        assert_eq!(page["sharedProps"], json!(["appName"]));
    }

    #[tokio::test]
    async fn shared_props_see_effective_non_get_context() {
        let shared_props = SharedProps::new().prop("partialComponent", |request| {
            request
                .context()
                .partial_component()
                .unwrap_or("none")
                .to_owned()
        });
        let response = app_with_shared_props_registry(shared_props)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/foo")
                    .header(X_INERTIA, "true")
                    .header(X_INERTIA_PARTIAL_COMPONENT, "foo")
                    .header(X_INERTIA_PARTIAL_DATA, "stats")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let page = body_json(response).await;

        assert_eq!(page["props"]["partialComponent"], "none");
        assert_eq!(page["props"]["n"], 42);
        assert_eq!(page["props"]["notifications"], json!(["welcome"]));
        assert!(page["props"].get("stats").is_none());
    }

    #[tokio::test]
    async fn optional_shared_props_can_skip_missing_values() {
        let shared_props =
            SharedProps::new()
                .value("appName", "Demo")
                .prop_optional("csrfToken", |request| {
                    request
                        .extension::<CsrfToken>()
                        .map(|token| token.0.clone())
                });
        let response = app_with_shared_props_registry(shared_props)
            .oneshot(
                Request::builder()
                    .uri("/foo")
                    .header(X_INERTIA, "true")
                    .header(X_INERTIA_VERSION, "1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let page = body_json(response).await;

        assert_eq!(page["props"]["appName"], "Demo");
        assert!(page["props"].get("csrfToken").is_none());
        assert_eq!(page["sharedProps"], json!(["appName"]));
    }

    #[tokio::test]
    async fn dotted_shared_props_merge_with_each_other() {
        let shared_props = SharedProps::new()
            .value("auth.user", json!({ "name": "Ada" }))
            .value("auth.csrf", "token-shared");
        let response = app_with_shared_props_registry(shared_props)
            .oneshot(
                Request::builder()
                    .uri("/lazy")
                    .header(X_INERTIA, "true")
                    .header(X_INERTIA_VERSION, "1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let page = body_json(response).await;

        assert_eq!(page["props"]["auth"]["user"]["name"], "Ada");
        assert_eq!(page["props"]["auth"]["csrf"], "token-shared");
        assert_eq!(page["sharedProps"], json!(["auth"]));
    }

    #[tokio::test]
    async fn empty_shared_props_are_a_noop_for_non_object_props() {
        let request = request_context(&HeaderMap::new());
        let request = InertiaRequest {
            context: request,
            extensions: Some(Arc::new(Extensions::new())),
            method: Method::GET,
            shared_props: Some(SharedProps::new()),
            uri: "/empty".into(),
            version: None,
        };
        let response = request
            .render(Inertia::response("empty", ()), |context| {
                Html(context.data_page().to_owned())
            })
            .unwrap();
        let body = ::axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let page: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(page["props"], serde_json::Value::Null);
        assert!(page.get("sharedProps").is_none());
    }

    #[tokio::test]
    async fn shared_prop_serialization_errors_become_internal_server_errors() {
        let shared_props = SharedProps::new().prop("bad", |_request| {
            let mut value = BTreeMap::new();
            value.insert((1, 2), 3);
            value
        });
        let response = app_with_shared_props_registry(shared_props)
            .oneshot(
                Request::builder()
                    .uri("/foo")
                    .header(X_INERTIA, "true")
                    .header(X_INERTIA_VERSION, "1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn shared_dotted_props_do_not_replace_route_owned_roots() {
        let response = app_with_shared_props()
            .oneshot(
                Request::builder()
                    .uri("/route-auth")
                    .header(X_INERTIA, "true")
                    .header(X_INERTIA_VERSION, "1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let page = body_json(response).await;

        assert_eq!(page["props"]["auth"]["user"]["name"], "Route");
        assert_eq!(page["props"]["appName"], "Demo");
        assert_eq!(page["props"]["n"], 99);
        assert_eq!(page["props"]["csrfToken"], "token-shared");
        assert_eq!(page["sharedProps"], json!(["appName", "n", "csrfToken"]));
    }

    #[tokio::test]
    async fn response_without_version_layer_omits_asset_version() {
        let response = app_without_layer()
            .oneshot(
                Request::builder()
                    .uri("/foo")
                    .header(X_INERTIA, "true")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let page = body_json(response).await;

        assert!(page.get("version").is_none());
    }

    #[tokio::test]
    async fn dynamic_version_is_resolved_for_each_page_response() {
        let version = Arc::new(AtomicUsize::new(1));
        let app = dynamic_app(version.clone());

        version.store(2, Ordering::SeqCst);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/foo")
                    .header(X_INERTIA, "true")
                    .header(X_INERTIA_VERSION, "dynamic-2")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let page = body_json(response).await;

        assert_eq!(page["version"], "dynamic-2");

        version.store(3, Ordering::SeqCst);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/foo")
                    .header(X_INERTIA, "true")
                    .header(X_INERTIA_VERSION, "dynamic-3")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let page = body_json(response).await;

        assert_eq!(page["version"], "dynamic-3");
    }

    #[tokio::test]
    async fn stale_inertia_version_conflicts_before_handler_runs() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/foo?bar=baz")
                    .header(X_INERTIA, "true")
                    .header(X_INERTIA_VERSION, "stale")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            response
                .headers()
                .get(X_INERTIA_LOCATION)
                .and_then(|value| value.to_str().ok()),
            Some("/foo?bar=baz")
        );
    }

    #[tokio::test]
    async fn nested_stale_version_conflicts_use_original_uri() {
        let response = nested_app()
            .oneshot(
                Request::builder()
                    .uri("/nested/foo?bar=baz")
                    .header(X_INERTIA, "true")
                    .header(X_INERTIA_VERSION, "stale")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            response
                .headers()
                .get(X_INERTIA_LOCATION)
                .and_then(|value| value.to_str().ok()),
            Some("/nested/foo?bar=baz")
        );
    }

    #[tokio::test]
    async fn nested_absolute_form_conflicts_use_local_location() {
        let response = nested_app()
            .oneshot(
                Request::builder()
                    .uri("http://example.test/nested/foo?bar=baz")
                    .header(X_INERTIA, "true")
                    .header(X_INERTIA_VERSION, "stale")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            response
                .headers()
                .get(X_INERTIA_LOCATION)
                .and_then(|value| value.to_str().ok()),
            Some("/nested/foo?bar=baz")
        );
    }

    #[tokio::test]
    async fn missing_inertia_version_conflicts_before_handler_runs() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/foo")
                    .header(X_INERTIA, "true")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            response
                .headers()
                .get(X_INERTIA_LOCATION)
                .and_then(|value| value.to_str().ok()),
            Some("/foo")
        );
    }

    #[tokio::test]
    async fn post_response_ignores_partial_reload_but_preserves_once_exclusions() {
        let response = app()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/foo")
                    .header(X_INERTIA, "true")
                    .header(X_INERTIA_VERSION, "stale")
                    .header(X_INERTIA_PARTIAL_COMPONENT, "foo")
                    .header(X_INERTIA_PARTIAL_DATA, "n")
                    .header(X_INERTIA_EXCEPT_ONCE_PROPS, "plans")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let page = body_json(response).await;

        assert_eq!(
            page["props"],
            json!({
                "errors": {},
                "n": 42,
                "notifications": ["welcome"]
            })
        );
        assert_eq!(
            page["onceProps"]["plans"],
            json!({ "prop": "plans", "expiresAt": null })
        );
    }

    #[tokio::test]
    async fn external_location_uses_conflict_for_inertia_requests() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/external")
                    .header(X_INERTIA, "true")
                    .header(X_INERTIA_VERSION, "1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            response
                .headers()
                .get(X_INERTIA_LOCATION)
                .and_then(|value| value.to_str().ok()),
            Some("https://example.com/outside")
        );
    }

    #[tokio::test]
    async fn external_location_falls_back_to_browser_redirects() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/external")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(
            response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok()),
            Some("https://example.com/outside")
        );

        let response = app()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/external")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok()),
            Some("https://example.com/outside")
        );
    }

    #[tokio::test]
    async fn relative_location_references_are_supported() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/relative-external")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(
            response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok()),
            Some("../outside?from=axum#fragment")
        );
    }

    #[tokio::test]
    async fn invalid_location_returns_internal_server_error() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/bad-location")
                    .header(X_INERTIA, "true")
                    .header(X_INERTIA_VERSION, "1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn invalid_redirect_returns_internal_server_error() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/bad-go")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn write_redirects_use_see_other_status() {
        let response = app()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/go")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok()),
            Some("/target")
        );
    }

    #[tokio::test]
    async fn get_redirects_use_found_status() {
        let response = app()
            .oneshot(Request::builder().uri("/go").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(
            response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok()),
            Some("/target")
        );
    }

    #[tokio::test]
    async fn relative_redirect_references_are_supported() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/relative-go")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(
            response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok()),
            Some("?next=target#fragment")
        );
    }
}
