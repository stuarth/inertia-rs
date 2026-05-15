# Rocket Svelte

Small Rocket + Svelte 5 + Inertia example.

The page demonstrates shared props, deferred props, optional props, and partial
reloads from the Svelte client.

## Build Frontend Assets

From the repository root:

```sh
cd examples/rocket-svelte/svelte-app
npm install
npm run build
cd ../../..
```

## Start The Server

```sh
cargo run --manifest-path examples/Cargo.toml -p rocket-svelte
```

Then open http://127.0.0.1:8000/hello.

An Inertia JSON request with the matching asset version returns the page object:

```sh
VERSION=$(jq -r '."src/main.js" | ([.file] + (.css // [])) | join("|")' \
  examples/rocket-svelte/public/build/.vite/manifest.json)

curl -H 'X-Inertia: true' -H "X-Inertia-Version: ${VERSION}" \
  http://127.0.0.1:8000/hello
```
