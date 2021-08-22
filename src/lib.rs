#[cfg(feature = "rocket")]
pub mod rocket;

pub(crate) static X_INERTIA: &str = "X-Inertia";
pub(crate) static X_INERTIA_VERSION: &str = "X-Inertia-Version";
pub(crate) static X_INERTIA_LOCATION: &str = "X-Inertia-Location";

pub struct Inertia<T> {
    component: String,
    props: T,
    url: Option<String>,
}
