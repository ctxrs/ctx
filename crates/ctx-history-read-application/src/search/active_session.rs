use std::collections::BTreeSet;

use anyhow::{anyhow, Result};
use ctx_history_index_query::{ExcludedSessionTree, SessionRecord, VerifiedIndex};
use uuid::Uuid;

use super::{ActiveSessionExclusion, SearchRequest};
use crate::{resolve_session_with_refs, CompactRefResolver};

pub(super) const MAX_ACTIVE_SESSION_ANCESTORS: usize = 64;
pub(super) const MAX_ACTIVE_SESSION_TREE_SESSIONS: usize = 4_096;

pub(super) fn normalize_manual_session_exclusions(request: &mut SearchRequest) -> Result<()> {
    request.exclude_sessions = request
        .exclude_sessions
        .iter()
        .map(|selector| {
            let selector = selector.trim();
            if selector.is_empty() {
                return Err(anyhow!("exclude_session selector is empty"));
            }
            Ok(selector.to_owned())
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(())
}

pub(super) fn validate_manual_session_exclusions(request: &SearchRequest) -> Result<()> {
    if request
        .exclude_sessions
        .iter()
        .any(|selector| selector.trim().is_empty())
    {
        return Err(anyhow!("exclude_session selector is empty"));
    }
    if request.session.is_some() && !request.exclude_sessions.is_empty() {
        return Err(anyhow!(
            "excluded sessions cannot be combined with a selected session"
        ));
    }
    Ok(())
}

pub(super) fn resolved_manual_session_exclusion_ids(
    request: &SearchRequest,
    references: &CompactRefResolver<'_>,
) -> Result<Vec<Uuid>> {
    let mut session_ids = Vec::with_capacity(request.exclude_sessions.len());
    let mut seen = BTreeSet::new();
    for selector in &request.exclude_sessions {
        let session_id = resolve_session_with_refs(references, selector)?
            .session_id
            .as_uuid();
        if seen.insert(session_id) {
            session_ids.push(session_id);
        }
    }
    Ok(session_ids)
}

pub(super) fn excluded_active_session_tree(
    index: &VerifiedIndex,
    active_session: &ActiveSessionExclusion,
) -> Result<ExcludedSessionTree> {
    let sessions = index.sessions_by_provider_session_id(
        &active_session.provider_session_id,
        Some(&active_session.provider),
        None,
        None,
    )?;
    let ancestries = sessions
        .iter()
        .map(SessionAncestry::from)
        .collect::<Vec<_>>();
    let root_session_id = resolved_unique_session_tree_root_id(&ancestries, |session_id| {
        Ok(index
            .session_by_id(session_id)?
            .as_ref()
            .map(SessionAncestry::from))
    })?;
    let session_ids = match root_session_id {
        Some(root_session_id) => resolved_session_tree_ids(root_session_id, |session_ids| {
            Ok(index.session_ids_claiming_lineage_to_any(
                session_ids,
                MAX_ACTIVE_SESSION_TREE_SESSIONS + 1,
            )?)
        })?
        .unwrap_or_default(),
        None => Vec::new(),
    };
    Ok(ExcludedSessionTree {
        provider: active_session.provider.clone(),
        provider_session_id: active_session.provider_session_id.clone(),
        session_ids,
    })
}

pub(super) fn resolved_session_tree_ids<F>(
    root_session_id: Uuid,
    mut related_session_ids: F,
) -> Result<Option<Vec<Uuid>>>
where
    F: FnMut(&[Uuid]) -> Result<Vec<Uuid>>,
{
    let mut session_ids = BTreeSet::from([root_session_id]);
    for _ in 0..=MAX_ACTIVE_SESSION_ANCESTORS {
        let anchors = session_ids.iter().copied().collect::<Vec<_>>();
        let related = related_session_ids(&anchors)?;
        if related.len() > MAX_ACTIVE_SESSION_TREE_SESSIONS {
            return Ok(None);
        }
        let previous_len = session_ids.len();
        session_ids.extend(related);
        if session_ids.len() > MAX_ACTIVE_SESSION_TREE_SESSIONS {
            return Ok(None);
        }
        if session_ids.len() == previous_len {
            return Ok(Some(session_ids.into_iter().collect()));
        }
    }
    Ok(None)
}

#[derive(Debug, Clone, Copy)]
pub(super) struct SessionAncestry {
    pub(super) session_id: Uuid,
    pub(super) parent_session_id: Option<Uuid>,
    pub(super) claimed_root_session_id: Option<Uuid>,
}

impl From<&SessionRecord> for SessionAncestry {
    fn from(session: &SessionRecord) -> Self {
        Self {
            session_id: session.session_id.as_uuid(),
            parent_session_id: session.parent_session_id.map(|id| id.as_uuid()),
            claimed_root_session_id: session.root_session_id.map(|id| id.as_uuid()),
        }
    }
}

pub(super) fn resolved_unique_session_tree_root_id<F>(
    sessions: &[SessionAncestry],
    session_by_id: F,
) -> Result<Option<Uuid>>
where
    F: FnMut(Uuid) -> Result<Option<SessionAncestry>>,
{
    let [session] = sessions else {
        return Ok(None);
    };
    resolved_session_tree_root_id(*session, session_by_id)
}

fn resolved_session_tree_root_id<F>(
    session: SessionAncestry,
    mut session_by_id: F,
) -> Result<Option<Uuid>>
where
    F: FnMut(Uuid) -> Result<Option<SessionAncestry>>,
{
    // Prove the complete parent chain against the pinned generation. Codex
    // may put an immediate parent in root_session_id for deeper descendants,
    // so a stored root is accepted only when it names a proven ancestor.
    let mut current = session;
    let mut visited = BTreeSet::new();
    let mut ancestry = Vec::with_capacity(MAX_ACTIVE_SESSION_ANCESTORS + 1);
    let root_id = loop {
        if !visited.insert(current.session_id) {
            return Ok(None);
        }
        ancestry.push(current);
        let Some(parent_id) = current.parent_session_id else {
            break current.session_id;
        };
        if ancestry.len() > MAX_ACTIVE_SESSION_ANCESTORS {
            return Ok(None);
        }
        let Some(parent) = session_by_id(parent_id)? else {
            return Ok(None);
        };
        current = parent;
    };

    for (position, session) in ancestry.iter().enumerate() {
        let Some(claimed_root_id) = session.claimed_root_session_id else {
            continue;
        };
        let claim_is_proven = if position + 1 == ancestry.len() {
            claimed_root_id == session.session_id
        } else {
            ancestry[position + 1..]
                .iter()
                .any(|ancestor| ancestor.session_id == claimed_root_id)
        };
        if !claim_is_proven {
            return Ok(None);
        }
    }

    Ok(Some(root_id))
}
