//! A small Rust adapter for the [Inertia.js](https://inertiajs.com/) protocol.
//!
//! The crate exposes a framework-neutral [`Inertia`] response description and
//! framework integrations behind feature flags. The `rocket` feature is enabled
//! by default and provides a Rocket
//! [`Responder`](https://api.rocket.rs/v0.5/rocket/response/trait.Responder.html)
//! implementation plus asset-versioning support.
//!
//! # Example
//!
//! ```rust
//! use inertia_rs::Inertia;
//!
//! #[derive(serde::Serialize)]
//! struct Props {
//!     name: String,
//! }
//!
//! let response = Inertia::response("Users/Show", Props { name: "Ada".into() });
//! assert_eq!(response.component(), "Users/Show");
//! ```

/// Rocket integration for Inertia responses and asset version checks.
#[cfg(feature = "rocket")]
pub mod rocket;

/// Request and response header used to mark Inertia protocol requests.
pub const X_INERTIA: &str = "X-Inertia";

/// Request header containing the client's current asset version.
pub const X_INERTIA_VERSION: &str = "X-Inertia-Version";

/// Response header used with `409 Conflict` to force a full-page visit.
pub const X_INERTIA_LOCATION: &str = "X-Inertia-Location";

/// Response header used to separate HTML and JSON variants in caches.
pub const VARY: &str = "Vary";

/// A framework-neutral Inertia page response.
///
/// Framework integrations convert this value into either an HTML first-load
/// response or a JSON Inertia response, depending on the incoming request
/// headers.
pub struct Inertia<T> {
    component: String,
    props: T,
    url: Option<String>,
}

impl<T> Inertia<T> {
    /// Constructs a response for `component` with serializable `props`.
    ///
    /// Framework integrations default the page object's `url` field to the
    /// current request URI unless [`with_url`](Self::with_url) is used.
    pub fn response<C: Into<String>>(component: C, props: T) -> Self {
        Self {
            component: component.into(),
            props,
            url: None,
        }
    }

    /// Overrides the page object's `url` field.
    pub fn with_url<U: Into<String>>(mut self, url: U) -> Self {
        self.url = Some(url.into());
        self
    }

    /// Returns the Inertia component name.
    pub fn component(&self) -> &str {
        &self.component
    }

    /// Returns a reference to the component props.
    pub fn props(&self) -> &T {
        &self.props
    }

    /// Returns the explicit page URL override, if one was set.
    pub fn url(&self) -> Option<&str> {
        self.url.as_deref()
    }
}
