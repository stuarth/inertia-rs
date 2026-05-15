# Inertia.rs

[![Current Crates.io Version](https://img.shields.io/crates/v/inertia_rs)](https://crates.io/crates/inertia_rs)
[![Build Status](https://github.com/stuarth/inertia-rs/workflows/CI/badge.svg)](https://github.com/stuarth/inertia-rs/actions)
[![docs.rs](https://img.shields.io/badge/docs-latest-blue.svg?style=flat)](https://docs.rs/inertia_rs/)

[Inertia.js](https://inertiajs.com/) adapter support for Rust web applications. The crate currently provides a Rocket integration.

Inertia lets you build server-driven applications that render client-side pages without adding a separate API or client-side router. Your Rust routes return page components and props; the Inertia client handles navigation and page swaps in the browser.

## Status

`inertia_rs` currently supports the core Rocket response flow:

- HTML first-page responses.
- JSON Inertia responses with `X-Inertia: true`.
- Asset version checks with `X-Inertia-Version`.
- `409 Conflict` responses with `X-Inertia-Location` for stale assets.
- Inertia v3 page-object metadata and response filtering for partial reloads, merge props, deferred prop keys, once props, history flags, and infinite-scroll metadata.
- Rocket shared props with request-aware providers.

Lazy or async prop resolvers, SSR, and non-Rocket framework integrations are planned but not fully implemented yet.

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
| Deferred props | Partial | `deferredProps` metadata is emitted and values are omitted until requested, but prop values are still serialized eagerly. |
| Lazy or async props | Planned | There is no lazy resolver container yet. |
| Once props | Supported | `onceProps` metadata and `X-Inertia-Except-Once-Props` filtering are modeled. |
| Shared props | Supported | Rocket managed state can merge common props into every page response. |
| External location redirects | Supported | `Inertia::location` maps Inertia visits to `409 Conflict` with `X-Inertia-Location`. |
| Method-aware redirects | Supported | `Inertia::redirect` returns `303 See Other` for write methods. |
| SSR | Not supported | No server-side rendering bridge is provided. |
| Axum | Planned | Framework expansion is deferred until the Rocket integration settles on the protocol core. |

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

## Inertia v3 Protocol Helpers

The simple API remains the shortest path for standard pages:

```rust
Inertia::response("Users/Index", props)
```

For v3 page metadata, chain explicit helpers on the response or use the `Inertia::page(...).props(...)` builder. Deferred props are listed in `deferredProps` and omitted until an Inertia partial reload explicitly requests them; this crate does not yet provide lazy async prop resolvers. `share()` marks `sharedProps` metadata, while the Rocket integration can register shared application state as shown below.

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

The root crate also exposes framework-neutral protocol types:

```rust
use inertia_rs::{Page, PageMetadata, RequestContext};
```

Rocket responses use these types internally. `RequestContext` parses Inertia headers such as `X-Inertia-Partial-Data`, `X-Inertia-Partial-Except`, `X-Inertia-Reset`, and `X-Inertia-Except-Once-Props`.

## Rocket Shared Props

Register `rocket::SharedProps` as managed state to merge common application data into every Rocket page response.

```rust
use inertia_rs::rocket::SharedProps;

let shared_props = SharedProps::new()
    .value("appName", "My App")
    .prop("auth.csrfToken", |request| {
        request.headers().get_one("X-CSRF").map(ToOwned::to_owned)
    });

let rocket = rocket::build().manage(shared_props);
```

Shared props are shallow-merged into page props for HTML first loads and JSON Inertia responses. Route props win on key collisions, including when a route-defined root such as `auth` collides with dotted shared keys such as `auth.user`. Keys may be top-level or dotted, where `auth.user` becomes `props.auth.user`; inserted top-level keys are listed in `sharedProps`. Keep shared props small and namespace them, since they are merged after partial-reload filtering and remain included on partial reload responses.

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
