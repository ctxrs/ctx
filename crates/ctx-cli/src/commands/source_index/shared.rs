use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use ctx_history_index::{CoreEventRecord, SessionRecord, VerifiedIndex};
use uuid::Uuid;

use crate::transcript::normalize_uuid_prefix;

const SEARCH_DIRECTORY: &str = "search";
const LEXICAL_DIRECTORY: &str = "lexical";

pub(super) fn resolve_core_event(index: &VerifiedIndex, id: &str) -> Result<CoreEventRecord> {
    if let Ok(uuid) = Uuid::parse_str(id.trim()) {
        return index
            .core_event_by_id(uuid)?
            .ok_or_else(|| anyhow!("event {uuid} was not found in the Core generation"));
    }
    let prefix = validate_ctx_id(id, "event")?;
    match index.events_by_id_prefix(&prefix)?.as_slice() {
        [] => Err(anyhow!(
            "event id prefix {prefix:?} was not found in the Core generation"
        )),
        [event] => index
            .core_event_by_id(event.event_id.as_uuid())?
            .ok_or_else(|| {
                anyhow!(
                    "event {} disappeared from the pinned Core generation",
                    event.event_id
                )
            }),
        matches => Err(anyhow!(
            "event id prefix {prefix:?} is ambiguous; first matches are {} and {}; use a longer ctx_event_id",
            matches[0].event_id,
            matches[1].event_id
        )),
    }
}

pub(super) fn resolve_session(index: &VerifiedIndex, id: &str) -> Result<SessionRecord> {
    if let Ok(uuid) = Uuid::parse_str(id.trim()) {
        return index
            .session_by_id(uuid)?
            .ok_or_else(|| anyhow!("session {uuid} was not found in the Core generation"));
    }
    let prefix = validate_ctx_id(id, "session")?;
    match index.sessions_by_id_prefix(&prefix)?.as_slice() {
        [] => Err(anyhow!(
            "session id prefix {prefix:?} was not found in the Core generation"
        )),
        [session] => Ok(session.clone()),
        matches => Err(anyhow!(
            "session id prefix {prefix:?} is ambiguous; first matches are {} and {}; use a longer ctx_session_id",
            matches[0].session_id,
            matches[1].session_id
        )),
    }
}

pub(super) fn validate_ctx_id(id: &str, kind: &str) -> Result<String> {
    let trimmed = id.trim();
    if Uuid::parse_str(trimmed).is_ok() {
        return Ok(trimmed.to_ascii_lowercase());
    }
    normalize_uuid_prefix(trimmed, kind)
}

pub(super) fn validate_session_selector(
    id: Option<&str>,
    provider_session_id: Option<&str>,
) -> Result<()> {
    match (id, provider_session_id) {
        (Some(id), None) => {
            validate_ctx_id(id, "session")?;
            Ok(())
        }
        (None, Some(provider_session_id)) if provider_session_id.trim().is_empty() => {
            Err(anyhow!("provider session ID must not be empty"))
        }
        (None, Some(_)) => Ok(()),
        (Some(_), Some(_)) => Err(anyhow!(
            "pass either a ctx session ID or --provider-session, not both"
        )),
        (None, None) => Err(anyhow!(
            "Core session lookup requires a ctx session ID or --provider-session"
        )),
    }
}

pub(super) fn open_index(data_root: &Path) -> Result<VerifiedIndex> {
    let root = index_root(data_root);
    if !root.join("meta.json").is_file() {
        return Err(anyhow!(
            "Core index is not initialized at {}",
            root.display()
        ));
    }
    VerifiedIndex::open_pinned(&root)
        .with_context(|| format!("open verified Core index {}", root.display()))
}

pub(super) fn index_root(data_root: &Path) -> PathBuf {
    data_root.join(SEARCH_DIRECTORY).join(LEXICAL_DIRECTORY)
}
