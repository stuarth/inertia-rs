#[macro_use]
extern crate rocket;

use inertia_rs::{
    rocket::{SharedProps, VersionFairing},
    Inertia, InertiaProps,
};
use rocket::fs::FileServer;
use rocket::response::Responder;
use rocket_dyn_templates::Template;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{collections::BTreeMap, fs};

#[derive(serde::Deserialize)]
struct ViteManifestEntry {
    file: String,
    #[serde(default)]
    css: Vec<String>,
}

impl ViteManifestEntry {
    fn asset_version(&self) -> String {
        std::iter::once(self.file.as_str())
            .chain(self.css.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join("|")
    }
}

#[derive(serde::Serialize)]
struct AppContext<'a> {
    data_page: &'a str,
    script_path: &'a str,
    style_path: Option<&'a str>,
}

fn vite_manifest_entry() -> ViteManifestEntry {
    let manifest_path = rocket::fs::relative!("public/build/.vite/manifest.json");
    let manifest = fs::read_to_string(manifest_path).expect(
        "missing Vite manifest; run `npm install && npm run build` in examples/rocket-svelte/svelte-app",
    );
    let manifest: BTreeMap<String, ViteManifestEntry> =
        serde_json::from_str(&manifest).expect("Vite manifest is not valid JSON");

    manifest
        .into_iter()
        .find_map(|(path, entry)| (path == "src/main.js").then_some(entry))
        .expect("Vite manifest does not contain the src/main.js entrypoint")
}

fn generated_at() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time is after the Unix epoch")
        .as_secs()
}

fn example_stats(adapter: &'static str) -> serde_json::Value {
    serde_json::json!({
        "adapter": adapter,
        "deferred": true,
        "generatedAt": generated_at(),
    })
}

#[get("/hello")]
fn hello() -> Inertia<InertiaProps> {
    Inertia::response(
        "Hello",
        InertiaProps::new()
            .value("name", "world")
            .value(
                "message",
                "Rendered by Rocket and hydrated by Svelte through Inertia.",
            )
            .defer("stats", || example_stats("Rocket"))
            .optional("debug", || {
                serde_json::json!({
                    "partialReload": true,
                    "loadedBy": "X-Inertia-Partial-Data: debug",
                })
            }),
    )
}

#[launch]
fn rocket() -> _ {
    let manifest_entry = vite_manifest_entry();
    let asset_version = manifest_entry.asset_version();
    let script_path = manifest_entry.file;
    let style_path = manifest_entry.css.into_iter().next();

    let figment =
        rocket::Config::figment().merge(("template_dir", rocket::fs::relative!("templates")));

    rocket::custom(figment)
        .manage(
            SharedProps::new()
                .value("appName", "Rocket Svelte")
                .value(
                    "auth.user",
                    serde_json::json!({
                        "name": "Ada Lovelace",
                        "role": "Example user",
                    }),
                ),
        )
        .mount("/", routes![hello])
        .attach(Template::fairing())
        .mount("/public", FileServer::from(rocket::fs::relative!("public")))
        .attach(VersionFairing::new(asset_version, move |request, ctx| {
            let template_ctx = AppContext {
                data_page: ctx.data_page(),
                script_path: script_path.as_str(),
                style_path: style_path.as_deref(),
            };

            Template::render("app", template_ctx).respond_to(request)
        }))
}
