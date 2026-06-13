use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug)]
pub enum AppServerInbound {
    Notification {
        method: String,
        params: Value,
    },
    Request {
        id: AppServerRequestId,
        method: String,
        params: Value,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppServerRequestId {
    Integer(i64),
    String(String),
}

impl AppServerRequestId {
    pub fn from_inbound_value(value: &Value) -> Option<Self> {
        if let Some(id) = value.as_i64() {
            return Some(Self::Integer(id));
        }
        value.as_str().map(|id| Self::String(id.to_string()))
    }

    pub fn json_value(&self) -> Value {
        match self {
            Self::Integer(id) => Value::from(*id),
            Self::String(id) => Value::from(id.clone()),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadRef {
    pub id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadLoadedListResponse {
    pub data: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadReadResponse {
    pub thread: ThreadStatusRef,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStatusRef {
    pub id: String,
    pub status: ThreadStatus,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ThreadStatus {
    NotLoaded,
    Idle,
    SystemError,
    Active {
        #[allow(dead_code)]
        #[serde(default)]
        active_flags: Vec<ThreadActiveFlag>,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ThreadActiveFlag {
    WaitingOnApproval,
    WaitingOnUserInput,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStartLikeResponse {
    pub thread: ThreadRef,
    pub model: String,
    pub cwd: String,
    #[serde(rename = "approvalPolicy")]
    pub _approval_policy: Value,
    #[serde(rename = "sandbox")]
    pub _sandbox: Value,
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnRef {
    pub id: String,
    pub status: String,
    pub error: Option<TurnError>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartedNotification {
    pub thread_id: String,
    pub turn: TurnRef,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnCompletedNotification {
    pub thread_id: String,
    pub turn: TurnRef,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartResponse {
    pub turn: TurnRef,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnError {
    pub message: String,
    pub codex_error_info: Option<Value>,
    pub additional_details: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadTokenUsageUpdatedNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub token_usage: ThreadTokenUsage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadTokenUsage {
    pub total: TokenUsageBreakdown,
    pub last: TokenUsageBreakdown,
    pub model_context_window: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsageBreakdown {
    pub total_tokens: u64,
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_output_tokens: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelListResponse {
    pub data: Vec<ModelInfo>,
    #[serde(rename = "nextCursor")]
    pub _next_cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    pub display_name: String,
    pub hidden: bool,
    pub supported_reasoning_efforts: Vec<ReasoningEffortOption>,
    pub default_reasoning_effort: String,
    pub is_default: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningEffortOption {
    pub reasoning_effort: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemLifecycleNotification {
    pub item: ThreadItem,
    pub thread_id: String,
    pub turn_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ThreadItem {
    #[serde(rename = "agentMessage")]
    AgentMessage {
        id: String,
        text: String,
        #[serde(rename = "phase")]
        #[serde(default)]
        _phase: Option<String>,
    },
    #[serde(rename = "reasoning")]
    Reasoning {
        id: String,
        summary: Vec<String>,
        content: Vec<String>,
    },
    #[serde(rename = "commandExecution")]
    CommandExecution {
        id: String,
        command: String,
        cwd: String,
        #[serde(rename = "processId")]
        #[serde(default)]
        _process_id: Option<String>,
        status: String,
        #[serde(rename = "commandActions")]
        #[serde(default)]
        command_actions: Vec<Value>,
        #[serde(rename = "aggregatedOutput")]
        #[serde(default)]
        aggregated_output: Option<String>,
        #[serde(rename = "exitCode")]
        #[serde(default)]
        exit_code: Option<i64>,
        #[serde(rename = "durationMs")]
        #[serde(default)]
        duration_ms: Option<u64>,
    },
    #[serde(rename = "fileChange")]
    FileChange {
        id: String,
        changes: Vec<FileUpdateChange>,
        status: String,
    },
    #[serde(rename = "mcpToolCall")]
    McpToolCall {
        id: String,
        server: String,
        tool: String,
        status: String,
        arguments: Value,
        #[serde(rename = "result")]
        #[serde(default)]
        result: Option<Value>,
        #[serde(rename = "error")]
        #[serde(default)]
        error: Option<McpToolCallError>,
        #[serde(rename = "durationMs")]
        #[serde(default)]
        duration_ms: Option<u64>,
    },
    #[serde(rename = "webSearch")]
    WebSearch {
        id: String,
        query: String,
        #[serde(rename = "action")]
        #[serde(default)]
        _action: Option<Value>,
    },
    #[serde(rename = "imageView")]
    ImageView { id: String, path: String },
    #[serde(rename = "enteredReviewMode")]
    EnteredReviewMode {
        #[serde(rename = "id")]
        _id: String,
        #[serde(rename = "review")]
        _review: String,
    },
    #[serde(rename = "exitedReviewMode")]
    ExitedReviewMode {
        #[serde(rename = "id")]
        _id: String,
        #[serde(rename = "review")]
        _review: String,
    },
    #[serde(rename = "contextCompaction")]
    ContextCompaction {
        #[serde(rename = "id")]
        _id: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileUpdateChange {
    pub path: String,
    pub kind: FileUpdateKind,
    pub diff: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FileUpdateKind {
    Legacy(String),
    Structured {
        #[serde(default)]
        move_path: Option<String>,
        #[serde(rename = "type")]
        r#type: String,
    },
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpToolCallError {
    pub message: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentMessageDeltaNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningSummaryTextDeltaNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
    pub summary_index: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningSummaryPartAddedNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub summary_index: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningTextDeltaNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
    pub content_index: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandExecutionOutputDeltaNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
}
