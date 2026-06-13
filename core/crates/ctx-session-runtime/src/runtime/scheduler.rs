use std::future::Future;

use super::*;

impl<SchedulerCommand> SessionRuntime<SchedulerCommand> {
    pub async fn get_order_seq_state(
        &self,
        store: &Store,
        session_id: SessionId,
    ) -> Arc<Mutex<OrderSeqState>> {
        let mut map = self.order_seq_states.lock().await;
        if let Some(entry) = map.get_mut(&session_id) {
            entry.touch();
            return entry.value.clone();
        }
        let start_seq = store
            .get_session_last_event_seq(session_id)
            .await
            .unwrap_or(0);
        let state = Arc::new(Mutex::new(OrderSeqState::new(start_seq.saturating_add(1))));
        map.insert(session_id, TimedEntry::new(state.clone()));
        state
    }

    pub async fn scheduler_sender(
        &self,
        session_id: SessionId,
    ) -> Option<mpsc::Sender<SchedulerCommand>> {
        let mut map = self.schedulers.lock().await;
        map.get_mut(&session_id).map(|entry| {
            entry.touch();
            entry.value.clone()
        })
    }
}

impl<SchedulerCommand: Send + 'static> SessionRuntime<SchedulerCommand> {
    pub async fn ensure_scheduler<F, Fut>(
        &self,
        session: Session,
        spawn_worker: F,
    ) -> mpsc::Sender<SchedulerCommand>
    where
        F: FnOnce(Session, mpsc::Receiver<SchedulerCommand>) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.remember_session_meta(&session).await;
        let mut map = self.schedulers.lock().await;
        if let Some(entry) = map.get_mut(&session.id) {
            if !entry.value.is_closed() {
                entry.touch();
                return entry.value.clone();
            }
            map.remove(&session.id);
            tracing::info!(
                session_id = %session.id.0,
                "scheduler sender closed; recreating"
            );
        }
        let (tx, rx) = mpsc::channel(64);
        map.insert(session.id, TimedEntry::new(tx.clone()));
        drop(map);
        tokio::spawn(spawn_worker(session, rx));
        tx
    }
}
