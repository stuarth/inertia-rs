# Protocol Support Matrix

This is the canonical support matrix for the built-in Rocket and Axum
adapters. Update this document in the same PR as any adapter feature change.

Status vocabulary:

- `Supported`: implemented and covered by representative tests.
- `Partial`: implemented with a documented limitation.
- `Not supported`: not implemented.

The "Verified by" column lists representative tests, not every test that
exercises the behavior.

Framework-neutral tests in `src/lib.rs` cover request parsing and page-object
serialization used by both adapters. Adapter-specific tests are listed when the
behavior depends on Rocket or Axum request/response plumbing.

| Feature | Rocket | Axum | Verified by |
| --- | --- | --- | --- |
| Initial HTML response | Supported | Supported | `rocket::tests::html_response_includes_query_string_and_version`; `axum::tests::html_response_includes_escaped_page_and_version` |
| JSON Inertia response | Supported | Supported | `rocket::tests::json_response_includes_query_string`; `axum::tests::inertia_json_response_includes_headers_url_and_version` |
| Asset version conflict | Supported | Supported | `rocket::tests::json_sent_versions_different`; `axum::tests::stale_inertia_version_conflicts_before_handler_runs` |
| Dynamic asset version | Supported | Supported | `rocket::tests::dynamic_version_is_resolved_for_page_responses`; `axum::tests::dynamic_version_is_resolved_for_each_page_response` |
| Query-string and local page URLs | Supported | Supported | `rocket::tests::with_url_overrides_request_uri`; `axum::tests::nested_routes_use_original_uri_for_page_urls` |
| Request header parsing | Supported | Supported | `tests::request_context_parses_inertia_headers`; `rocket::tests::inertia_headers_guard_exposes_request_context`; `axum::tests::inertia_json_response_includes_headers_url_and_version` |
| Partial reloads | Supported | Supported | `rocket::tests::rocket_partial_reload_includes_only_requested_props`; `axum::tests::partial_reloads_include_shared_props_but_preserve_route_owned_roots` |
| Merge and deep-merge metadata | Supported | Supported | `tests::page_serializes_v3_metadata`; `rocket::tests::rocket_reset_omits_merge_and_scroll_metadata_for_reset_props`; `axum::tests::render_supports_advanced_builder_pages` |
| Deferred props | Partial | Partial | `tests::deferred_props_emit_metadata_and_resolve_only_when_requested`; `rocket::tests::rocket_json_response_serializes_v3_metadata_and_omits_deferred_props`; `axum::tests::render_supports_lazy_props` |
| Lazy and optional props | Partial | Partial | `tests::lazy_props_are_only_resolved_when_included`; `tests::optional_props_resolve_only_when_explicitly_requested`; `axum::tests::render_supports_lazy_props` |
| Once props | Supported | Supported | `tests::once_lazy_props_are_not_resolved_when_client_already_has_them`; `rocket::tests::rocket_once_props_already_on_client_are_omitted`; `axum::tests::post_response_ignores_partial_reload_but_preserves_once_exclusions` |
| Shared props | Supported | Supported | `rocket::tests::shared_props_are_merged_into_json_responses`; `axum::tests::shared_props_are_merged_into_json_responses` |
| History flags | Supported | Supported | `tests::page_serializes_v3_metadata`; `rocket::tests::rocket_json_response_serializes_v3_metadata_and_omits_deferred_props` |
| Scroll and infinite-scroll metadata | Supported | Supported | `tests::infinite_scroll_merge_intent_can_prepend_scroll_props`; `rocket::tests::rocket_infinite_scroll_prepend_intent_sets_prepend_metadata` |
| Errors prop and error-bag headers | Partial | Partial | `tests::request_context_parses_inertia_headers`; `tests::lazy_errors_are_preserved_during_partial_reloads` |
| External location redirects | Supported | Supported | `rocket::tests::external_location_response_uses_inertia_conflict_header_for_inertia_requests`; `axum::tests::external_location_uses_conflict_for_inertia_requests` |
| Write-method redirects | Supported | Supported | `rocket::tests::write_redirects_use_see_other_status`; `axum::tests::write_redirects_use_see_other_status` |
| SSR bridge | Not supported | Not supported | Not applicable |

Deferred, lazy, and optional prop support is marked `Partial` because the
current prop container supports synchronous resolvers only. Async prop
resolvers remain planned.

Errors support is marked `Partial` because the protocol header is parsed and
the `errors` prop shape is preserved, but the adapters do not provide a
framework-level validation error bag or flash-message integration.
