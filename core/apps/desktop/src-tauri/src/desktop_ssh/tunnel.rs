use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TunnelOwnership {
    EphemeralProbe,
    CommittedConnection,
}

pub(super) struct TunnelHandle {
    local_port: u16,
    child: Option<Child>,
    stderr_log: std::sync::Arc<std::sync::Mutex<String>>,
    ownership: TunnelOwnership,
}

impl TunnelHandle {
    pub(super) fn start(
        host: &str,
        user: Option<&str>,
        local_port: u16,
        remote_port: u16,
    ) -> Result<Self> {
        let target = ssh_target(host, user);
        let mut cmd = new_ssh_command();
        cmd.arg("-N")
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg("ConnectTimeout=15")
            .arg("-o")
            .arg("ExitOnForwardFailure=yes")
            .arg("-o")
            .arg("ServerAliveInterval=30")
            .arg("-o")
            .arg("ServerAliveCountMax=3")
            .arg("-L")
            .arg(format!("{local_port}:127.0.0.1:{remote_port}"))
            .arg(target)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn().context("spawning ssh tunnel")?;
        let stderr_log = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        if let Some(stderr) = child.stderr.take() {
            let stderr_log = std::sync::Arc::clone(&stderr_log);
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stderr);
                let mut buf = String::new();
                loop {
                    buf.clear();
                    let read = match reader.read_line(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(_) => break,
                    };
                    if read == 0 {
                        break;
                    }
                    let mut log = match stderr_log.lock() {
                        Ok(guard) => guard,
                        Err(_) => return,
                    };
                    if log.len() + buf.len() > super::model::SSH_TUNNEL_LOG_BYTES {
                        let excess = (log.len() + buf.len()) - super::model::SSH_TUNNEL_LOG_BYTES;
                        log.drain(..excess);
                    }
                    log.push_str(&buf);
                }
            });
        }
        Ok(Self {
            local_port,
            child: Some(child),
            stderr_log,
            ownership: TunnelOwnership::EphemeralProbe,
        })
    }

    pub(super) fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.local_port)
    }

    pub(super) fn probe_health_quick_for_bootstrap(
        &mut self,
        base_url: &str,
        auth_token: Option<&str>,
    ) -> Result<()> {
        let stderr_log = std::sync::Arc::clone(&self.stderr_log);
        let child = self.child_mut()?;
        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 0..SSH_TUNNEL_BOOTSTRAP_HEALTH_RETRIES {
            match probe_daemon_health_with_auth(base_url, auth_token) {
                Ok(()) => return Ok(()),
                Err(err) => last_err = Some(err),
            }
            if let Ok(Some(status)) = child.try_wait() {
                let stderr = ssh_stderr_snippet(&self.stderr_log);
                if stderr.is_empty() {
                    return Err(anyhow!("ssh tunnel exited ({status})"));
                }
                return Err(anyhow!("ssh tunnel exited ({status}): {stderr}"));
            }
            if attempt + 1 < SSH_TUNNEL_BOOTSTRAP_HEALTH_RETRIES {
                let delay =
                    SSH_TUNNEL_BOOTSTRAP_HEALTH_BASE_DELAY_MS.saturating_mul((attempt + 1) as u64);
                std::thread::sleep(Duration::from_millis(delay));
            }
        }
        let err = last_err.unwrap_or_else(|| anyhow!("requesting /api/health failed"));
        let stderr = ssh_stderr_snippet(&stderr_log);
        let tunnel_state = match child.try_wait() {
            Ok(Some(status)) => format!("ssh tunnel exited ({status})"),
            Ok(None) => "ssh tunnel still running".to_string(),
            Err(e) => format!("ssh tunnel state unknown ({e})"),
        };
        let mut details = format!("{tunnel_state}; quick bootstrap health probe exhausted");
        if !stderr.is_empty() {
            details.push_str(&format!("; ssh stderr: {stderr}"));
        }
        Err(anyhow!("{err:#}; {details}"))
    }

    pub(super) fn probe_health_with_retry(
        &mut self,
        base_url: &str,
        auth_token: Option<&str>,
    ) -> Result<()> {
        let local_port = self.local_port;
        let stderr_log = std::sync::Arc::clone(&self.stderr_log);
        let child = self.child_mut()?;
        probe_daemon_health_with_retry(base_url, auth_token, local_port, child, &stderr_log)
    }

    pub(super) fn kill(mut self) -> Result<()> {
        if let Some(child) = self.child.take() {
            return try_kill_child(child);
        }
        Ok(())
    }

    pub(super) fn into_connection_child(mut self) -> Result<Child> {
        self.ownership = TunnelOwnership::CommittedConnection;
        self.child
            .take()
            .ok_or_else(|| anyhow!("ssh tunnel child missing on handoff"))
    }

    #[cfg(test)]
    pub(super) fn from_child_for_test(local_port: u16, child: Child) -> Self {
        Self {
            local_port,
            child: Some(child),
            stderr_log: std::sync::Arc::new(std::sync::Mutex::new(String::new())),
            ownership: TunnelOwnership::EphemeralProbe,
        }
    }

    fn child_mut(&mut self) -> Result<&mut Child> {
        self.child
            .as_mut()
            .ok_or_else(|| anyhow!("ssh tunnel child missing"))
    }
}

fn ssh_stderr_snippet(stderr_log: &std::sync::Arc<std::sync::Mutex<String>>) -> String {
    let guard = match stderr_log.lock() {
        Ok(guard) => guard,
        Err(_) => return String::new(),
    };
    let trimmed = guard.trim();
    if trimmed.is_empty() {
        String::new()
    } else {
        trimmed.to_string()
    }
}
