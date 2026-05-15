# Changelog

All notable changes to this project will be documented in this file.

## Unreleased

### Added

- Rocket 0.5.1, Svelte 5, and Vite example updates.
- Public Inertia protocol header constants and `RequestContext` parsing.
- Inertia v3 page-object metadata for history flags, merge props, deferred props, once props, shared props, and scroll props.
- Partial reload filtering for matching Inertia components.
- Asset version handling for successful page responses and stale Inertia visits.
- `VersionFairing::dynamic` for request-time asset version values.
- `InertiaHeaders` Rocket request guard.
- `Inertia::location` for external Inertia redirects.
- `Inertia::redirect` for method-aware application redirects.
- Rocket `SharedProps` managed state for common page props.
- Initial Axum integration with `InertiaRequest`, `VersionLayer`, page response rendering, external locations, and method-aware redirects.
- Minimal Axum example.
- `InertiaProps` and `ScopedInertiaProps` for synchronous lazy, optional, deferred, and once prop resolvers.
- Axum `SharedProps` extension support for common page props.
- README protocol support matrix.

### Changed

- Preserved query strings in generated page object URLs.
- Added `Vary: X-Inertia` to responses that vary between HTML and JSON.
- Modernized GitHub Actions CI and declared an MSRV of Rust 1.88.
- Expanded crate and public API rustdoc.

### Fixed

- Escaped serialized page JSON for safe embedding in HTML script contexts.
- Kept shared dotted props from merging into route-owned prop roots.
- Preserved route-owned prop roots across partial filtering before shared-prop merging.
- Kept internal route-prop tracking out of `Page` equality.
