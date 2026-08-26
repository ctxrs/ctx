use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::{
    analytics::{
        McpErrorClassV1, McpErrorLayerV1, McpLifecycleCountsV1, McpResultMetadataV1,
        McpRuntimeObservationV1, McpStopReasonV1, OperationCompletedV1, Outcome, PublicEventV1,
        RuntimeObservationV1,
    },
    operation_descriptor::{McpOperation, ObservedMcpProductOperation},
};

pub const MCP_TELEMETRY_QUEUE_CAPACITY: usize = 64;
pub const MCP_TELEMETRY_BATCH_LIMIT: usize = 25;
pub const MCP_TELEMETRY_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpObservedTool {
    Product(ObservedMcpProductOperation),
    Unknown,
    Missing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpRequestObservation {
    Initialize,
    Ping,
    ToolsList,
    ToolCall(McpObservedTool),
    UnknownRequest,
    MissingRequest,
    InitializedNotification,
    UnknownNotification,
    InvalidJson,
    InvalidUtf8,
    LineTooLarge,
}

impl McpRequestObservation {
    fn operation(self) -> McpOperation {
        match self {
            Self::ToolCall(McpObservedTool::Product(operation)) => {
                McpOperation::tool_call(operation)
            }
            Self::ToolCall(McpObservedTool::Unknown) => McpOperation::unknown_tool(),
            Self::ToolCall(McpObservedTool::Missing) => McpOperation::missing_tool(),
            Self::UnknownRequest => McpOperation::unknown_request(),
            Self::Initialize
            | Self::Ping
            | Self::ToolsList
            | Self::MissingRequest
            | Self::InitializedNotification
            | Self::UnknownNotification
            | Self::InvalidJson
            | Self::InvalidUtf8
            | Self::LineTooLarge => McpOperation::missing_request(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct McpDeliveredResponse {
    pub error_class: Option<McpErrorClassV1>,
    pub tool_error: bool,
    pub result: McpResultMetadataV1,
}

type Dispatch = dyn Fn(&[PublicEventV1]) -> Result<(), ()> + Send + Sync;

pub struct McpObservation {
    sender: AsyncMcpSender,
    lifecycle: McpLifecycle,
}

impl McpObservation {
    /// Starts an already-authorized observation worker.
    ///
    /// The caller must resolve enablement before constructing this value. The
    /// callback owns every config, identity, endpoint, and network decision and
    /// may re-check opt-out before doing any of that work.
    pub fn start(
        dispatch: impl Fn(&[PublicEventV1]) -> Result<(), ()> + Send + Sync + 'static,
    ) -> Self {
        Self {
            sender: AsyncMcpSender::start(MCP_TELEMETRY_QUEUE_CAPACITY, Arc::new(dispatch)),
            lifecycle: McpLifecycle::new(),
        }
    }

    pub fn record_delivered(
        &mut self,
        descriptor: McpRequestObservation,
        response: Option<McpDeliveredResponse>,
        duration: Duration,
    ) {
        if let Some(event) = self
            .lifecycle
            .record_delivered(descriptor, response, duration)
        {
            self.sender.try_submit(event);
        }
    }

    pub fn record_response_failure(
        &mut self,
        descriptor: McpRequestObservation,
        duration: Duration,
        class: McpErrorClassV1,
    ) {
        self.record_response_failure_with_result(
            descriptor,
            duration,
            class,
            McpResultMetadataV1::default(),
        );
    }

    pub fn record_response_failure_with_result(
        &mut self,
        descriptor: McpRequestObservation,
        duration: Duration,
        class: McpErrorClassV1,
        result: McpResultMetadataV1,
    ) {
        self.lifecycle.count_descriptor(descriptor);
        if matches!(descriptor, McpRequestObservation::ToolCall(_)) {
            let operation = descriptor
                .operation()
                .with_error(McpErrorLayerV1::Response, class)
                .with_result(result);
            self.sender
                .try_submit(operation_event(operation, Outcome::Failure, duration));
        }
    }

    pub fn submit_post_flush_event(&self, event: PublicEventV1) {
        self.sender.try_submit(event);
    }

    pub fn stop(mut self, reason: McpStopReasonV1, outcome: Outcome, duration: Duration) {
        self.lifecycle.counts.telemetry_dropped = self.sender.dropped_count();
        self.sender.try_submit(PublicEventV1::RuntimeObservation(
            RuntimeObservationV1::mcp(
                McpRuntimeObservationV1::stopped(
                    self.lifecycle.initialized,
                    reason,
                    self.lifecycle.counts,
                ),
                outcome,
                duration,
            ),
        ));
        self.sender.shutdown(MCP_TELEMETRY_SHUTDOWN_TIMEOUT);
    }
}

struct McpLifecycle {
    started: Instant,
    initialized: bool,
    counts: McpLifecycleCountsV1,
}

impl McpLifecycle {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            initialized: false,
            counts: McpLifecycleCountsV1::default(),
        }
    }

    fn count_descriptor(&mut self, descriptor: McpRequestObservation) {
        match descriptor {
            McpRequestObservation::InitializedNotification => {
                self.counts.initialized_notifications =
                    self.counts.initialized_notifications.saturating_add(1);
            }
            McpRequestObservation::UnknownNotification => {
                self.counts.unknown_notifications =
                    self.counts.unknown_notifications.saturating_add(1);
            }
            McpRequestObservation::ToolCall(_) => {
                self.counts.requests = self.counts.requests.saturating_add(1);
                self.counts.tool_requests = self.counts.tool_requests.saturating_add(1);
            }
            McpRequestObservation::Ping => {
                self.counts.requests = self.counts.requests.saturating_add(1);
                self.counts.pings = self.counts.pings.saturating_add(1);
            }
            McpRequestObservation::ToolsList => {
                self.counts.requests = self.counts.requests.saturating_add(1);
                self.counts.tools_lists = self.counts.tools_lists.saturating_add(1);
            }
            McpRequestObservation::Initialize
            | McpRequestObservation::UnknownRequest
            | McpRequestObservation::MissingRequest
            | McpRequestObservation::InvalidJson
            | McpRequestObservation::InvalidUtf8
            | McpRequestObservation::LineTooLarge => {
                self.counts.requests = self.counts.requests.saturating_add(1);
            }
        }
    }

    fn record_delivered(
        &mut self,
        descriptor: McpRequestObservation,
        response: Option<McpDeliveredResponse>,
        duration: Duration,
    ) -> Option<PublicEventV1> {
        self.count_descriptor(descriptor);
        if let Some(response) = response {
            if let Some(class) = response.error_class {
                if matches!(
                    descriptor,
                    McpRequestObservation::InitializedNotification
                        | McpRequestObservation::UnknownNotification
                ) {
                    self.counts.requests = self.counts.requests.saturating_add(1);
                }
                self.counts.malformed_requests = self.counts.malformed_requests.saturating_add(1);
                let layer = if matches!(
                    descriptor,
                    McpRequestObservation::InvalidJson
                        | McpRequestObservation::InvalidUtf8
                        | McpRequestObservation::LineTooLarge
                ) {
                    McpErrorLayerV1::Input
                } else {
                    McpErrorLayerV1::JsonRpc
                };
                return Some(operation_event(
                    descriptor
                        .operation()
                        .with_error(layer, class)
                        .with_result(response.result),
                    Outcome::Failure,
                    duration,
                ));
            }
        }
        if matches!(
            descriptor,
            McpRequestObservation::InitializedNotification
                | McpRequestObservation::UnknownNotification
        ) {
            return None;
        }
        match descriptor {
            McpRequestObservation::Initialize if !self.initialized => {
                self.initialized = true;
                Some(PublicEventV1::RuntimeObservation(
                    RuntimeObservationV1::mcp(
                        McpRuntimeObservationV1::initialized(self.counts),
                        Outcome::Success,
                        self.started.elapsed(),
                    ),
                ))
            }
            McpRequestObservation::ToolCall(_) => {
                let response = response?;
                if response.tool_error {
                    self.counts.tool_failures = self.counts.tool_failures.saturating_add(1);
                }
                let mut operation = descriptor.operation().with_result(response.result);
                let outcome = if response.tool_error {
                    operation =
                        operation.with_error(McpErrorLayerV1::Tool, McpErrorClassV1::ToolFailure);
                    Outcome::Failure
                } else {
                    Outcome::Success
                };
                Some(operation_event(operation, outcome, duration))
            }
            McpRequestObservation::Ping
            | McpRequestObservation::ToolsList
            | McpRequestObservation::Initialize
            | McpRequestObservation::UnknownRequest
            | McpRequestObservation::MissingRequest
            | McpRequestObservation::InvalidJson
            | McpRequestObservation::InvalidUtf8
            | McpRequestObservation::LineTooLarge
            | McpRequestObservation::InitializedNotification
            | McpRequestObservation::UnknownNotification => None,
        }
    }
}

fn operation_event(operation: McpOperation, outcome: Outcome, duration: Duration) -> PublicEventV1 {
    PublicEventV1::OperationCompleted(OperationCompletedV1::for_mcp(operation, outcome, duration))
}

enum SenderMessage {
    Event(PublicEventV1),
}

struct AsyncMcpSender {
    tx: Option<SyncSender<SenderMessage>>,
    dropped: Arc<AtomicU64>,
    worker: Option<JoinHandle<()>>,
}

impl AsyncMcpSender {
    fn start(capacity: usize, dispatch: Arc<Dispatch>) -> Self {
        let (tx, rx) = mpsc::sync_channel(capacity);
        let dropped = Arc::new(AtomicU64::new(0));
        let worker = thread::Builder::new()
            .name("ctx-mcp-telemetry".to_owned())
            .spawn(move || sender_loop(&rx, &dispatch))
            .ok();
        Self {
            tx: worker.as_ref().map(|_| tx),
            dropped,
            worker,
        }
    }

    fn try_submit(&self, event: PublicEventV1) {
        let Some(tx) = &self.tx else {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        };
        if matches!(
            tx.try_send(SenderMessage::Event(event)),
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_))
        ) {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn dropped_count(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    fn shutdown(&mut self, timeout: Duration) {
        self.tx.take();
        let Some(worker) = self.worker.take() else {
            return;
        };
        let deadline = Instant::now() + timeout;
        while !worker.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        if worker.is_finished() {
            let _ = worker.join();
        }
    }
}

fn sender_loop(rx: &Receiver<SenderMessage>, dispatch: &Arc<Dispatch>) {
    loop {
        let first = match rx.recv() {
            Ok(message) => message,
            Err(_) => return,
        };
        let mut events = Vec::with_capacity(MCP_TELEMETRY_BATCH_LIMIT);
        let SenderMessage::Event(first) = first;
        events.push(first);
        while events.len() < MCP_TELEMETRY_BATCH_LIMIT {
            match rx.try_recv() {
                Ok(SenderMessage::Event(event)) => events.push(event),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
        let _ = dispatch(&events);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Condvar, Mutex};

    use serde_json::{json, Map};

    use super::*;
    use crate::{
        analytics::RuntimeObservationKindV1,
        operation_descriptor::{ObservedMcpProductOperation, OperationDescriptor},
    };

    fn test_event() -> PublicEventV1 {
        operation_event(
            McpOperation::tool_call(ObservedMcpProductOperation::Status),
            Outcome::Success,
            Duration::ZERO,
        )
    }

    #[test]
    fn resource_limits_match_the_public_contract() {
        assert_eq!(MCP_TELEMETRY_QUEUE_CAPACITY, 64);
        assert_eq!(MCP_TELEMETRY_BATCH_LIMIT, 25);
        assert_eq!(MCP_TELEMETRY_SHUTDOWN_TIMEOUT, Duration::from_secs(2));
    }

    #[test]
    fn housekeeping_is_coalesced_into_content_free_lifecycle_counts() {
        let mut lifecycle = McpLifecycle::new();
        let delivered = McpDeliveredResponse::default();
        assert!(matches!(
            lifecycle.record_delivered(
                McpRequestObservation::Initialize,
                Some(delivered),
                Duration::ZERO
            ),
            Some(PublicEventV1::RuntimeObservation(_))
        ));
        assert!(lifecycle
            .record_delivered(McpRequestObservation::Ping, Some(delivered), Duration::ZERO)
            .is_none());
        assert!(lifecycle
            .record_delivered(
                McpRequestObservation::ToolsList,
                Some(delivered),
                Duration::ZERO
            )
            .is_none());
        assert!(lifecycle
            .record_delivered(
                McpRequestObservation::InitializedNotification,
                None,
                Duration::ZERO
            )
            .is_none());
        assert!(lifecycle
            .record_delivered(
                McpRequestObservation::UnknownNotification,
                None,
                Duration::ZERO
            )
            .is_none());

        let stopped = RuntimeObservationV1::mcp(
            McpRuntimeObservationV1::stopped(
                lifecycle.initialized,
                McpStopReasonV1::Eof,
                lifecycle.counts,
            ),
            Outcome::Success,
            Duration::ZERO,
        );
        let RuntimeObservationKindV1::Mcp(observation) = stopped.kind else {
            panic!("expected MCP lifecycle observation");
        };
        let mut properties = Map::new();
        observation.insert_properties(&mut properties);
        assert_eq!(properties["ping_count_bucket"], "1");
        assert_eq!(properties["tools_list_count_bucket"], "1");
        assert_eq!(properties["initialized_notification_count_bucket"], "1");
        assert_eq!(properties["unknown_notification_count_bucket"], "1");
        assert!(properties
            .values()
            .all(|value| value.is_boolean() || value.is_string()));
    }

    #[test]
    fn malformed_request_observation_cannot_retain_raw_error_content() {
        let mut lifecycle = McpLifecycle::new();
        let event = lifecycle
            .record_delivered(
                McpRequestObservation::InvalidJson,
                Some(McpDeliveredResponse {
                    error_class: Some(McpErrorClassV1::InvalidJson),
                    ..McpDeliveredResponse::default()
                }),
                Duration::ZERO,
            )
            .unwrap();
        let PublicEventV1::OperationCompleted(event) = event else {
            panic!("expected operation event");
        };
        let OperationDescriptor::Mcp(operation) = event.descriptor else {
            panic!("expected MCP operation");
        };
        let mut properties = Map::new();
        operation.insert_properties(&mut properties);
        assert_eq!(properties["error_class"], "invalid_json");
        assert_eq!(
            serde_json::to_value(properties).unwrap(),
            json!({
                "error_class": "invalid_json",
                "error_layer": "input",
                "method": "missing",
                "tool": "missing"
            })
        );
    }

    #[test]
    fn sender_batches_never_exceed_twenty_five_events() {
        let batch_sizes = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&batch_sizes);
        let mut sender = AsyncMcpSender::start(
            MCP_TELEMETRY_QUEUE_CAPACITY,
            Arc::new(move |events| {
                observed.lock().unwrap().push(events.len());
                Ok(())
            }),
        );
        for _ in 0..MCP_TELEMETRY_QUEUE_CAPACITY {
            sender.try_submit(test_event());
        }
        sender.shutdown(MCP_TELEMETRY_SHUTDOWN_TIMEOUT);

        let batch_sizes = batch_sizes.lock().unwrap();
        assert_eq!(
            batch_sizes.iter().sum::<usize>(),
            MCP_TELEMETRY_QUEUE_CAPACITY
        );
        assert!(batch_sizes
            .iter()
            .all(|size| *size <= MCP_TELEMETRY_BATCH_LIMIT));
        assert_eq!(sender.dropped_count(), 0);
    }

    #[test]
    fn queue_pressure_and_dispatch_failure_remain_best_effort() {
        let gate = Arc::new((Mutex::new((false, false)), Condvar::new()));
        let worker_gate = Arc::clone(&gate);
        let mut sender = AsyncMcpSender::start(
            1,
            Arc::new(move |_| {
                let (lock, wake) = &*worker_gate;
                let mut state = lock.lock().unwrap();
                state.0 = true;
                wake.notify_all();
                while !state.1 {
                    state = wake.wait(state).unwrap();
                }
                Err(())
            }),
        );
        sender.try_submit(test_event());
        {
            let (lock, wake) = &*gate;
            let mut state = lock.lock().unwrap();
            while !state.0 {
                state = wake.wait(state).unwrap();
            }
        }
        sender.try_submit(test_event());
        sender.try_submit(test_event());
        assert_eq!(sender.dropped_count(), 1);
        {
            let (lock, wake) = &*gate;
            lock.lock().unwrap().1 = true;
            wake.notify_all();
        }
        sender.shutdown(Duration::from_secs(1));
        assert_eq!(sender.dropped_count(), 1);
    }
}
