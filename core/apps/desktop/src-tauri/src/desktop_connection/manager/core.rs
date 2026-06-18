use super::*;
use sha2::Digest;

fn derive_browser_query_secret(token: &str) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(b"ctx-desktop-browser-query-secret|");
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

impl ConnectionManager {
    pub(crate) fn info(&self) -> DesktopConnectionInfo {
        self.info_for_scope(DEFAULT_CONNECTION_SCOPE)
    }

    pub(crate) fn info_for_scope(&self, scope: &str) -> DesktopConnectionInfo {
        let guard = self.0.lock().ok();
        let Some(guard) = guard.as_ref() else {
            return DesktopConnectionInfo {
                kind: DesktopConnectionKind::None,
                base_url: None,
                intent: ConnectionIntent::ExplicitDisconnected.as_ipc(),
                local_auto_bootstrap_allowed: false,
                browser_query_secret: None,
                token: None,
                host: None,
                user: None,
                remote_port: None,
                remote_data_dir: None,
                remote_update_message: None,
                remote_update_state: None,
            };
        };
        let scoped = guard.scope(scope);
        let intent = scoped.intent.as_ipc();
        let local_auto_bootstrap_allowed = scoped.local_auto_bootstrap_allowed();
        match scoped.active {
            None => DesktopConnectionInfo {
                kind: DesktopConnectionKind::None,
                base_url: None,
                intent,
                local_auto_bootstrap_allowed,
                browser_query_secret: None,
                token: None,
                host: None,
                user: None,
                remote_port: None,
                remote_data_dir: None,
                remote_update_message: None,
                remote_update_state: None,
            },
            Some(ActiveConnection::Local(c)) => DesktopConnectionInfo {
                kind: DesktopConnectionKind::Local,
                base_url: Some(c.base_url.clone()),
                intent,
                local_auto_bootstrap_allowed,
                browser_query_secret: Some(derive_browser_query_secret(&c.token)),
                token: Some(c.token.clone()),
                host: None,
                user: None,
                remote_port: None,
                remote_data_dir: None,
                remote_update_message: None,
                remote_update_state: None,
            },
            Some(ActiveConnection::Ssh(c)) => DesktopConnectionInfo {
                kind: DesktopConnectionKind::Ssh,
                base_url: Some(c.base_url.clone()),
                intent,
                local_auto_bootstrap_allowed,
                browser_query_secret: c.token.as_deref().map(derive_browser_query_secret),
                token: c.token.clone(),
                host: Some(c.host.clone()),
                user: c.user.clone(),
                remote_port: Some(c.remote_port),
                remote_data_dir: c.remote_data_dir.clone(),
                remote_update_message: c
                    .remote_update_status
                    .as_ref()
                    .and_then(|status| status.message.clone()),
                remote_update_state: c.remote_update_status.as_ref().map(|status| status.state),
            },
        }
    }

    #[cfg(test)]
    pub(crate) fn local_shutdown_token_for_scope(&self, scope: &str) -> Option<String> {
        let guard = self.0.lock().ok()?;
        match guard.scope(scope).active? {
            ActiveConnection::Local(local) => local.local_shutdown_token.clone(),
            ActiveConnection::Ssh(_) => None,
        }
    }

    pub(crate) fn is_remote(&self) -> bool {
        self.is_remote_for_scope(DEFAULT_CONNECTION_SCOPE)
    }

    pub(crate) fn is_remote_for_scope(&self, scope: &str) -> bool {
        let guard = self.0.lock().ok();
        matches!(
            guard.as_ref().and_then(|g| g.scope(scope).active),
            Some(ActiveConnection::Ssh(_))
        )
    }

    pub(crate) fn daemon_target_key_for_scope(&self, scope: &str) -> Option<String> {
        let guard = self.0.lock().ok()?;
        match guard.scope(scope).active? {
            ActiveConnection::Local(_) => Some("local".to_string()),
            ActiveConnection::Ssh(c) => Some(format!(
                "ssh|{}|{}|{}|{}",
                c.host.trim(),
                c.user
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .unwrap_or(""),
                c.remote_port,
                c.remote_data_dir
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .unwrap_or("")
            )),
        }
    }

    #[cfg(test)]
    pub(crate) fn local_auto_bootstrap_allowed(&self) -> bool {
        self.local_auto_bootstrap_allowed_for_scope(DEFAULT_CONNECTION_SCOPE)
    }

    pub(crate) fn local_auto_bootstrap_allowed_for_scope(&self, scope: &str) -> bool {
        let guard = match self.0.lock() {
            Ok(g) => g,
            Err(_) => return false,
        };
        guard.scope(scope).local_auto_bootstrap_allowed()
    }

    #[cfg(test)]
    pub(crate) fn mark_explicit_local_intent_if_local(&self) {
        self.mark_explicit_local_intent_if_local_for_scope(DEFAULT_CONNECTION_SCOPE);
    }

    pub(crate) fn mark_explicit_local_intent_if_local_for_scope(&self, scope: &str) {
        let mut guard = match self.0.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let scoped = guard.scope_mut(scope);
        if matches!(scoped.active, Some(ActiveConnection::Local(_))) {
            scoped.intent = ConnectionIntent::ExplicitLocal;
        }
    }

    #[cfg(test)]
    pub(crate) fn mark_explicit_remote_intent(&self) {
        self.mark_explicit_remote_intent_for_scope(DEFAULT_CONNECTION_SCOPE);
    }

    pub(crate) fn mark_explicit_remote_intent_for_scope(&self, scope: &str) {
        let mut guard = match self.0.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        guard.scope_mut(scope).intent = ConnectionIntent::ExplicitRemote;
    }

    #[cfg(test)]
    pub(crate) fn disconnect(&self) {
        self.disconnect_for_scope(DEFAULT_CONNECTION_SCOPE);
    }

    pub(crate) fn disconnect_all(&self) {
        let active = {
            let mut guard = match self.0.lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            let mut active = Vec::new();
            for state in guard.scopes.values_mut() {
                state.intent = ConnectionIntent::ExplicitDisconnected;
                if let Some(connection) = state.active.take() {
                    active.push(connection);
                }
            }
            active
        };
        for connection in active {
            cleanup_active_connection(connection);
        }
    }

    pub(crate) fn disconnect_for_scope(&self, scope: &str) {
        let active = {
            let mut guard = match self.0.lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            let scoped = guard.scope_mut(scope);
            scoped.intent = ConnectionIntent::ExplicitDisconnected;
            let active = scoped.active.take();
            match active {
                Some(ActiveConnection::Local(mut local)) => {
                    if guard.transfer_local_ownership_if_shared(scope, &mut local) {
                        None
                    } else {
                        Some(ActiveConnection::Local(local))
                    }
                }
                other => other,
            }
        };
        if let Some(active) = active {
            cleanup_active_connection(active);
        }
    }

    #[cfg(test)]
    pub(crate) fn disconnect_for_local_restart(&self) -> Result<()> {
        self.disconnect_for_local_restart_for_scope(DEFAULT_CONNECTION_SCOPE)
    }

    pub(crate) fn disconnect_for_local_restart_for_scope(&self, scope: &str) -> Result<()> {
        let active = {
            let mut guard = self
                .0
                .lock()
                .map_err(|e| anyhow!("connection manager lock poisoned: {e}"))?;
            let active = guard.scope_mut(scope).active.take();
            match active {
                Some(ActiveConnection::Local(mut local)) => {
                    if guard.transfer_local_ownership_if_shared(scope, &mut local) {
                        None
                    } else {
                        Some(ActiveConnection::Local(local))
                    }
                }
                other => other,
            }
        };
        if let Some(active) = active {
            cleanup_active_connection_result_for_restart(active)?;
        }
        Ok(())
    }

    pub(crate) fn disconnect_owned_local_daemons_for_restart(&self) -> Result<()> {
        let active = {
            let mut guard = self
                .0
                .lock()
                .map_err(|e| anyhow!("connection manager lock poisoned: {e}"))?;
            let mut active = Vec::new();
            let mut removed_locals = Vec::new();

            for state in guard.scopes.values_mut() {
                let Some(ActiveConnection::Local(local)) = state.active.as_ref() else {
                    continue;
                };
                if !matches!(local.ownership, LocalConnectionOwnership::OwnedChild { .. }) {
                    continue;
                }
                let Some(ActiveConnection::Local(local)) = state.active.take() else {
                    continue;
                };
                removed_locals.push(LocalConnection {
                    base_url: local.base_url.clone(),
                    token: local.token.clone(),
                    local_shutdown_token: local.local_shutdown_token.clone(),
                    daemon_pid: local.daemon_pid,
                    source: local.source,
                    ownership: LocalConnectionOwnership::UnownedExternal,
                    http_client: std::sync::OnceLock::new(),
                });
                active.push(ActiveConnection::Local(local));
            }

            for state in guard.scopes.values_mut() {
                let Some(ActiveConnection::Local(local)) = state.active.as_ref() else {
                    continue;
                };
                if removed_locals
                    .iter()
                    .any(|removed| same_local_daemon_for_ownership(removed, local))
                {
                    state.active.take();
                }
            }

            active
        };
        for active in active {
            cleanup_active_connection_result_for_restart(active)?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn should_disconnect_for_local_restart(&self) -> bool {
        self.should_disconnect_for_local_restart_for_scope(DEFAULT_CONNECTION_SCOPE)
    }

    pub(crate) fn should_disconnect_for_local_restart_for_scope(&self, scope: &str) -> bool {
        let guard = match self.0.lock() {
            Ok(g) => g,
            Err(_) => return false,
        };
        matches!(
            guard.scope(scope).active,
            Some(ActiveConnection::Local(LocalConnection {
                ownership: LocalConnectionOwnership::OwnedChild { .. },
                ..
            }))
        )
    }
}
