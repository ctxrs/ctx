use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use serde_json::Value;

use crate::{
    analytics::{
        self, McpErrorClassV1, McpErrorLayerV1, McpLifecycleCountsV1, McpOperationV1,
        McpResponseBoundV1, McpResultMetadataV1, McpRuntimeObservationV1, McpStopReasonV1,
        McpToolV1, OperationCompletedV1, Outcome, PublicEventV1, RuntimeObservationV1,
    },
    config::AppConfig,
};

const MCP_TELEMETRY_QUEUE_CAPACITY: usize = 64;
const MCP_TELEMETRY_BATCH_LIMIT: usize = 25;
const MCP_TELEMETRY_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RequestDescriptor {
    Initialize,
    Ping,
    ToolsList,
    ToolCall { tool: McpToolV1 },
    UnknownRequest,
    MissingRequest,
    InitializedNotification,
    UnknownNotification,
    InvalidJson,
    InvalidUtf8,
    LineTooLarge,
}

impl RequestDescriptor {
    pub(super) fn from_message(message: &Value) -> Self {
        let Some(object) = message.as_object() else {
            return Self::MissingRequest;
        };
        let method = object.get("method").and_then(Value::as_str);
        let has_id = object.contains_key("id");
        if !has_id {
            return if method == Some("notifications/initialized") {
                Self::InitializedNotification
            } else {
                Self::UnknownNotification
            };
        }
        match method {
            Some("initialize") => Self::Initialize,
            Some("ping") => Self::Ping,
            Some("tools/list") => Self::ToolsList,
            Some("tools/call") => Self::ToolCall {
                tool: McpToolV1::from_name(message.pointer("/params/name").and_then(Value::as_str)),
            },
            Some(_) => Self::UnknownRequest,
            None => Self::MissingRequest,
        }
    }

    fn operation(self) -> McpOperationV1 {
        match self {
            Self::ToolCall { tool, .. } => McpOperationV1::tool_call(tool),
            Self::UnknownRequest => McpOperationV1::unknown_method(),
            Self::Initialize
            | Self::Ping
            | Self::ToolsList
            | Self::MissingRequest
            | Self::InvalidJson
            | Self::InvalidUtf8
            | Self::LineTooLarge
            | Self::InitializedNotification
            | Self::UnknownNotification => McpOperationV1::missing(),
        }
    }
}

pub(super) struct McpHandled<T> {
    pub(super) value: T,
    pub(super) pro_event: Option<PublicEventV1>,
}

impl<T> McpHandled<T> {
    pub(super) fn plain(value: T) -> Self {
        Self {
            value,
            pro_event: None,
        }
    }

    pub(super) fn with_pro_event(value: T, pro_event: PublicEventV1) -> Self {
        Self {
            value,
            pro_event: Some(pro_event),
        }
    }
}

pub(super) struct McpTelemetry {
    state: McpTelemetryState,
}

enum McpTelemetryState {
    Disabled,
    Enabled {
        sender: AsyncMcpSender,
        lifecycle: McpLifecycle,
    },
}

impl McpTelemetry {
    pub(super) fn start(data_root: PathBuf) -> Self {
        let Ok(config) = AppConfig::load(&data_root) else {
            return Self {
                state: McpTelemetryState::Disabled,
            };
        };
        if !config.analytics.enabled {
            return Self {
                state: McpTelemetryState::Disabled,
            };
        }
        Self {
            state: McpTelemetryState::Enabled {
                sender: AsyncMcpSender::start(data_root),
                lifecycle: McpLifecycle::new(),
            },
        }
    }

    pub(super) fn record_delivered(
        &mut self,
        descriptor: RequestDescriptor,
        response: Option<&Value>,
        duration: Duration,
    ) {
        let McpTelemetryState::Enabled { sender, lifecycle } = &mut self.state else {
            return;
        };
        if let Some(event) = lifecycle.record_delivered(descriptor, response, duration) {
            sender.try_submit(event);
        }
    }

    pub(super) fn record_response_failure(
        &mut self,
        descriptor: RequestDescriptor,
        duration: Duration,
        class: McpErrorClassV1,
    ) {
        let McpTelemetryState::Enabled { sender, lifecycle } = &mut self.state else {
            return;
        };
        lifecycle.count_descriptor(descriptor);
        if matches!(descriptor, RequestDescriptor::ToolCall { .. }) {
            let operation = descriptor
                .operation()
                .with_error(McpErrorLayerV1::Response, class);
            sender.try_submit(operation_event(operation, Outcome::Failure, duration));
        }
    }

    pub(super) fn submit_pro_event(&self, event: PublicEventV1) {
        let McpTelemetryState::Enabled { sender, .. } = &self.state else {
            return;
        };
        sender.try_submit(event);
    }

    pub(super) fn stop(mut self, reason: McpStopReasonV1, outcome: Outcome, duration: Duration) {
        let McpTelemetryState::Enabled { sender, lifecycle } = &mut self.state else {
            return;
        };
        lifecycle.counts.telemetry_dropped = sender.dropped_count();
        sender.try_submit(PublicEventV1::RuntimeObservation(
            RuntimeObservationV1::mcp(
                McpRuntimeObservationV1::stopped(lifecycle.initialized, reason, lifecycle.counts),
                outcome,
                duration,
            ),
        ));
        sender.shutdown(MCP_TELEMETRY_SHUTDOWN_TIMEOUT);
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

    fn count_descriptor(&mut self, descriptor: RequestDescriptor) {
        match descriptor {
            RequestDescriptor::InitializedNotification => {
                self.counts.initialized_notifications =
                    self.counts.initialized_notifications.saturating_add(1);
            }
            RequestDescriptor::UnknownNotification => {
                self.counts.unknown_notifications =
                    self.counts.unknown_notifications.saturating_add(1);
            }
            RequestDescriptor::ToolCall { .. } => {
                self.counts.requests = self.counts.requests.saturating_add(1);
                self.counts.tool_requests = self.counts.tool_requests.saturating_add(1);
            }
            RequestDescriptor::Ping => {
                self.counts.requests = self.counts.requests.saturating_add(1);
                self.counts.pings = self.counts.pings.saturating_add(1);
            }
            RequestDescriptor::ToolsList => {
                self.counts.requests = self.counts.requests.saturating_add(1);
                self.counts.tools_lists = self.counts.tools_lists.saturating_add(1);
            }
            RequestDescriptor::Initialize
            | RequestDescriptor::UnknownRequest
            | RequestDescriptor::MissingRequest
            | RequestDescriptor::InvalidJson
            | RequestDescriptor::InvalidUtf8
            | RequestDescriptor::LineTooLarge => {
                self.counts.requests = self.counts.requests.saturating_add(1);
            }
        }
    }

    fn record_delivered(
        &mut self,
        descriptor: RequestDescriptor,
        response: Option<&Value>,
        duration: Duration,
    ) -> Option<PublicEventV1> {
        self.count_descriptor(descriptor);
        if let Some(error) = response.and_then(|response| response.get("error")) {
            if matches!(
                descriptor,
                RequestDescriptor::InitializedNotification | RequestDescriptor::UnknownNotification
            ) {
                self.counts.requests = self.counts.requests.saturating_add(1);
            }
            self.counts.malformed_requests = self.counts.malformed_requests.saturating_add(1);
            let layer = if matches!(
                descriptor,
                RequestDescriptor::InvalidJson
                    | RequestDescriptor::InvalidUtf8
                    | RequestDescriptor::LineTooLarge
            ) {
                McpErrorLayerV1::Input
            } else {
                McpErrorLayerV1::JsonRpc
            };
            let operation = descriptor
                .operation()
                .with_error(layer, json_rpc_error_class(descriptor, error));
            return Some(operation_event(operation, Outcome::Failure, duration));
        }
        if matches!(
            descriptor,
            RequestDescriptor::InitializedNotification | RequestDescriptor::UnknownNotification
        ) {
            return None;
        }

        match descriptor {
            RequestDescriptor::Initialize if !self.initialized => {
                self.initialized = true;
                Some(PublicEventV1::RuntimeObservation(
                    RuntimeObservationV1::mcp(
                        McpRuntimeObservationV1::initialized(self.counts),
                        Outcome::Success,
                        self.started.elapsed(),
                    ),
                ))
            }
            RequestDescriptor::Ping
            | RequestDescriptor::ToolsList
            | RequestDescriptor::Initialize => None,
            RequestDescriptor::ToolCall { tool } => {
                let response = response?;
                let tool_error =
                    response.pointer("/result/isError").and_then(Value::as_bool) == Some(true);
                if tool_error {
                    self.counts.tool_failures = self.counts.tool_failures.saturating_add(1);
                }
                let mut result = result_metadata(tool, response);
                if matches!(tool, McpToolV1::ShowSession | McpToolV1::ShowEvent)
                    && response.get("result").is_some()
                {
                    result.response_bound = Some(
                        if response
                            .pointer("/result/structuredContent/error_code")
                            .and_then(Value::as_str)
                            == Some("output_limit_exceeded")
                        {
                            McpResponseBoundV1::Replaced
                        } else {
                            McpResponseBoundV1::WithinLimit
                        },
                    );
                }
                let mut operation = McpOperationV1::tool_call(tool).with_result(result);
                let outcome = if tool_error {
                    operation =
                        operation.with_error(McpErrorLayerV1::Tool, McpErrorClassV1::ToolFailure);
                    Outcome::Failure
                } else {
                    Outcome::Success
                };
                Some(operation_event(operation, outcome, duration))
            }
            RequestDescriptor::UnknownRequest
            | RequestDescriptor::MissingRequest
            | RequestDescriptor::InvalidJson
            | RequestDescriptor::InvalidUtf8
            | RequestDescriptor::LineTooLarge => None,
            RequestDescriptor::InitializedNotification | RequestDescriptor::UnknownNotification => {
                None
            }
        }
    }
}

fn operation_event(
    operation: McpOperationV1,
    outcome: Outcome,
    duration: Duration,
) -> PublicEventV1 {
    PublicEventV1::OperationCompleted(OperationCompletedV1::for_mcp(operation, outcome, duration))
}

fn json_rpc_error_class(descriptor: RequestDescriptor, error: &Value) -> McpErrorClassV1 {
    if matches!(descriptor, RequestDescriptor::InvalidUtf8) {
        return McpErrorClassV1::InvalidUtf8;
    }
    if matches!(descriptor, RequestDescriptor::LineTooLarge) {
        return McpErrorClassV1::LineTooLarge;
    }
    if matches!(descriptor, RequestDescriptor::InvalidJson) {
        return McpErrorClassV1::InvalidJson;
    }
    if matches!(
        descriptor,
        RequestDescriptor::ToolCall {
            tool: McpToolV1::Missing,
            ..
        }
    ) {
        return McpErrorClassV1::MissingTool;
    }
    if matches!(
        descriptor,
        RequestDescriptor::ToolCall {
            tool: McpToolV1::Unknown,
            ..
        }
    ) {
        return McpErrorClassV1::UnknownTool;
    }
    match error.get("code").and_then(Value::as_i64) {
        Some(-32700) => McpErrorClassV1::InvalidJson,
        Some(-32600) => McpErrorClassV1::InvalidRequest,
        Some(-32602) => McpErrorClassV1::InvalidParams,
        Some(-32002) => McpErrorClassV1::ServerNotInitialized,
        Some(-32601) => McpErrorClassV1::MethodNotFound,
        _ => McpErrorClassV1::InvalidRequest,
    }
}

fn result_metadata(tool: McpToolV1, response: &Value) -> McpResultMetadataV1 {
    let Some(result) = response.pointer("/result/structuredContent") else {
        return McpResultMetadataV1::default();
    };
    let mut metadata = McpResultMetadataV1::default();
    match tool {
        McpToolV1::Sources => {
            if let Some(count) = result
                .get("sources")
                .and_then(Value::as_array)
                .map(Vec::len)
            {
                metadata = metadata.with_result_count(count);
            }
        }
        McpToolV1::Search => {
            if let Some(count) = result
                .get("results")
                .and_then(Value::as_array)
                .map(Vec::len)
            {
                metadata = metadata.with_result_count(count);
            }
            let truncated = result
                .pointer("/truncation/truncated")
                .and_then(Value::as_bool);
            let has_more = result
                .pointer("/pagination/has_more")
                .and_then(Value::as_bool);
            metadata.result_truncated = match (truncated, has_more) {
                (Some(truncated), Some(has_more)) => Some(truncated || has_more),
                (value @ Some(_), None) | (None, value @ Some(_)) => value,
                (None, None) => None,
            };
        }
        McpToolV1::ShowSession | McpToolV1::ShowEvent => {
            if let Some(count) = result.get("events").and_then(Value::as_array).map(Vec::len) {
                metadata = metadata.with_result_count(count);
            }
            metadata.events_truncated =
                result.pointer("/truncated/events").and_then(Value::as_bool);
        }
        McpToolV1::Blame | McpToolV1::ProStatus => {
            // MCP owns protocol and delivery telemetry. Pro-host telemetry owns
            // Pro product outcomes, result counts, and materialization facts.
        }
        McpToolV1::Status | McpToolV1::Unknown | McpToolV1::Missing => {}
    }
    metadata
}

type Dispatch = dyn Fn(&Path, &AppConfig, &[PublicEventV1]) -> Result<(), ()> + Send + Sync;
#[cfg(test)]
type SubmitObserver = dyn Fn(&PublicEventV1) + Send + Sync;

enum SenderMessage {
    Event(PublicEventV1),
}

struct AsyncMcpSender {
    tx: Option<SyncSender<SenderMessage>>,
    dropped: Arc<AtomicU64>,
    worker: Option<JoinHandle<()>>,
    #[cfg(test)]
    submit_observer: Option<Arc<SubmitObserver>>,
}

impl AsyncMcpSender {
    fn start(data_root: PathBuf) -> Self {
        Self::start_with(
            data_root,
            MCP_TELEMETRY_QUEUE_CAPACITY,
            Arc::new(|data_root, config, events| {
                analytics::send_batch(data_root, config, events);
                Ok(())
            }),
        )
    }

    fn start_with(data_root: PathBuf, capacity: usize, dispatch: Arc<Dispatch>) -> Self {
        let (tx, rx) = mpsc::sync_channel(capacity);
        let dropped = Arc::new(AtomicU64::new(0));
        let worker = thread::Builder::new()
            .name("ctx-mcp-telemetry".to_owned())
            .spawn(move || sender_loop(&data_root, &rx, &dispatch))
            .ok();
        Self {
            tx: worker.as_ref().map(|_| tx),
            dropped,
            worker,
            #[cfg(test)]
            submit_observer: None,
        }
    }

    fn try_submit(&self, event: PublicEventV1) {
        #[cfg(test)]
        if let Some(observer) = &self.submit_observer {
            observer(&event);
        }
        let Some(tx) = &self.tx else {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        };
        match tx.try_send(SenderMessage::Event(event)) {
            Ok(()) => {}
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
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

fn sender_loop(data_root: &Path, rx: &Receiver<SenderMessage>, dispatch: &Arc<Dispatch>) {
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
        let Ok(config) = AppConfig::load(data_root) else {
            continue;
        };
        if !config.analytics.enabled {
            continue;
        }
        let _ = dispatch(data_root, &config, &events);
    }
}

#[cfg(test)]
mod tests;
