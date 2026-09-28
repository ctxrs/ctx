use std::path::Path;

use serde_json::{json, Value};

use super::{read_json_file, state_path, STATE_SCHEMA_VERSION};

pub fn read_state_json() -> Option<Value> {
    let install_path = super::super::install::current_install_path().ok()?;
    read_state_json_for_path(&install_path)
}

pub(super) fn read_state_json_for_path(install_path: &Path) -> Option<Value> {
    let state = read_json_file(&state_path(install_path))?;
    // A fresh hosted install supersedes failures from the removed executable.
    // Preserve the scheduler/recovery receipt on disk; a status read neither
    // rewrites upgrade state nor claims that the new install checked a feed.
    if state["status"] == "error" {
        let finished_at = state
            .get("last_attempt_finished_at")
            .or_else(|| state.get("last_attempt_at"))
            .or_else(|| state.get("checked_at"))
            .and_then(Value::as_str)
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok());
        let superseded =
            super::super::install::managed_install_receipt(install_path).is_some_and(|install| {
                // An attempt can publish a new marker and then fail during
                // finalization. Its old binding alone does not make it stale.
                finished_at.is_some_and(|finished| finished < install.installed_at)
                    && state
                        .get("install_attempt_id")
                        .and_then(Value::as_str)
                        .is_none_or(|id| id != install.install_attempt_id)
            });
        if superseded {
            return Some(json!({
                "schema_version": STATE_SCHEMA_VERSION,
                "status": "never_checked",
            }));
        }
    }
    Some(state)
}
