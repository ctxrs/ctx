use std::{
    collections::{hash_map::Entry, HashMap},
    hash::Hash,
    time::Duration,
};

use super::{
    store, LocalUsageStorageAuthority, McpCompletionFacts, McpContextTarget, McpCorrelationFact,
    McpInvocation, Outcome, UsageControlRevision, UsageControlSnapshot,
};

pub(super) const CONTEXT_CORRELATION_MAX_RECORDS: usize = 1_024;

#[derive(Debug, Clone, Copy, Default)]
struct ContextRecordState {
    opened: bool,
}

/// Bounded, process-local search-to-open correlation.
///
/// The keys never cross the persistence boundary. This state is observational
/// only and is cleared on a local-usage control revision.
#[derive(Debug, Clone)]
struct EphemeralContextCorrelation<K> {
    records: HashMap<K, ContextRecordState>,
}

impl<K> Default for EphemeralContextCorrelation<K> {
    fn default() -> Self {
        Self {
            records: HashMap::new(),
        }
    }
}

impl<K: Eq + Hash> EphemeralContextCorrelation<K> {
    fn record_found(&mut self, key: K) -> bool {
        if self.records.len() >= CONTEXT_CORRELATION_MAX_RECORDS {
            return false;
        }
        if let Entry::Vacant(entry) = self.records.entry(key) {
            entry.insert(ContextRecordState::default());
            true
        } else {
            false
        }
    }

    fn record_opened(&mut self, key: &K) -> bool {
        let Some(state) = self.records.get_mut(key) else {
            return false;
        };
        if state.opened {
            false
        } else {
            state.opened = true;
            true
        }
    }
}

pub struct McpUsageRecorder {
    storage: LocalUsageStorageAuthority,
    control: Box<dyn FnMut() -> UsageControlSnapshot>,
    enabled: bool,
    control_revision: Option<UsageControlRevision>,
    context: EphemeralContextCorrelation<McpContextTarget>,
    #[cfg(any(test, feature = "test-support"))]
    trace: Option<std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>>,
}

impl McpUsageRecorder {
    pub fn start(
        storage: LocalUsageStorageAuthority,
        mut control: impl FnMut() -> UsageControlSnapshot + 'static,
    ) -> Self {
        let snapshot = control();
        Self {
            storage,
            control: Box::new(control),
            enabled: snapshot.enabled(),
            control_revision: snapshot.revision().cloned(),
            context: EphemeralContextCorrelation::default(),
            #[cfg(any(test, feature = "test-support"))]
            trace: None,
        }
    }

    pub fn record_delivered(
        &mut self,
        duration: Duration,
        complete: impl FnOnce() -> Option<(McpInvocation, McpCompletionFacts)>,
    ) {
        self.refresh_control();
        if !self.enabled {
            return;
        }
        let Some((invocation, facts)) = complete() else {
            return;
        };
        let operation = invocation.completed(&facts, duration);
        let mut next_context = self.context.clone();
        if operation.outcome == Outcome::Success {
            Self::apply_delivered_correlation(&mut next_context, &facts);
        }
        if store::record_authorized(&self.storage, operation).is_ok() {
            self.context = next_context;
            #[cfg(any(test, feature = "test-support"))]
            if let Some(trace) = &self.trace {
                trace.lock().unwrap().push("local_usage");
            }
        }
    }

    pub fn record_companion_blame_delivered(
        &mut self,
        failed: bool,
        delivered_output_bytes: usize,
        duration: Duration,
    ) {
        self.record_delivered(duration, || {
            Some((
                McpInvocation::blame(),
                McpCompletionFacts {
                    failed,
                    delivered_output_bytes,
                    ..McpCompletionFacts::default()
                },
            ))
        });
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn set_test_trace(&mut self, trace: std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>) {
        self.trace = Some(trace);
    }

    fn refresh_control(&mut self) {
        let snapshot = (self.control)();
        if !snapshot.available()
            || snapshot.revision().is_none()
            || self.control_revision.as_ref() != snapshot.revision()
            || self.enabled && !snapshot.enabled()
        {
            self.context = EphemeralContextCorrelation::default();
        }
        self.control_revision = snapshot.revision().cloned();
        self.enabled = snapshot.enabled();
    }

    fn apply_delivered_correlation(
        context: &mut EphemeralContextCorrelation<McpContextTarget>,
        facts: &McpCompletionFacts,
    ) -> bool {
        let mut opened = false;
        for fact in &facts.correlation {
            match fact {
                McpCorrelationFact::Found(target) => {
                    context.record_found(*target);
                }
                McpCorrelationFact::Opened(target) => {
                    opened |= context.record_opened(target);
                }
            }
        }
        opened
    }

    #[cfg(test)]
    pub(in crate::local_usage) fn correlate_delivered_for_test(
        &mut self,
        facts: &McpCompletionFacts,
    ) -> bool {
        Self::apply_delivered_correlation(&mut self.context, facts)
    }
}
