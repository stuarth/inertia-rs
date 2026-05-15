use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Router};
use inertia_rs::axum::{InertiaError, InertiaRequest, SharedProps, VersionLayer};
use inertia_rs::{Inertia, InertiaProps};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tower_http::services::ServeDir;

#[derive(Clone, Debug)]
struct Assets {
    script_path: String,
    style_path: Option<String>,
    version: String,
}

#[derive(Deserialize)]
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

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn vite_manifest_entry() -> ViteManifestEntry {
    let manifest_path = example_dir().join("public/build/.vite/manifest.json");
    let manifest = fs::read_to_string(&manifest_path).expect(
        "missing Vite manifest; run `npm install && npm run build` in examples/axum-svelte/svelte-app",
    );
    let manifest: BTreeMap<String, ViteManifestEntry> =
        serde_json::from_str(&manifest).expect("Vite manifest is not valid JSON");

    manifest
        .into_iter()
        .find_map(|(path, entry)| (path == "src/main.js").then_some(entry))
        .expect("Vite manifest does not contain the src/main.js entrypoint")
}

fn load_assets() -> Assets {
    let manifest_entry = vite_manifest_entry();

    Assets {
        version: manifest_entry.asset_version(),
        script_path: manifest_entry.file,
        style_path: manifest_entry.css.into_iter().next(),
    }
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

async fn hello(request: InertiaRequest) -> Result<Response, InertiaError> {
    // InertiaRequest keeps extension snapshots for shared-prop providers, so
    // this example registers SharedProps even though the assets are separate.
    let assets = request
        .extension::<Arc<Assets>>()
        .expect("assets extension is registered")
        .clone();

    request.render(
        Inertia::response(
            "Hello",
            InertiaProps::new()
                .value("name", "world")
                .value(
                    "message",
                    "Rendered by Axum and hydrated by Svelte through Inertia.",
                )
                .defer("stats", || example_stats("Axum"))
                .optional("debug", || {
                    serde_json::json!({
                        "partialReload": true,
                        "loadedBy": "X-Inertia-Partial-Data: debug",
                    })
                }),
        ),
        |context| {
            let style = assets.style_path.as_deref().map_or_else(String::new, |path| {
                format!(r#"<link rel="stylesheet" href="/public/build/{path}" />"#)
            });

            Html(format!(
                r#"<!doctype html>
<html>
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <title>Axum Svelte Inertia Example</title>
    {style}
    <script type="module" src="/public/build/{script}"></script>
  </head>
  <body>
    <script data-page="app" type="application/json">{page}</script>
    <div id="app"></div>
  </body>
</html>"#,
                script = assets.script_path.as_str(),
                page = context.data_page(),
            ))
            .into_response()
        },
    )
}

#[tokio::main]
async fn main() {
    let assets = Arc::new(load_assets());
    let public_dir = example_dir().join("public");
    let app = Router::new()
        .route("/hello", get(hello))
        .nest_service("/public", ServeDir::new(public_dir))
        .layer(Extension(
            SharedProps::new()
                .value("appName", "Axum Svelte")
                .value(
                    "auth.user",
                    serde_json::json!({
                        "name": "Grace Hopper",
                        "role": "Example user",
                    }),
                ),
        ))
        .layer(Extension(assets.clone()))
        .layer(VersionLayer::new(assets.version.clone()));

    let addr = std::env::var("ADDR").unwrap_or_else(|_| "127.0.0.1:3002".to_owned());
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind Axum Svelte example listener");

    println!("listening on http://{addr}/hello");

    axum::serve(listener, app)
        .await
        .expect("Axum Svelte example server failed");
}
