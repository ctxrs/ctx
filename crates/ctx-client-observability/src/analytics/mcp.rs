use serde_json::{json, Map, Value};

use crate::operation_descriptor::McpOperation;

use super::{count_bucket, duration_bucket, CountBucket, DurationBucket, SearchTerminalFacts};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpMethodV1 {
    ToolsCall,
    Unknown,
    Missing,
}

impl McpMethodV1 {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::ToolsCall => "tools_call",
            Self::Unknown => "unknown",
            Self::Missing => "missing",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpErrorLayerV1 {
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
pub enum McpErrorClassV1 {
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
pub enum McpResponseBoundV1 {
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
pub struct McpResultMetadataV1 {
    pub result_count: Option<CountBucket>,
    pub zero_result: Option<bool>,
    pub result_truncated: Option<bool>,
    pub events_truncated: Option<bool>,
    pub response_bound: Option<McpResponseBoundV1>,
    pub search: Option<SearchTerminalFacts>,
}

impl McpResultMetadataV1 {
    pub fn with_result_count(mut self, count: usize) -> Self {
        self.result_count = Some(count_bucket(count as u64));
        self.zero_result = Some(count == 0);
        self
    }
}

impl McpOperation {
    pub fn name(&self) -> &'static str {
        self.tool_dimension()
    }

    pub fn insert_properties(&self, properties: &mut Map<String, Value>) {
        properties.insert("method".to_owned(), json!(self.method().as_str()));
        properties.insert("tool".to_owned(), json!(self.tool_dimension()));
        insert_optional_enum(
            properties,
            "error_layer",
            self.error_layer().map(McpErrorLayerV1::as_str),
        );
        insert_optional_enum(
            properties,
            "error_class",
            self.error_class().map(McpErrorClassV1::as_str),
        );
        let result = self.result();
        let search = result.search;
        insert_optional_bucket(properties, "result_count_bucket", result.result_count);
        insert_optional_bool(properties, "zero_result", result.zero_result);
        insert_optional_bool(properties, "result_truncated", result.result_truncated);
        insert_optional_bool(properties, "events_truncated", result.events_truncated);
        insert_optional_enum(
            properties,
            "response_bound",
            result.response_bound.map(McpResponseBoundV1::as_str),
        );
        if let Some(search) = search {
            insert_optional_duration(
                properties,
                "refresh_duration_bucket",
                search.refresh_duration.map(duration_bucket),
            );
            insert_optional_enum(
                properties,
                "search_refresh_status",
                search.refresh_status.map(|status| status.as_str()),
            );
            insert_optional_bucket(
                properties,
                "search_refresh_source_count_bucket",
                search.refresh_source_count.map(count_bucket),
            );
            insert_optional_duration(
                properties,
                "query_duration_bucket",
                search.query_duration.map(duration_bucket),
            );
            insert_optional_enum(
                properties,
                "search_backend_requested",
                search.backend_requested.map(|backend| backend.as_str()),
            );
            insert_optional_enum(
                properties,
                "search_backend_effective",
                search.backend_effective.map(|backend| backend.as_str()),
            );
            insert_optional_duration(
                properties,
                "search_output_duration_bucket",
                search.output_duration.map(duration_bucket),
            );
            insert_optional_bool(properties, "search_output_served", search.output_served);
            search.health.insert_properties(properties);
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct McpLifecycleCountsV1 {
    pub requests: u64,
    pub tool_requests: u64,
    pub tool_failures: u64,
    pub malformed_requests: u64,
    pub pings: u64,
    pub tools_lists: u64,
    pub initialized_notifications: u64,
    pub unknown_notifications: u64,
    pub telemetry_dropped: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpStopReasonV1 {
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
pub struct McpRuntimeObservationV1 {
    phase: McpRuntimePhaseV1,
    initialized: bool,
    stop_reason: Option<McpStopReasonV1>,
    counts: McpLifecycleCountsV1,
}

#[allow(dead_code, non_upper_case_globals)]
impl McpRuntimeObservationV1 {
    pub const Initialized: Self = Self::initialized(McpLifecycleCountsV1 {
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
    pub const Stopped: Self = Self::stopped(
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

    pub const fn initialized(counts: McpLifecycleCountsV1) -> Self {
        Self {
            phase: McpRuntimePhaseV1::Initialized,
            initialized: true,
            stop_reason: None,
            counts,
        }
    }

    pub const fn stopped(
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

    pub fn name(&self) -> &'static str {
        match self.phase {
            McpRuntimePhaseV1::Initialized => "initialized",
            McpRuntimePhaseV1::Stopped => "stopped",
        }
    }

    pub fn insert_properties(&self, properties: &mut Map<String, Value>) {
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

fn insert_optional_duration(
    properties: &mut Map<String, Value>,
    name: &'static str,
    value: Option<DurationBucket>,
) {
    if let Some(value) = value {
        properties.insert(name.to_owned(), json!(value.as_str()));
    }
}

fn insert_count(properties: &mut Map<String, Value>, key: &'static str, value: u64) {
    properties.insert(key.to_owned(), json!(count_bucket(value).as_str()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation_descriptor::ObservedMcpProductOperation;

    #[test]
    fn tool_names_and_properties_are_closed_and_content_free() {
        let operation = McpOperation::tool_call(ObservedMcpProductOperation::Search)
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
    fn search_serialization_preserves_authoritative_existing_generation_status() {
        let operation = McpOperation::tool_call(ObservedMcpProductOperation::Search).with_result(
            McpResultMetadataV1 {
                search: Some(SearchTerminalFacts {
                    refresh_status: Some(crate::analytics::RefreshStatus::ExistingGeneration),
                    ..SearchTerminalFacts::default()
                }),
                ..McpResultMetadataV1::default()
            },
        );
        let mut properties = Map::new();

        operation.insert_properties(&mut properties);

        assert_eq!(properties["search_refresh_status"], "existing_generation");
    }

    #[test]
    fn unknown_tool_observation_cannot_preserve_input() {
        assert_eq!(McpOperation::unknown_tool().name(), "unknown");
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
