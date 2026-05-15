#[macro_use]
extern crate rocket;

use inertia_rs::rocket::{SharedProps, VersionFairing};
use inertia_rs::Inertia;
use rocket::response::content::RawHtml;
use rocket::response::Responder;

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
        .manage(SharedProps::new().value("appName", "Rocket Minimal"))
        .attach(VersionFairing::new("asset-version-1", |request, context| {
            RawHtml(format!(
                r#"<!doctype html>
<html>
  <head>
    <meta charset="utf-8" />
    <title>Rocket Inertia Example</title>
  </head>
  <body>
    <script data-page="app" type="application/json">{}</script>
    <main id="app">Open this route with an Inertia request to receive JSON.</main>
  </body>
</html>"#,
                context.data_page()
            ))
            .respond_to(request)
        }))
}
