use super::*;

impl ConnectionManager {
    #[cfg(test)]
    pub(crate) fn set_local(
        &self,
        base_url: String,
        token: String,
        child: Child,
        systemd_scope: bool,
    ) {
        self.set_local_for_scope(
            DEFAULT_CONNECTION_SCOPE,
            base_url,
            token,
            child,
            systemd_scope,
        );
    }

    #[cfg(test)]
    pub(crate) fn set_local_for_scope(
        &self,
        scope: &str,
        base_url: String,
        token: String,
        child: Child,
        systemd_scope: bool,
    ) {
        self.set_local_for_scope_with_shutdown_token(
            scope,
            base_url,
            token,
            None,
            child,
            systemd_scope,
        );
    }

    pub(crate) fn set_local_for_scope_with_shutdown_token(
        &self,
        scope: &str,
        base_url: String,
        token: String,
        local_shutdown_token: Option<String>,
        child: Child,
        systemd_scope: bool,
    ) {
        self.set_local_with_intent(
            scope,
            base_url,
            token,
            local_shutdown_token,
            child,
            systemd_scope,
            ConnectionIntent::ExplicitLocal,
        );
    }

    #[cfg(test)]
    pub(crate) fn set_local_auto_bootstrap(
        &self,
        base_url: String,
        token: String,
        child: Child,
        systemd_scope: bool,
    ) -> bool {
        self.set_local_auto_bootstrap_for_scope(
            DEFAULT_CONNECTION_SCOPE,
            base_url,
            token,
            child,
            systemd_scope,
        )
    }

    #[cfg(test)]
    pub(crate) fn set_local_auto_bootstrap_for_scope(
        &self,
        scope: &str,
        base_url: String,
        token: String,
        child: Child,
        systemd_scope: bool,
    ) -> bool {
        self.set_local_auto_bootstrap_for_scope_with_shutdown_token(
            scope,
            base_url,
            token,
            None,
            child,
            systemd_scope,
        )
    }

    pub(crate) fn set_local_auto_bootstrap_for_scope_with_shutdown_token(
        &self,
        scope: &str,
        base_url: String,
        token: String,
        local_shutdown_token: Option<String>,
        child: Child,
        systemd_scope: bool,
    ) -> bool {
        self.set_local_with_auto_bootstrap_gate(
            scope,
            base_url,
            token,
            local_shutdown_token,
            child,
            systemd_scope,
        )
    }

    fn set_local_with_auto_bootstrap_gate(
        &self,
        scope: &str,
        base_url: String,
        token: String,
        local_shutdown_token: Option<String>,
        child: Child,
        systemd_scope: bool,
    ) -> bool {
        let daemon_pid = Some(child.id());
        let next = ActiveConnection::Local(LocalConnection {
            base_url,
            token,
            local_shutdown_token,
            daemon_pid,
            source: LocalConnectionSource::SpawnedByDesktop,
            ownership: LocalConnectionOwnership::OwnedChild {
                child,
                systemd_scope,
            },
            http_client: std::sync::OnceLock::new(),
        });
        {
            let mut guard = match self.0.lock() {
                Ok(g) => g,
                Err(_) => {
                    cleanup_active_connection(next);
                    return false;
                }
            };
            let scoped = guard.scope_mut(scope);
            let Some(intent) = scoped.as_ref().auto_local_install_intent() else {
                drop(guard);
                cleanup_active_connection(next);
                return false;
            };
            scoped.intent = intent;
            scoped.active = Some(next);
        }
        log_local_connection_established(LocalConnectionSource::SpawnedByDesktop, daemon_pid);
        true
    }

    fn set_local_with_intent(
        &self,
        scope: &str,
        base_url: String,
        token: String,
        local_shutdown_token: Option<String>,
        child: Child,
        systemd_scope: bool,
        intent: ConnectionIntent,
    ) {
        let daemon_pid = Some(child.id());
        let previous = {
            let mut guard = match self.0.lock() {
                Ok(g) => g,
                Err(_) => {
                    let _ = try_kill_child(child);
                    return;
                }
            };
            let scoped = guard.scope_mut(scope);
            scoped.intent = intent;
            let previous = scoped
                .active
                .replace(ActiveConnection::Local(LocalConnection {
                    base_url,
                    token,
                    local_shutdown_token,
                    daemon_pid,
                    source: LocalConnectionSource::SpawnedByDesktop,
                    ownership: LocalConnectionOwnership::OwnedChild {
                        child,
                        systemd_scope,
                    },
                    http_client: std::sync::OnceLock::new(),
                }));
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
        };
        if let Some(previous) = previous {
            cleanup_active_connection(previous);
        }
        log_local_connection_established(LocalConnectionSource::SpawnedByDesktop, daemon_pid);
    }

    #[cfg(test)]
    pub(crate) fn set_local_attached(
        &self,
        base_url: String,
        token: String,
        daemon_pid: Option<u32>,
        source: LocalConnectionSource,
    ) {
        self.set_local_attached_for_scope(
            DEFAULT_CONNECTION_SCOPE,
            base_url,
            token,
            daemon_pid,
            source,
        );
    }

    pub(crate) fn set_local_attached_for_scope(
        &self,
        scope: &str,
        base_url: String,
        token: String,
        daemon_pid: Option<u32>,
        source: LocalConnectionSource,
    ) {
        self.set_local_attached_with_intent(
            scope,
            base_url,
            token,
            daemon_pid,
            source,
            ConnectionIntent::ExplicitLocal,
        );
    }

    #[cfg(test)]
    pub(crate) fn set_local_attached_auto_bootstrap(
        &self,
        base_url: String,
        token: String,
        daemon_pid: Option<u32>,
        source: LocalConnectionSource,
    ) -> bool {
        self.set_local_attached_auto_bootstrap_for_scope(
            DEFAULT_CONNECTION_SCOPE,
            base_url,
            token,
            daemon_pid,
            source,
        )
    }

    pub(crate) fn set_local_attached_auto_bootstrap_for_scope(
        &self,
        scope: &str,
        base_url: String,
        token: String,
        daemon_pid: Option<u32>,
        source: LocalConnectionSource,
    ) -> bool {
        self.set_local_attached_with_auto_bootstrap_gate(scope, base_url, token, daemon_pid, source)
    }

    fn set_local_attached_with_auto_bootstrap_gate(
        &self,
        scope: &str,
        base_url: String,
        token: String,
        daemon_pid: Option<u32>,
        source: LocalConnectionSource,
    ) -> bool {
        let mut guard = match self.0.lock() {
            Ok(g) => g,
            Err(_) => return false,
        };
        let scoped = guard.scope_mut(scope);
        let Some(intent) = scoped.as_ref().auto_local_install_intent() else {
            return false;
        };
        scoped.intent = intent;
        scoped.active = Some(ActiveConnection::Local(LocalConnection {
            base_url,
            token,
            local_shutdown_token: None,
            daemon_pid,
            source,
            ownership: LocalConnectionOwnership::UnownedExternal,
            http_client: std::sync::OnceLock::new(),
        }));
        drop(guard);
        log_local_connection_established(source, daemon_pid);
        true
    }

    fn set_local_attached_with_intent(
        &self,
        scope: &str,
        base_url: String,
        token: String,
        daemon_pid: Option<u32>,
        source: LocalConnectionSource,
        intent: ConnectionIntent,
    ) {
        let previous = {
            let mut next = LocalConnection {
                base_url,
                token,
                local_shutdown_token: None,
                daemon_pid,
                source,
                ownership: LocalConnectionOwnership::UnownedExternal,
                http_client: std::sync::OnceLock::new(),
            };
            let mut guard = match self.0.lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            let scoped = guard.scope_mut(scope);
            let previous = scoped.active.take();
            let previous = match previous {
                Some(ActiveConnection::Local(c))
                    if should_preserve_local_handoff(
                        &next.base_url,
                        &next.token,
                        next.daemon_pid,
                        &c.base_url,
                        &c.token,
                        c.daemon_pid,
                    ) =>
                {
                    next.ownership = c.ownership;
                    next.source = c.source;
                    next.http_client = c.http_client;
                    next.local_shutdown_token = c.local_shutdown_token;
                    scoped.intent = intent;
                    None
                }
                other => other,
            };
            scoped.intent = intent;
            scoped.active = Some(ActiveConnection::Local(next));
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
        };
        if let Some(previous) = previous {
            cleanup_active_connection(previous);
        }
        log_local_connection_established(source, daemon_pid);
    }
}
