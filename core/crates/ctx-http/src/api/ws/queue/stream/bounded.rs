use super::super::Duration;
use crate::api::ws::{Instant, Mutex, Notify, VecDeque};

pub(crate) struct StreamQueueEntry<T> {
    enqueued_at: Instant,
    message: T,
}

impl<T> StreamQueueEntry<T> {
    pub(crate) fn into_parts(self) -> (Instant, T) {
        (self.enqueued_at, self.message)
    }
}

#[derive(Debug)]
pub(crate) enum StreamQueuePushError {
    QueueFull { len: usize, limit: usize },
    QueueStale { age_ms: u64, max_age_ms: u64 },
}

pub(crate) struct StreamQueue<T> {
    pending: Mutex<VecDeque<StreamQueueEntry<T>>>,
    notify: Notify,
    limit: usize,
    max_age: Duration,
}

impl<T> StreamQueue<T> {
    pub(crate) fn new(limit: usize, max_age: Duration) -> Self {
        Self {
            pending: Mutex::new(VecDeque::new()),
            notify: Notify::new(),
            limit,
            max_age,
        }
    }

    pub(crate) async fn push(&self, message: T) -> Result<(), StreamQueuePushError> {
        let now = Instant::now();
        let mut guard = self.pending.lock().await;
        let len = guard.len();
        if len >= self.limit {
            return Err(StreamQueuePushError::QueueFull {
                len,
                limit: self.limit,
            });
        }
        if let Some(front) = guard.front() {
            let age = now.duration_since(front.enqueued_at);
            if age >= self.max_age {
                return Err(StreamQueuePushError::QueueStale {
                    age_ms: age.as_millis() as u64,
                    max_age_ms: self.max_age.as_millis() as u64,
                });
            }
        }
        guard.push_back(StreamQueueEntry {
            enqueued_at: now,
            message,
        });
        self.notify.notify_one();
        Ok(())
    }

    pub(crate) async fn clear(&self) {
        let mut guard = self.pending.lock().await;
        guard.clear();
    }

    pub(crate) async fn pop(&self) -> Option<StreamQueueEntry<T>> {
        let mut guard = self.pending.lock().await;
        guard.pop_front()
    }

    pub(crate) async fn is_empty(&self) -> bool {
        self.pending.lock().await.is_empty()
    }

    pub(crate) fn notify(&self) -> &Notify {
        &self.notify
    }

    pub(crate) fn wake(&self) {
        self.notify.notify_one();
    }
}
