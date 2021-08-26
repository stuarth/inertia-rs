#[cfg(feature = "rocket")]
pub mod rocket;

#[cfg(feature = "tower")]
pub mod tower;

pub(crate) static X_INERTIA: &str = "X-Inertia";
pub(crate) static X_INERTIA_VERSION: &str = "X-Inertia-Version";
pub(crate) static X_INERTIA_LOCATION: &str = "X-Inertia-Location";

pub struct Inertia<T> {
    component: String,
    props: T,
    url: Option<String>,
}

trait InertiaRequest {
    fn inertia_request(&self) -> bool;

    fn inertia_version(&self) -> Option<String>;
}
