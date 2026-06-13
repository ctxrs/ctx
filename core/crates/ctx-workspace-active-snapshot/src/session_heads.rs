use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

use ctx_core::ids::{SessionId, WorkspaceId};
use ctx_core::models::{
    MessageRole, Session, SessionHeadDelta, SessionHeadSnapshot, SessionTurnStatus,
    WorkspaceActiveSnapshotEvent,
};

use crate::cache::{CachedSessionHead, SessionHeadCapability, SessionHeadCompleteness};
use crate::delta::apply_head_delta;
use crate::entry::WorkspaceActiveSnapshotEntry;
use crate::trim::{compact_active_head_snapshot, new_head_snapshot};
use crate::{SessionReplayCursor, WorkspaceActiveSnapshotHub};

impl WorkspaceActiveSnapshotHub {
    fn completed_head_is_visibly_complete(head: &SessionHeadSnapshot) -> bool {
        let Some(latest_turn) = head.turns.last() else {
            return true;
        };
        if latest_turn.status != SessionTurnStatus::Completed {
            return true;
        }
        head.messages.iter().any(|message| {
            message.role == MessageRole::Assistant
                && message.turn_id == Some(latest_turn.turn_id)
                && !message.content.trim().is_empty()
        })
    }

    pub(crate) fn now_ms() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64
    }

    pub(crate) async fn prune_non_active_session_heads(&self) {
        let active_session_ids = {
            let index = self.active_head_index.lock().await;
            index.keys().copied().collect::<HashSet<_>>()
        };
        let mut session_heads = self.session_heads.lock().await;
        if session_heads.len() <= self.session_head_limit {
            return;
        }
        let removable = session_heads
            .iter()
            .filter_map(|(session_id, cached)| {
                (!active_session_ids.contains(session_id)).then_some((
                    *session_id,
                    cached.capability == SessionHeadCapability::ReplayCapable,
                    cached.completeness == SessionHeadCompleteness::Hydrated,
                    cached.last_touched_at_ms,
                ))
            })
            .collect::<Vec<_>>();
        let excess = session_heads.len().saturating_sub(self.session_head_limit);
        if excess == 0 || removable.is_empty() {
            return;
        }
        let mut removable = removable;
        removable.sort_by_key(
            |(session_id, replay_capable, hydrated, last_touched_at_ms)| {
                (
                    *replay_capable,
                    *hydrated,
                    *last_touched_at_ms,
                    session_id.0,
                )
            },
        );
        for (session_id, _, _, _) in removable.into_iter().take(excess) {
            session_heads.remove(&session_id);
        }
    }

    pub(crate) async fn seed_cached_session_heads(
        &self,
        workspace_id: WorkspaceId,
        heads: &[SessionHeadSnapshot],
    ) {
        if heads.is_empty() {
            return;
        }
        let touched_at_ms = Self::now_ms();
        {
            let mut session_heads = self.session_heads.lock().await;
            for head in heads {
                match session_heads.get(&head.session.id) {
                    Some(existing)
                        if existing.capability == SessionHeadCapability::ReplayCapable => {}
                    _ => {
                        session_heads.insert(
                            head.session.id,
                            CachedSessionHead {
                                workspace_id,
                                head: head.clone(),
                                completeness: SessionHeadCompleteness::Hydrated,
                                capability: SessionHeadCapability::CompactOnly,
                                last_touched_at_ms: touched_at_ms,
                            },
                        );
                    }
                }
            }
        }
        let mut index = self.active_head_index.lock().await;
        for head in heads {
            index.insert(head.session.id, workspace_id);
        }
        drop(index);
        self.prune_non_active_session_heads().await;
    }

    pub async fn update_session_head(&self, head: SessionHeadSnapshot) {
        let session_id = head.session.id;
        let workspace_id = head.session.workspace_id;
        let cursor = SessionReplayCursor::from_head(&head);
        let touched_at_ms = Self::now_ms();
        let mut session_heads = self.session_heads.lock().await;
        session_heads.insert(
            session_id,
            CachedSessionHead {
                workspace_id,
                head: head.clone(),
                completeness: SessionHeadCompleteness::Hydrated,
                capability: SessionHeadCapability::ReplayCapable,
                last_touched_at_ms: touched_at_ms,
            },
        );
        drop(session_heads);
        let mut guard = self.inner.lock().await;
        let entry = guard
            .entry(workspace_id)
            .or_insert_with(WorkspaceActiveSnapshotEntry::new);
        entry.seed_session_replay(session_id, cursor);
        if entry.is_primary_session(session_id) {
            entry
                .active_heads
                .insert(session_id, compact_active_head_snapshot(&head));
            let mut index = self.active_head_index.lock().await;
            index.insert(session_id, workspace_id);
        }
        self.prune_non_active_session_heads().await;
    }

    pub async fn update_compact_session_head(&self, head: SessionHeadSnapshot) {
        let session_id = head.session.id;
        let workspace_id = head.session.workspace_id;
        let cursor = SessionReplayCursor::from_head(&head);
        let touched_at_ms = Self::now_ms();
        let mut heads = self.session_heads.lock().await;
        let should_replace = !matches!(
            heads.get(&session_id),
            Some(existing) if existing.capability == SessionHeadCapability::ReplayCapable
        );
        if should_replace {
            heads.insert(
                session_id,
                CachedSessionHead {
                    workspace_id,
                    head: head.clone(),
                    completeness: SessionHeadCompleteness::Hydrated,
                    capability: SessionHeadCapability::CompactOnly,
                    last_touched_at_ms: touched_at_ms,
                },
            );
        } else if let Some(existing) = heads.get_mut(&session_id) {
            existing.touch(touched_at_ms);
        }
        drop(heads);
        let mut guard = self.inner.lock().await;
        let entry = guard
            .entry(workspace_id)
            .or_insert_with(WorkspaceActiveSnapshotEntry::new);
        entry.seed_session_replay(session_id, cursor);
        if entry.is_primary_session(session_id) {
            entry
                .active_heads
                .insert(session_id, compact_active_head_snapshot(&head));
            let mut index = self.active_head_index.lock().await;
            index.insert(session_id, workspace_id);
        }
        self.prune_non_active_session_heads().await;
    }

    pub async fn remove_session_head(&self, session_id: SessionId) {
        let mut session_heads = self.session_heads.lock().await;
        session_heads.remove(&session_id);
    }

    async fn remove_session_inner(
        &self,
        session_id: SessionId,
        workspace_id_hint: Option<WorkspaceId>,
    ) {
        let cached_workspace_id = {
            let mut session_heads = self.session_heads.lock().await;
            session_heads
                .remove(&session_id)
                .map(|cached| cached.workspace_id)
        };
        let indexed_workspace_id = {
            let mut index = self.active_head_index.lock().await;
            index.remove(&session_id)
        };
        let removed = {
            let mut guard = self.inner.lock().await;
            if let Some(workspace_id) = cached_workspace_id
                .or(indexed_workspace_id)
                .or(workspace_id_hint)
            {
                let Some(entry) = guard.get_mut(&workspace_id) else {
                    return;
                };
                let changed = entry.active_heads.remove(&session_id).is_some()
                    || entry.session_replay.remove(&session_id).is_some();
                if changed || workspace_id_hint.is_some() {
                    entry.snapshot_rev += 1;
                    Some((entry.tx.clone(), entry.snapshot_rev, workspace_id))
                } else {
                    None
                }
            } else {
                let mut removed = None;
                for (workspace_id, entry) in guard.iter_mut() {
                    let changed = entry.active_heads.remove(&session_id).is_some()
                        || entry.session_replay.remove(&session_id).is_some();
                    if changed {
                        entry.snapshot_rev += 1;
                        removed = Some((entry.tx.clone(), entry.snapshot_rev, *workspace_id));
                        break;
                    }
                }
                removed
            }
        };

        if let Some((tx, snapshot_rev, workspace_id)) = removed {
            let _ = tx.send(WorkspaceActiveSnapshotEvent::SessionRemoved {
                workspace_id,
                snapshot_rev,
                session_id,
            });
        }
    }

    pub async fn remove_session(&self, session_id: SessionId) {
        self.remove_session_inner(session_id, None).await;
    }

    pub async fn remove_session_with_workspace_hint(
        &self,
        workspace_id: WorkspaceId,
        session_id: SessionId,
    ) {
        self.remove_session_inner(session_id, Some(workspace_id))
            .await;
    }

    pub async fn remove_workspace(&self, workspace_id: WorkspaceId) {
        let entry = {
            let mut guard = self.inner.lock().await;
            guard.remove(&workspace_id)
        };
        let mut session_ids: HashSet<SessionId> = entry
            .map(|entry| {
                entry
                    .active_heads
                    .keys()
                    .copied()
                    .chain(entry.session_replay.keys().copied())
                    .collect()
            })
            .unwrap_or_default();
        {
            let mut session_heads = self.session_heads.lock().await;
            for session_id in session_heads
                .iter()
                .filter_map(|(session_id, cached)| {
                    (cached.workspace_id == workspace_id).then_some(*session_id)
                })
                .collect::<Vec<_>>()
            {
                session_ids.insert(session_id);
            }
            for session_id in &session_ids {
                session_heads.remove(session_id);
            }
        }
        let mut index = self.active_head_index.lock().await;
        index.retain(|session_id, owner_workspace_id| {
            if *owner_workspace_id == workspace_id {
                session_ids.insert(*session_id);
                return false;
            }
            true
        });
    }

    pub async fn get_session_head(&self, session_id: SessionId) -> Option<SessionHeadSnapshot> {
        let mut heads = self.session_heads.lock().await;
        heads
            .get_mut(&session_id)
            .filter(|cached| {
                cached.completeness == SessionHeadCompleteness::Hydrated
                    && cached.capability == SessionHeadCapability::ReplayCapable
            })
            .map(|cached| {
                cached.touch(Self::now_ms());
                cached.head.clone()
            })
    }

    pub async fn get_cached_session_head_for_request(
        &self,
        session_id: SessionId,
        include_events: bool,
        limit: u32,
        min_event_seq: Option<i64>,
    ) -> Option<SessionHeadSnapshot> {
        let requested = limit as usize;
        let mut head = {
            let mut heads = self.session_heads.lock().await;
            let cached = heads.get_mut(&session_id)?;
            if cached.completeness != SessionHeadCompleteness::Hydrated {
                return None;
            }
            if include_events && cached.capability != SessionHeadCapability::ReplayCapable {
                return None;
            }
            if include_events && !Self::completed_head_is_visibly_complete(&cached.head) {
                return None;
            }
            cached.touch(Self::now_ms());
            let head = &cached.head;
            if cached.capability == SessionHeadCapability::CompactOnly && head.head_window.truncated
            {
                return None;
            }
            if head.turns.len() > requested {
                return None;
            }
            if head.has_more_turns && head.turns.len() < requested {
                return None;
            }
            if let Some(min_event_seq) = min_event_seq {
                if head.last_event_seq < min_event_seq {
                    return None;
                }
            }
            head.clone()
        };
        if !include_events {
            head.events.clear();
            head.head_window.event_count = 0;
        }
        Some(head)
    }

    pub async fn get_cached_session_head_for_read(
        &self,
        session_id: SessionId,
    ) -> Option<SessionHeadSnapshot> {
        let mut heads = self.session_heads.lock().await;
        heads
            .get_mut(&session_id)
            .filter(|cached| cached.completeness == SessionHeadCompleteness::Hydrated)
            .map(|cached| {
                cached.touch(Self::now_ms());
                let mut head = cached.head.clone();
                if cached.capability == SessionHeadCapability::CompactOnly {
                    head.events.clear();
                    head.head_window.event_count = 0;
                }
                head
            })
    }

    pub async fn publish_session_head_delta(
        &self,
        workspace_id: WorkspaceId,
        session: &Session,
        delta: SessionHeadDelta,
        bump_snapshot: bool,
    ) {
        let mut delta = delta;
        if delta.emitted_at_ms.is_none() {
            delta.emitted_at_ms = Some(Self::now_ms());
        }
        let (tx, snapshot_rev) = {
            let mut guard = self.inner.lock().await;
            let entry = guard
                .entry(workspace_id)
                .or_insert_with(WorkspaceActiveSnapshotEntry::new);
            if bump_snapshot {
                entry.snapshot_rev += 1;
            }
            entry.record_session_delta(&delta);
            if session.parent_session_id.is_none() {
                if let Some(head) = entry.active_heads.get_mut(&delta.session_id) {
                    apply_head_delta(head, &delta);
                } else {
                    let mut head = new_head_snapshot(session);
                    apply_head_delta(&mut head, &delta);
                    entry.active_heads.insert(delta.session_id, head);
                }
            }
            (entry.tx.clone(), entry.snapshot_rev)
        };
        {
            let touched_at_ms = Self::now_ms();
            let mut session_heads = self.session_heads.lock().await;
            if let Some(cached) = session_heads.get_mut(&delta.session_id) {
                apply_head_delta(&mut cached.head, &delta);
                cached.touch(touched_at_ms);
            } else if session.parent_session_id.is_none() {
                let mut head = new_head_snapshot(session);
                apply_head_delta(&mut head, &delta);
                session_heads.insert(
                    delta.session_id,
                    CachedSessionHead {
                        workspace_id,
                        head,
                        completeness: SessionHeadCompleteness::DeltaOnly,
                        capability: SessionHeadCapability::CompactOnly,
                        last_touched_at_ms: touched_at_ms,
                    },
                );
            }
        }
        self.prune_non_active_session_heads().await;
        let _ = tx.send(WorkspaceActiveSnapshotEvent::SessionHeadDelta {
            workspace_id,
            snapshot_rev,
            delta: Box::new(delta),
        });
    }
}
