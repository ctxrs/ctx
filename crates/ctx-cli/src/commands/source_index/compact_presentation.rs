use std::{collections::BTreeSet, path::Path};

use anyhow::Result;
use ctx_history_index::{IndexError, VerifiedIndex};
use ctx_history_refresh::verify_generation_query_authority;
use serde_json::Value;
use uuid::Uuid;

use crate::transcript::normalize_uuid_prefix;

use super::compact_ref::{CompactRefMap, CompactRefResolver, MAX_COMPACT_REF_HEX_LEN};

const EVENT_ID_FIELDS: &[&str] = &[
    "ctx_event_id",
    "event_id",
    "ancestor_event_id",
    "copied_from_ctx_event_id",
];
const SESSION_ID_FIELDS: &[&str] = &[
    "ctx_session_id",
    "session_id",
    "parent_ctx_session_id",
    "root_ctx_session_id",
    "ancestor_session_id",
    "copied_from_ctx_session_id",
];

/// Owns the two-generation namespace used by one human-facing render pass.
///
/// The structured read model remains untouched. Callers project a clone only
/// for ANSI/plain/Markdown or MCP text after the machine result is complete.
pub(super) struct CompactPresentation<'index> {
    current: &'index VerifiedIndex,
    retained_peer: Option<VerifiedIndex>,
}

impl<'index> CompactPresentation<'index> {
    pub(super) fn open_if_needed(
        current: &'index VerifiedIndex,
        index_root: &Path,
        needed: bool,
    ) -> Result<Option<Self>> {
        needed.then(|| Self::open(current, index_root)).transpose()
    }

    pub(super) fn open(current: &'index VerifiedIndex, index_root: &Path) -> Result<Self> {
        let retained_peer =
            VerifiedIndex::open_retained_generation_peer(index_root, current.generation_id())
                .map_err(|error| match error {
                    IndexError::PinnedGenerationNotRetained { .. } => {
                        IndexError::ConcurrentGenerationChange
                    }
                    error => error,
                })?;
        if let Some(peer) = retained_peer.as_ref() {
            verify_generation_query_authority(peer).map_err(anyhow::Error::new)?;
        }
        Ok(Self {
            current,
            retained_peer,
        })
    }

    pub(super) fn resolver(&self) -> CompactRefResolver<'_> {
        CompactRefResolver::new(self.current, self.retained_peer.as_ref())
    }

    pub(super) fn project(&self, value: &Value) -> Result<Value> {
        let mut event_ids = BTreeSet::new();
        let mut session_ids = BTreeSet::new();
        collect_rendered_ids(value, None, &mut event_ids, &mut session_ids);
        let references = self
            .resolver()
            .compact_refs(event_ids.iter().copied(), session_ids.iter().copied())?;
        let mut projected = value.clone();
        project_rendered_ids(&mut projected, None, &references, &event_ids, &session_ids)?;
        Ok(projected)
    }
}

pub(super) fn reference_needs_retained_peer(reference: &str) -> bool {
    let reference = reference.trim();
    Uuid::parse_str(reference).is_err()
        && normalize_uuid_prefix(reference, "id prefix")
            .is_ok_and(|prefix| prefix.len() <= MAX_COMPACT_REF_HEX_LEN)
}

fn collect_rendered_ids(
    value: &Value,
    field: Option<&str>,
    event_ids: &mut BTreeSet<Uuid>,
    session_ids: &mut BTreeSet<Uuid>,
) {
    if let Some(id) = value.as_str().and_then(|value| Uuid::parse_str(value).ok()) {
        if field.is_some_and(|field| EVENT_ID_FIELDS.contains(&field)) {
            event_ids.insert(id);
        } else if field.is_some_and(|field| SESSION_ID_FIELDS.contains(&field)) {
            session_ids.insert(id);
        }
    }
    match value {
        Value::Array(values) => {
            for value in values {
                collect_rendered_ids(value, field, event_ids, session_ids);
            }
        }
        Value::Object(object) => {
            for (field, value) in object {
                collect_rendered_ids(value, Some(field), event_ids, session_ids);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn project_rendered_ids(
    value: &mut Value,
    field: Option<&str>,
    references: &CompactRefMap,
    event_ids: &BTreeSet<Uuid>,
    session_ids: &BTreeSet<Uuid>,
) -> Result<()> {
    if let Value::String(text) = value {
        if let Ok(id) = Uuid::parse_str(text) {
            let replacement = if field.is_some_and(|field| EVENT_ID_FIELDS.contains(&field)) {
                Some(references.event(id)?)
            } else if field.is_some_and(|field| SESSION_ID_FIELDS.contains(&field)) {
                Some(references.session(id)?)
            } else {
                None
            };
            if let Some(replacement) = replacement {
                *text = replacement.to_owned();
                return Ok(());
            }
        }
        if field == Some("suggested_next_commands") {
            for id in event_ids {
                *text = text.replace(&id.to_string(), references.event(*id)?);
            }
            for id in session_ids {
                *text = text.replace(&id.to_string(), references.session(*id)?);
            }
        }
    }
    match value {
        Value::Array(values) => {
            for value in values {
                project_rendered_ids(value, field, references, event_ids, session_ids)?;
            }
        }
        Value::Object(object) => {
            for (field, value) in object {
                project_rendered_ids(value, Some(field), references, event_ids, session_ids)?;
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
    Ok(())
}
