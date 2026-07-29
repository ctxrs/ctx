use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use ctx_history_index::{EventRecord, SessionRecord, VerifiedIndex};
use uuid::Uuid;

use crate::transcript::normalize_uuid_prefix;

const SEARCH_DIRECTORY: &str = "search";
const LEXICAL_DIRECTORY: &str = "lexical";

pub(super) fn resolve_event(index: &VerifiedIndex, id: &str) -> Result<EventRecord> {
    if let Ok(uuid) = Uuid::parse_str(id.trim()) {
        return index.event_by_id(uuid)?.ok_or_else(|| {
            anyhow!("event {uuid} was not found in the source-backed Core generation")
        });
    }
    let prefix = normalize_uuid_prefix(id, "event")?;
    match index.events_by_id_prefix(&prefix)?.as_slice() {
        [] => Err(anyhow!(
            "event id prefix {prefix:?} was not found in the source-backed Core generation"
        )),
        [event] => Ok(event.clone()),
        matches => Err(anyhow!(
            "event id prefix {prefix:?} is ambiguous; first matches are {} and {}; use a longer ctx_event_id",
            matches[0].event_id,
            matches[1].event_id
        )),
    }
}

pub(super) fn resolve_session(index: &VerifiedIndex, id: &str) -> Result<SessionRecord> {
    if let Ok(uuid) = Uuid::parse_str(id.trim()) {
        return index.session_by_id(uuid)?.ok_or_else(|| {
            anyhow!("session {uuid} was not found in the source-backed Core generation")
        });
    }
    let prefix = normalize_uuid_prefix(id, "session")?;
    match index.sessions_by_id_prefix(&prefix)?.as_slice() {
        [] => Err(anyhow!(
            "session id prefix {prefix:?} was not found in the source-backed Core generation"
        )),
        [session] => Ok(session.clone()),
        matches => Err(anyhow!(
            "session id prefix {prefix:?} is ambiguous; first matches are {} and {}; use a longer ctx_session_id",
            matches[0].session_id,
            matches[1].session_id
        )),
    }
}

pub(super) fn open_index(data_root: &Path) -> Result<VerifiedIndex> {
    let root = index_root(data_root);
    if !root.join("meta.json").is_file() {
        return Err(anyhow!(
            "source-backed Core index is not initialized at {}",
            root.display()
        ));
    }
    VerifiedIndex::open(&root)
        .with_context(|| format!("open verified source-backed Core index {}", root.display()))
}

pub(super) fn index_root(data_root: &Path) -> PathBuf {
    data_root.join(SEARCH_DIRECTORY).join(LEXICAL_DIRECTORY)
}
