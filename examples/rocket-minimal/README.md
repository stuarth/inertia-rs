# Rocket Minimal Example

Minimal Rocket integration example for `inertia_rs`.

Run it with:

```sh
ROCKET_PORT=8001 cargo run --manifest-path examples/Cargo.toml -p rocket-minimal
```

Open `http://127.0.0.1:8001/hello` for the HTML first-page response. The
explicit port keeps this example separate from `rocket-svelte`, which uses
Rocket's default port.

The example also registers a Rocket shared prop with `SharedProps`.

An Inertia request with the matching asset version returns JSON:

```sh
curl -H 'X-Inertia: true' -H 'X-Inertia-Version: asset-version-1' \
  http://127.0.0.1:8001/hello
```

A stale or missing Inertia version returns `409 Conflict` with
`X-Inertia-Location`.

```sh
curl -i -H 'X-Inertia: true' -H 'X-Inertia-Version: stale' \
  http://127.0.0.1:8001/hello
```
