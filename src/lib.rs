//! A small Rust adapter for the [Inertia.js](https://inertiajs.com/) protocol.
//!
//! The crate exposes framework-neutral protocol types plus framework
//! integrations behind feature flags. The `rocket` feature is enabled by
//! default and provides a Rocket
//! [`Responder`](https://api.rocket.rs/v0.5/rocket/response/trait.Responder.html)
//! implementation plus asset-versioning support.
//!
//! # Example
//!
//! ```rust
//! use inertia_rs::Inertia;
//!
//! #[derive(serde::Serialize)]
//! struct Props {
//!     name: String,
//! }
//!
//! let response = Inertia::response("Users/Show", Props { name: "Ada".into() });
//! assert_eq!(response.component(), "Users/Show");
//! ```

/// Rocket integration for Inertia responses and asset version checks.
#[cfg(feature = "rocket")]
pub mod rocket;

use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Request header set by Inertia XHR visits.
pub const X_REQUESTED_WITH: &str = "X-Requested-With";

/// Request header containing accepted response content types.
pub const ACCEPT: &str = "Accept";

/// Request and response header used to mark Inertia protocol requests.
pub const X_INERTIA: &str = "X-Inertia";

/// Request header containing the client's current asset version.
pub const X_INERTIA_VERSION: &str = "X-Inertia-Version";

/// Request header containing the component targeted by a partial reload.
pub const X_INERTIA_PARTIAL_COMPONENT: &str = "X-Inertia-Partial-Component";

/// Request header listing props to include in a partial reload.
pub const X_INERTIA_PARTIAL_DATA: &str = "X-Inertia-Partial-Data";

/// Request header listing props to exclude from a partial reload.
pub const X_INERTIA_PARTIAL_EXCEPT: &str = "X-Inertia-Partial-Except";

/// Request header listing props to reset on navigation.
pub const X_INERTIA_RESET: &str = "X-Inertia-Reset";

/// Request header identifying a validation error bag.
pub const X_INERTIA_ERROR_BAG: &str = "X-Inertia-Error-Bag";

/// Request header used by Inertia's infinite scroll protocol.
pub const X_INERTIA_INFINITE_SCROLL_MERGE_INTENT: &str = "X-Inertia-Infinite-Scroll-Merge-Intent";

/// Request header listing once-prop keys the client already has.
pub const X_INERTIA_EXCEPT_ONCE_PROPS: &str = "X-Inertia-Except-Once-Props";

/// Response header used with `409 Conflict` to force a full-page visit.
pub const X_INERTIA_LOCATION: &str = "X-Inertia-Location";

/// Response header used with fragment redirects.
pub const X_INERTIA_REDIRECT: &str = "X-Inertia-Redirect";

/// Response header used to separate HTML and JSON variants in caches.
pub const VARY: &str = "Vary";

/// Request header set to `prefetch` for Inertia prefetch requests.
pub const PURPOSE: &str = "Purpose";

/// Request header set to `no-cache` for Inertia reload requests.
pub const CACHE_CONTROL: &str = "Cache-Control";

/// Inertia protocol header constants.
///
/// Constants are also re-exported at the crate root for the existing API.
pub mod headers {
    pub use super::{
        ACCEPT, CACHE_CONTROL, PURPOSE, VARY, X_INERTIA, X_INERTIA_ERROR_BAG,
        X_INERTIA_EXCEPT_ONCE_PROPS, X_INERTIA_INFINITE_SCROLL_MERGE_INTENT, X_INERTIA_LOCATION,
        X_INERTIA_PARTIAL_COMPONENT, X_INERTIA_PARTIAL_DATA, X_INERTIA_PARTIAL_EXCEPT,
        X_INERTIA_REDIRECT, X_INERTIA_RESET, X_INERTIA_VERSION, X_REQUESTED_WITH,
    };
}

fn is_false(value: &bool) -> bool {
    !value
}

fn empty_map<K, V>(map: &BTreeMap<K, V>) -> bool {
    map.is_empty()
}

#[derive(Clone, Debug, Default)]
struct RouteProps(Vec<String>);

// Internal responder bookkeeping should not affect protocol-level Page equality.
impl PartialEq for RouteProps {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

fn prop_root(prop: &str) -> &str {
    prop.split_once('.')
        .map(|(root, _suffix)| root)
        .unwrap_or(prop)
}

fn prop_is_in_set(prop: &str, set: &BTreeSet<&str>) -> bool {
    set.contains(prop) || set.contains(prop_root(prop))
}

fn prop_matches_reset(prop: &str, reset_props: &BTreeSet<&str>) -> bool {
    reset_props.iter().any(|reset| {
        prop == *reset
            || prop_root(prop) == *reset
            || prop
                .strip_prefix(*reset)
                .map(|suffix| suffix.starts_with('.'))
                .unwrap_or(false)
    })
}

fn scroll_merge_target(prop: &str) -> String {
    format!("{prop}.data")
}

fn dedup_strings(values: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    values.retain(|value| seen.insert(value.clone()));
}

fn parse_header_list(value: Option<&str>) -> Vec<String> {
    value
        .into_iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn contains_no_cache(value: &str) -> bool {
    value
        .split(',')
        .map(str::trim)
        .any(|part| part.eq_ignore_ascii_case("no-cache"))
}

/// Parsed Inertia request headers.
///
/// Framework integrations use this type to avoid duplicating header parsing
/// and partial-reload semantics.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RequestContext {
    is_inertia: bool,
    version: Option<String>,
    partial_component: Option<String>,
    partial_data: Vec<String>,
    partial_except: Vec<String>,
    reset: Vec<String>,
    error_bag: Option<String>,
    infinite_scroll_merge_intent: Option<String>,
    except_once_props: Vec<String>,
    is_prefetch: bool,
    is_reload: bool,
}

impl RequestContext {
    /// Parses a request context from a function that returns header values.
    ///
    /// The callback is invoked with canonical protocol header names such as
    /// `X-Inertia-Partial-Data`. HTTP frameworks usually handle
    /// case-insensitive lookup at this boundary.
    pub fn from_header_fn<'a, F>(mut header: F) -> Self
    where
        F: FnMut(&str) -> Option<&'a str>,
    {
        Self {
            is_inertia: header(X_INERTIA).is_some(),
            version: header(X_INERTIA_VERSION).map(ToOwned::to_owned),
            partial_component: header(X_INERTIA_PARTIAL_COMPONENT).map(ToOwned::to_owned),
            partial_data: parse_header_list(header(X_INERTIA_PARTIAL_DATA)),
            partial_except: parse_header_list(header(X_INERTIA_PARTIAL_EXCEPT)),
            reset: parse_header_list(header(X_INERTIA_RESET)),
            error_bag: header(X_INERTIA_ERROR_BAG).map(ToOwned::to_owned),
            infinite_scroll_merge_intent: header(X_INERTIA_INFINITE_SCROLL_MERGE_INTENT)
                .map(ToOwned::to_owned),
            except_once_props: parse_header_list(header(X_INERTIA_EXCEPT_ONCE_PROPS)),
            is_prefetch: header(PURPOSE)
                .map(|purpose| purpose.eq_ignore_ascii_case("prefetch"))
                .unwrap_or(false),
            is_reload: header(CACHE_CONTROL)
                .map(contains_no_cache)
                .unwrap_or(false),
        }
    }

    /// Returns `true` when the request includes the `X-Inertia` header.
    pub fn is_inertia(&self) -> bool {
        self.is_inertia
    }

    /// Returns the request's `X-Inertia-Version` header value.
    pub fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    /// Returns the partial reload component, if present.
    pub fn partial_component(&self) -> Option<&str> {
        self.partial_component.as_deref()
    }

    /// Returns the props requested through `X-Inertia-Partial-Data`.
    pub fn partial_data(&self) -> &[String] {
        &self.partial_data
    }

    /// Returns the props excluded through `X-Inertia-Partial-Except`.
    pub fn partial_except(&self) -> &[String] {
        &self.partial_except
    }

    /// Returns the props requested for reset on navigation.
    pub fn reset(&self) -> &[String] {
        &self.reset
    }

    /// Returns the validation error bag name, if present.
    pub fn error_bag(&self) -> Option<&str> {
        self.error_bag.as_deref()
    }

    /// Returns the infinite-scroll merge intent, if present.
    pub fn infinite_scroll_merge_intent(&self) -> Option<&str> {
        self.infinite_scroll_merge_intent.as_deref()
    }

    /// Returns once-prop keys the client already has.
    pub fn except_once_props(&self) -> &[String] {
        &self.except_once_props
    }

    /// Returns `true` when the request purpose is `prefetch`.
    pub fn is_prefetch(&self) -> bool {
        self.is_prefetch
    }

    /// Returns `true` when the request cache control contains `no-cache`.
    pub fn is_reload(&self) -> bool {
        self.is_reload
    }

    /// Returns `true` when partial reload headers target `component`.
    pub fn partial_reload_matches(&self, component: &str) -> bool {
        self.is_inertia && self.partial_component.as_deref() == Some(component)
    }

    /// Returns a copy with partial reload headers ignored.
    ///
    /// Framework integrations can use this for non-GET responses, where the
    /// Inertia protocol does not apply partial reload filtering. Once-prop
    /// exclusions are preserved because they are independent of partial reloads.
    pub fn without_partial_reload(mut self) -> Self {
        self.partial_component = None;
        self.partial_data.clear();
        self.partial_except.clear();
        self.reset.clear();
        self.infinite_scroll_merge_intent = None;
        self
    }

    /// Filters a serialized props object according to Inertia request headers.
    ///
    /// Non-object props are left untouched. Object props always contain an
    /// `errors` object after filtering, matching Inertia's page object shape.
    /// When both partial-data and partial-except headers are present,
    /// partial-except takes precedence, matching the Inertia v3 protocol.
    pub fn filter_props(&self, component: &str, props: &mut Value, metadata: &PageMetadata) {
        let Some(props) = props.as_object_mut() else {
            return;
        };

        ensure_errors_prop(props);

        let partial_matches = self.partial_reload_matches(component);
        let partial_requested = string_set(&self.partial_data);
        let partial_excluded = string_set(&self.partial_except);
        let always_props = string_set(metadata.always_props());
        let deferred_props = metadata.deferred_prop_names();
        let once_excluded_props = metadata.once_props_excluded_by(self);

        let keys = props.keys().cloned().collect::<Vec<_>>();

        for key in keys {
            if key == "errors" {
                continue;
            }

            if always_props.contains(key.as_str()) {
                continue;
            }

            let explicitly_requested = partial_matches && partial_requested.contains(key.as_str());
            let mut include = true;

            if partial_matches {
                include = if !partial_excluded.is_empty() {
                    !partial_excluded.contains(key.as_str())
                } else if !partial_requested.is_empty() {
                    partial_requested.contains(key.as_str())
                } else {
                    true
                };
            }

            if deferred_props.contains(key.as_str()) && !explicitly_requested {
                include = false;
            }

            if once_excluded_props.contains(key.as_str()) && !explicitly_requested {
                include = false;
            }

            if !include {
                props.remove(&key);
            }
        }
    }
}

fn ensure_errors_prop(props: &mut Map<String, Value>) {
    props
        .entry("errors")
        .or_insert_with(|| Value::Object(Map::new()));
}

fn insert_shared_prop_path(props: &mut Map<String, Value>, path: &[&str], value: Value) -> bool {
    if path.is_empty() {
        return false;
    }

    if path.len() == 1 {
        if props.contains_key(path[0]) {
            return false;
        }

        props.insert(path[0].to_owned(), value);
        return true;
    }

    let entry = props
        .entry(path[0].to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    let Value::Object(nested) = entry else {
        return false;
    };

    insert_shared_prop_path(nested, &path[1..], value)
}

fn string_set(values: &[String]) -> BTreeSet<&str> {
    values.iter().map(String::as_str).collect()
}

/// Additional Inertia v3 page-object metadata.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageMetadata {
    encrypt_history: bool,
    clear_history: bool,
    preserve_fragment: bool,
    always_props: Vec<String>,
    merge_props: Vec<String>,
    prepend_props: Vec<String>,
    deep_merge_props: Vec<String>,
    match_props_on: Vec<String>,
    scroll_props: BTreeMap<String, ScrollProps>,
    deferred_props: BTreeMap<String, Vec<String>>,
    rescued_props: Vec<String>,
    shared_props: Vec<String>,
    once_props: BTreeMap<String, OnceProp>,
}

impl PageMetadata {
    /// Creates empty page metadata.
    pub fn new() -> Self {
        Self::default()
    }

    /// Marks the page's history state for encryption.
    pub fn encrypt_history(mut self) -> Self {
        self.encrypt_history = true;
        self
    }

    /// Marks the response as clearing encrypted history state.
    pub fn clear_history(mut self) -> Self {
        self.clear_history = true;
        self
    }

    /// Preserves the original URL fragment across a redirect.
    pub fn preserve_fragment(mut self) -> Self {
        self.preserve_fragment = true;
        self
    }

    /// Marks a prop key to always be included during partial reloads.
    pub fn always<P: Into<String>>(mut self, prop: P) -> Self {
        self.always_props.push(prop.into());
        self
    }

    /// Marks a prop key for append-style merging.
    pub fn merge<P: Into<String>>(mut self, prop: P) -> Self {
        self.merge_props.push(prop.into());
        self
    }

    /// Marks a prop key for prepend-style merging.
    pub fn prepend<P: Into<String>>(mut self, prop: P) -> Self {
        self.prepend_props.push(prop.into());
        self
    }

    /// Marks a prop key for deep merging.
    pub fn deep_merge<P: Into<String>>(mut self, prop: P) -> Self {
        self.deep_merge_props.push(prop.into());
        self
    }

    /// Adds a matching key used by merge metadata.
    pub fn match_on<P: Into<String>>(mut self, prop: P) -> Self {
        self.match_props_on.push(prop.into());
        self
    }

    /// Adds infinite-scroll metadata for a prop.
    pub fn scroll<P: Into<String>>(mut self, prop: P, scroll: ScrollProps) -> Self {
        let prop = prop.into();
        let merge_target = scroll_merge_target(&prop);

        if !self.merge_props.contains(&merge_target) && !self.prepend_props.contains(&merge_target)
        {
            self.merge_props.push(merge_target);
        }

        self.scroll_props.insert(prop, scroll);
        self
    }

    /// Marks a prop as deferred in the default group.
    ///
    /// This declares page-object metadata and omits the prop value until it is
    /// explicitly requested by a partial reload. It does not install a lazy or
    /// async resolver.
    pub fn defer<P: Into<String>>(self, prop: P) -> Self {
        self.defer_group("default", prop)
    }

    /// Marks a prop as deferred in `group`.
    ///
    /// This declares page-object metadata and omits the prop value until it is
    /// explicitly requested by a partial reload. It does not install a lazy or
    /// async resolver.
    pub fn defer_group<G: Into<String>, P: Into<String>>(mut self, group: G, prop: P) -> Self {
        self.deferred_props
            .entry(group.into())
            .or_default()
            .push(prop.into());
        self
    }

    /// Marks a deferred prop as rescued.
    ///
    /// This only serializes the `rescuedProps` metadata. It does not catch
    /// errors while resolving prop values.
    pub fn rescue<P: Into<String>>(mut self, prop: P) -> Self {
        self.rescued_props.push(prop.into());
        self
    }

    /// Marks a top-level prop as shared.
    ///
    /// This only serializes the `sharedProps` metadata. It does not register or
    /// merge global shared application state.
    pub fn share<P: Into<String>>(mut self, prop: P) -> Self {
        self.shared_props.push(prop.into());
        self
    }

    /// Marks a prop as a once prop using the prop name as the once key.
    pub fn once<P: Into<String>>(mut self, prop: P) -> Self {
        let prop = prop.into();
        self.once_props.insert(prop.clone(), OnceProp::new(prop));
        self
    }

    /// Marks a prop as a once prop with an explicit once key.
    pub fn once_with_key<K: Into<String>>(mut self, key: K, once: OnceProp) -> Self {
        self.once_props.insert(key.into(), once);
        self
    }

    /// Returns whether encrypted history is enabled.
    pub fn encrypt_history_enabled(&self) -> bool {
        self.encrypt_history
    }

    /// Returns whether clear history is enabled.
    pub fn clear_history_enabled(&self) -> bool {
        self.clear_history
    }

    /// Returns whether fragment preservation is enabled.
    pub fn preserve_fragment_enabled(&self) -> bool {
        self.preserve_fragment
    }

    /// Returns props that always survive partial-reload filtering.
    pub fn always_props(&self) -> &[String] {
        &self.always_props
    }

    /// Returns append-merge prop keys.
    pub fn merge_props(&self) -> &[String] {
        &self.merge_props
    }

    /// Returns prepend-merge prop keys.
    pub fn prepend_props(&self) -> &[String] {
        &self.prepend_props
    }

    /// Returns deep-merge prop keys.
    pub fn deep_merge_props(&self) -> &[String] {
        &self.deep_merge_props
    }

    /// Returns merge matching keys.
    pub fn match_props_on(&self) -> &[String] {
        &self.match_props_on
    }

    /// Returns infinite-scroll prop metadata.
    pub fn scroll_props(&self) -> &BTreeMap<String, ScrollProps> {
        &self.scroll_props
    }

    /// Returns deferred prop groups.
    pub fn deferred_props(&self) -> &BTreeMap<String, Vec<String>> {
        &self.deferred_props
    }

    /// Returns rescued deferred prop keys.
    pub fn rescued_props(&self) -> &[String] {
        &self.rescued_props
    }

    /// Returns shared prop keys.
    pub fn shared_props(&self) -> &[String] {
        &self.shared_props
    }

    /// Returns once-prop metadata.
    pub fn once_props(&self) -> &BTreeMap<String, OnceProp> {
        &self.once_props
    }

    fn deferred_prop_names(&self) -> BTreeSet<&str> {
        self.deferred_props
            .values()
            .flat_map(|props| props.iter().map(String::as_str))
            .collect()
    }

    fn once_props_excluded_by(&self, context: &RequestContext) -> BTreeSet<&str> {
        let excluded_once_keys = string_set(context.except_once_props());

        self.once_props
            .iter()
            .filter(|(key, _once)| excluded_once_keys.contains(key.as_str()))
            .map(|(_key, once)| once.prop())
            .collect()
    }

    fn for_response(
        &self,
        context: &RequestContext,
        component: &str,
        props: Option<&Map<String, Value>>,
    ) -> Self {
        let mut metadata = self.clone();
        let Some(props) = props else {
            return metadata;
        };

        let included_props = props.keys().map(String::as_str).collect::<BTreeSet<_>>();
        let partial_matches = context.partial_reload_matches(component);
        let reset_props = if partial_matches {
            string_set(context.reset())
        } else {
            BTreeSet::new()
        };

        metadata.merge_props.retain(|prop| {
            prop_is_in_set(prop, &included_props) && !prop_matches_reset(prop, &reset_props)
        });
        metadata.prepend_props.retain(|prop| {
            prop_is_in_set(prop, &included_props) && !prop_matches_reset(prop, &reset_props)
        });
        metadata.deep_merge_props.retain(|prop| {
            prop_is_in_set(prop, &included_props) && !prop_matches_reset(prop, &reset_props)
        });
        metadata.match_props_on.retain(|prop| {
            prop_is_in_set(prop, &included_props) && !prop_matches_reset(prop, &reset_props)
        });
        metadata.scroll_props.retain(|prop, _scroll| {
            prop_is_in_set(prop, &included_props) && !prop_matches_reset(prop, &reset_props)
        });

        if let Some(intent) = partial_matches
            .then(|| context.infinite_scroll_merge_intent())
            .flatten()
        {
            for prop in metadata.scroll_props.keys() {
                let target = scroll_merge_target(prop);

                if intent.eq_ignore_ascii_case("prepend") {
                    metadata.merge_props.retain(|prop| prop != &target);

                    if !metadata.prepend_props.contains(&target) {
                        metadata.prepend_props.push(target);
                    }
                } else if intent.eq_ignore_ascii_case("append") {
                    metadata.prepend_props.retain(|prop| prop != &target);

                    if !metadata.merge_props.contains(&target) {
                        metadata.merge_props.push(target);
                    }
                }
            }
        }

        dedup_strings(&mut metadata.merge_props);
        dedup_strings(&mut metadata.prepend_props);
        dedup_strings(&mut metadata.deep_merge_props);
        dedup_strings(&mut metadata.match_props_on);

        metadata
    }
}

/// Metadata for a prop that should only be resolved once.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OnceProp {
    prop: String,
    expires_at: Option<u64>,
}

impl OnceProp {
    /// Creates once-prop metadata for `prop`.
    pub fn new<P: Into<String>>(prop: P) -> Self {
        Self {
            prop: prop.into(),
            expires_at: None,
        }
    }

    /// Sets the client-side expiration timestamp in milliseconds.
    pub fn expires_at(mut self, expires_at: u64) -> Self {
        self.expires_at = Some(expires_at);
        self
    }

    /// Returns the prop name associated with this once key.
    pub fn prop(&self) -> &str {
        &self.prop
    }

    /// Returns the optional expiration timestamp in milliseconds.
    pub fn expiration(&self) -> Option<u64> {
        self.expires_at
    }
}

/// Infinite-scroll metadata for one page prop.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScrollProps {
    page_name: String,
    previous_page: Option<u64>,
    next_page: Option<u64>,
    current_page: u64,
}

impl ScrollProps {
    /// Creates scroll metadata for `page_name` at `current_page`.
    pub fn new<P: Into<String>>(page_name: P, current_page: u64) -> Self {
        Self {
            page_name: page_name.into(),
            previous_page: None,
            next_page: None,
            current_page,
        }
    }

    /// Sets the previous page number.
    pub fn previous_page(mut self, previous_page: u64) -> Self {
        self.previous_page = Some(previous_page);
        self
    }

    /// Sets the next page number.
    pub fn next_page(mut self, next_page: u64) -> Self {
        self.next_page = Some(next_page);
        self
    }

    /// Returns the query parameter name used for pagination.
    pub fn page_name(&self) -> &str {
        &self.page_name
    }

    /// Returns the previous page number, if any.
    pub fn previous(&self) -> Option<u64> {
        self.previous_page
    }

    /// Returns the next page number, if any.
    pub fn next(&self) -> Option<u64> {
        self.next_page
    }

    /// Returns the current page number.
    pub fn current(&self) -> u64 {
        self.current_page
    }
}

/// A serializable Inertia page object.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Page<T> {
    component: String,
    props: T,
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    encrypt_history: bool,
    #[serde(skip_serializing_if = "is_false")]
    clear_history: bool,
    #[serde(skip_serializing_if = "is_false")]
    preserve_fragment: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    merge_props: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    prepend_props: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    deep_merge_props: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    match_props_on: Vec<String>,
    #[serde(skip_serializing_if = "empty_map")]
    scroll_props: BTreeMap<String, ScrollProps>,
    #[serde(skip_serializing_if = "empty_map")]
    deferred_props: BTreeMap<String, Vec<String>>,
    #[serde(skip)]
    route_props: RouteProps,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    rescued_props: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    shared_props: Vec<String>,
    #[serde(skip_serializing_if = "empty_map")]
    once_props: BTreeMap<String, OnceProp>,
}

impl<T> Page<T> {
    /// Creates a minimal page object.
    pub fn new<C: Into<String>, U: Into<String>>(component: C, props: T, url: U) -> Self {
        Self::from_parts(component, props, url, None, PageMetadata::new())
    }

    /// Creates a page object from explicit parts.
    pub fn from_parts<C: Into<String>, U: Into<String>>(
        component: C,
        props: T,
        url: U,
        version: Option<String>,
        metadata: PageMetadata,
    ) -> Self {
        Self {
            component: component.into(),
            props,
            url: url.into(),
            version,
            encrypt_history: metadata.encrypt_history,
            clear_history: metadata.clear_history,
            preserve_fragment: metadata.preserve_fragment,
            merge_props: metadata.merge_props,
            prepend_props: metadata.prepend_props,
            deep_merge_props: metadata.deep_merge_props,
            match_props_on: metadata.match_props_on,
            scroll_props: metadata.scroll_props,
            deferred_props: metadata.deferred_props,
            route_props: RouteProps::default(),
            rescued_props: metadata.rescued_props,
            shared_props: metadata.shared_props,
            once_props: metadata.once_props,
        }
    }

    /// Sets the page object's asset version.
    pub fn version<V: Into<String>>(mut self, version: V) -> Self {
        self.version = Some(version.into());
        self
    }

    /// Returns the component name.
    pub fn component(&self) -> &str {
        &self.component
    }

    /// Returns the page props.
    pub fn props(&self) -> &T {
        &self.props
    }

    /// Returns the page URL.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Returns the asset version, if present.
    pub fn asset_version(&self) -> Option<&str> {
        self.version.as_deref()
    }
}

impl Page<Value> {
    /// Merges shared props into the page object.
    ///
    /// Existing page props take precedence when keys collide. Dotted keys are
    /// expanded into nested objects, and inserted top-level keys are added to
    /// the page object's `sharedProps` metadata.
    pub fn with_shared_props<I, K>(mut self, shared_props: I) -> Self
    where
        I: IntoIterator<Item = (K, Value)>,
        K: Into<String>,
    {
        if !self.props.is_object() {
            self.props = Value::Object(Map::new());
        }

        let props = self
            .props
            .as_object_mut()
            .expect("props was normalized to an object");
        ensure_errors_prop(props);
        let route_roots = if self.route_props.0.is_empty() {
            props.keys().cloned().collect::<BTreeSet<_>>()
        } else {
            self.route_props.0.iter().cloned().collect()
        };

        for (key, value) in shared_props {
            let key = key.into();
            let path = key
                .split('.')
                .filter(|segment| !segment.is_empty())
                .collect::<Vec<_>>();
            let Some(root) = path.first() else {
                continue;
            };
            let root = (*root).to_owned();

            if route_roots.contains(&root) {
                continue;
            }

            if insert_shared_prop_path(props, &path, value) && !self.shared_props.contains(&root) {
                self.shared_props.push(root);
            }
        }

        self
    }
}

/// Builder for the advanced `Inertia::page(...).props(...)` API.
pub struct InertiaPageBuilder {
    component: String,
    url: Option<String>,
    metadata: PageMetadata,
}

impl InertiaPageBuilder {
    /// Marks the page's history state for encryption.
    pub fn encrypt_history(mut self) -> Self {
        self.metadata = self.metadata.encrypt_history();
        self
    }

    /// Marks the response as clearing encrypted history state.
    pub fn clear_history(mut self) -> Self {
        self.metadata = self.metadata.clear_history();
        self
    }

    /// Preserves the original URL fragment across a redirect.
    pub fn preserve_fragment(mut self) -> Self {
        self.metadata = self.metadata.preserve_fragment();
        self
    }

    /// Marks a prop key to always be included during partial reloads.
    pub fn always<P: Into<String>>(mut self, prop: P) -> Self {
        self.metadata = self.metadata.always(prop);
        self
    }

    /// Marks a prop key for append-style merging.
    pub fn merge<P: Into<String>>(mut self, prop: P) -> Self {
        self.metadata = self.metadata.merge(prop);
        self
    }

    /// Marks a prop key for prepend-style merging.
    pub fn prepend<P: Into<String>>(mut self, prop: P) -> Self {
        self.metadata = self.metadata.prepend(prop);
        self
    }

    /// Marks a prop key for deep merging.
    pub fn deep_merge<P: Into<String>>(mut self, prop: P) -> Self {
        self.metadata = self.metadata.deep_merge(prop);
        self
    }

    /// Adds a matching key used by merge metadata.
    pub fn match_on<P: Into<String>>(mut self, prop: P) -> Self {
        self.metadata = self.metadata.match_on(prop);
        self
    }

    /// Adds infinite-scroll metadata for a prop.
    pub fn scroll<P: Into<String>>(mut self, prop: P, scroll: ScrollProps) -> Self {
        self.metadata = self.metadata.scroll(prop, scroll);
        self
    }

    /// Marks a prop as deferred in the default group.
    ///
    /// This declares page-object metadata and omits the prop value until it is
    /// explicitly requested by a partial reload. It does not install a lazy or
    /// async resolver.
    pub fn defer<P: Into<String>>(mut self, prop: P) -> Self {
        self.metadata = self.metadata.defer(prop);
        self
    }

    /// Marks a prop as deferred in `group`.
    ///
    /// This declares page-object metadata and omits the prop value until it is
    /// explicitly requested by a partial reload. It does not install a lazy or
    /// async resolver.
    pub fn defer_group<G: Into<String>, P: Into<String>>(mut self, group: G, prop: P) -> Self {
        self.metadata = self.metadata.defer_group(group, prop);
        self
    }

    /// Marks a deferred prop as rescued.
    ///
    /// This only serializes the `rescuedProps` metadata. It does not catch
    /// errors while resolving prop values.
    pub fn rescue<P: Into<String>>(mut self, prop: P) -> Self {
        self.metadata = self.metadata.rescue(prop);
        self
    }

    /// Marks a top-level prop as shared.
    ///
    /// This only serializes the `sharedProps` metadata. It does not register or
    /// merge global shared application state.
    pub fn share<P: Into<String>>(mut self, prop: P) -> Self {
        self.metadata = self.metadata.share(prop);
        self
    }

    /// Marks a prop as a once prop using the prop name as the once key.
    pub fn once<P: Into<String>>(mut self, prop: P) -> Self {
        self.metadata = self.metadata.once(prop);
        self
    }

    /// Marks a prop as a once prop with an explicit once key.
    pub fn once_with_key<K: Into<String>>(mut self, key: K, once: OnceProp) -> Self {
        self.metadata = self.metadata.once_with_key(key, once);
        self
    }

    /// Sets the page props and returns an [`Inertia`] response.
    pub fn props<T>(self, props: T) -> Inertia<T> {
        Inertia {
            component: self.component,
            props,
            url: self.url,
            metadata: self.metadata,
        }
    }

    /// Overrides the page object's `url` field.
    pub fn with_url<U: Into<String>>(mut self, url: U) -> Self {
        self.url = Some(url.into());
        self
    }
}

impl Inertia<()> {
    /// Starts the advanced page builder API.
    pub fn page<C: Into<String>>(component: C) -> InertiaPageBuilder {
        InertiaPageBuilder {
            component: component.into(),
            url: None,
            metadata: PageMetadata::new(),
        }
    }

    /// Creates an external redirect response.
    ///
    /// Framework integrations should convert this into a `409 Conflict`
    /// response with the destination URL in the `X-Inertia-Location` header.
    pub fn location<U: Into<String>>(url: U) -> Location {
        Location::new(url)
    }

    /// Creates a method-aware redirect response.
    ///
    /// Framework integrations should use `303 See Other` for write-method
    /// requests so the follow-up request is a `GET`.
    pub fn redirect<U: Into<String>>(url: U) -> Redirect {
        Redirect::new(url)
    }
}

/// A server-initiated external location visit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    url: String,
}

impl Location {
    /// Creates an external location redirect to `url`.
    pub fn new<U: Into<String>>(url: U) -> Self {
        Self { url: url.into() }
    }

    /// Returns the destination URL.
    pub fn url(&self) -> &str {
        &self.url
    }
}

/// A method-aware redirect response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Redirect {
    url: String,
}

impl Redirect {
    /// Creates a redirect response to `url`.
    pub fn new<U: Into<String>>(url: U) -> Self {
        Self { url: url.into() }
    }

    /// Returns the redirect destination URL.
    pub fn url(&self) -> &str {
        &self.url
    }
}

/// A framework-neutral Inertia page response.
///
/// Framework integrations convert this value into either an HTML first-load
/// response or a JSON Inertia response, depending on the incoming request
/// headers.
pub struct Inertia<T> {
    component: String,
    props: T,
    url: Option<String>,
    metadata: PageMetadata,
}

impl<T> Inertia<T> {
    /// Constructs a response for `component` with serializable `props`.
    ///
    /// Framework integrations default the page object's `url` field to the
    /// current request URI unless [`with_url`](Self::with_url) is used.
    pub fn response<C: Into<String>>(component: C, props: T) -> Self {
        Self {
            component: component.into(),
            props,
            url: None,
            metadata: PageMetadata::new(),
        }
    }

    /// Overrides the page object's `url` field.
    pub fn with_url<U: Into<String>>(mut self, url: U) -> Self {
        self.url = Some(url.into());
        self
    }

    /// Marks the page's history state for encryption.
    pub fn encrypt_history(mut self) -> Self {
        self.metadata = self.metadata.encrypt_history();
        self
    }

    /// Marks the response as clearing encrypted history state.
    pub fn clear_history(mut self) -> Self {
        self.metadata = self.metadata.clear_history();
        self
    }

    /// Preserves the original URL fragment across a redirect.
    pub fn preserve_fragment(mut self) -> Self {
        self.metadata = self.metadata.preserve_fragment();
        self
    }

    /// Marks a prop key to always be included during partial reloads.
    pub fn always<P: Into<String>>(mut self, prop: P) -> Self {
        self.metadata = self.metadata.always(prop);
        self
    }

    /// Marks a prop key for append-style merging.
    pub fn merge<P: Into<String>>(mut self, prop: P) -> Self {
        self.metadata = self.metadata.merge(prop);
        self
    }

    /// Marks a prop key for prepend-style merging.
    pub fn prepend<P: Into<String>>(mut self, prop: P) -> Self {
        self.metadata = self.metadata.prepend(prop);
        self
    }

    /// Marks a prop key for deep merging.
    pub fn deep_merge<P: Into<String>>(mut self, prop: P) -> Self {
        self.metadata = self.metadata.deep_merge(prop);
        self
    }

    /// Adds a matching key used by merge metadata.
    pub fn match_on<P: Into<String>>(mut self, prop: P) -> Self {
        self.metadata = self.metadata.match_on(prop);
        self
    }

    /// Adds infinite-scroll metadata for a prop.
    pub fn scroll<P: Into<String>>(mut self, prop: P, scroll: ScrollProps) -> Self {
        self.metadata = self.metadata.scroll(prop, scroll);
        self
    }

    /// Marks a prop as deferred in the default group.
    ///
    /// This declares page-object metadata and omits the prop value until it is
    /// explicitly requested by a partial reload. It does not install a lazy or
    /// async resolver.
    pub fn defer<P: Into<String>>(mut self, prop: P) -> Self {
        self.metadata = self.metadata.defer(prop);
        self
    }

    /// Marks a prop as deferred in `group`.
    ///
    /// This declares page-object metadata and omits the prop value until it is
    /// explicitly requested by a partial reload. It does not install a lazy or
    /// async resolver.
    pub fn defer_group<G: Into<String>, P: Into<String>>(mut self, group: G, prop: P) -> Self {
        self.metadata = self.metadata.defer_group(group, prop);
        self
    }

    /// Marks a deferred prop as rescued.
    ///
    /// This only serializes the `rescuedProps` metadata. It does not catch
    /// errors while resolving prop values.
    pub fn rescue<P: Into<String>>(mut self, prop: P) -> Self {
        self.metadata = self.metadata.rescue(prop);
        self
    }

    /// Marks a top-level prop as shared.
    ///
    /// This only serializes the `sharedProps` metadata. It does not register or
    /// merge global shared application state.
    pub fn share<P: Into<String>>(mut self, prop: P) -> Self {
        self.metadata = self.metadata.share(prop);
        self
    }

    /// Marks a prop as a once prop using the prop name as the once key.
    pub fn once<P: Into<String>>(mut self, prop: P) -> Self {
        self.metadata = self.metadata.once(prop);
        self
    }

    /// Marks a prop as a once prop with an explicit once key.
    pub fn once_with_key<K: Into<String>>(mut self, key: K, once: OnceProp) -> Self {
        self.metadata = self.metadata.once_with_key(key, once);
        self
    }

    /// Returns the Inertia component name.
    pub fn component(&self) -> &str {
        &self.component
    }

    /// Returns a reference to the component props.
    pub fn props(&self) -> &T {
        &self.props
    }

    /// Returns the explicit page URL override, if one was set.
    pub fn url(&self) -> Option<&str> {
        self.url.as_deref()
    }

    /// Returns the configured page metadata.
    pub fn metadata(&self) -> &PageMetadata {
        &self.metadata
    }
}

impl<T: Serialize> Inertia<T> {
    /// Builds a concrete Inertia page object.
    ///
    /// Framework integrations pass the resolved request URL, asset version,
    /// and parsed request context so props can be filtered for partial reloads,
    /// deferred props, and once props.
    pub fn into_page(
        self,
        url: impl Into<String>,
        version: Option<String>,
        request: &RequestContext,
    ) -> Result<Page<Value>, serde_json::Error> {
        let component = self.component;
        let metadata = self.metadata;
        let mut props = serde_json::to_value(self.props)?;
        let route_props = props
            .as_object()
            .map(|props| props.keys().cloned().collect())
            .unwrap_or_default();

        request.filter_props(&component, &mut props, &metadata);
        let metadata = metadata.for_response(request, &component, props.as_object());

        let mut page = Page::from_parts(component, props, url, version, metadata);
        page.route_props = RouteProps(route_props);

        Ok(page)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    fn request_context_from(headers: &[(&str, &str)]) -> RequestContext {
        let headers = headers.iter().copied().collect::<HashMap<_, _>>();

        RequestContext::from_header_fn(|name| headers.get(name).copied())
    }

    #[test]
    fn request_context_parses_inertia_headers() {
        let context = request_context_from(&[
            (X_INERTIA, "true"),
            (X_INERTIA_VERSION, "abc"),
            (X_INERTIA_PARTIAL_COMPONENT, "Users/Index"),
            (X_INERTIA_PARTIAL_DATA, "users, stats"),
            (X_INERTIA_RESET, "users"),
            (X_INERTIA_ERROR_BAG, "createUser"),
            (X_INERTIA_INFINITE_SCROLL_MERGE_INTENT, "append"),
            (X_INERTIA_EXCEPT_ONCE_PROPS, "plans,features"),
            (PURPOSE, "prefetch"),
            (CACHE_CONTROL, "max-age=0, no-cache"),
        ]);

        assert!(context.is_inertia());
        assert_eq!(context.version(), Some("abc"));
        assert_eq!(context.partial_component(), Some("Users/Index"));
        assert_eq!(context.partial_data(), ["users", "stats"]);
        assert_eq!(context.reset(), ["users"]);
        assert_eq!(context.error_bag(), Some("createUser"));
        assert_eq!(context.infinite_scroll_merge_intent(), Some("append"));
        assert_eq!(context.except_once_props(), ["plans", "features"]);
        assert!(context.is_prefetch());
        assert!(context.is_reload());
    }

    #[test]
    fn page_serializes_v3_metadata() {
        let page = Page::from_parts(
            "Feed/Index",
            json!({ "errors": {}, "posts": [{ "id": 1 }] }),
            "/feed",
            Some("version-1".into()),
            PageMetadata::new()
                .encrypt_history()
                .clear_history()
                .preserve_fragment()
                .merge("posts")
                .prepend("notifications")
                .deep_merge("conversations")
                .match_on("posts.id")
                .scroll("posts", ScrollProps::new("page", 1).next_page(2))
                .defer("analytics")
                .rescue("analytics")
                .share("auth")
                .once("plans"),
        );

        let value = serde_json::to_value(page).unwrap();

        assert_eq!(value["component"], "Feed/Index");
        assert_eq!(value["version"], "version-1");
        assert_eq!(value["encryptHistory"], true);
        assert_eq!(value["clearHistory"], true);
        assert_eq!(value["preserveFragment"], true);
        assert_eq!(value["mergeProps"], json!(["posts", "posts.data"]));
        assert_eq!(value["prependProps"], json!(["notifications"]));
        assert_eq!(value["deepMergeProps"], json!(["conversations"]));
        assert_eq!(value["matchPropsOn"], json!(["posts.id"]));
        assert_eq!(value["scrollProps"]["posts"]["pageName"], "page");
        assert_eq!(value["scrollProps"]["posts"]["nextPage"], 2);
        assert_eq!(value["deferredProps"], json!({ "default": ["analytics"] }));
        assert_eq!(value["rescuedProps"], json!(["analytics"]));
        assert_eq!(value["sharedProps"], json!(["auth"]));
        assert_eq!(
            value["onceProps"]["plans"],
            json!({ "prop": "plans", "expiresAt": null })
        );
    }

    #[test]
    fn partial_data_filters_matching_component_props() {
        let context = request_context_from(&[
            (X_INERTIA, "true"),
            (X_INERTIA_PARTIAL_COMPONENT, "Events"),
            (X_INERTIA_PARTIAL_DATA, "events"),
        ]);
        let mut props = json!({
            "auth": { "name": "Ada" },
            "events": [1, 2],
            "categories": ["meetups"]
        });

        context.filter_props("Events", &mut props, &PageMetadata::new().always("auth"));

        assert_eq!(
            props,
            json!({
                "errors": {},
                "auth": { "name": "Ada" },
                "events": [1, 2]
            })
        );
    }

    #[test]
    fn partial_except_takes_precedence_over_partial_data() {
        let context = request_context_from(&[
            (X_INERTIA, "true"),
            (X_INERTIA_PARTIAL_COMPONENT, "Events"),
            (X_INERTIA_PARTIAL_DATA, "events"),
            (X_INERTIA_PARTIAL_EXCEPT, "categories"),
        ]);
        let mut props = json!({
            "auth": { "name": "Ada" },
            "events": [1, 2],
            "categories": ["meetups"]
        });

        context.filter_props("Events", &mut props, &PageMetadata::new());

        assert_eq!(
            props,
            json!({
                "errors": {},
                "auth": { "name": "Ada" },
                "events": [1, 2]
            })
        );
    }

    #[test]
    fn partial_except_without_partial_data_excludes_listed_props() {
        let context = request_context_from(&[
            (X_INERTIA, "true"),
            (X_INERTIA_PARTIAL_COMPONENT, "Events"),
            (X_INERTIA_PARTIAL_EXCEPT, "categories"),
        ]);
        let mut props = json!({
            "events": [1, 2],
            "categories": ["meetups"],
            "filters": { "open": true }
        });

        context.filter_props("Events", &mut props, &PageMetadata::new());

        assert_eq!(
            props,
            json!({
                "errors": {},
                "events": [1, 2],
                "filters": { "open": true }
            })
        );
    }

    #[test]
    fn partial_headers_are_ignored_for_different_components() {
        let context = request_context_from(&[
            (X_INERTIA, "true"),
            (X_INERTIA_PARTIAL_COMPONENT, "Events"),
            (X_INERTIA_PARTIAL_DATA, "events"),
        ]);
        let mut props = json!({
            "auth": { "name": "Ada" },
            "events": [1, 2]
        });

        context.filter_props("Dashboard", &mut props, &PageMetadata::new());

        assert_eq!(
            props,
            json!({
                "errors": {},
                "auth": { "name": "Ada" },
                "events": [1, 2]
            })
        );
    }

    #[test]
    fn deferred_and_once_props_are_omitted_until_explicitly_requested() {
        let context =
            request_context_from(&[(X_INERTIA, "true"), (X_INERTIA_EXCEPT_ONCE_PROPS, "plans")]);
        let mut props = json!({
            "analytics": { "views": 10 },
            "plans": ["basic"],
            "user": { "name": "Ada" }
        });
        let metadata = PageMetadata::new().defer("analytics").once("plans");

        context.filter_props("Dashboard", &mut props, &metadata);

        assert_eq!(
            props,
            json!({
                "errors": {},
                "user": { "name": "Ada" }
            })
        );

        let context = request_context_from(&[
            (X_INERTIA, "true"),
            (X_INERTIA_PARTIAL_COMPONENT, "Dashboard"),
            (X_INERTIA_PARTIAL_DATA, "analytics,plans"),
            (X_INERTIA_EXCEPT_ONCE_PROPS, "plans"),
        ]);
        let mut props = json!({
            "analytics": { "views": 10 },
            "plans": ["basic"],
            "user": { "name": "Ada" }
        });

        context.filter_props("Dashboard", &mut props, &metadata);

        assert_eq!(
            props,
            json!({
                "analytics": { "views": 10 },
                "errors": {},
                "plans": ["basic"]
            })
        );
    }

    #[test]
    fn request_reset_filters_merge_and_scroll_metadata() {
        let request = request_context_from(&[
            (X_INERTIA, "true"),
            (X_INERTIA_PARTIAL_COMPONENT, "Feed"),
            (X_INERTIA_PARTIAL_DATA, "posts"),
            (X_INERTIA_RESET, "posts"),
        ]);
        let response = Inertia::page("Feed")
            .scroll("posts", ScrollProps::new("page", 1).next_page(2))
            .match_on("posts.data.id")
            .props(json!({ "posts": { "data": [1, 2] } }))
            .into_page("/feed", Some("version-1".into()), &request)
            .unwrap();
        let value = serde_json::to_value(response).unwrap();

        assert_eq!(value["props"]["posts"]["data"], json!([1, 2]));
        assert!(value.get("mergeProps").is_none());
        assert!(value.get("matchPropsOn").is_none());
        assert!(value.get("scrollProps").is_none());
    }

    #[test]
    fn reset_and_scroll_intent_are_ignored_when_partial_component_differs() {
        let request = request_context_from(&[
            (X_INERTIA, "true"),
            (X_INERTIA_PARTIAL_COMPONENT, "Other"),
            (X_INERTIA_PARTIAL_DATA, "posts"),
            (X_INERTIA_RESET, "posts"),
            (X_INERTIA_INFINITE_SCROLL_MERGE_INTENT, "prepend"),
        ]);
        let response = Inertia::page("Feed")
            .scroll("posts", ScrollProps::new("page", 1).next_page(2))
            .props(json!({ "posts": { "data": [1, 2] } }))
            .into_page("/feed", Some("version-1".into()), &request)
            .unwrap();
        let value = serde_json::to_value(response).unwrap();

        assert_eq!(value["props"]["posts"]["data"], json!([1, 2]));
        assert_eq!(value["mergeProps"], json!(["posts.data"]));
        assert_eq!(value["scrollProps"]["posts"]["nextPage"], 2);
        assert!(value.get("prependProps").is_none());
    }

    #[test]
    fn infinite_scroll_merge_intent_can_prepend_scroll_props() {
        let request = request_context_from(&[
            (X_INERTIA, "true"),
            (X_INERTIA_PARTIAL_COMPONENT, "Feed"),
            (X_INERTIA_PARTIAL_DATA, "posts"),
            (X_INERTIA_INFINITE_SCROLL_MERGE_INTENT, "prepend"),
        ]);
        let response = Inertia::page("Feed")
            .scroll("posts", ScrollProps::new("page", 1).next_page(2))
            .props(json!({ "posts": { "data": [1, 2] } }))
            .into_page("/feed", Some("version-1".into()), &request)
            .unwrap();
        let value = serde_json::to_value(response).unwrap();

        assert_eq!(value["prependProps"], json!(["posts.data"]));
        assert!(value.get("mergeProps").is_none());
        assert_eq!(value["scrollProps"]["posts"]["nextPage"], 2);
    }

    #[test]
    fn once_with_custom_key_omits_loaded_prop_until_requested() {
        let request = request_context_from(&[
            (X_INERTIA, "true"),
            (X_INERTIA_EXCEPT_ONCE_PROPS, "billing"),
        ]);
        let response = Inertia::response(
            "Billing",
            json!({
                "current_plan": "basic",
                "plans": ["basic", "pro"]
            }),
        )
        .once_with_key("billing", OnceProp::new("plans").expires_at(123))
        .into_page("/billing", Some("version-1".into()), &request)
        .unwrap();
        let value = serde_json::to_value(response).unwrap();

        assert!(value["props"].get("plans").is_none());
        assert_eq!(
            value["onceProps"]["billing"],
            json!({ "prop": "plans", "expiresAt": 123 })
        );
    }

    #[test]
    fn page_equality_ignores_internal_route_prop_tracking() {
        let request = request_context_from(&[]);
        let response = Inertia::response(
            "Users",
            json!({
                "auth": {
                    "user": {
                        "name": "Ada"
                    }
                }
            }),
        )
        .into_page("/users", Some("version-1".into()), &request)
        .unwrap();
        let manual = Page::from_parts(
            "Users",
            json!({
                "errors": {},
                "auth": {
                    "user": {
                        "name": "Ada"
                    }
                }
            }),
            "/users",
            Some("version-1".into()),
            PageMetadata::new(),
        );

        assert_eq!(response, manual);
    }
}
