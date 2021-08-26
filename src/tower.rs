use super::{Inertia, InertiaRequest, X_INERTIA, X_INERTIA_LOCATION, X_INERTIA_VERSION};
use http::{Method, Request};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tower::{util::Either, Service};

struct Middleware<S> {
    inner: S,
    version: String,
}

impl<Body> InertiaRequest for Request<Body> {
    fn inertia_request(&self) -> bool {
        self.headers().get(X_INERTIA).is_some()
    }

    fn inertia_version(&self) -> Option<String> {
        self.headers()
            .get(X_INERTIA_VERSION)
            .and_then(|s| s.to_str().ok())
            .map(|s| s.into())
    }
}

impl<S, ReqBody> Service<Request<ReqBody>> for Middleware<S>
where
    S: Service<Request<ReqBody>>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request<ReqBody>) -> Self::Future {
        if request.method() == Method::GET && request.inertia_request() {
            // if the version header isn't sent, assume it's OK??
            if let Some(version) = request.inertia_version() {
                // trace!(
                //     "request version {} / asset version {}",
                //     &version,
                //     &self.version
                // );

                if version != self.version {
                    // early return
                }
            }
        }
        self.inner.call(request)
    }
}
