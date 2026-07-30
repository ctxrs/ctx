use anyhow::{anyhow, Result};
use ctx_history_core::{HydrationFailureKind, SourceRecordLocator, StableEntityId};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::output::compact_json;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SourceHydrationBatchItem {
    pub(super) event_identity: StableEntityId,
    pub(super) locator: SourceRecordLocator,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum SourceHydrationMode {
    SearchDisplay { max_chars: usize },
    Complete,
}

pub(super) fn source_hydration_mode(request: &Value) -> Result<SourceHydrationMode> {
    match request.get("mode").and_then(Value::as_str) {
        Some("search_display") => {
            let max_chars = request
                .get("max_chars")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
                .filter(|value| (1..=2_048).contains(value))
                .ok_or_else(|| anyhow!("search display max_chars must be between 1 and 2048"))?;
            Ok(SourceHydrationMode::SearchDisplay { max_chars })
        }
        Some("complete") if request.get("max_chars").is_none_or(Value::is_null) => {
            Ok(SourceHydrationMode::Complete)
        }
        Some(mode) => Err(anyhow!("invalid source hydration mode `{mode}`")),
        None => Err(anyhow!("source hydration mode is missing")),
    }
}

pub(super) fn valid_source_generation_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(super) fn source_hydration_protocol_failure(
    code: &str,
    failure_kind: &str,
    detail: &str,
    refresh_scheduled: bool,
) -> Value {
    compact_json(json!({
        "ok": false,
        "schema_version": 1,
        "code": code,
        "failure_kind": failure_kind,
        "detail": detail,
        "refresh_scheduled": refresh_scheduled,
    }))
}

pub(super) fn hydration_failure_kind_name(kind: HydrationFailureKind) -> &'static str {
    kind.as_str()
}
