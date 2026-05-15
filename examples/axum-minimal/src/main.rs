use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Extension;
use axum::Router;
use inertia_rs::axum::{InertiaError, InertiaRequest, SharedProps, VersionLayer};
use inertia_rs::{Inertia, InertiaProps};

async fn hello(request: InertiaRequest) -> Result<Response, InertiaError> {
    request.render(
        Inertia::response(
            "Hello",
            InertiaProps::new()
                .value("name", "world")
                .defer("stats", || 1)
                .optional("debug", || "partial"),
        ),
        |context| {
            Html(format!(
                r#"<!doctype html>
<html>
  <head>
    <meta charset="utf-8" />
    <title>Axum Inertia Example</title>
  </head>
  <body>
    <script data-page="app" type="application/json">{}</script>
    <main id="app">Open this route with an Inertia request to receive JSON.</main>
  </body>
</html>"#,
                context.data_page()
            ))
            .into_response()
        },
    )
}

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/hello", get(hello))
        .layer(Extension(SharedProps::new().value("appName", "Axum Demo")))
        .layer(VersionLayer::new("asset-version-1"));
    let addr = std::env::var("ADDR").unwrap_or_else(|_| "127.0.0.1:3001".to_owned());
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind Axum example listener");

    println!("listening on http://{addr}/hello");

    axum::serve(listener, app)
        .await
        .expect("Axum example server failed");
}
