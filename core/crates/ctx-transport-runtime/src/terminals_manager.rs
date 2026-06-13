use super::*;

#[derive(Debug, Clone, Serialize)]
pub struct TerminalManagerStats {
    pub session_count: usize,
    pub output_buffer_bytes: usize,
    pub max_output_buffer_bytes: usize,
    pub connected_clients: usize,
}

impl TerminalManager {
    pub async fn list(&self, workspace_id: WorkspaceId) -> Vec<TerminalSession> {
        let sessions = self.sessions.lock().await;
        sessions
            .values()
            .filter(|sess| sess.info.workspace_id == workspace_id)
            .map(|sess| sess.snapshot())
            .collect()
    }

    pub async fn stats(&self) -> TerminalManagerStats {
        let sessions = self.sessions.lock().await;
        let mut output_buffer_bytes = 0;
        let mut max_output_buffer_bytes = 0;
        let mut connected_clients = 0;
        for handle in sessions.values() {
            let buffer_len = {
                let buffer = lock_or_recover(handle.output_buffer.as_ref(), "terminal buffer");
                buffer.len()
            };
            output_buffer_bytes += buffer_len;
            if buffer_len > max_output_buffer_bytes {
                max_output_buffer_bytes = buffer_len;
            }
            let runtime = lock_or_recover(handle.runtime.as_ref(), "terminal runtime");
            connected_clients += runtime.connected_clients;
        }
        TerminalManagerStats {
            session_count: sessions.len(),
            output_buffer_bytes,
            max_output_buffer_bytes,
            connected_clients,
        }
    }

    pub async fn start_reaper(self: Arc<Self>) {
        let Some(idle_for) = terminal_idle_timeout() else {
            return;
        };
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(TERMINAL_REAPER_INTERVAL);
            loop {
                interval.tick().await;
                if let Err(err) = self.reap_idle(idle_for).await {
                    tracing::warn!("terminal reap failed: {err:#}");
                }
            }
        });
    }

    pub async fn has_running(&self) -> bool {
        let sessions = self.sessions.lock().await;
        sessions.values().any(|sess| sess.is_running())
    }

    pub async fn has_running_container_backed(&self) -> bool {
        let sessions = self.sessions.lock().await;
        sessions
            .values()
            .any(|sess| sess.container_backed && sess.is_running())
    }

    pub async fn get(&self, id: TerminalId) -> Option<Arc<TerminalSessionHandle>> {
        let sessions = self.sessions.lock().await;
        sessions.get(&id).cloned()
    }

    pub async fn require_stream_access(
        &self,
        id: TerminalId,
        token: &str,
    ) -> Result<TerminalStreamSession, TerminalStreamAccessError> {
        let handle = self
            .get(id)
            .await
            .ok_or(TerminalStreamAccessError::NotFound)?;
        if !handle.consume_stream_token(token) {
            return Err(TerminalStreamAccessError::Unauthorized);
        }
        Ok(TerminalStreamSession::new(handle))
    }

    pub async fn remove(&self, id: TerminalId) -> Option<Arc<TerminalSessionHandle>> {
        let mut sessions = self.sessions.lock().await;
        sessions.remove(&id)
    }

    async fn reap_idle(&self, idle_for: Duration) -> Result<()> {
        let handles = {
            let sessions = self.sessions.lock().await;
            sessions
                .iter()
                .map(|(id, handle)| (*id, handle.clone()))
                .collect::<Vec<_>>()
        };
        let mut to_close = Vec::new();
        for (id, handle) in handles {
            let runtime = lock_or_recover(handle.runtime.as_ref(), "terminal runtime");
            if !matches!(runtime.status, TerminalStatus::Running) {
                continue;
            }
            if runtime.connected_clients > 0 {
                continue;
            }
            let idle = Utc::now() - runtime.last_activity;
            if idle.to_std().unwrap_or_default() > idle_for {
                to_close.push(id);
            }
        }
        for id in to_close {
            if let Some(handle) = self.remove(id).await {
                let _ = handle.kill();
                handle.mark_exited(None);
            }
        }
        Ok(())
    }
}

pub(super) fn push_output(
    output_buffer: &Arc<Mutex<VecDeque<u8>>>,
    output_tx: &broadcast::Sender<Vec<u8>>,
    runtime: &Arc<Mutex<TerminalRuntime>>,
    bytes: &[u8],
) {
    {
        let mut buffer = lock_or_recover(output_buffer.as_ref(), "terminal output buffer");
        for b in bytes {
            buffer.push_back(*b);
        }
        while buffer.len() > MAX_OUTPUT_BYTES {
            buffer.pop_front();
        }
    }
    {
        let mut runtime = lock_or_recover(runtime.as_ref(), "terminal runtime");
        runtime.last_activity = Utc::now();
        runtime.updated_at = runtime.last_activity;
    }
    let _ = output_tx.send(bytes.to_vec());
}
