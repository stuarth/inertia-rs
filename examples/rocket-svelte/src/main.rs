#[macro_use]
extern crate rocket;

use inertia_rs::rocket::{Inertia, VersionFairing};
use rocket::response::Responder;
use rocket_dyn_templates::Template;
use rocket::fs::FileServer;

#[derive(serde::Serialize)]
struct Hello {
    name: String,
}

#[get("/hello")]
fn hello() -> Inertia<Hello> {
    Inertia::response(
        // the component to render
        "Hello",
        // the props to pass our component
        Hello { name: "world".into() },
    )
}

#[launch]
fn rocket() -> _ {
    rocket::build()
        .mount("/", routes![hello])
        .attach(Template::fairing())
        .mount("/public", FileServer::from(rocket::fs::relative!("public")))
        // Version fairing is configured with current asset version, and a 
        // closure to generate the html template response
        .attach(VersionFairing::new("1", |request, ctx| {
            Template::render("app", ctx).respond_to(request)
        }))
}
