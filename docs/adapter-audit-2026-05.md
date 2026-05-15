# Rocket and Axum Adapter Audit

Audit date: 2026-05-15

This audit checks the current Rocket and Axum integrations against the
currently supported dependency floors:

- `rocket` 0.5.1
- `axum` 0.8.9
- `tower` 0.5.3

The Rocket/Svelte example also uses `rocket_dyn_templates` 0.2.0, but the
library's Rocket adapter is template-engine-agnostic.

The audit is intentionally read-only. It records whether the adapter code is
aligned with the current framework APIs and identifies follow-up work that
should be reviewed separately.

## Summary

The Rocket adapter is aligned with Rocket 0.5.1 and does not need a
framework-version modernization pass beyond keeping the dependency floor
explicit.

The Axum adapter is aligned with Axum 0.8.9 and Tower 0.5.3. Its custom
`VersionLayer`/`VersionService` and `InertiaRequest` extractor are appropriate
for the current public API. The only notable design tradeoff is the shared-prop
extension snapshot used by `InertiaRequest::extension`; it is documented and
covered indirectly by shared-prop tests, so it should not be changed without a
dedicated API review.

No public API changes are recommended as part of this audit.

## Rocket Findings

| Area | Finding | Follow-up |
| --- | --- | --- |
| Request guard | `InertiaHeaders` uses Rocket's current async `FromRequest` shape and always succeeds as documented. | None. |
| Response integration | `Inertia<T>`, `Location`, and `Redirect` implement Rocket responders directly and match Rocket 0.5.1 response conventions. | None. |
| Version handling | `VersionFairing` uses `on_ignite` for route/state setup and `on_request` for pre-handler version conflict routing. | None. |
| Shared props | Managed state is the idiomatic Rocket mechanism for application-wide shared data. | Consider adding an optional provider-skip API only if Rocket users need parity with Axum's `prop_optional`. |
| HTML rendering | The fairing-owned renderer keeps template integration application-defined and template-engine-agnostic. | None. |

## Axum Findings

| Area | Finding | Follow-up |
| --- | --- | --- |
| Extractor | `InertiaRequest` implements `FromRequestParts<S>` with `S: Send + Sync`, matching Axum 0.8 extractor expectations. | None. |
| Version middleware | The custom Tower `Layer`/`Service` is appropriate because it must insert a version extension and short-circuit stale Inertia GET requests before handlers run. | Keep as-is unless a future API redesign replaces it with a higher-level helper. |
| Shared props | `SharedProps` via `Extension` is idiomatic Axum and keeps shared props opt-in. | None. |
| Extension access | `InertiaRequest` snapshots extensions only when `SharedProps` is present, then removes `SharedProps` from the snapshot before provider access. This avoids exposing the registry back to providers and is documented. | If this becomes a performance concern, evaluate a typed state extractor or first-class layer in a separate PR. |
| Handler ergonomics | `request.render(...)`, `request.location(...)`, and `request.redirect(...)` are explicit and avoid requiring users to construct protocol responses by hand. | None. |

## Follow-up Candidates

These are intentionally outside this audit PR:

- Add Rocket `SharedProps::prop_optional` if parity with Axum's optional shared-prop provider becomes important.
- Consider a first-class Axum shared-props layer only if real applications find `Extension(SharedProps)` confusing or if extension snapshot costs show up in profiling.
- Revisit the hand-written Axum `VersionService` only if Axum or Tower introduces a simpler primitive that can still short-circuit before route handlers and preserve the current public API.
- Keep future adapter work, such as Actix Web or Poem, separate from Rocket/Axum modernization.

## Verification Notes

The existing test suite already exercises the important adapter contracts:

- Rocket HTML and JSON page responses, version conflicts, redirects, shared props, lazy props, and partial reloads.
- Axum HTML and JSON page responses, version conflicts, redirects, shared props, lazy props, extension access, and partial reloads.
- Feature-specific builds for `rocket`, `axum`, all features, and no framework features.

Any future adapter behavior change should update the protocol support matrix in
the same PR.
