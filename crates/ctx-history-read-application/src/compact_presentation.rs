use std::collections::BTreeSet;

use anyhow::Result;
use ctx_history_index_query::VerifiedIndex;
use serde_json::Value;
use uuid::Uuid;

use crate::selector::{
    CompactRefMap, CompactRefNamespace, CompactRefResolver, PreparedCompactRefCurrent,
    PreparedCompactRefResolver,
};
use crate::MAX_COMPACT_REF_HEX_LEN;

#[derive(Clone, Copy)]
enum IdKind {
    Event,
    Session,
}

fn id_field(field: &str) -> Option<(IdKind, bool)> {
    Some(match field {
        "ctx_event_id" | "event_id" => (IdKind::Event, false),
        "ancestor_event_id" | "copied_from_ctx_event_id" => (IdKind::Event, true),
        "ctx_session_id" | "session_id" => (IdKind::Session, false),
        "parent_ctx_session_id"
        | "root_ctx_session_id"
        | "claimed_root_ctx_session_id"
        | "ancestor_session_id"
        | "copied_from_ctx_session_id" => (IdKind::Session, true),
        _ => return None,
    })
}

#[derive(Default)]
struct IdSet {
    rendered: BTreeSet<Uuid>,
    optional: BTreeSet<Uuid>,
    preserved: BTreeSet<Uuid>,
}

#[derive(Default)]
struct RenderedIds {
    events: IdSet,
    sessions: IdSet,
}

pub struct CompactPresentationProjection<'index> {
    resolver: CompactRefResolver<'index>,
}

impl<'index> CompactPresentationProjection<'index> {
    pub const fn new(
        current: &'index VerifiedIndex,
        retained_peer: Option<&'index VerifiedIndex>,
    ) -> Self {
        Self {
            resolver: CompactRefResolver::new(current, retained_peer),
        }
    }

    pub const fn resolver(&self) -> &CompactRefResolver<'index> {
        &self.resolver
    }

    pub fn project(&self, value: &Value) -> Result<Value> {
        project_with_resolver(
            value,
            |namespace, id| self.resolver.contains_exact(namespace, id),
            |ids| {
                self.resolver.compact_refs(
                    ids.events.rendered.iter().copied(),
                    ids.sessions.rendered.iter().copied(),
                )
            },
        )
    }
}

/// Collect the bounded active-generation probes needed by a deferred compact
/// projection while its searcher is still available.
pub(crate) fn prepare_deferred_current(
    current: &VerifiedIndex,
    value: &Value,
) -> Result<PreparedCompactRefCurrent> {
    let mut rendered_ids = RenderedIds::default();
    collect_rendered_ids(value, None, &mut rendered_ids);
    PreparedCompactRefCurrent::from_ids(
        current,
        rendered_ids
            .events
            .rendered
            .iter()
            .chain(rendered_ids.events.optional.iter())
            .copied(),
        rendered_ids
            .sessions
            .rendered
            .iter()
            .chain(rendered_ids.sessions.optional.iter())
            .copied(),
    )
}

/// Projects compact IDs after the active searcher has been released and the
/// retained peer, if any, has been opened.
pub(crate) fn project_deferred(
    value: &Value,
    current: PreparedCompactRefCurrent,
    retained_peer: Option<&VerifiedIndex>,
) -> Result<Value> {
    let resolver = PreparedCompactRefResolver::new(current, retained_peer);
    project_with_resolver(
        value,
        |namespace, id| resolver.contains_exact(namespace, id),
        |ids| {
            resolver.compact_refs(
                ids.events.rendered.iter().copied(),
                ids.sessions.rendered.iter().copied(),
            )
        },
    )
}

fn project_with_resolver(
    value: &Value,
    contains_exact: impl Fn(CompactRefNamespace, Uuid) -> Result<bool>,
    compact_refs: impl Fn(&RenderedIds) -> Result<CompactRefMap>,
) -> Result<Value> {
    let mut rendered_ids = RenderedIds::default();
    collect_rendered_ids(value, None, &mut rendered_ids);
    for (ids, namespace) in [
        (&mut rendered_ids.events, CompactRefNamespace::Event),
        (&mut rendered_ids.sessions, CompactRefNamespace::Session),
    ] {
        for id in &ids.preserved {
            ids.rendered.remove(id);
        }
        for id in &ids.optional {
            if !contains_exact(namespace, *id)? {
                ids.rendered.remove(id);
                ids.preserved.insert(*id);
            }
        }
    }
    let references = compact_refs(&rendered_ids)?;
    let mut projected = value.clone();
    project_rendered_ids(&mut projected, None, &references, &rendered_ids)?;
    Ok(projected)
}

pub fn reference_needs_retained_peer(reference: &str) -> bool {
    let reference = reference.trim();
    Uuid::parse_str(reference).is_err()
        && normalize_uuid_prefix(reference)
            .is_ok_and(|prefix| prefix.len() <= MAX_COMPACT_REF_HEX_LEN)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UuidPrefixError {
    #[error("prefix is shorter than the minimum identity length")]
    TooShort,
    #[error("prefix is not unhyphenated hexadecimal")]
    InvalidHex,
}

pub fn normalize_uuid_prefix(value: &str) -> std::result::Result<String, UuidPrefixError> {
    let prefix = value.trim();
    if prefix.len() < 8 {
        return Err(UuidPrefixError::TooShort);
    }
    if prefix.contains('-') || !prefix.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(UuidPrefixError::InvalidHex);
    }
    Ok(prefix.to_ascii_lowercase())
}

fn collect_rendered_ids(value: &Value, field: Option<&str>, rendered_ids: &mut RenderedIds) {
    if let Some((id, (kind, optional))) = value
        .as_str()
        .and_then(|value| Uuid::parse_str(value).ok())
        .zip(field.and_then(id_field))
    {
        let ids = match kind {
            IdKind::Event => &mut rendered_ids.events,
            IdKind::Session => &mut rendered_ids.sessions,
        };
        ids.rendered.insert(id);
        if optional {
            ids.optional.insert(id);
        }
    }
    match value {
        Value::Array(values) => {
            for value in values {
                collect_rendered_ids(value, field, rendered_ids);
            }
        }
        Value::Object(object) => {
            if field == Some("resolution")
                && object.get("state").and_then(Value::as_str) == Some("unresolved")
            {
                for (field, ids) in [
                    ("ctx_event_id", &mut rendered_ids.events),
                    ("ctx_session_id", &mut rendered_ids.sessions),
                ] {
                    if let Some(id) = uuid_member(object, field) {
                        ids.preserved.insert(id);
                    }
                }
            }
            for (field, value) in object {
                collect_rendered_ids(value, Some(field), rendered_ids);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn uuid_member(object: &serde_json::Map<String, Value>, field: &str) -> Option<Uuid> {
    object
        .get(field)
        .and_then(Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok())
}

fn project_rendered_ids(
    value: &mut Value,
    field: Option<&str>,
    references: &CompactRefMap,
    ids: &RenderedIds,
) -> Result<()> {
    if let Value::String(text) = value {
        if let Some((id, (kind, _))) = Uuid::parse_str(&*text).ok().zip(field.and_then(id_field)) {
            let id_set = match kind {
                IdKind::Event => &ids.events,
                IdKind::Session => &ids.sessions,
            };
            if !id_set.preserved.contains(&id) {
                *text = compact_reference(references, kind, id)?.to_owned();
            }
            return Ok(());
        }
        if field == Some("suggested_next_commands") {
            for (kind, id_set) in [
                (IdKind::Event, &ids.events),
                (IdKind::Session, &ids.sessions),
            ] {
                for id in &id_set.rendered {
                    *text =
                        text.replace(&id.to_string(), compact_reference(references, kind, *id)?);
                }
            }
        }
    }
    match value {
        Value::Array(values) => {
            for value in values {
                project_rendered_ids(value, field, references, ids)?;
            }
        }
        Value::Object(object) => {
            for (field, value) in object {
                project_rendered_ids(value, Some(field), references, ids)?;
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
    Ok(())
}

fn compact_reference(references: &CompactRefMap, kind: IdKind, id: Uuid) -> Result<&str> {
    match kind {
        IdKind::Event => references.event(id),
        IdKind::Session => references.session(id),
    }
}
