use super::*;

const TERMINAL_PING_INTERVAL: Duration = Duration::from_secs(25);
const TERMINAL_RECONNECT_BASE_MS: u64 = 500;
const TERMINAL_RECONNECT_MAX_MS: u64 = 10_000;

impl TerminalManager {
    pub async fn create_remote(
        &self,
        req: TerminalCreateRequest,
        remote: RemoteTerminalRequest,
    ) -> Result<Arc<TerminalSessionHandle>> {
        let id = remote.terminal_id;
        let title = PathBuf::from(&req.shell)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("terminal")
            .to_string();

        let (output_tx, _) = broadcast::channel(1024);
        let (status_tx, _) = broadcast::channel(16);
        let output_buffer = Arc::new(Mutex::new(VecDeque::with_capacity(8192)));
        let (outbound_tx, outbound_rx) = mpsc::unbounded_channel::<RemoteTerminalOutgoing>();

        let now = Utc::now();
        let runtime = Arc::new(Mutex::new(TerminalRuntime {
            status: TerminalStatus::Running,
            exit_code: None,
            updated_at: now,
            last_activity: now,
            connected_clients: 0,
        }));

        let info = TerminalSession {
            id,
            workspace_id: req.workspace_id,
            task_id: req.task_id,
            session_id: req.session_id,
            worktree_id: req.worktree_id,
            cwd: req.cwd.to_string_lossy().to_string(),
            shell: req.shell,
            title,
            status: TerminalStatus::Running,
            exit_code: None,
            stream_path: build_stream_path(id),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let session = Arc::new(TerminalSessionHandle {
            info,
            stream_tokens: Arc::new(Mutex::new(HashMap::new())),
            container_backed: false,
            runtime: runtime.clone(),
            output_tx: output_tx.clone(),
            status_tx: status_tx.clone(),
            output_buffer: output_buffer.clone(),
            backend: TerminalBackend::Remote {
                outbound_tx: outbound_tx.clone(),
            },
        });

        let remote_clone = remote.clone();
        let output_buffer_clone = output_buffer.clone();
        let output_tx_clone = output_tx.clone();
        let status_tx_clone = status_tx.clone();
        let runtime_clone = runtime.clone();

        tokio::spawn(async move {
            let mut outbound_rx = outbound_rx;
            let mut backoff = Duration::from_millis(TERMINAL_RECONNECT_BASE_MS);
            loop {
                let ws_stream = match connect_terminal_gateway(&remote_clone).await {
                    Ok(stream) => {
                        backoff = Duration::from_millis(TERMINAL_RECONNECT_BASE_MS);
                        stream
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "terminal gateway connection failed");
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff + backoff)
                            .min(Duration::from_millis(TERMINAL_RECONNECT_MAX_MS));
                        continue;
                    }
                };

                let (mut ws_write, mut ws_read) = ws_stream.split();
                let mut ping = tokio::time::interval(TERMINAL_PING_INTERVAL);
                ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

                loop {
                    tokio::select! {
                        outbound = outbound_rx.recv() => {
                            let Some(outbound) = outbound else {
                                return;
                            };
                            let send_result = match outbound {
                                RemoteTerminalOutgoing::Binary(data) => {
                                    ws_write.send(Message::Binary(data.into())).await
                                }
                                RemoteTerminalOutgoing::Text(text) => {
                                    ws_write.send(Message::Text(text.into())).await
                                }
                                RemoteTerminalOutgoing::Close => ws_write.send(Message::Close(None)).await,
                            };
                            if send_result.is_err() {
                                break;
                            }
                        }
                        msg = ws_read.next() => {
                            let msg = match msg {
                                Some(Ok(msg)) => msg,
                                Some(Err(_)) | None => break,
                            };
                            match msg {
                                Message::Binary(data) => {
                                    push_output(&output_buffer_clone, &output_tx_clone, &runtime_clone, &data);
                                }
                                Message::Text(text) => {
                                    if let Ok(parsed) =
                                        serde_json::from_str::<TerminalServerMessage>(text.as_str())
                                    {
                                        match parsed {
                                            TerminalServerMessage::Status { status, exit_code } => {
                                                let mut runtime =
                                                    lock_or_recover(runtime_clone.as_ref(), "terminal runtime");
                                                runtime.status = status.clone();
                                                runtime.exit_code = exit_code;
                                                runtime.updated_at = Utc::now();
                                                runtime.last_activity = runtime.updated_at;
                                                let _ = status_tx_clone
                                                    .send(TerminalStatusEvent { status, exit_code });
                                            }
                                            TerminalServerMessage::Pong => {}
                                        }
                                    } else {
                                        push_output(
                                            &output_buffer_clone,
                                            &output_tx_clone,
                                            &runtime_clone,
                                            text.as_bytes(),
                                        );
                                    }
                                }
                                Message::Ping(payload) => {
                                    let _ = ws_write.send(Message::Pong(payload)).await;
                                }
                                Message::Pong(_) => {}
                                Message::Close(_) => break,
                                Message::Frame(_) => {}
                            }
                        }
                        _ = ping.tick() => {
                            if ws_write.send(Message::Ping(Vec::new().into())).await.is_err() {
                                break;
                            }
                        }
                    }
                }

                tokio::time::sleep(backoff).await;
                backoff = (backoff + backoff).min(Duration::from_millis(TERMINAL_RECONNECT_MAX_MS));
            }
        });

        let mut sessions = self.sessions.lock().await;
        sessions.insert(id, session.clone());
        Ok(session)
    }
}
