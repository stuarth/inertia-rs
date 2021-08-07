# Inertia.rs

[![Current Crates.io Version](https://img.shields.io/crates/v/inertia-rs)](https://crates.io/crates/inertia_rs)
[![Build Status](https://github.com/stuarth/inertia-rs/workflows/CI/badge.svg)](https://github.com/stuarth/inertia-rs/actions)
[![docs.rs](https://img.shields.io/badge/docs-latest-blue.svg?style=flat)](https://docs.rs/inertia_rs/)

[Inertia.js](https://inertiajs.com/) implementations for Rust. Currently supports [Rocket](https://rocket.rs/).

## Why Inertia?

From [inertiajs.com](https://inertiajs.com/)

> Inertia is a new approach to building classic server-driven web apps. We call it the modern monolith.
>
> Inertia allows you to create fully client-side rendered, single-page apps, without much of the complexity that comes with modern SPAs. It does this by leveraging existing server-side frameworks.
>
> Inertia has no client-side routing, nor does it require an API. Simply build controllers and page views like you've always done!

Inertia.rs brings a straightforward integration to Rust.

## Installation

Add the following line to your `Cargo.toml`
```toml
inertia_rs = { version = "0.2.0", features = ["rocket"] }
```

## Usage

`inertia_rs` defines a succinct interface for creating Inertia.js apps in Rocket. It's comprised of two elements, `Inertia<T>`, a [Responder](https://api.rocket.rs/v0.5-rc/rocket/response/trait.Responder.html) that's generic over `T`, the Inertia component's properties, and `VersionFairing`, which is responsible for asset version checks.

### Sample Rocket Server

```rust
#[macro_use]
extern crate rocket;

use inertia_rs::rocket::{Inertia, VersionFairing};
use rocket::response::Responder;
use rocket_dyn_templates::Template;

#[derive(serde::Serialize)]
struct Hello {
    some_property: String,
}

#[get("/hello")]
fn hello() -> Inertia<Hello> {
    Inertia::response(
        // the component to render
        "hello",
        // the props to pass our component
        Hello { some_property: "hello world!".into() },
    )
}

#[launch]
fn rocket() -> _ {
    rocket::build()
        .mount("/", routes![hello])
        .attach(Template::fairing())
        // Version fairing is configured with current asset version, and a 
        // closure to generate the html template response
        .attach(VersionFairing::new("1", |request, ctx| {
            Template::render("app", ctx).respond_to(request)
        }))
}

```