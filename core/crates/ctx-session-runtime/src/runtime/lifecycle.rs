use async_trait::async_trait;

use super::*;

impl<SchedulerCommand> SessionRuntime<SchedulerCommand> {
    async fn update_pin_state<F>(&self, session_id: SessionId, update: F) -> Option<bool>
    where
        F: FnOnce(&mut SessionPinState),
    {
        let mut pins = self.session_pins.lock().await;
        let entry = pins.entry(session_id).or_default();
        let was_pinned = entry.is_pinned();
        update(entry);
        let is_pinned = entry.is_pinned();
        if !is_pinned {
            pins.remove(&session_id);
        }
        (was_pinned != is_pinned).then_some(is_pinned)
    }

    pub async fn set_running(&self, session_id: SessionId, running: bool) -> Option<bool> {
        let mut set = self.running_sessions.lock().await;
        let changed = if running {
            set.insert(session_id)
        } else {
            set.remove(&session_id)
        };
        drop(set);
        if !changed {
            return None;
        }
        self.update_pin_state(session_id, |state| state.running = running)
            .await
    }

    pub async fn attach_session(&self, session_id: SessionId) -> Option<bool> {
        self.update_pin_state(session_id, |state| {
            state.attached_clients = state.attached_clients.saturating_add(1);
        })
        .await
    }

    pub async fn detach_session(&self, session_id: SessionId) -> Option<bool> {
        self.update_pin_state(session_id, |state| {
            state.attached_clients = state.attached_clients.saturating_sub(1);
        })
        .await
    }

    pub async fn clear_pin_state(&self, session_id: SessionId) -> bool {
        self.running_sessions.lock().await.remove(&session_id);
        self.session_pins
            .lock()
            .await
            .remove(&session_id)
            .is_some_and(SessionPinState::is_pinned)
    }

    pub async fn set_running_with_host<H>(&self, host: &H, session_id: SessionId, running: bool)
    where
        H: SessionLifecycleHost,
    {
        if let Some(pinned) = self.set_running(session_id, running).await {
            host.set_provider_session_pinned(session_id, pinned).await;
        }
    }

    pub async fn attach_session_with_host<H>(&self, host: &H, session_id: SessionId)
    where
        H: SessionLifecycleHost,
    {
        if let Some(pinned) = self.attach_session(session_id).await {
            host.set_provider_session_pinned(session_id, pinned).await;
        }
    }

    pub async fn detach_session_with_host<H>(&self, host: &H, session_id: SessionId)
    where
        H: SessionLifecycleHost,
    {
        if let Some(pinned) = self.detach_session(session_id).await {
            host.set_provider_session_pinned(session_id, pinned).await;
        }
    }

    pub async fn cleanup_session_with_host<H>(&self, host: &H, session_id: SessionId)
    where
        H: SessionLifecycleHost,
    {
        if self.clear_pin_state(session_id).await {
            host.set_provider_session_pinned(session_id, false).await;
        }
        host.remove_workspace_active_session(session_id).await;
        self.remove_session_state(session_id).await;
    }
}

#[async_trait]
pub trait SessionLifecycleHost: Send + Sync {
    async fn set_provider_session_pinned(&self, session_id: SessionId, pinned: bool);

    async fn remove_workspace_active_session(&self, session_id: SessionId);
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SessionPinState {
    pub running: bool,
    pub attached_clients: usize,
}

impl SessionPinState {
    pub fn is_pinned(self) -> bool {
        self.running || self.attached_clients > 0
    }
}
