#![allow(dead_code)]

use std::{
    fmt,
    sync::{Arc, Condvar, Mutex, MutexGuard},
    time::Instant,
};

use serde::{Deserialize, Serialize};

use super::query_service_contract::AuthenticatedSemanticQueryRequest;

#[derive(Debug, Clone, Default)]
pub(crate) struct SemanticQueryPriorityGate {
    inner: Arc<SemanticQueryPriorityInner>,
}

#[derive(Debug, Default)]
struct SemanticQueryPriorityInner {
    state: Mutex<SemanticQueryPriorityState>,
    changed: Condvar,
}

#[derive(Debug, Default)]
struct SemanticQueryPriorityState {
    waiting_foreground_queries: usize,
    active_foreground_queries: usize,
    document_batch_active: bool,
    generation: u64,
}

impl SemanticQueryPriorityInner {
    fn state(&self) -> MutexGuard<'_, SemanticQueryPriorityState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn wait<'a>(
        &self,
        state: MutexGuard<'a, SemanticQueryPriorityState>,
    ) -> MutexGuard<'a, SemanticQueryPriorityState> {
        self.changed
            .wait(state)
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn wait_until<'a>(
        &self,
        state: MutexGuard<'a, SemanticQueryPriorityState>,
        deadline: Instant,
    ) -> (MutexGuard<'a, SemanticQueryPriorityState>, bool) {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return (state, true);
        };
        let (state, timeout) = self
            .changed
            .wait_timeout(state, remaining)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        (state, timeout.timed_out())
    }
}

impl SemanticQueryPriorityGate {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    // Call only after the complete request has been read and authenticated.
    // The gate mutex is released before this returns, so embedder and store
    // locks can be acquired while the permit is held without lock inversion.
    pub(crate) fn begin_authenticated_query(
        &self,
        _request: &AuthenticatedSemanticQueryRequest,
        deadline: Option<Instant>,
    ) -> Result<SemanticForegroundQueryPermit, SemanticQueryPriorityError> {
        let mut state = self.inner.state();
        state.waiting_foreground_queries = state.waiting_foreground_queries.saturating_add(1);
        state.generation = state.generation.wrapping_add(1);
        self.inner.changed.notify_all();

        while state.document_batch_active {
            match deadline {
                Some(deadline) => {
                    let (next, timed_out) = self.inner.wait_until(state, deadline);
                    state = next;
                    if timed_out && state.document_batch_active {
                        state.waiting_foreground_queries =
                            state.waiting_foreground_queries.saturating_sub(1);
                        state.generation = state.generation.wrapping_add(1);
                        self.inner.changed.notify_all();
                        return Err(SemanticQueryPriorityError::DeadlineElapsed);
                    }
                }
                None => state = self.inner.wait(state),
            }
        }

        state.waiting_foreground_queries = state.waiting_foreground_queries.saturating_sub(1);
        state.active_foreground_queries = state.active_foreground_queries.saturating_add(1);
        state.generation = state.generation.wrapping_add(1);
        drop(state);
        Ok(SemanticForegroundQueryPermit {
            inner: self.inner.clone(),
        })
    }

    // Acquire one permit immediately before each background document batch.
    // A foreground waiter prevents the next batch from reserving the model.
    pub(crate) fn begin_document_batch(
        &self,
        deadline: Option<Instant>,
    ) -> Result<SemanticDocumentBatchPermit, SemanticQueryPriorityError> {
        let mut state = self.inner.state();
        while state.document_batch_active
            || state.waiting_foreground_queries > 0
            || state.active_foreground_queries > 0
        {
            match deadline {
                Some(deadline) => {
                    let (next, timed_out) = self.inner.wait_until(state, deadline);
                    state = next;
                    if timed_out
                        && (state.document_batch_active
                            || state.waiting_foreground_queries > 0
                            || state.active_foreground_queries > 0)
                    {
                        return Err(SemanticQueryPriorityError::DeadlineElapsed);
                    }
                }
                None => state = self.inner.wait(state),
            }
        }
        state.document_batch_active = true;
        state.generation = state.generation.wrapping_add(1);
        drop(state);
        Ok(SemanticDocumentBatchPermit {
            inner: self.inner.clone(),
        })
    }

    pub(crate) fn try_begin_document_batch(&self) -> Option<SemanticDocumentBatchPermit> {
        let mut state = self.inner.state();
        if state.document_batch_active
            || state.waiting_foreground_queries > 0
            || state.active_foreground_queries > 0
        {
            return None;
        }
        state.document_batch_active = true;
        state.generation = state.generation.wrapping_add(1);
        drop(state);
        Some(SemanticDocumentBatchPermit {
            inner: self.inner.clone(),
        })
    }

    pub(crate) fn snapshot(&self) -> SemanticQueryPrioritySnapshot {
        let state = self.inner.state();
        SemanticQueryPrioritySnapshot {
            waiting_foreground_queries: state.waiting_foreground_queries,
            active_foreground_queries: state.active_foreground_queries,
            document_batch_active: state.document_batch_active,
            generation: state.generation,
        }
    }
}

#[derive(Debug)]
#[must_use = "the foreground permit must cover semantic retrieval"]
pub(crate) struct SemanticForegroundQueryPermit {
    inner: Arc<SemanticQueryPriorityInner>,
}

impl Drop for SemanticForegroundQueryPermit {
    fn drop(&mut self) {
        let mut state = self.inner.state();
        state.active_foreground_queries = state.active_foreground_queries.saturating_sub(1);
        state.generation = state.generation.wrapping_add(1);
        self.inner.changed.notify_all();
    }
}

#[derive(Debug)]
#[must_use = "the batch permit must cover exactly one document embedding batch"]
pub(crate) struct SemanticDocumentBatchPermit {
    inner: Arc<SemanticQueryPriorityInner>,
}

impl Drop for SemanticDocumentBatchPermit {
    fn drop(&mut self) {
        let mut state = self.inner.state();
        state.document_batch_active = false;
        state.generation = state.generation.wrapping_add(1);
        self.inner.changed.notify_all();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SemanticQueryPrioritySnapshot {
    pub(crate) waiting_foreground_queries: usize,
    pub(crate) active_foreground_queries: usize,
    pub(crate) document_batch_active: bool,
    pub(crate) generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SemanticQueryPriorityError {
    DeadlineElapsed,
}

impl fmt::Display for SemanticQueryPriorityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DeadlineElapsed => formatter.write_str("semantic priority gate deadline elapsed"),
        }
    }
}

impl std::error::Error for SemanticQueryPriorityError {}
