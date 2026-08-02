use serde_json::{json, Map, Value};

use super::{count_bucket, CountBucket};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum McpMethodV1 {
    ToolsCall,
    Unknown,
    Missing,
}

impl McpMethodV1 {
    fn as_str(self) -> &'static str {
        match self {
            Self::ToolsCall => "tools_call",
            Self::Unknown => "unknown",
            Self::Missing => "missing",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum McpToolV1 {
    Status,
    Sources,
    Search,
    ShowSession,
    ShowEvent,
    Blame,
    ProStatus,
    Unknown,
    Missing,
}

impl McpToolV1 {
    pub(crate) fn from_name(name: Option<&str>) -> Self {
        match name {
            Some("status") => Self::Status,
            Some("sources") => Self::Sources,
            Some("search") => Self::Search,
            Some("show_session") => Self::ShowSession,
            Some("show_event") => Self::ShowEvent,
            Some("blame") => Self::Blame,
            Some("pro_status") => Self::ProStatus,
            Some(_) => Self::Unknown,
            None => Self::Missing,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Sources => "sources",
            Self::Search => "search",
            Self::ShowSession => "show_session",
            Self::ShowEvent => "show_event",
            Self::Blame => "blame",
            Self::ProStatus => "pro_status",
            Self::Unknown => "unknown",
            Self::Missing => "missing",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum McpErrorLayerV1 {
    Input,
    JsonRpc,
    Tool,
    Response,
}

impl McpErrorLayerV1 {
    fn as_str(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::JsonRpc => "json_rpc",
            Self::Tool => "tool",
            Self::Response => "response",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum McpErrorClassV1 {
    InvalidUtf8,
    LineTooLarge,
    InvalidJson,
    InvalidRequest,
    InvalidParams,
    ServerNotInitialized,
    MethodNotFound,
    MissingTool,
    UnknownTool,
    ToolFailure,
    ResponseSerialize,
    ResponseWrite,
    ResponseFlush,
}

impl McpErrorClassV1 {
    fn as_str(self) -> &'static str {
        match self {
            Self::InvalidUtf8 => "invalid_utf8",
            Self::LineTooLarge => "line_too_large",
            Self::InvalidJson => "invalid_json",
            Self::InvalidRequest => "invalid_request",
            Self::InvalidParams => "invalid_params",
            Self::ServerNotInitialized => "server_not_initialized",
            Self::MethodNotFound => "method_not_found",
            Self::MissingTool => "missing_tool",
            Self::UnknownTool => "unknown_tool",
            Self::ToolFailure => "tool_failure",
            Self::ResponseSerialize => "response_serialize",
            Self::ResponseWrite => "response_write",
            Self::ResponseFlush => "response_flush",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum McpResponseBoundV1 {
    WithinLimit,
    Replaced,
}

impl McpResponseBoundV1 {
    fn as_str(self) -> &'static str {
        match self {
            Self::WithinLimit => "within_limit",
            Self::Replaced => "replaced",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct McpResultMetadataV1 {
    pub(crate) result_count: Option<CountBucket>,
    pub(crate) zero_result: Option<bool>,
    pub(crate) result_truncated: Option<bool>,
    pub(crate) events_truncated: Option<bool>,
    pub(crate) response_bound: Option<McpResponseBoundV1>,
}

impl McpResultMetadataV1 {
    pub(crate) fn with_result_count(mut self, count: usize) -> Self {
        self.result_count = Some(count_bucket(count as u64));
        self.zero_result = Some(count == 0);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct McpOperationV1 {
    method: McpMethodV1,
    tool: McpToolV1,
    error_layer: Option<McpErrorLayerV1>,
    error_class: Option<McpErrorClassV1>,
    result: McpResultMetadataV1,
}

#[allow(dead_code, non_upper_case_globals)]
impl McpOperationV1 {
    // Compatibility constants keep shared serializer tests independent of MCP wiring details.
    pub(crate) const Initialize: Self = Self::missing();
    pub(crate) const ToolCall: Self = Self::tool_call(McpToolV1::Missing);
    pub(crate) const Shutdown: Self = Self::missing();

    pub(crate) const fn tool_call(tool: McpToolV1) -> Self {
        Self {
            method: McpMethodV1::ToolsCall,
            tool,
            error_layer: None,
            error_class: None,
            result: McpResultMetadataV1 {
                result_count: None,
                zero_result: None,
                result_truncated: None,
                events_truncated: None,
                response_bound: None,
            },
        }
    }

    pub(crate) const fn missing() -> Self {
        Self {
            method: McpMethodV1::Missing,
            tool: McpToolV1::Missing,
            error_layer: None,
            error_class: None,
            result: McpResultMetadataV1 {
                result_count: None,
                zero_result: None,
                result_truncated: None,
                events_truncated: None,
                response_bound: None,
            },
        }
    }

    pub(crate) fn unknown_method() -> Self {
        Self {
            method: McpMethodV1::Unknown,
            tool: McpToolV1::Unknown,
            ..Self::missing()
        }
    }

    pub(crate) fn with_error(mut self, layer: McpErrorLayerV1, class: McpErrorClassV1) -> Self {
        self.error_layer = Some(layer);
        self.error_class = Some(class);
        self
    }

    pub(crate) fn with_result(mut self, result: McpResultMetadataV1) -> Self {
        self.result = result;
        self
    }

    pub(crate) fn name(&self) -> &'static str {
        self.tool.as_str()
    }

    pub(crate) fn insert_properties(&self, properties: &mut Map<String, Value>) {
        properties.insert("method".to_owned(), json!(self.method.as_str()));
        properties.insert("tool".to_owned(), json!(self.tool.as_str()));
        insert_optional_enum(
            properties,
            "error_layer",
            self.error_layer.map(McpErrorLayerV1::as_str),
        );
        insert_optional_enum(
            properties,
            "error_class",
            self.error_class.map(McpErrorClassV1::as_str),
        );
        insert_optional_bucket(properties, "result_count_bucket", self.result.result_count);
        insert_optional_bool(properties, "zero_result", self.result.zero_result);
        insert_optional_bool(properties, "result_truncated", self.result.result_truncated);
        insert_optional_bool(properties, "events_truncated", self.result.events_truncated);
        insert_optional_enum(
            properties,
            "response_bound",
            self.result.response_bound.map(McpResponseBoundV1::as_str),
        );
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct McpLifecycleCountsV1 {
    pub(crate) requests: u64,
    pub(crate) tool_requests: u64,
    pub(crate) tool_failures: u64,
    pub(crate) malformed_requests: u64,
    pub(crate) pings: u64,
    pub(crate) tools_lists: u64,
    pub(crate) initialized_notifications: u64,
    pub(crate) unknown_notifications: u64,
    pub(crate) telemetry_dropped: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum McpStopReasonV1 {
    Eof,
    StdinReadError,
    ResponseSerializeError,
    StdoutWriteError,
    StdoutFlushError,
}

impl McpStopReasonV1 {
    fn as_str(self) -> &'static str {
        match self {
            Self::Eof => "eof",
            Self::StdinReadError => "stdin_read_error",
            Self::ResponseSerializeError => "response_serialize_error",
            Self::StdoutWriteError => "stdout_write_error",
            Self::StdoutFlushError => "stdout_flush_error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum McpRuntimePhaseV1 {
    Initialized,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct McpRuntimeObservationV1 {
    phase: McpRuntimePhaseV1,
    initialized: bool,
    stop_reason: Option<McpStopReasonV1>,
    counts: McpLifecycleCountsV1,
}

#[allow(dead_code, non_upper_case_globals)]
impl McpRuntimeObservationV1 {
    pub(crate) const Initialized: Self = Self::initialized(McpLifecycleCountsV1 {
        requests: 0,
        tool_requests: 0,
        tool_failures: 0,
        malformed_requests: 0,
        pings: 0,
        tools_lists: 0,
        initialized_notifications: 0,
        unknown_notifications: 0,
        telemetry_dropped: 0,
    });
    pub(crate) const Stopped: Self = Self::stopped(
        false,
        McpStopReasonV1::Eof,
        McpLifecycleCountsV1 {
            requests: 0,
            tool_requests: 0,
            tool_failures: 0,
            malformed_requests: 0,
            pings: 0,
            tools_lists: 0,
            initialized_notifications: 0,
            unknown_notifications: 0,
            telemetry_dropped: 0,
        },
    );

    pub(crate) const fn initialized(counts: McpLifecycleCountsV1) -> Self {
        Self {
            phase: McpRuntimePhaseV1::Initialized,
            initialized: true,
            stop_reason: None,
            counts,
        }
    }

    pub(crate) const fn stopped(
        initialized: bool,
        reason: McpStopReasonV1,
        counts: McpLifecycleCountsV1,
    ) -> Self {
        Self {
            phase: McpRuntimePhaseV1::Stopped,
            initialized,
            stop_reason: Some(reason),
            counts,
        }
    }

    pub(crate) fn name(&self) -> &'static str {
        match self.phase {
            McpRuntimePhaseV1::Initialized => "initialized",
            McpRuntimePhaseV1::Stopped => "stopped",
        }
    }

    pub(crate) fn insert_properties(&self, properties: &mut Map<String, Value>) {
        properties.insert("initialized".to_owned(), json!(self.initialized));
        insert_optional_enum(
            properties,
            "stop_reason",
            self.stop_reason.map(McpStopReasonV1::as_str),
        );
        insert_count(properties, "request_count_bucket", self.counts.requests);
        insert_count(
            properties,
            "tool_request_count_bucket",
            self.counts.tool_requests,
        );
        insert_count(
            properties,
            "tool_failure_count_bucket",
            self.counts.tool_failures,
        );
        insert_count(
            properties,
            "malformed_request_count_bucket",
            self.counts.malformed_requests,
        );
        insert_count(properties, "ping_count_bucket", self.counts.pings);
        insert_count(
            properties,
            "tools_list_count_bucket",
            self.counts.tools_lists,
        );
        insert_count(
            properties,
            "initialized_notification_count_bucket",
            self.counts.initialized_notifications,
        );
        insert_count(
            properties,
            "unknown_notification_count_bucket",
            self.counts.unknown_notifications,
        );
        insert_count(
            properties,
            "telemetry_dropped_count_bucket",
            self.counts.telemetry_dropped,
        );
    }
}

fn insert_optional_enum(
    properties: &mut Map<String, Value>,
    key: &'static str,
    value: Option<&'static str>,
) {
    if let Some(value) = value {
        properties.insert(key.to_owned(), json!(value));
    }
}

fn insert_optional_bool(
    properties: &mut Map<String, Value>,
    key: &'static str,
    value: Option<bool>,
) {
    if let Some(value) = value {
        properties.insert(key.to_owned(), json!(value));
    }
}

fn insert_optional_bucket(
    properties: &mut Map<String, Value>,
    key: &'static str,
    value: Option<CountBucket>,
) {
    if let Some(value) = value {
        properties.insert(key.to_owned(), json!(value.as_str()));
    }
}

fn insert_count(properties: &mut Map<String, Value>, key: &'static str, value: u64) {
    properties.insert(key.to_owned(), json!(count_bucket(value).as_str()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_and_properties_are_closed_and_content_free() {
        let operation = McpOperationV1::tool_call(McpToolV1::Search)
            .with_result(McpResultMetadataV1::default().with_result_count(0))
            .with_error(McpErrorLayerV1::Tool, McpErrorClassV1::ToolFailure);
        let mut properties = Map::new();
        operation.insert_properties(&mut properties);

        assert_eq!(operation.name(), "search");
        assert_eq!(properties["method"], "tools_call");
        assert_eq!(properties["tool"], "search");
        assert_eq!(properties["error_layer"], "tool");
        assert_eq!(properties["error_class"], "tool_failure");
        assert_eq!(properties["result_count_bucket"], "0");
        assert_eq!(properties["zero_result"], true);
        assert_eq!(
            properties.keys().map(String::as_str).collect::<Vec<_>>(),
            [
                "error_class",
                "error_layer",
                "method",
                "result_count_bucket",
                "tool",
                "zero_result",
            ]
        );
    }

    #[test]
    fn unknown_tool_names_collapse_without_preserving_input() {
        let sensitive = "SELECT secret FROM private_table WHERE token = 'raw'";
        assert_eq!(McpToolV1::from_name(Some(sensitive)), McpToolV1::Unknown);
        assert_eq!(McpToolV1::Unknown.as_str(), "unknown");
    }

    #[test]
    fn lifecycle_properties_are_only_booleans_enums_and_count_buckets() {
        let observation = McpRuntimeObservationV1::stopped(
            true,
            McpStopReasonV1::Eof,
            McpLifecycleCountsV1 {
                requests: 3,
                tool_requests: 2,
                unknown_notifications: 1,
                telemetry_dropped: 9,
                ..McpLifecycleCountsV1::default()
            },
        );
        let mut properties = Map::new();
        observation.insert_properties(&mut properties);

        assert_eq!(observation.name(), "stopped");
        assert_eq!(properties["initialized"], true);
        assert_eq!(properties["stop_reason"], "eof");
        assert_eq!(properties["request_count_bucket"], "2-5");
        assert_eq!(properties["telemetry_dropped_count_bucket"], "6-20");
        assert!(properties
            .values()
            .all(|value| value.is_boolean() || value.is_string()));
    }
}
