# Axum Minimal Example

Minimal Axum integration example for `inertia_rs`.

Run it with:

```sh
cargo run --manifest-path examples/Cargo.toml -p axum-minimal
```

Open `http://127.0.0.1:3001/hello` for the HTML first-page response.

The example also registers an Axum shared prop with `SharedProps`.

An Inertia request with the matching asset version returns JSON:

```sh
curl -H 'X-Inertia: true' -H 'X-Inertia-Version: asset-version-1' \
  http://127.0.0.1:3001/hello
```

Deferred and optional props are resolved when a matching partial reload asks
for them:

```sh
curl -H 'X-Inertia: true' -H 'X-Inertia-Version: asset-version-1' \
  -H 'X-Inertia-Partial-Component: Hello' \
  -H 'X-Inertia-Partial-Data: stats,debug' \
  http://127.0.0.1:3001/hello
```

A stale or missing Inertia version returns `409 Conflict` with
`X-Inertia-Location`.
