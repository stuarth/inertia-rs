# Inertia.rs

[![Current Crates.io Version](https://img.shields.io/crates/v/inertia_rs)](https://crates.io/crates/inertia_rs)
[![Build Status](https://github.com/stuarth/inertia-rs/workflows/CI/badge.svg)](https://github.com/stuarth/inertia-rs/actions)
[![docs.rs](https://img.shields.io/badge/docs-latest-blue.svg?style=flat)](https://docs.rs/inertia_rs/)

[Inertia.js](https://inertiajs.com/) adapter support for Rust web applications. The crate currently provides Rocket integration and an initial Axum integration.

Inertia lets you build server-driven applications that render client-side pages without adding a separate API or client-side router. Your Rust routes return page components and props; the Inertia client handles navigation and page swaps in the browser.

## Status

`inertia_rs` currently supports the core Rocket response flow:

- HTML first-page responses.
- JSON Inertia responses with `X-Inertia: true`.
- Asset version checks with `X-Inertia-Version`.
- `409 Conflict` responses with `X-Inertia-Location` for stale assets.
- Inertia v3 page-object metadata and response filtering for partial reloads, merge props, deferred prop keys, once props, history flags, and infinite-scroll metadata.
- Framework-neutral `InertiaProps` for synchronous lazy, optional, deferred, and once prop resolvers.
- Rocket shared props with request-aware providers.
- Axum request extraction, shared props, response helpers, and asset-version middleware.

Async prop resolvers and SSR are planned but not fully implemented yet.

The minimum supported Rust version is 1.88.

## Protocol Support

| Feature | Status | Notes |
| --- | --- | --- |
| Initial HTML response | Supported | Rocket renders the application shell through `VersionFairing`. |
| JSON Inertia response | Supported | Responses include `X-Inertia: true` and `Vary: X-Inertia`. |
| Asset version conflicts | Supported | Stale Inertia `GET` requests return `409 Conflict` with `X-Inertia-Location`. |
| Dynamic asset versions | Supported | `VersionFairing::dynamic` reads the current version when page responses or version checks need it. |
| Query-string URLs | Supported | Page object URLs preserve the request query string. |
| Public header helpers | Supported | Header constants are available at the crate root and through `inertia_rs::headers`. |
| Request header parsing | Supported | `RequestContext` is framework-neutral; Rocket exposes `InertiaHeaders`. |
| Partial reloads | Supported | Matching components honor `X-Inertia-Partial-Data` and `X-Inertia-Partial-Except`. |
| Merge props | Supported | `mergeProps`, `prependProps`, `deepMergeProps`, `matchPropsOn`, reset handling, and infinite-scroll intent are modeled. |
| Deferred props | Partial | `InertiaProps::defer` emits `deferredProps` metadata and resolves synchronous values only when requested. Async deferred resolvers are planned. |
| Lazy or async props | Partial | `InertiaProps` supports synchronous lazy, optional, always, deferred, and once props. Async resolvers are planned. |
| Once props | Supported | `onceProps` metadata and `X-Inertia-Except-Once-Props` filtering are modeled. |
| Shared props | Supported | Rocket managed state and Axum extension layers can merge common props into every page response. |
| External location redirects | Supported | `Inertia::location` maps Inertia visits to `409 Conflict` with `X-Inertia-Location`. |
| Method-aware redirects | Supported | `Inertia::redirect` returns `303 See Other` for write methods. |
| SSR | Not supported | No server-side rendering bridge is provided. |
| Axum | Partial | `InertiaRequest`, `VersionLayer`, `SharedProps`, page rendering, external locations, and method-aware redirects are available. |

## Installation

Add the crate to your `Cargo.toml`:

```toml
[dependencies]
inertia_rs = { version = "0.3.0", features = ["rocket"] }
rocket = { version = "0.5.1", features = ["json"] }

[dependencies.rocket_dyn_templates]
version = "0.2.0"
features = ["handlebars"]
```

For Axum applications, enable the `axum` feature instead:

```toml
[dependencies]
inertia_rs = { version = "0.3.0", default-features = false, features = ["axum"] }
axum = "0.8"
```

## Rocket Usage

`Inertia<T>` is a Rocket responder for serializable page props. `VersionFairing` installs the HTML renderer and performs asset-version checks for Inertia visits.

```rust
#[macro_use]
extern crate rocket;

use inertia_rs::rocket::VersionFairing;
use inertia_rs::Inertia;
use rocket::response::Responder;
use rocket_dyn_templates::Template;

#[derive(serde::Serialize)]
struct Hello {
    name: String,
}

#[get("/hello")]
fn hello() -> Inertia<Hello> {
    Inertia::response(
        "Hello",
        Hello {
            name: "world".into(),
        },
    )
}

#[launch]
fn rocket() -> _ {
    rocket::build()
        .mount("/", routes![hello])
        .attach(Template::fairing())
        .attach(VersionFairing::new("asset-version-1", |request, ctx| {
            Template::render("app", ctx).respond_to(request)
        }))
}
```

Use `VersionFairing::dynamic` when the asset version should be loaded or
computed while the server is running. The provider runs during request
handling, so keep it fast and avoid blocking I/O; load files or manifests
outside the provider and read a cached value here.

```rust
let asset_version = std::sync::Arc::new(std::sync::RwLock::new(String::from("asset-version-1")));
let version_for_requests = asset_version.clone();

let fairing = VersionFairing::dynamic(
    move || {
        version_for_requests
            .read()
            .map(|version| version.clone())
            .unwrap_or_default()
    },
    |request, ctx| Template::render("app", ctx).respond_to(request),
);
```

Your root HTML template receives `data_page`, a JSON-serialized Inertia page object escaped for safe use in a `<script>` tag. With Handlebars, the template can expose it to the frontend app like this. The `script_path` value below is an application-level value, typically read from a Vite manifest.

```html
<!DOCTYPE html>
<html>
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <script type="module" src="/public/build/{{ script_path }}"></script>
  </head>
  <body>
    <script data-page="app" type="application/json">{{{ data_page }}}</script>
    <div id="app"></div>
  </body>
</html>
```

The repository includes a Rocket + Svelte 5 + Vite example under `examples/rocket-svelte`.

## Axum Usage

`axum::InertiaRequest` extracts the parsed Inertia request context, current URI, request method, and optional asset version. Add `axum::VersionLayer` to install asset-version checks and include the active version in page objects.

```rust
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Router};
use inertia_rs::axum::{InertiaError, InertiaRequest, SharedProps, VersionLayer};
use inertia_rs::Inertia;

#[derive(serde::Serialize)]
struct Hello {
    name: String,
}

async fn hello(request: InertiaRequest) -> Result<Response, InertiaError> {
    request.render(
        Inertia::response("Hello", Hello { name: "world".into() }),
        // data_page is already escaped for safe embedding in a script element.
        |ctx| Html(format!(r#"<script data-page="app" type="application/json">{}</script>"#, ctx.data_page())).into_response(),
    )
}

let app = Router::new()
    .route("/hello", get(hello))
    .layer(Extension(SharedProps::new().value("appName", "Demo")))
    .layer(VersionLayer::new("asset-version-1"));
```

The repository includes a minimal Axum example under `examples/axum-minimal`.

## Inertia v3 Protocol Helpers

The simple API remains the shortest path for standard pages:

```rust
Inertia::response("Users/Index", props)
```

For v3 page metadata, chain explicit helpers on the response or use the `Inertia::page(...).props(...)` builder. Deferred props are listed in `deferredProps` and omitted until an Inertia partial reload explicitly requests them. When props are ordinary serializable values, they are serialized before filtering. Use `InertiaProps` when expensive synchronous values should only be resolved after the request headers determine they are needed. `share()` marks `sharedProps` metadata, while the Rocket integration can register shared application state as shown below.

```rust
Inertia::response("Users/Index", props)
    .always("auth")
    .merge("users")
    .defer("stats")
    .once("plans")
    .encrypt_history()
```

```rust
Inertia::page("Users/Index")
    .always("auth")
    .defer("stats")
    .props(props)
```

```rust
use inertia_rs::{Inertia, InertiaProps};

let props = InertiaProps::new()
    .value("user", user)
    .lazy("companies", || load_companies())
    .optional("auditTrail", || load_audit_trail())
    .defer("analytics", || load_analytics())
    .once("plans", || load_plans());

Inertia::response("Users/Index", props)
```

For immediate rendering paths that borrow local values, use
`ScopedInertiaProps`.

The root crate also exposes framework-neutral protocol types:

```rust
use inertia_rs::{Page, PageMetadata, RequestContext};
```

Rocket responses use these types internally. `RequestContext` parses Inertia headers such as `X-Inertia-Partial-Data`, `X-Inertia-Partial-Except`, `X-Inertia-Reset`, and `X-Inertia-Except-Once-Props`.

## Shared Props

For Rocket, register `rocket::SharedProps` as managed state to merge common application data into every page response.

```rust
use inertia_rs::rocket::SharedProps;

let shared_props = SharedProps::new()
    .value("appName", "My App")
    .prop("auth.csrfToken", |request| {
        request.headers().get_one("X-CSRF").map(ToOwned::to_owned)
    });

let rocket = rocket::build().manage(shared_props);
```

Axum apps can register `axum::SharedProps` through an `Extension` layer. Providers receive the extracted `InertiaRequest` and can read values inserted by other Axum layers through `request.extension::<T>()`.

```rust
use axum::{Extension, Router};
use inertia_rs::axum::SharedProps;

let shared_props = SharedProps::new()
    .value("appName", "My App")
.prop("auth.user", |request| {
        request.extension::<User>().map(|user| user.summary())
    });

let app = Router::new().layer(Extension(shared_props));
```

Use `prop_optional` when a missing value should omit the key instead of
serializing as `null`.

Shared props are shallow-merged into page props for HTML first loads and JSON Inertia responses. Route props win on key collisions, including when a route-defined root such as `auth` collides with dotted shared keys such as `auth.user`. Keys may be top-level or dotted, where `auth.user` becomes `props.auth.user`; inserted top-level keys are listed in `sharedProps`. Keep shared props small and namespace them, since they are merged after partial-reload filtering and remain included on partial reload responses. This crate intentionally keeps shared props in partial reload responses; request specific values explicitly with route props when a large shared value should be omitted from a reload.

## Redirect Helpers

Use `Inertia::location(url)` for external redirects from Inertia visits. Rocket converts Inertia requests to the `409 Conflict` response with `X-Inertia-Location`, and falls back to method-aware normal redirects for direct browser requests.

```rust
#[get("/billing")]
fn billing() -> inertia_rs::Location {
    Inertia::location("https://billing.example.com")
}
```

Use `Inertia::redirect(url)` for application redirects that should be method-aware. Rocket returns `302 Found` for read-style requests and `303 See Other` for `POST`, `PUT`, `PATCH`, and `DELETE`; this helper does not branch on Inertia request headers.

```rust
#[post("/users")]
fn create_user() -> inertia_rs::Redirect {
    Inertia::redirect("/users")
}
```

## Request Helpers

Rocket handlers can inspect Inertia headers through the `InertiaHeaders` request guard:

```rust
use inertia_rs::rocket::InertiaHeaders;

#[get("/debug")]
fn debug(headers: InertiaHeaders) -> String {
    format!(
        "is_inertia={}, version={:?}",
        headers.is_inertia(),
        headers.version()
    )
}
```

The raw protocol header constants are also public:

```rust
use inertia_rs::headers::{X_INERTIA, X_INERTIA_LOCATION, X_INERTIA_VERSION};
```
