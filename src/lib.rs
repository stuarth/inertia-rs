#[macro_use]
extern crate rocket;

use std::sync::Arc;
use rocket::fairing::{Fairing, Info, Kind};
use rocket::http::{self, Method};
use rocket::request::Request;
use rocket::response::{self, Responder, Response};
use rocket::serde::json::Json;
use rocket::Data;
use serde::Serialize;


#[derive(Serialize)]
struct InertiaResponse<T> {
    component: String,
    props: T,
    url: String,
    version: Option<InertiaVersion>,
}

static X_INERTIA: &str = "X-Inertia";
static X_INERTIA_VERSION: &str = "X-Inertia-Version";
static X_INERTIA_LOCATION: &str = "X-Inertia-Location";
const BASE_ROUTE: &str = "/inertia-rs";

trait InertiaRequest {
    fn inertia_request(&self) -> bool;

    fn inertia_version(&self) -> Option<String>;
}

impl<'a> InertiaRequest for Request<'a> {
    fn inertia_request(&self) -> bool {
        self.headers().get_one(X_INERTIA).is_some()
    }

    fn inertia_version(&self) -> Option<String> {
        self.headers().get_one(X_INERTIA_VERSION).map(|s| s.into())
    }
}

#[derive(Serialize)]
pub struct HtmlResponseContext {
    data_page: String,
}

#[derive(Serialize, Clone)]
struct InertiaVersion(String);

impl<'r, 'o: 'r, R: Serialize> Responder<'r, 'o> for Inertia<R> {
    #[inline(always)]
    fn respond_to(self, request: &'r Request<'_>) -> response::Result<'o> {
        // todo: not right, needs query
        let url = self.url.unwrap_or_else(|| request.uri().path().to_string());
        let version = request.local_cache(|| None);

        let inertia_response = InertiaResponse {
            component: self.component,
            props: self.props,
            url,
            version: version.clone(),
        };

        if request.inertia_request() {
            Response::build()
                .merge(Json(inertia_response).respond_to(request)?)
                .raw_header(X_INERTIA, "true")
                .ok()
        } else {
            let ctx = HtmlResponseContext {
                data_page: serde_json::to_string(&inertia_response)
                    .map_err(|_e| http::Status::InternalServerError)?,
            };

            match request.rocket().state::<ResponderFn>() {
                Some(f) => f.0(request, &ctx),
                None => {
                    error!("Responder not found");
                    http::Status::InternalServerError.respond_to(request)
                },
            }
        }
    }
}

#[derive(Serialize)]
struct PageObject<T> {
    component: String,
    props: T,
    url: String,
    version: String,
}

pub struct Inertia<T> {
    component: String,
    props: T,
    url: Option<String>,
}

impl<T> Inertia<T> {
    /// Construct a response for the given component and props. Defaults to 
    /// the request's url. 
    pub fn response<C: Into<String>>(component: C, props: T) -> Self {
        Self {
            component: component.into(),
            props,
            url: None,
        }
    }

    /// Specify the url. Defaults to request's
    pub fn with_url<U: Into<String>>(mut self, url: U) -> Self {
        self.url = Some(url.into());
        self
    }
}

pub struct VersionFairing<'resp> {
    version: String,
    html_response: Arc<dyn Fn(&Request<'_>, &HtmlResponseContext) -> response::Result<'resp> + Send + Sync>,
}

impl<'resp> VersionFairing<'resp> {
    pub fn new<'a, 'b, F, V: Into<String>>(version: V, html_response: F) -> Self
    where
        F: Fn(&Request<'_>, &HtmlResponseContext) -> response::Result<'resp> + Send + Sync + 'static,
    {
        Self {
            version: version.into(),
            html_response: Arc::new(html_response),
        }
    }
}

struct VersionConflictResponse(String);

impl<'r, 'o: 'r> Responder<'r, 'o> for VersionConflictResponse {
    #[inline(always)]
    fn respond_to(self, _request: &'r Request<'_>) -> response::Result<'o> {
        Response::build()
            .status(http::Status::Conflict)
            .raw_header(X_INERTIA_LOCATION, self.0)
            .ok()
    }
}

#[get("/version-conflict?<location>")]
fn version_conflict(location: String) -> VersionConflictResponse {
    VersionConflictResponse(location)
}

struct ResponderFn<'resp>(Arc<dyn Fn(&Request<'_>, &HtmlResponseContext) -> response::Result<'resp> + Send + Sync>);

#[rocket::async_trait]
impl Fairing for VersionFairing<'static> {
    fn info(&self) -> Info {
        Info {
            name: "Inertia Asset Versioning",
            kind: Kind::Ignite | Kind::Request,
        }
    }

    async fn on_ignite(&self, rocket: rocket::Rocket<rocket::Build>) -> rocket::fairing::Result {
        Ok(rocket
            .mount(BASE_ROUTE, routes![version_conflict])
            .manage(ResponderFn(self.html_response.clone())))
    }

    async fn on_request(&self, request: &mut Request<'_>, _: &mut Data<'_>) {
        if request.method() == Method::Get && request.inertia_request() {

            // if the version header isn't sent, assume it's OK??
            if let Some(version) = request.inertia_version() {
                info!(
                    "request version {} / asset version {}",
                    &version, &self.version
                );

                if version != self.version {
                    let uri = uri!(
                        "/inertia-rs",
                        version_conflict(location = request.uri().path().as_str().to_owned())
                    );

                    info!("\tredirecting to {}", &uri.to_string());

                    request.set_uri(uri);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}
