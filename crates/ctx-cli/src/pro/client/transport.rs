use super::*;

pub(crate) struct ProClient {
    pub(super) stdin: Option<ChildStdin>,
    pub(super) stdout: ChildStdout,
    pub(super) child: Arc<Mutex<Child>>,
    pub(super) stderr: StderrDrain,
    pub(super) sequence: u64,
    pub(super) capabilities: BTreeSet<Capability>,
    pub(super) helper_version: String,
    pub(super) authorization_state: Option<EntitlementAccessState>,
    pub(super) entitlement_schedule: Option<EntitlementSchedule>,
    pub(super) _execution_guard: Option<VerifiedHelperExecutable>,
}

impl ProClient {
    pub(super) fn connect(data_root: &Path, required: &BTreeSet<Capability>) -> Result<Self> {
        Self::connect_with_authorization_mode(data_root, required, None, false)
    }

    pub(super) fn connect_for_status(
        data_root: &Path,
        required: &BTreeSet<Capability>,
    ) -> Result<Self> {
        Self::connect_with_authorization_mode(data_root, required, None, true)
    }

    pub(super) fn connect_with_authorization_mode(
        data_root: &Path,
        required: &BTreeSet<Capability>,
        authorization: Option<&dyn AuthorizationProvider>,
        bind_status_identity: bool,
    ) -> Result<Self> {
        let executable = helper_executable(data_root)?;
        let path = executable.path().to_path_buf();
        Self::connect_to_path_with_authorization_mode(
            data_root,
            &path,
            Some(executable),
            required,
            authorization,
            bind_status_identity,
        )
    }

    pub(super) fn connect_to_path_with_authorization_mode(
        data_root: &Path,
        path: &Path,
        execution_guard: Option<VerifiedHelperExecutable>,
        required: &BTreeSet<Capability>,
        authorization: Option<&dyn AuthorizationProvider>,
        bind_status_identity: bool,
    ) -> Result<Self> {
        // Commit and PR blame are graph-only Query sessions and never authorize repository
        // access. Do not make those requests depend on the caller's Git installation. Every
        // other helper session preserves the existing startup binding, including file blame,
        // staged setup smoke, status, journal, and deletion operations.
        let query_only = required.len() == 1 && required.contains(&Capability::Query);
        let git_executable = (!query_only).then(git_executable).transpose()?;
        let mut command = helper_command::new(path, data_root, git_executable.as_deref())?;
        command
            .arg("serve-stdio")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(target_os = "linux")]
        {
            let expected_parent = unsafe { libc::getpid() };
            unsafe {
                command.pre_exec(move || {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::getppid() != expected_parent {
                        libc::kill(libc::getpid(), libc::SIGKILL);
                        libc::_exit(127);
                    }
                    Ok(())
                });
            }
        }
        #[cfg(unix)]
        command.process_group(0);
        if let Some(executable) = execution_guard.as_ref() {
            executable.verify_execution_identity()?;
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("helper_crashed: start Pro helper {}", path.display()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("helper_crashed: Pro helper stdin was unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("helper_crashed: Pro helper stdout was unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("helper_crashed: Pro helper stderr was unavailable"))?;
        let mut client = Self {
            stdin: Some(stdin),
            stdout,
            child: Arc::new(Mutex::new(child)),
            stderr: StderrDrain::start(stderr),
            sequence: 0,
            capabilities: BTreeSet::new(),
            helper_version: String::new(),
            authorization_state: None,
            entitlement_schedule: None,
            _execution_guard: execution_guard,
        };
        let offered = BTreeSet::from([
            Capability::EntitlementAuthorization,
            Capability::GraphKeyDeletion,
            Capability::Status,
            Capability::JournalSync,
            Capability::OutputMaterialization,
            Capability::Query,
            Capability::GitRead,
        ]);
        let response = client.exchange(
            HostMessage::Hello(HelloRequest::current(
                env!("CARGO_PKG_VERSION"),
                offered.clone(),
            )),
            HANDSHAKE_TIMEOUT,
        )?;
        let hello = match response {
            HelperMessage::Hello(hello) => hello,
            HelperMessage::Error(error) => return Err(protocol_error(error)),
            _ => bail!("protocol_mismatch: helper did not answer hello negotiation"),
        };
        if hello.protocol_version != PROTOCOL_VERSION
            || hello.protocol_fingerprint != PROTOCOL_FINGERPRINT
        {
            bail!("protocol_mismatch: helper does not implement the exact Protocol V1 inventory");
        }
        if !hello.capabilities.is_subset(&offered) {
            bail!("protocol_mismatch: helper advertised capabilities the host did not offer");
        }
        if let Some(missing) = required
            .iter()
            .find(|capability| !hello.capabilities.contains(capability))
        {
            bail!("protocol_mismatch: helper does not support required capability {missing:?}");
        }
        let authorization_selected = hello
            .capabilities
            .contains(&Capability::EntitlementAuthorization);
        if authorization_selected && authorization_required(required, bind_status_identity) {
            let challenge =
                ctx_pro_host_protocol::decode_base64url(&hello.authorization_challenge_base64url)
                    .and_then(|bytes| bytes.try_into().ok())
                    .ok_or_else(|| {
                        anyhow!(
                            "protocol_mismatch: helper returned an invalid authorization challenge"
                        )
                    })?;
            let stored;
            let entitlement_schedule;
            let provider: &dyn AuthorizationProvider = if let Some(provider) = authorization {
                entitlement_schedule = None;
                provider
            } else if bind_status_identity {
                stored = StoredAuthorizationProvider::load_for_status(data_root)?;
                entitlement_schedule = Some(stored.entitlement_schedule());
                &stored
            } else {
                stored = StoredAuthorizationProvider::load(data_root)?;
                entitlement_schedule = Some(stored.entitlement_schedule());
                &stored
            };
            let request = provider.authorization_for_challenge(&challenge)?;
            match client.exchange(HostMessage::Authorize(request), HANDSHAKE_TIMEOUT)? {
                HelperMessage::Authorized(result) => {
                    client.authorization_state = Some(result.state);
                    client.entitlement_schedule = entitlement_schedule;
                }
                HelperMessage::Error(error) => return Err(protocol_error(error)),
                _ => bail!("invalid_response: helper returned a non-authorization response"),
            }
        }
        client.capabilities = hello.capabilities;
        client.helper_version = hello.helper_version;
        Ok(client)
    }

    pub(super) fn public_access_status(&self) -> PublicAccessStatus {
        PublicAccessStatus {
            state: self.authorization_state.map(access_state_name),
            refresh_after_unix: self
                .entitlement_schedule
                .map(|schedule| schedule.refresh_after_unix),
            access_deadline_unix: self
                .entitlement_schedule
                .map(|schedule| schedule.access_deadline_unix),
            grace_deadline_unix: self
                .entitlement_schedule
                .map(|schedule| schedule.grace_deadline_unix),
        }
    }

    pub(super) fn exchange(
        &mut self,
        message: HostMessage,
        timeout: Duration,
    ) -> Result<HelperMessage> {
        let request_id = Uuid::new_v4();
        let sequence = self.sequence;
        let request = HostEnvelope {
            sequence,
            request_id,
            message,
        };
        if matches!(&request.message, HostMessage::SyncJournal(_))
            && serde_json::to_vec(&request)
                .context("invalid_request: encode journal request")?
                .len()
                > MAX_JOURNAL_SYNC_ENVELOPE_BYTES
        {
            bail!("invalid_request: journal request exceeds the Protocol V1 envelope bound");
        }
        let timed_out = Arc::new(AtomicBool::new(false));
        let (stop_tx, stop_rx) = mpsc::channel();
        let watchdog_child = Arc::clone(&self.child);
        let watchdog_timed_out = Arc::clone(&timed_out);
        let watchdog = thread::spawn(move || {
            if stop_rx.recv_timeout(timeout).is_err() {
                watchdog_timed_out.store(true, Ordering::Release);
                if let Ok(mut child) = watchdog_child.lock() {
                    kill_helper_process(&mut child);
                }
            }
        });
        let response = (|| -> Result<_> {
            let stdin = self
                .stdin
                .as_mut()
                .ok_or_else(|| anyhow!("helper_crashed: helper stdin is closed"))?;
            write_frame(stdin, &request).context("helper_crashed: write framed request")?;
            Ok(read_frame::<_, HelperEnvelope>(&mut self.stdout))
        })();
        let _ = stop_tx.send(());
        let _ = watchdog.join();
        if timed_out.load(Ordering::Acquire) {
            self.stdin.take();
            if let Ok(mut child) = self.child.lock() {
                kill_helper_process(&mut child);
                let _ = child.wait();
            }
            bail!("helper_timeout: Pro helper exceeded its exchange deadline");
        }
        let response = response?;
        let response = match response {
            Ok(response) => response,
            Err(ctx_pro_host_protocol::FrameError::UnsupportedVersion {
                received,
                supported,
            }) => bail!(
                "protocol_mismatch: helper frame version {received} does not equal {supported}"
            ),
            Err(error) => {
                let exited = self
                    .child
                    .lock()
                    .ok()
                    .and_then(|mut child| child.try_wait().ok().flatten());
                if let Some(status) = exited {
                    bail!("helper_crashed: Pro helper exited with {status}");
                }
                return Err(error).context("invalid_response: read framed helper response");
            }
        };
        if response.sequence != sequence || response.request_id != request_id {
            bail!(
                "invalid_response: helper response identity or sequence did not match the request"
            );
        }
        self.sequence = self.sequence.saturating_add(1);
        Ok(response.message)
    }
}

pub(super) struct PublicAccessStatus {
    pub(super) state: Option<String>,
    pub(super) refresh_after_unix: Option<i64>,
    pub(super) access_deadline_unix: Option<i64>,
    pub(super) grace_deadline_unix: Option<i64>,
}

pub(super) fn access_state_name(state: EntitlementAccessState) -> String {
    match state {
        EntitlementAccessState::Trial => "trial",
        EntitlementAccessState::Active => "active",
        EntitlementAccessState::CancelingPaid => "canceling_paid",
        EntitlementAccessState::OfflineGrace => "offline_grace",
        EntitlementAccessState::Locked => "locked",
    }
    .to_owned()
}

pub(super) fn authorization_required(
    required: &BTreeSet<Capability>,
    bind_status_identity: bool,
) -> bool {
    bind_status_identity
        || required.iter().any(|capability| {
            !matches!(
                *capability,
                Capability::Status | Capability::GraphKeyDeletion
            )
        })
}

impl Drop for ProClient {
    fn drop(&mut self) {
        self.stdin.take();
        if let Ok(mut child) = self.child.lock() {
            match child.try_wait() {
                Ok(Some(_)) => {}
                _ => {
                    kill_helper_process(&mut child);
                    let _ = child.wait();
                }
            }
        }
        self.stderr.finish();
    }
}

pub(super) fn kill_helper_process(child: &mut Child) {
    #[cfg(unix)]
    {
        let process_group = i32::try_from(child.id()).ok().and_then(i32::checked_neg);
        if let Some(process_group) = process_group {
            // The child is placed in a fresh process group before spawn. Killing
            // the group prevents descendants from retaining inherited IPC pipes.
            unsafe {
                libc::kill(process_group, libc::SIGKILL);
            }
        }
    }
    let _ = child.kill();
}

pub(super) struct StderrDrain {
    pub(super) bytes: Arc<AtomicUsize>,
    pub(super) thread: Option<thread::JoinHandle<()>>,
}

impl StderrDrain {
    pub(super) fn start(mut stderr: ChildStderr) -> Self {
        let bytes = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&bytes);
        let thread = thread::spawn(move || {
            let mut buffer = [0_u8; 8192];
            loop {
                match stderr.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        let current = observed.load(Ordering::Relaxed);
                        observed.store(
                            current.saturating_add(read).min(STDERR_MAX_BYTES),
                            Ordering::Relaxed,
                        );
                    }
                }
            }
        });
        Self {
            bytes,
            thread: Some(thread),
        }
    }

    pub(super) fn finish(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = self.bytes.load(Ordering::Relaxed);
    }
}
