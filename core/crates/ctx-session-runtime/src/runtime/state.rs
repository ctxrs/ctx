use super::*;

impl<SchedulerCommand> SessionRuntime<SchedulerCommand> {
    pub async fn cached_session_head_snapshot(
        &self,
        session_id: SessionId,
        limit: u32,
        include_events: bool,
    ) -> Option<SessionHeadSnapshot> {
        let mut cache = self.session_head_cache.lock().await;
        cache
            .get_mut(&session_id)
            .and_then(|entry| {
                entry.touch();
                entry.value.get(&SessionHeadCacheKey {
                    limit,
                    include_events,
                })
            })
            .cloned()
    }

    pub async fn cache_session_head_snapshot(
        &self,
        session_id: SessionId,
        limit: u32,
        include_events: bool,
        snapshot: SessionHeadSnapshot,
    ) {
        let mut cache = self.session_head_cache.lock().await;
        let entry = cache
            .entry(session_id)
            .or_insert_with(|| TimedEntry::new(HashMap::new()));
        entry.touch();
        entry.value.insert(
            SessionHeadCacheKey {
                limit,
                include_events,
            },
            snapshot,
        );
    }

    pub async fn get_broadcaster(&self, session_id: SessionId) -> broadcast::Sender<SessionEvent> {
        let mut map = self.broadcasters.lock().await;
        let entry = map.entry(session_id).or_insert_with(|| {
            let (tx, _) = broadcast::channel(256);
            TimedEntry::new(tx)
        });
        entry.touch();
        entry.value.clone()
    }

    pub async fn subscribe_session_event_head(
        &self,
        session_id: SessionId,
    ) -> watch::Receiver<i64> {
        let mut map = self.session_event_heads.lock().await;
        if let Some(entry) = map.get_mut(&session_id) {
            entry.touch();
            return entry.value.subscribe();
        }
        let (tx, rx) = watch::channel::<i64>(0);
        map.insert(session_id, TimedEntry::new(tx));
        rx
    }

    pub async fn publish_session_event_head(&self, session_id: SessionId, seq: i64) {
        let mut map = self.session_event_heads.lock().await;
        let sender = map.entry(session_id).or_insert_with(|| {
            let (tx, _rx) = watch::channel::<i64>(0);
            TimedEntry::new(tx)
        });
        sender.touch();
        let _ = sender.value.send(seq);
    }

    pub async fn remember_session_meta(&self, session: &Session) {
        let mut cache = self.session_meta_cache.lock().await;
        cache.insert(session.id, TimedEntry::new(session.clone()));
    }

    pub(super) async fn cached_session_meta(&self, session_id: SessionId) -> Option<Session> {
        let mut cache = self.session_meta_cache.lock().await;
        cache.get_mut(&session_id).map(|entry| {
            entry.touch();
            entry.value.clone()
        })
    }

    pub async fn session_meta_workspace(&self, session_id: SessionId) -> Option<WorkspaceId> {
        let mut cache = self.session_meta_cache.lock().await;
        cache.get_mut(&session_id).map(|entry| {
            entry.touch();
            entry.value.workspace_id
        })
    }

    pub async fn protected_session_ids(&self) -> HashSet<SessionId> {
        let mut active_sessions: HashSet<SessionId> = HashSet::new();
        {
            let set = self.running_sessions.lock().await;
            active_sessions.extend(set.iter().copied());
        }
        {
            let map = self.schedulers.lock().await;
            active_sessions.extend(map.keys().copied());
        }
        {
            let map = self.broadcasters.lock().await;
            active_sessions.extend(map.keys().copied());
        }
        {
            let map = self.session_event_heads.lock().await;
            active_sessions.extend(map.keys().copied());
        }
        active_sessions
    }

    pub async fn runtime_stats(&self) -> SessionRuntimeStats {
        SessionRuntimeStats {
            session_head_cache: self.session_head_cache.lock().await.len(),
            session_meta_cache: self.session_meta_cache.lock().await.len(),
            session_event_heads: self.session_event_heads.lock().await.len(),
            schedulers: self.schedulers.lock().await.len(),
            broadcasters: self.broadcasters.lock().await.len(),
            running_sessions: self.running_sessions.lock().await.len(),
            active_task_refreshes: self.active_task_refreshes.lock().await.len(),
        }
    }

    pub async fn cache_debug_stats(&self) -> SessionRuntimeCacheDebugStats {
        let head_cache = self.session_head_cache.lock().await;
        let head_cache_entries = head_cache.len();
        let head_cache_keys = head_cache.values().map(|entry| entry.value.len()).sum();
        let mut session_head_cache_bytes = 0;
        let mut session_head_cache_max_bytes = 0;
        for entry in head_cache.values() {
            for head in entry.value.values() {
                let bytes = json_bytes(head);
                session_head_cache_bytes += bytes;
                session_head_cache_max_bytes = session_head_cache_max_bytes.max(bytes);
            }
        }
        drop(head_cache);

        let session_meta_cache = self.session_meta_cache.lock().await;
        let session_meta_cache_entries = session_meta_cache.len();
        let mut session_meta_cache_bytes = 0;
        for entry in session_meta_cache.values() {
            session_meta_cache_bytes += json_bytes(&entry.value);
        }
        drop(session_meta_cache);

        let session_event_heads = self.session_event_heads.lock().await.len();
        let schedulers = self.schedulers.lock().await.len();
        let broadcasters_guard = self.broadcasters.lock().await;
        let broadcasters = broadcasters_guard.len();
        let mut broadcast_buffer_total = 0;
        let mut broadcast_buffer_max = 0;
        let mut broadcast_receivers_total = 0;
        let mut broadcast_receivers_max = 0;
        for entry in broadcasters_guard.values() {
            let sender = &entry.value;
            let len = sender.len();
            broadcast_buffer_total += len;
            broadcast_buffer_max = broadcast_buffer_max.max(len);
            let receivers = sender.receiver_count();
            broadcast_receivers_total += receivers;
            broadcast_receivers_max = broadcast_receivers_max.max(receivers);
        }
        drop(broadcasters_guard);
        let running_sessions = self.running_sessions.lock().await.len();
        let active_task_refreshes = self.active_task_refreshes.lock().await.len();

        SessionRuntimeCacheDebugStats {
            session_head_cache_entries: head_cache_entries,
            session_head_cache_keys: head_cache_keys,
            session_head_cache_bytes,
            session_head_cache_max_bytes,
            session_meta_cache_entries,
            session_meta_cache_bytes,
            session_event_heads,
            schedulers,
            broadcasters,
            broadcast_buffer_total,
            broadcast_buffer_max,
            broadcast_receivers_total,
            broadcast_receivers_max,
            running_sessions,
            active_task_refreshes,
        }
    }

    pub async fn cached_session_ids_for_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> Vec<SessionId> {
        let cache = self.session_meta_cache.lock().await;
        cache
            .iter()
            .filter_map(|(session_id, entry)| {
                if entry.value.workspace_id == workspace_id {
                    Some(*session_id)
                } else {
                    None
                }
            })
            .collect()
    }

    pub async fn sweep_idle_caches(
        &self,
        now: Instant,
        session_ttl: Duration,
    ) -> SessionCacheSweepStats {
        let mut stats = SessionCacheSweepStats::default();
        let running_sessions = {
            let set = self.running_sessions.lock().await;
            set.iter().copied().collect::<HashSet<_>>()
        };
        {
            let mut cache = self.session_head_cache.lock().await;
            let expired: Vec<SessionId> = cache
                .iter()
                .filter_map(|(session_id, entry)| {
                    if running_sessions.contains(session_id) {
                        return None;
                    }
                    if now.duration_since(entry.last_access) >= session_ttl {
                        Some(*session_id)
                    } else {
                        None
                    }
                })
                .collect();
            for session_id in &expired {
                cache.remove(session_id);
            }
            stats.session_head_evicted += expired.len();
        }
        {
            let mut cache = self.session_meta_cache.lock().await;
            let expired: Vec<SessionId> = cache
                .iter()
                .filter_map(|(session_id, entry)| {
                    if running_sessions.contains(session_id) {
                        return None;
                    }
                    if now.duration_since(entry.last_access) >= session_ttl {
                        Some(*session_id)
                    } else {
                        None
                    }
                })
                .collect();
            for session_id in &expired {
                cache.remove(session_id);
            }
            stats.session_meta_evicted += expired.len();
        }
        {
            let mut map = self.schedulers.lock().await;
            let expired: Vec<SessionId> = map
                .iter()
                .filter_map(|(session_id, entry)| {
                    if running_sessions.contains(session_id) {
                        return None;
                    }
                    if entry.value.is_closed()
                        || now.duration_since(entry.last_access) >= session_ttl
                    {
                        Some(*session_id)
                    } else {
                        None
                    }
                })
                .collect();
            for session_id in &expired {
                map.remove(session_id);
            }
            stats.schedulers_evicted += expired.len();
        }
        {
            let mut map = self.broadcasters.lock().await;
            let expired: Vec<SessionId> = map
                .iter()
                .filter_map(|(session_id, entry)| {
                    if running_sessions.contains(session_id) {
                        return None;
                    }
                    if now.duration_since(entry.last_access) >= session_ttl {
                        Some(*session_id)
                    } else {
                        None
                    }
                })
                .collect();
            for session_id in &expired {
                map.remove(session_id);
            }
            stats.broadcasters_evicted += expired.len();
        }
        {
            let mut map = self.session_event_heads.lock().await;
            let expired: Vec<SessionId> = map
                .iter()
                .filter_map(|(session_id, entry)| {
                    if running_sessions.contains(session_id) {
                        return None;
                    }
                    if now.duration_since(entry.last_access) >= session_ttl {
                        Some(*session_id)
                    } else {
                        None
                    }
                })
                .collect();
            for session_id in &expired {
                map.remove(session_id);
            }
            stats.session_event_heads_evicted += expired.len();
        }
        stats
    }

    pub async fn remove_session_state(&self, session_id: SessionId) {
        self.session_head_cache.lock().await.remove(&session_id);
        self.schedulers.lock().await.remove(&session_id);
        self.broadcasters.lock().await.remove(&session_id);
        self.session_event_heads.lock().await.remove(&session_id);
        self.running_sessions.lock().await.remove(&session_id);
        self.session_pins.lock().await.remove(&session_id);
        self.session_meta_cache.lock().await.remove(&session_id);
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SessionHeadCacheKey {
    pub limit: u32,
    pub include_events: bool,
}

#[derive(Debug)]
pub struct TimedEntry<T> {
    pub value: T,
    pub last_access: Instant,
}

impl<T> TimedEntry<T> {
    pub fn new(value: T) -> Self {
        Self {
            value,
            last_access: Instant::now(),
        }
    }

    pub fn touch(&mut self) {
        self.last_access = Instant::now();
    }

    pub fn touch_at(&mut self, now: Instant) {
        self.last_access = now;
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SessionCacheSweepStats {
    pub session_head_evicted: usize,
    pub session_meta_evicted: usize,
    pub schedulers_evicted: usize,
    pub broadcasters_evicted: usize,
    pub session_event_heads_evicted: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SessionRuntimeStats {
    pub session_head_cache: usize,
    pub session_meta_cache: usize,
    pub session_event_heads: usize,
    pub schedulers: usize,
    pub broadcasters: usize,
    pub running_sessions: usize,
    pub active_task_refreshes: usize,
}

#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct SessionRuntimeCacheDebugStats {
    pub session_head_cache_entries: usize,
    pub session_head_cache_keys: usize,
    pub session_head_cache_bytes: usize,
    pub session_head_cache_max_bytes: usize,
    pub session_meta_cache_entries: usize,
    pub session_meta_cache_bytes: usize,
    pub session_event_heads: usize,
    pub schedulers: usize,
    pub broadcasters: usize,
    pub broadcast_buffer_total: usize,
    pub broadcast_buffer_max: usize,
    pub broadcast_receivers_total: usize,
    pub broadcast_receivers_max: usize,
    pub running_sessions: usize,
    pub active_task_refreshes: usize,
}

fn json_bytes<T: serde::Serialize>(value: &T) -> usize {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .unwrap_or_default()
}
