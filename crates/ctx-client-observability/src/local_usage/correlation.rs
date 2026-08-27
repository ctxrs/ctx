use std::time::Duration;

use super::{
    store, LocalUsageStorageAuthority, McpCompletionFacts, McpInvocation, UsageControlSnapshot,
};

pub struct McpUsageRecorder {
    storage: LocalUsageStorageAuthority,
    control: Box<dyn FnMut() -> UsageControlSnapshot>,
    enabled: bool,
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
        if store::record_authorized(&self.storage, operation).is_ok() {
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
        self.enabled = (self.control)().enabled();
    }
}
