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

The broader Inertia v3 protocol surface, including partial reloads, deferred props, merge props, once props, shared props, and SSR, is planned but not fully implemented yet.

The minimum supported Rust version is 1.88.

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
use inertia_rs::{X_INERTIA, X_INERTIA_LOCATION, X_INERTIA_VERSION};
```
