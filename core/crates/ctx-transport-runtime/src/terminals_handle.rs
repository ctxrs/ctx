use super::*;

impl TerminalSessionHandle {
    pub fn snapshot(&self) -> TerminalSession {
        let runtime = lock_or_recover(self.runtime.as_ref(), "terminal runtime");
        TerminalSession {
            status: runtime.status.clone(),
            exit_code: runtime.exit_code,
            updated_at: runtime.updated_at,
            ..self.info.clone()
        }
    }

    pub fn issue_stream_connect_path(&self) -> (String, chrono::DateTime<chrono::Utc>) {
        let now = Utc::now();
        let expires_at = now + chrono::Duration::seconds(TERMINAL_STREAM_TOKEN_TTL_SECS);
        let mut tokens = lock_or_recover(self.stream_tokens.as_ref(), "terminal stream tokens");
        tokens.retain(|_, expiry| *expiry > now);
        let token = Uuid::new_v4().to_string();
        tokens.insert(token.clone(), expires_at);
        (build_stream_connect_path(self.info.id, &token), expires_at)
    }

    pub fn consume_stream_token(&self, token: &str) -> bool {
        let now = Utc::now();
        let mut tokens = lock_or_recover(self.stream_tokens.as_ref(), "terminal stream tokens");
        tokens.retain(|_, expiry| *expiry > now);
        matches!(tokens.remove(token), Some(expiry) if expiry > now)
    }

    fn touch_activity(&self) {
        let mut runtime = lock_or_recover(self.runtime.as_ref(), "terminal runtime");
        runtime.last_activity = Utc::now();
        runtime.updated_at = runtime.last_activity;
    }

    pub fn mark_client_connected(&self) {
        let mut runtime = lock_or_recover(self.runtime.as_ref(), "terminal runtime");
        runtime.connected_clients = runtime.connected_clients.saturating_add(1);
        runtime.last_activity = Utc::now();
        runtime.updated_at = runtime.last_activity;
    }

    pub fn mark_client_disconnected(&self) {
        let mut runtime = lock_or_recover(self.runtime.as_ref(), "terminal runtime");
        runtime.connected_clients = runtime.connected_clients.saturating_sub(1);
        runtime.updated_at = Utc::now();
    }

    pub fn output_receiver(&self) -> broadcast::Receiver<Vec<u8>> {
        self.output_tx.subscribe()
    }

    pub fn status_receiver(&self) -> broadcast::Receiver<TerminalStatusEvent> {
        self.status_tx.subscribe()
    }

    pub fn output_snapshot(&self) -> Vec<u8> {
        let buffer = lock_or_recover(self.output_buffer.as_ref(), "terminal buffer");
        buffer.iter().copied().collect()
    }

    pub fn output_snapshot_tail(&self, tail: usize) -> Vec<u8> {
        let buffer = lock_or_recover(self.output_buffer.as_ref(), "terminal buffer");
        let len = buffer.len();
        if len == 0 || tail == 0 {
            return Vec::new();
        }
        let tail = tail.min(len);
        buffer.iter().skip(len - tail).copied().collect()
    }

    pub fn send_input(&self, data: Vec<u8>) {
        self.touch_activity();
        match &self.backend {
            TerminalBackend::Local { input_tx, .. } => {
                let _ = input_tx.send(data);
            }
            TerminalBackend::Remote { outbound_tx } => {
                let _ = outbound_tx.send(RemoteTerminalOutgoing::Binary(data));
            }
        }
    }

    #[doc(hidden)]
    pub fn test_handle_with_output(output: &[u8]) -> Arc<Self> {
        let now = Utc::now();
        let id = TerminalId::new();
        let (output_tx, _) = broadcast::channel(16);
        let (status_tx, _) = broadcast::channel(16);
        let (_outbound_tx, outbound_rx) = mpsc::unbounded_channel();
        let mut output_buffer = VecDeque::with_capacity(output.len());
        output_buffer.extend(output.iter().copied());
        Arc::new(Self {
            info: TerminalSession {
                id,
                workspace_id: WorkspaceId::new(),
                task_id: None,
                session_id: None,
                worktree_id: None,
                cwd: "/tmp".to_string(),
                shell: "/bin/sh".to_string(),
                title: "test-terminal".to_string(),
                status: TerminalStatus::Running,
                exit_code: None,
                stream_path: build_stream_path(id),
                created_at: now,
                updated_at: now,
            },
            stream_tokens: Arc::new(Mutex::new(HashMap::new())),
            container_backed: false,
            runtime: Arc::new(Mutex::new(TerminalRuntime {
                status: TerminalStatus::Running,
                exit_code: None,
                updated_at: now,
                last_activity: now,
                connected_clients: 0,
            })),
            output_tx,
            status_tx,
            output_buffer: Arc::new(Mutex::new(output_buffer)),
            backend: TerminalBackend::Remote {
                outbound_tx: {
                    drop(outbound_rx);
                    _outbound_tx
                },
            },
        })
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.touch_activity();
        match &self.backend {
            TerminalBackend::Local { master, .. } => {
                let master = lock_or_recover(master.as_ref(), "terminal master");
                master
                    .resize(PtySize {
                        rows,
                        cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    })
                    .context("resize pty")?;
            }
            TerminalBackend::Remote { outbound_tx } => {
                let payload = serde_json::to_string(&TerminalClientMessage::Resize { cols, rows })
                    .unwrap_or_else(|_| "{\"type\":\"resize\"}".to_string());
                let _ = outbound_tx.send(RemoteTerminalOutgoing::Text(payload));
            }
        }
        Ok(())
    }

    pub fn kill(&self) -> Result<()> {
        match &self.backend {
            TerminalBackend::Local { child, .. } => {
                let mut child = lock_or_recover(child.as_ref(), "terminal child");
                child.kill().context("kill terminal")?;
            }
            TerminalBackend::Remote { outbound_tx } => {
                let _ = outbound_tx.send(RemoteTerminalOutgoing::Close);
            }
        }
        Ok(())
    }

    pub fn mark_exited(&self, exit_code: Option<i32>) {
        let mut runtime = lock_or_recover(self.runtime.as_ref(), "terminal runtime");
        runtime.status = TerminalStatus::Exited;
        runtime.exit_code = exit_code;
        runtime.updated_at = Utc::now();
        runtime.last_activity = runtime.updated_at;
        let _ = self.status_tx.send(TerminalStatusEvent {
            status: TerminalStatus::Exited,
            exit_code,
        });
    }

    pub fn is_running(&self) -> bool {
        let runtime = lock_or_recover(self.runtime.as_ref(), "terminal runtime");
        matches!(runtime.status, TerminalStatus::Running)
    }
}

pub(super) fn build_stream_path(id: TerminalId) -> String {
    format!("/api/terminals/{}/stream", id.0)
}

pub(super) fn build_stream_connect_path(id: TerminalId, stream_token: &str) -> String {
    format!("/api/terminals/{}/stream?token={stream_token}", id.0)
}
