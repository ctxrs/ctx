use std::sync::{Arc, OnceLock};

use ctx_core::models::Session;
use tokio::sync::mpsc;

use crate::daemon::scheduler::{
    session_worker, SchedulerCommand, SessionSchedulerWorkerHost, SessionSchedulerWorkerHostParts,
};
use crate::daemon::state::SessionRuntime;

pub(in crate::daemon) struct SessionSchedulerWorkerHostFactory {
    host_parts: SessionSchedulerWorkerHostParts,
    host: OnceLock<Arc<SessionSchedulerWorkerHost>>,
}

impl SessionSchedulerWorkerHostFactory {
    pub(in crate::daemon) fn new(host_parts: SessionSchedulerWorkerHostParts) -> Self {
        Self {
            host_parts,
            host: OnceLock::new(),
        }
    }

    pub(in crate::daemon) fn worker_host(&self) -> Arc<SessionSchedulerWorkerHost> {
        Arc::clone(
            self.host
                .get_or_init(|| Arc::new(SessionSchedulerWorkerHost::new(self.host_parts.clone()))),
        )
    }

    #[cfg(test)]
    pub(in crate::daemon) fn configure_tool_output_spool_for_test(
        &mut self,
        enabled: bool,
        dir: std::path::PathBuf,
    ) {
        assert!(
            self.host.get().is_none(),
            "scheduler worker host must not be initialized before test spool configuration"
        );
        self.host_parts.tool_output_spool_enabled = enabled;
        self.host_parts.tool_output_spool_dir = dir;
    }

    pub(in crate::daemon) async fn ensure_scheduler(
        &self,
        session_runtime: &Arc<SessionRuntime>,
        session: Session,
    ) -> mpsc::Sender<SchedulerCommand> {
        let host_weak = Arc::downgrade(&self.worker_host());
        session_runtime
            .ensure_scheduler(session, move |session, rx| {
                session_worker(host_weak, session, rx)
            })
            .await
    }
}
