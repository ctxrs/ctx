use async_trait::async_trait;

use super::*;

impl<SchedulerCommand> SessionRuntime<SchedulerCommand> {
    pub async fn queue_task_delta_refresh_with_host<H>(&self, host: Arc<H>, task_id: TaskId)
    where
        H: SessionTaskDeltaRefreshHost,
    {
        self.queue_task_delta_refresh_with_debounce(
            host,
            task_id,
            Duration::from_millis(TASK_DELTA_REFRESH_DEBOUNCE_MS.max(1)),
        )
        .await;
    }

    pub(super) async fn queue_task_delta_refresh_with_debounce<H>(
        &self,
        host: Arc<H>,
        task_id: TaskId,
        debounce: Duration,
    ) where
        H: SessionTaskDeltaRefreshHost,
    {
        let should_spawn = {
            let mut map = self.active_task_refreshes.lock().await;
            if let Some(entry) = map.get_mut(&task_id) {
                entry.generation = entry.generation.wrapping_add(1);
                false
            } else {
                map.insert(task_id, ActiveTaskRefreshEntry { generation: 1 });
                true
            }
        };
        if should_spawn {
            let active_task_refreshes = Arc::clone(&self.active_task_refreshes);
            tokio::spawn(async move {
                run_task_delta_refresh_loop(active_task_refreshes, host, task_id, debounce).await;
            });
        }
    }
}

#[async_trait]
pub trait SessionTaskDeltaRefreshHost: Send + Sync + 'static {
    async fn emit_task_delta_refresh(&self, task_id: TaskId);
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ActiveTaskRefreshEntry {
    pub generation: u64,
}

async fn run_task_delta_refresh_loop<H>(
    active_task_refreshes: Arc<Mutex<HashMap<TaskId, ActiveTaskRefreshEntry>>>,
    host: Arc<H>,
    task_id: TaskId,
    debounce: Duration,
) where
    H: SessionTaskDeltaRefreshHost,
{
    let debounce = debounce.max(Duration::from_millis(1));
    loop {
        let generation = {
            let map = active_task_refreshes.lock().await;
            match map.get(&task_id) {
                Some(entry) => entry.generation,
                None => return,
            }
        };

        tokio::time::sleep(debounce).await;

        let current = {
            let map = active_task_refreshes.lock().await;
            match map.get(&task_id) {
                Some(entry) => entry.generation,
                None => return,
            }
        };
        if current != generation {
            continue;
        }

        host.emit_task_delta_refresh(task_id).await;

        let mut map = active_task_refreshes.lock().await;
        match map.get(&task_id) {
            Some(entry) if entry.generation == current => {
                map.remove(&task_id);
                return;
            }
            Some(_) => continue,
            None => return,
        }
    }
}
