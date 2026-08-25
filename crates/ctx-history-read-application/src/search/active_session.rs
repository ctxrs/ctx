use std::collections::{BTreeMap, BTreeSet};

use anyhow::{anyhow, Result};
use ctx_history_core::StableEntityId;
use ctx_history_index_query::{ExcludedSessionTree, SessionGroupingClaims, VerifiedIndex};
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
) -> Option<ExcludedSessionTree> {
    // Automatic exclusion is a safety exception: any lookup or proof failure
    // abstains instead of widening to provider-native metadata.
    let sessions = index
        .sessions_by_provider_session_id(
            &active_session.provider_session_id,
            Some(&active_session.provider),
            None,
            None,
        )
        .ok()?;
    let [active_session] = sessions.as_slice() else {
        return None;
    };
    let active_claims = index
        .session_grouping_claims_by_id(active_session.session_id.as_uuid())
        .ok()
        .flatten()?;
    if active_claims.session_id != active_session.session_id {
        return None;
    }
    let ancestries = [SessionAncestry::from(&active_claims)];
    let session_ids = proven_active_session_tree_ids(
        &ancestries,
        |session_id| {
            Ok(index
                .session_grouping_claims_by_id(session_id)?
                .as_ref()
                .map(SessionAncestry::from))
        },
        |session_ids| {
            Ok(index
                .session_grouping_claims_claiming_lineage_to_any(
                    session_ids,
                    MAX_ACTIVE_SESSION_TREE_SESSIONS + 1,
                )?
                .iter()
                .map(SessionAncestry::from)
                .collect())
        },
    )?;
    Some(ExcludedSessionTree { session_ids })
}

pub(super) fn proven_active_session_tree_ids<F, G>(
    sessions: &[SessionAncestry],
    session_by_id: F,
    related_session_ids: G,
) -> Option<Vec<Uuid>>
where
    F: FnMut(Uuid) -> Result<Option<SessionAncestry>>,
    G: FnMut(&[Uuid]) -> Result<Vec<SessionAncestry>>,
{
    let [active_session] = sessions else {
        return None;
    };
    let root_session_id = resolved_unique_session_tree_root_id(sessions, session_by_id)
        .ok()
        .flatten()?;
    resolved_session_tree_ids(
        root_session_id,
        active_session.source_owner,
        related_session_ids,
    )
    .ok()
    .flatten()
}

pub(super) fn resolved_session_tree_ids<F>(
    root_session_id: Uuid,
    source_owner: StableEntityId,
    mut related_session_ids: F,
) -> Result<Option<Vec<Uuid>>>
where
    F: FnMut(&[Uuid]) -> Result<Vec<SessionAncestry>>,
{
    let mut session_ids = BTreeSet::from([root_session_id]);
    for _ in 0..=MAX_ACTIVE_SESSION_ANCESTORS {
        let anchors = session_ids.iter().copied().collect::<Vec<_>>();
        let related = related_session_ids(&anchors)?;
        if related.len() > MAX_ACTIVE_SESSION_TREE_SESSIONS {
            return Ok(None);
        }
        let mut discovered = BTreeMap::new();
        for candidate in related {
            if session_ids.contains(&candidate.session_id) {
                continue;
            }
            if candidate.source_owner != source_owner
                || discovered.insert(candidate.session_id, candidate).is_some()
            {
                return Ok(None);
            }
            let claims = [
                candidate.parent_session_id,
                candidate.claimed_root_session_id,
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
            if claims.is_empty() {
                return Ok(None);
            }
        }
        if discovered.is_empty() {
            return Ok(Some(session_ids.into_iter().collect()));
        }
        if session_ids.len() + discovered.len() > MAX_ACTIVE_SESSION_TREE_SESSIONS
            || discovered.values().any(|candidate| {
                [
                    candidate.parent_session_id,
                    candidate.claimed_root_session_id,
                ]
                .into_iter()
                .flatten()
                .any(|claim| !session_ids.contains(&claim) && !discovered.contains_key(&claim))
            })
        {
            return Ok(None);
        }
        while !discovered.is_empty() {
            let ready = discovered
                .values()
                .filter(|candidate| {
                    [
                        candidate.parent_session_id,
                        candidate.claimed_root_session_id,
                    ]
                    .into_iter()
                    .flatten()
                    .all(|claim| session_ids.contains(&claim))
                })
                .map(|candidate| candidate.session_id)
                .collect::<Vec<_>>();
            if ready.is_empty() {
                return Ok(None);
            }
            for session_id in ready {
                discovered.remove(&session_id);
                session_ids.insert(session_id);
            }
        }
    }
    Ok(None)
}

#[derive(Debug, Clone, Copy)]
pub(super) struct SessionAncestry {
    pub(super) session_id: Uuid,
    pub(super) source_owner: StableEntityId,
    pub(super) parent_session_id: Option<Uuid>,
    pub(super) claimed_root_session_id: Option<Uuid>,
}

impl From<&SessionGroupingClaims> for SessionAncestry {
    fn from(session: &SessionGroupingClaims) -> Self {
        Self {
            session_id: session.session_id.as_uuid(),
            source_owner: session.source_owner,
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
        if current.source_owner != session.source_owner {
            return Ok(None);
        }
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
