use super::*;

impl ConnectionManager {
    #[cfg(test)]
    pub(crate) fn set_ssh(
        &self,
        base_url: String,
        token: Option<String>,
        tunnel: Child,
        host: String,
        user: Option<String>,
        remote_port: u16,
        remote_data_dir: Option<String>,
        runtime: SshRuntimeMetadata,
    ) {
        self.set_ssh_for_scope(
            DEFAULT_CONNECTION_SCOPE,
            base_url,
            token,
            tunnel,
            host,
            user,
            remote_port,
            remote_data_dir,
            runtime,
        );
    }

    #[cfg(test)]
    pub(crate) fn set_ssh_for_scope(
        &self,
        scope: &str,
        base_url: String,
        token: Option<String>,
        tunnel: Child,
        host: String,
        user: Option<String>,
        remote_port: u16,
        remote_data_dir: Option<String>,
        runtime: SshRuntimeMetadata,
    ) {
        let previous = self.replace_with_ssh_for_scope(
            scope,
            base_url,
            token,
            tunnel,
            host,
            user,
            remote_port,
            remote_data_dir,
            runtime,
        );
        if let Some(previous) = previous {
            cleanup_active_connection(previous);
        }
    }

    #[cfg(test)]
    pub(crate) async fn set_ssh_with_blocking_cleanup(
        &self,
        base_url: String,
        token: Option<String>,
        tunnel: Child,
        host: String,
        user: Option<String>,
        remote_port: u16,
        remote_data_dir: Option<String>,
        runtime: SshRuntimeMetadata,
    ) -> Result<(), String> {
        self.set_ssh_with_blocking_cleanup_for_scope(
            DEFAULT_CONNECTION_SCOPE,
            base_url,
            token,
            tunnel,
            host,
            user,
            remote_port,
            remote_data_dir,
            runtime,
        )
        .await
    }

    pub(crate) async fn set_ssh_with_blocking_cleanup_for_scope(
        &self,
        scope: &str,
        base_url: String,
        token: Option<String>,
        tunnel: Child,
        host: String,
        user: Option<String>,
        remote_port: u16,
        remote_data_dir: Option<String>,
        runtime: SshRuntimeMetadata,
    ) -> Result<(), String> {
        let log_host = host.clone();
        let log_user = user.clone();
        let previous = self.replace_with_ssh_for_scope(
            scope,
            base_url,
            token,
            tunnel,
            host,
            user,
            remote_port,
            remote_data_dir,
            runtime,
        );
        if let Some(previous) = previous {
            tauri::async_runtime::spawn_blocking(move || {
                cleanup_active_connection(previous);
            })
            .await
            .map_err(|e| {
                format!("failed to clean up previous desktop connection after ssh handoff: {e}")
            })?;
        }
        log_ssh_connection_established(&log_host, log_user.as_deref(), remote_port);
        Ok(())
    }

    pub(super) fn replace_with_ssh_for_scope(
        &self,
        scope: &str,
        base_url: String,
        token: Option<String>,
        tunnel: Child,
        host: String,
        user: Option<String>,
        remote_port: u16,
        remote_data_dir: Option<String>,
        runtime: SshRuntimeMetadata,
    ) -> Option<ActiveConnection> {
        let next = build_ssh_connection(
            base_url,
            token,
            tunnel,
            host,
            user,
            remote_port,
            remote_data_dir,
            runtime,
        );
        let mut guard = match self.0.lock() {
            Ok(g) => g,
            Err(_) => {
                if let ActiveConnection::Ssh(ssh) = next {
                    let _ = try_kill_child(ssh.tunnel);
                }
                return None;
            }
        };
        let scoped = guard.scope_mut(scope);
        scoped.intent = ConnectionIntent::ExplicitRemote;
        let previous = scoped.active.replace(next);
        match previous {
            Some(ActiveConnection::Local(mut local)) => {
                if guard.transfer_local_ownership_if_shared(scope, &mut local) {
                    None
                } else {
                    Some(ActiveConnection::Local(local))
                }
            }
            other => other,
        }
    }

    #[cfg(test)]
    pub(crate) fn replace_with_ssh(
        &self,
        base_url: String,
        token: Option<String>,
        tunnel: Child,
        host: String,
        user: Option<String>,
        remote_port: u16,
        remote_data_dir: Option<String>,
        runtime: SshRuntimeMetadata,
    ) -> Option<ActiveConnection> {
        self.replace_with_ssh_for_scope(
            DEFAULT_CONNECTION_SCOPE,
            base_url,
            token,
            tunnel,
            host,
            user,
            remote_port,
            remote_data_dir,
            runtime,
        )
    }

    #[cfg(test)]
    pub(crate) fn ssh_target(&self) -> Result<SshConnectionTarget> {
        self.ssh_target_for_scope(DEFAULT_CONNECTION_SCOPE)
    }

    pub(crate) fn ssh_target_for_scope(&self, scope: &str) -> Result<SshConnectionTarget> {
        let guard = self
            .0
            .lock()
            .map_err(|e| anyhow!("connection manager lock poisoned: {e}"))?;
        let scoped = guard.scope(scope);
        let Some(active) = scoped.active else {
            anyhow::bail!("not connected (open a workspace first)");
        };
        let ActiveConnection::Ssh(c) = active else {
            anyhow::bail!("current connection is not SSH");
        };
        Ok(SshConnectionTarget {
            host: c.host.clone(),
            user: c.user.clone(),
            remote_port: c.remote_port,
            remote_data_dir: c.remote_data_dir.clone(),
            runtime: c.runtime.clone(),
        })
    }

    #[cfg(test)]
    pub(crate) fn update_ssh_token(&self, token: String) -> Result<()> {
        self.update_ssh_token_for_scope(DEFAULT_CONNECTION_SCOPE, token)
    }

    #[cfg(test)]
    pub(crate) fn update_ssh_token_for_scope(&self, scope: &str, token: String) -> Result<()> {
        let mut guard = self
            .0
            .lock()
            .map_err(|e| anyhow!("connection manager lock poisoned: {e}"))?;
        let Some(active) = guard.scope_mut(scope).active.as_mut() else {
            anyhow::bail!("not connected (open a workspace first)");
        };
        let ActiveConnection::Ssh(c) = active else {
            anyhow::bail!("current connection is not SSH");
        };
        c.token = Some(token);
        Ok(())
    }

    pub(crate) fn update_ssh_auth_and_runtime_for_matching_target(
        &self,
        host: &str,
        user: Option<&str>,
        remote_port: u16,
        remote_data_dir: Option<&str>,
        token: String,
        runtime: SshRuntimeMetadata,
    ) -> Result<usize> {
        let mut guard = self
            .0
            .lock()
            .map_err(|e| anyhow!("connection manager lock poisoned: {e}"))?;
        let mut updated = 0;
        for state in guard.scopes.values_mut() {
            let Some(ActiveConnection::Ssh(c)) = state.active.as_mut() else {
                continue;
            };
            if !ssh_connection_matches_target(c, host, user, remote_port, remote_data_dir) {
                continue;
            }
            c.token = Some(token.clone());
            c.runtime = runtime.clone();
            updated += 1;
        }
        if updated == 0 {
            anyhow::bail!("current connection is not SSH");
        }
        Ok(updated)
    }

    #[cfg(test)]
    pub(crate) fn update_ssh_runtime(&self, runtime: SshRuntimeMetadata) -> Result<()> {
        self.update_ssh_runtime_for_scope(DEFAULT_CONNECTION_SCOPE, runtime)
    }

    pub(crate) fn update_ssh_runtime_for_scope(
        &self,
        scope: &str,
        runtime: SshRuntimeMetadata,
    ) -> Result<()> {
        let mut guard = self
            .0
            .lock()
            .map_err(|e| anyhow!("connection manager lock poisoned: {e}"))?;
        let Some(active) = guard.scope_mut(scope).active.as_mut() else {
            anyhow::bail!("not connected (open a workspace first)");
        };
        let ActiveConnection::Ssh(c) = active else {
            anyhow::bail!("current connection is not SSH");
        };
        c.runtime = runtime;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn set_ssh_remote_update_state(
        &self,
        state: DesktopRemoteDaemonUpdateState,
        message: Option<String>,
    ) -> Result<()> {
        self.set_ssh_remote_update_state_for_scope(DEFAULT_CONNECTION_SCOPE, state, message)
    }

    pub(crate) fn set_ssh_remote_update_state_for_scope(
        &self,
        scope: &str,
        state: DesktopRemoteDaemonUpdateState,
        message: Option<String>,
    ) -> Result<()> {
        let mut guard = self
            .0
            .lock()
            .map_err(|e| anyhow!("connection manager lock poisoned: {e}"))?;
        let Some(active) = guard.scope_mut(scope).active.as_mut() else {
            anyhow::bail!("not connected (open a workspace first)");
        };
        let ActiveConnection::Ssh(c) = active else {
            anyhow::bail!("current connection is not SSH");
        };
        c.remote_update_status = Some(SshRemoteUpdateStatus {
            state,
            message: message
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
        });
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn clear_ssh_remote_update_state(&self) -> Result<()> {
        self.clear_ssh_remote_update_state_for_scope(DEFAULT_CONNECTION_SCOPE)
    }

    #[cfg(test)]
    pub(crate) fn clear_ssh_remote_update_state_for_scope(&self, scope: &str) -> Result<()> {
        let mut guard = self
            .0
            .lock()
            .map_err(|e| anyhow!("connection manager lock poisoned: {e}"))?;
        let Some(active) = guard.scope_mut(scope).active.as_mut() else {
            anyhow::bail!("not connected (open a workspace first)");
        };
        let ActiveConnection::Ssh(c) = active else {
            anyhow::bail!("current connection is not SSH");
        };
        c.remote_update_status = None;
        Ok(())
    }

    pub(crate) fn set_ssh_remote_update_state_for_matching_target(
        &self,
        host: &str,
        user: Option<&str>,
        remote_port: u16,
        remote_data_dir: Option<&str>,
        state: DesktopRemoteDaemonUpdateState,
        message: Option<String>,
    ) -> Result<usize> {
        let mut guard = self
            .0
            .lock()
            .map_err(|e| anyhow!("connection manager lock poisoned: {e}"))?;
        let mut updated = 0;
        let normalized_message = message
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        for scoped in guard.scopes.values_mut() {
            let Some(ActiveConnection::Ssh(c)) = scoped.active.as_mut() else {
                continue;
            };
            if !ssh_connection_matches_target(c, host, user, remote_port, remote_data_dir) {
                continue;
            }
            c.remote_update_status = Some(SshRemoteUpdateStatus {
                state,
                message: normalized_message.clone(),
            });
            updated += 1;
        }
        if updated == 0 {
            anyhow::bail!("current connection is not SSH");
        }
        Ok(updated)
    }

    pub(crate) fn clear_ssh_remote_update_state_for_matching_target(
        &self,
        host: &str,
        user: Option<&str>,
        remote_port: u16,
        remote_data_dir: Option<&str>,
    ) -> Result<usize> {
        let mut guard = self
            .0
            .lock()
            .map_err(|e| anyhow!("connection manager lock poisoned: {e}"))?;
        let mut cleared = 0;
        for scoped in guard.scopes.values_mut() {
            let Some(ActiveConnection::Ssh(c)) = scoped.active.as_mut() else {
                continue;
            };
            if !ssh_connection_matches_target(c, host, user, remote_port, remote_data_dir) {
                continue;
            }
            c.remote_update_status = None;
            cleared += 1;
        }
        if cleared == 0 {
            anyhow::bail!("current connection is not SSH");
        }
        Ok(cleared)
    }
}

fn normalize_optional_target_part(value: Option<&str>) -> &str {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("")
}

fn ssh_connection_matches_target(
    connection: &SshConnection,
    host: &str,
    user: Option<&str>,
    remote_port: u16,
    remote_data_dir: Option<&str>,
) -> bool {
    connection.host.trim() == host.trim()
        && normalize_optional_target_part(connection.user.as_deref())
            == normalize_optional_target_part(user)
        && connection.remote_port == remote_port
        && normalize_optional_target_part(connection.remote_data_dir.as_deref())
            == normalize_optional_target_part(remote_data_dir)
}
