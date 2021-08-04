# Inertia on Rust

[Inertia.js](https://inertiajs.com/) for [Rocket](https://rocket.rs/)

## Installation

Add the following line to your `Cargo.toml`
```toml
inertia_rs = "0.1.0"
```

## Usage

`inertia_rs` defines a succinct interface for creating Inertia.js apps in Rocket. It's comprised of two elements, `Inertia<T>`, a [Responder](https://api.rocket.rs/v0.5-rc/rocket/response/trait.Responder.html) that's generic over `T`, the Inertia component's properties, and `VersionFairing`, which is responsible for asset version checks.

### Sample App

```rust
#[macro_use]
extern crate rocket;

use inertia_rs::{Inertia, VersionFairing};
use rocket::response::Responder;
use rocket_dyn_templates::Template;

#[derive(serde::Serialize)]
struct Hello {
    secret: String,
}

#[get("/hello")]
fn hello() -> Inertia<Hello> {
    Inertia::response(
        "hello",
        Hello {
            secret: "hello secret".into(),
        },
    )
}

#[launch]
fn rocket() -> _ {
    rocket::build()
        .mount("/", routes![hello])
        .attach(Template::fairing())
        // Version fairing is configured with current asset version, and a 
        // closure to generate the html template response
        .attach(VersionFairing::new("X2", |request, ctx| {
            Template::render("app", ctx).respond_to(request)
        }))
}

```