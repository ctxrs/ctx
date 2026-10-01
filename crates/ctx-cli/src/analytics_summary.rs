//! Bounded, best-effort product windows. These are counters, not a second
//! delivery queue: taking a window retires it even when outbox admission fails.
//! Only the existing outbox owns immutable events, acknowledgements and retries.
use ctx_client_observability::analytics::{PublicEventV1, SiftSummaryV1};
pub(crate) use ctx_client_observability::analytics::{Summary, Window};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedWindow {
    schema_version: u16,
    producer_version: String,
    owner: String,
    endpoint_fingerprint: String,
    window: Window,
}
impl SavedWindow {
    fn new(owner: &str, endpoint: &str, now: i64) -> Self {
        Self {
            schema_version: 1,
            producer_version: env!("CARGO_PKG_VERSION").into(),
            owner: owner.into(),
            endpoint_fingerprint: crate::analytics_outbox::endpoint_fingerprint(endpoint),
            window: Window::new(now),
        }
    }
    fn matches(&self, owner: &str, endpoint: &str) -> bool {
        self.schema_version == 1
            && self.producer_version == env!("CARGO_PKG_VERSION")
            && self.owner == owner
            && self.endpoint_fingerprint == crate::analytics_outbox::endpoint_fingerprint(endpoint)
            && self.window.is_bounded()
    }
}
fn state_path(root: &Path, owner: &str) -> anyhow::Result<PathBuf> {
    // owner is the validated, random consent-root UUID, never a source path.
    crate::identity::device_state_path(&format!("analytics-summary-{owner}.json"), root)
}
fn reset_existing(root: &Path, owner: &str) {
    let Ok(path) = state_path(root, owner) else {
        return;
    };
    if !path.exists() {
        return;
    }
    if let Ok(Some(mut state)) = crate::analytics_state::StateFile::try_open(&path) {
        let _ = state.clear();
    }
}

#[cfg(test)]
pub(crate) fn record_sift(root: &Path, observation: SiftSummaryV1) {
    let Some(endpoint) = crate::observability_composition::optional_analytics_endpoint(root) else {
        if let Ok(Some(owner)) = crate::identity::try_existing_installation_id(root) {
            reset_existing(root, &owner);
        }
        return;
    };
    let Ok(Some(owner)) = crate::identity::try_installation_id(root) else {
        return;
    };
    record_sift_for_owner(root, &owner, &endpoint, observation);
}

pub(crate) fn record_sift_for_owner(
    root: &Path,
    owner: &str,
    endpoint: &str,
    observation: SiftSummaryV1,
) {
    let current_endpoint = crate::observability_composition::optional_analytics_endpoint(root);
    if current_endpoint.is_none() {
        reset_existing(root, owner);
        return;
    }
    if current_endpoint.as_deref() != Some(endpoint)
        || crate::identity::try_existing_installation_id(root)
            .ok()
            .flatten()
            .as_deref()
            != Some(owner)
    {
        return;
    }
    let Ok(path) = state_path(root, owner) else {
        return;
    };
    let Ok(Some(mut file)) = crate::analytics_state::StateFile::try_open(&path) else {
        return;
    };
    let now = ctx_history_core::utc_now().timestamp();
    let mut state = match file.read::<SavedWindow>() {
        Ok(Some(state)) if state.matches(owner, endpoint) => state,
        Ok(_) => SavedWindow::new(owner, endpoint, now),
        Err(error) if error.is::<serde_json::Error>() => {
            let mut state = SavedWindow::new(owner, endpoint, now);
            state.window.mark_limited();
            state
        }
        Err(_) => return,
    };
    state.window.record(Summary::Sift(observation), now);
    if crate::observability_composition::optional_analytics_endpoint(root).as_deref()
        != Some(endpoint)
        || crate::identity::try_existing_installation_id(root)
            .ok()
            .flatten()
            .as_deref()
            != Some(owner)
    {
        let _ = file.clear();
        return;
    }
    let _ = file.write(&state);
}

/// Materialize only the captured owner's window. Version/endpoint changes drop
/// old counters rather than claiming that a newer producer performed that work.
pub(crate) fn take_saved(root: &Path, owner: &str) -> Vec<PublicEventV1> {
    let Some(endpoint) = crate::observability_composition::optional_analytics_endpoint(root) else {
        reset_existing(root, owner);
        return Vec::new();
    };
    if crate::identity::try_existing_installation_id(root)
        .ok()
        .flatten()
        .as_deref()
        != Some(owner)
    {
        return Vec::new();
    }
    let Ok(path) = state_path(root, owner) else {
        return Vec::new();
    };
    if !path.exists() {
        return Vec::new();
    }
    let Ok(Some(mut file)) = crate::analytics_state::StateFile::try_open(&path) else {
        return Vec::new();
    };
    let state = file.read::<SavedWindow>().ok().flatten();
    // Retire under the same lock before queue admission; never replay a counter
    // window under a new event ID following a lost acknowledgement.
    if file.clear().is_err() {
        return Vec::new();
    }
    drop(file);
    let Some(mut state) = state.filter(|state| state.matches(owner, &endpoint)) else {
        return Vec::new();
    };
    let events = state.window.take(ctx_history_core::utc_now().timestamp());
    if crate::observability_composition::optional_analytics_endpoint(root).as_deref()
        == Some(&endpoint)
        && crate::identity::try_existing_installation_id(root)
            .ok()
            .flatten()
            .as_deref()
            == Some(owner)
    {
        return events;
    }
    Vec::new()
}

pub(crate) fn flush(root: &Path, owner: &str) {
    let Some(endpoint) = crate::observability_composition::optional_analytics_endpoint(root) else {
        reset_existing(root, owner);
        return;
    };
    let events = take_saved(root, owner);
    let _ = crate::observability_composition::append_analytics_summary_for_owner(
        root, owner, &endpoint, &events,
    );
}
