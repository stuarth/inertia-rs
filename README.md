# Inertia on Rust

[Inertia.js](https://inertiajs.com/) for [Rocket](https://rocket.rs/)

## Sample App

```rust
#[macro_use]
extern crate rocket;

use inertia_rs::{Inertia, VersionFairing};
use rocket::{fs::FileServer, response::Responder};
use rocket_dyn_templates::Template;
use serde::Serialize;

#[derive(Serialize)]
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

#[get("/stu")]
fn stu() -> Inertia<Hello> {
    Inertia::response(
        "stu",
        Hello {
            secret: "stu's secret".into(),
        },
    )
}

#[launch]
fn rocket() -> _ {
    rocket::build()
        .mount("/", routes![hello, stu])
        .mount("/public", FileServer::from(rocket::fs::relative!("public")))
        .attach(Template::fairing())
        .attach(VersionFairing::new("X2", |request, ctx| {
            Template::render("app", ctx).respond_to(request)
        }))
}
```