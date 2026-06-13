use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use ctx_core::ids::SessionId;
use ctx_core::models::{Message, Session, SessionEvent};
use ctx_session_runtime::runtime::SessionRuntime;
use ctx_store::Store;
use ctx_update_service::UpdateDrainCoordinator;
use tokio::sync::{mpsc, Mutex};

use super::SessionTitleModelModeHandle;
use crate::daemon::scheduler::SessionSchedulerWorkerHost;
use crate::daemon::state::SessionStoreLookup;
#[cfg(test)]
use std::future::Future;

#[derive(Clone)]
pub(in crate::daemon) struct SessionMessageSchedulerSpawner {
    host: Weak<SessionSchedulerWorkerHost>,
}

impl SessionMessageSchedulerSpawner {
    pub(in crate::daemon) fn new(host: Weak<SessionSchedulerWorkerHost>) -> Self {
        Self { host }
    }

    pub(in crate::daemon) async fn ensure_scheduler(
        &self,
        runtime: &SessionRuntime<crate::daemon::scheduler::SchedulerCommand>,
        session: Session,
    ) -> mpsc::Sender<crate::daemon::scheduler::SchedulerCommand> {
        let host = self.host.clone();
        runtime
            .ensure_scheduler(session, move |session, rx| {
                crate::daemon::scheduler::session_worker(host, session, rx)
            })
            .await
    }
}

#[derive(Clone)]
pub struct SessionMessageCommandHandle {
    global_store: Store,
    session_stores: SessionStoreLookup,
    session_runtime: Arc<SessionRuntime<crate::daemon::scheduler::SchedulerCommand>>,
    update_drain: Arc<UpdateDrainCoordinator>,
    data_root: PathBuf,
    title_model_mode: SessionTitleModelModeHandle,
    scheduler_spawner: SessionMessageSchedulerSpawner,
}

impl SessionMessageCommandHandle {
    pub(in crate::daemon) fn new(
        global_store: Store,
        session_stores: SessionStoreLookup,
        session_runtime: Arc<SessionRuntime<crate::daemon::scheduler::SchedulerCommand>>,
        update_drain: Arc<UpdateDrainCoordinator>,
        data_root: PathBuf,
        title_model_mode: SessionTitleModelModeHandle,
        scheduler_spawner: SessionMessageSchedulerSpawner,
    ) -> Self {
        Self {
            global_store,
            session_stores,
            session_runtime,
            update_drain,
            data_root,
            title_model_mode,
            scheduler_spawner,
        }
    }

    pub(in crate::daemon) fn global_store(&self) -> &Store {
        &self.global_store
    }

    pub(in crate::daemon) fn data_root(&self) -> &Path {
        &self.data_root
    }

    pub(in crate::daemon) async fn existing_session_store_for_write(
        &self,
        session_id: SessionId,
    ) -> Result<Store, crate::daemon::SessionStoreAccessError> {
        self.session_stores
            .existing_session_store_for_write(session_id)
            .await
    }

    pub(in crate::daemon) async fn remember_session_meta(&self, session: &Session) {
        self.session_runtime.remember_session_meta(session).await;
    }

    pub(in crate::daemon) async fn session_order_seq_state(
        &self,
        store: &Store,
        session_id: SessionId,
    ) -> Arc<Mutex<ctx_session_tools::order_seq::OrderSeqState>> {
        self.session_runtime
            .get_order_seq_state(store, session_id)
            .await
    }

    pub(in crate::daemon) async fn is_session_running(&self, session_id: SessionId) -> bool {
        self.session_runtime.is_running(session_id).await
    }

    pub(in crate::daemon) async fn publish_event(&self, event: SessionEvent) {
        self.title_model_mode.publish_event(event).await;
    }

    pub(in crate::daemon) async fn post_message_update_drain_reason(&self) -> Option<String> {
        self.update_drain.snapshot().await.map(|drain| drain.reason)
    }

    pub(in crate::daemon) async fn scheduler_sender(
        &self,
        session_id: SessionId,
    ) -> Option<mpsc::Sender<crate::daemon::scheduler::SchedulerCommand>> {
        self.session_runtime.scheduler_sender(session_id).await
    }

    pub(in crate::daemon) async fn ensure_scheduler(
        &self,
        session: Session,
    ) -> mpsc::Sender<crate::daemon::scheduler::SchedulerCommand> {
        self.scheduler_spawner
            .ensure_scheduler(&self.session_runtime, session)
            .await
    }

    #[cfg(test)]
    pub(in crate::daemon) async fn ensure_scheduler_for_test<F, Fut>(
        &self,
        session: Session,
        spawn_worker: F,
    ) -> mpsc::Sender<crate::daemon::scheduler::SchedulerCommand>
    where
        F: FnOnce(Session, mpsc::Receiver<crate::daemon::scheduler::SchedulerCommand>) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.session_runtime
            .ensure_scheduler(session, spawn_worker)
            .await
    }

    #[cfg(test)]
    pub(in crate::daemon) async fn subscribe_session_event_head_for_test(
        &self,
        session_id: SessionId,
    ) -> tokio::sync::watch::Receiver<i64> {
        self.session_runtime
            .subscribe_session_event_head(session_id)
            .await
    }

    pub(in crate::daemon) async fn maybe_schedule_first_message_title_generation(
        &self,
        store: &Store,
        session: Session,
        message: &Message,
    ) {
        if let Ok(count) = store.count_user_messages_for_session(session.id).await {
            if count == 1 {
                let _ = self
                    .title_model_mode
                    .schedule_session_title_generation(session, message.content.clone(), false)
                    .await;
            }
        }
    }
}
