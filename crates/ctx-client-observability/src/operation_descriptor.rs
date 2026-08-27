use crate::analytics::{
    DaemonOperationV1, DocsTelemetry, DoctorTelemetry, ImportTelemetry, IndexTelemetry,
    IntegrationTelemetry, LocateTelemetry, McpErrorClassV1, McpErrorLayerV1, McpMethodV1,
    McpResultMetadataV1, SearchTelemetry, SetupTelemetry, ShowTelemetry, SourcesTelemetry,
    StatusTelemetry, UpgradeTelemetry,
};

/// Closed, transport-neutral identity and telemetry facts for one product operation.
///
/// CLI and MCP adapters classify their transport input once, then analytics and
/// aggregate-only local usage project this value without inspecting Clap values,
/// JSON-RPC payloads, or open-ended operation strings.
#[derive(Debug)]
pub enum OperationDescriptor {
    Cli(CliOperation),
    Mcp(McpOperation),
    Daemon(DaemonOperationV1),
}

#[derive(Debug)]
pub enum CliOperation {
    Setup(SetupTelemetry),
    Status(StatusTelemetry),
    Stats,
    Index(IndexTelemetry),
    Sources(SourcesTelemetry),
    Import(ImportTelemetry),
    ShowSession(ShowTelemetry),
    ShowEvent(ShowTelemetry),
    Locate(LocateTelemetry),
    Search(SearchTelemetry),
    Docs(DocsTelemetry),
    Integrations(IntegrationTelemetry),
    McpServe,
    DaemonRun,
    DaemonStatus,
    DaemonEnable,
    DaemonDisable,
    Upgrade {
        telemetry: UpgradeTelemetry,
        record_local_usage: bool,
    },
    Doctor(DoctorTelemetry),
}

impl CliOperation {
    pub const fn analytics_name(&self) -> &'static str {
        match self {
            Self::Setup(_) => "setup",
            Self::Status(_) => "status",
            Self::Stats => "stats",
            Self::Index(_) => "index",
            Self::Sources(_) => "sources",
            Self::Import(_) => "import",
            Self::ShowSession(_) | Self::ShowEvent(_) => "show",
            Self::Locate(_) => "locate",
            Self::Search(_) => "search",
            Self::Docs(_) => "docs",
            Self::Integrations(_) => "integration",
            Self::McpServe => "serve",
            Self::DaemonRun => "run",
            Self::DaemonStatus => "status",
            Self::DaemonEnable => "enable",
            Self::DaemonDisable => "disable",
            Self::Upgrade { .. } => "upgrade",
            Self::Doctor(_) => "doctor",
        }
    }

    pub const fn emits_client_analytics(&self) -> bool {
        matches!(
            self,
            Self::Setup(_)
                | Self::Status(_)
                | Self::Index(_)
                | Self::Sources(_)
                | Self::Import(_)
                | Self::ShowSession(_)
                | Self::ShowEvent(_)
                | Self::Locate(_)
                | Self::Search(_)
                | Self::Docs(_)
                | Self::Integrations(_)
                | Self::Upgrade { .. }
                | Self::Doctor(_)
        )
    }

    pub const fn local_usage_operation(&self) -> Option<LocalUsageOperation> {
        match self {
            Self::Setup(_) => Some(LocalUsageOperation::Setup),
            Self::Status(_) | Self::Stats => None,
            Self::Index(_) => Some(LocalUsageOperation::Index),
            Self::Sources(_) => Some(LocalUsageOperation::Sources),
            Self::Import(_) => Some(LocalUsageOperation::Import),
            Self::ShowSession(_) => Some(LocalUsageOperation::ShowSession),
            Self::ShowEvent(_) => Some(LocalUsageOperation::ShowEvent),
            Self::Locate(_) => Some(LocalUsageOperation::Locate),
            Self::Search(_) => Some(LocalUsageOperation::Search),
            Self::Docs(_) => Some(LocalUsageOperation::Docs),
            Self::Integrations(_) => Some(LocalUsageOperation::Integrations),
            Self::McpServe | Self::DaemonRun => None,
            Self::DaemonStatus => Some(LocalUsageOperation::DaemonStatus),
            Self::DaemonEnable => Some(LocalUsageOperation::DaemonEnable),
            Self::DaemonDisable => Some(LocalUsageOperation::DaemonDisable),
            Self::Upgrade {
                record_local_usage: true,
                ..
            } => Some(LocalUsageOperation::Upgrade),
            Self::Upgrade {
                record_local_usage: false,
                ..
            } => None,
            Self::Doctor(_) => Some(LocalUsageOperation::Doctor),
        }
    }
}

/// A content-free product fact supplied after protocol-owned MCP classification.
/// This deliberately has no raw tool-name parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservedMcpProductOperation {
    Status,
    Sources,
    Search,
    ShowSession,
    ShowEvent,
    QueryEvents,
}

impl ObservedMcpProductOperation {
    pub const fn analytics_name(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Sources => "sources",
            Self::Search => "search",
            Self::ShowSession => "show_session",
            Self::ShowEvent => "show_event",
            Self::QueryEvents => "query_events",
        }
    }

    pub const fn local_usage_operation(self) -> LocalUsageOperation {
        match self {
            Self::Status => LocalUsageOperation::Status,
            Self::Sources => LocalUsageOperation::Sources,
            Self::Search => LocalUsageOperation::Search,
            Self::ShowSession => LocalUsageOperation::ShowSession,
            Self::ShowEvent | Self::QueryEvents => LocalUsageOperation::ShowEvent,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum McpToolDimension {
    Product(ObservedMcpProductOperation),
    Unknown,
    Missing,
}

impl McpToolDimension {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Product(operation) => operation.analytics_name(),
            Self::Unknown => "unknown",
            Self::Missing => "missing",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpOperation {
    method: McpMethodV1,
    tool: McpToolDimension,
    error_layer: Option<McpErrorLayerV1>,
    error_class: Option<McpErrorClassV1>,
    result: McpResultMetadataV1,
}

impl McpOperation {
    const fn new(method: McpMethodV1, tool: McpToolDimension) -> Self {
        Self {
            method,
            tool,
            error_layer: None,
            error_class: None,
            result: McpResultMetadataV1 {
                result_count: None,
                zero_result: None,
                result_truncated: None,
                events_truncated: None,
                response_bound: None,
                search: None,
            },
        }
    }

    pub const fn tool_call(operation: ObservedMcpProductOperation) -> Self {
        Self::new(McpMethodV1::ToolsCall, McpToolDimension::Product(operation))
    }

    pub const fn unknown_tool() -> Self {
        Self::new(McpMethodV1::ToolsCall, McpToolDimension::Unknown)
    }

    pub const fn missing_tool() -> Self {
        Self::new(McpMethodV1::ToolsCall, McpToolDimension::Missing)
    }

    pub const fn unknown_request() -> Self {
        Self::new(McpMethodV1::Unknown, McpToolDimension::Missing)
    }

    pub const fn missing_request() -> Self {
        Self::new(McpMethodV1::Missing, McpToolDimension::Missing)
    }

    pub const fn method(self) -> McpMethodV1 {
        self.method
    }

    pub const fn product_operation(self) -> Option<ObservedMcpProductOperation> {
        match self.tool {
            McpToolDimension::Product(operation) => Some(operation),
            McpToolDimension::Unknown | McpToolDimension::Missing => None,
        }
    }

    pub(crate) const fn tool_dimension(self) -> &'static str {
        self.tool.as_str()
    }

    pub const fn error_layer(self) -> Option<McpErrorLayerV1> {
        self.error_layer
    }

    pub const fn error_class(self) -> Option<McpErrorClassV1> {
        self.error_class
    }

    pub const fn result(self) -> McpResultMetadataV1 {
        self.result
    }

    pub fn with_error(mut self, layer: McpErrorLayerV1, class: McpErrorClassV1) -> Self {
        self.error_layer = Some(layer);
        self.error_class = Some(class);
        self
    }

    pub fn with_result(mut self, result: McpResultMetadataV1) -> Self {
        self.result = result;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalUsageOperation {
    Setup,
    Status,
    Index,
    Sources,
    Import,
    ShowSession,
    ShowEvent,
    Locate,
    Search,
    Docs,
    Integrations,
    DaemonStatus,
    DaemonEnable,
    DaemonDisable,
    Upgrade,
    Doctor,
    Blame,
}

impl LocalUsageOperation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Status => "status",
            Self::Index => "index",
            Self::Sources => "sources",
            Self::Import => "import",
            Self::ShowSession => "show_session",
            Self::ShowEvent => "show_event",
            Self::Locate => "locate",
            Self::Search => "search",
            Self::Docs => "docs",
            Self::Integrations => "integrations",
            Self::DaemonStatus => "daemon_status",
            Self::DaemonEnable => "daemon_enable",
            Self::DaemonDisable => "daemon_disable",
            Self::Upgrade => "upgrade",
            Self::Doctor => "doctor",
            Self::Blame => "blame",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultObservationAction {
    Search,
    OpenSession,
    OpenEvent,
    Locate,
    Sources,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_product_facts_are_closed_and_have_no_raw_classifier() {
        assert_eq!(
            ObservedMcpProductOperation::Search.analytics_name(),
            "search"
        );
        assert_eq!(McpOperation::unknown_tool().tool_dimension(), "unknown");
        assert_eq!(McpOperation::missing_tool().tool_dimension(), "missing");
    }

    #[test]
    fn local_usage_projection_preserves_surface_specific_names() {
        assert_eq!(
            ObservedMcpProductOperation::QueryEvents.local_usage_operation(),
            LocalUsageOperation::ShowEvent
        );
        assert_eq!(LocalUsageOperation::Integrations.as_str(), "integrations");
    }
}
