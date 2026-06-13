use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const CRP_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrpCommandEnvelope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub v: Option<u32>,
    #[serde(flatten)]
    pub command: CrpCommand,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
#[allow(clippy::enum_variant_names)]
pub enum CrpCommand {
    #[serde(rename = "session.open")]
    SessionOpen {
        session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        config: Option<CrpSessionConfig>,
    },
    #[serde(rename = "session.prompt")]
    SessionPrompt {
        session_id: Option<String>,
        turn_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        items: Option<Vec<Value>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_effort: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<PathBuf>,
    },
    #[serde(rename = "session.compact")]
    SessionCompact {
        session_id: Option<String>,
        turn_id: Option<String>,
    },
    #[serde(rename = "session.undo")]
    SessionUndo {
        session_id: Option<String>,
        turn_id: Option<String>,
    },
    #[serde(rename = "session.review")]
    SessionReview {
        session_id: Option<String>,
        turn_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instructions: Option<String>,
    },
    #[serde(rename = "session.authenticate")]
    SessionAuthenticate {
        session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        method_id: Option<String>,
    },
    #[serde(rename = "session.set_model")]
    SessionSetModel {
        session_id: Option<String>,
        model_id: Option<String>,
    },
    #[serde(rename = "session.status")]
    SessionStatus { session_id: Option<String> },
    #[serde(rename = "session.cancel")]
    SessionCancel {
        session_id: Option<String>,
        turn_id: Option<String>,
    },
    #[serde(rename = "models.list")]
    ModelsList {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        config: Option<CrpSessionConfig>,
    },
    #[serde(rename = "tool.result")]
    ToolResult {
        session_id: Option<String>,
        turn_id: Option<String>,
        tool_call_id: String,
        status: CrpToolStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CrpSessionConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawn_cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openai_base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_trace_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub personality: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_servers: Option<HashMap<String, CrpMcpServerConfig>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CrpMcpServerConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_vars: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_headers: Option<HashMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_http_headers: Option<HashMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled_tools: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_tools: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_timeout_sec: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CrpModelInfo {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CrpCommandInfo {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub argument_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrpEventEnvelope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub v: Option<u32>,
    pub seq: u64,
    pub channel: CrpChannel,
    #[serde(flatten)]
    pub event: CrpEvent,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CrpChannel {
    Control,
    Data,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CrpEvent {
    #[serde(rename = "session.opened")]
    SessionOpened {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        supports_session_status: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        commands: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        slash_commands: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        models: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        current_model_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agents: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_style: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        available_output_styles: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        skills: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plugins: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tools: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        permission_mode: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mcp_servers: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account: Option<Box<Value>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fast_mode_state: Option<String>,
    },
    #[serde(rename = "turn.started")]
    TurnStarted { session_id: String, turn_id: String },
    #[serde(rename = "message.delta")]
    MessageDelta {
        session_id: String,
        turn_id: String,
        message_id: String,
        delta: String,
    },
    #[serde(rename = "message.final")]
    MessageFinal {
        session_id: String,
        turn_id: String,
        message_id: String,
        content: String,
    },
    #[serde(rename = "reasoning.summary")]
    ReasoningSummary {
        session_id: String,
        turn_id: String,
        #[serde(default)]
        summary_index: i64,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        item_id: Option<String>,
    },
    #[serde(rename = "reasoning.trace")]
    ReasoningTrace {
        session_id: String,
        turn_id: String,
        chunk: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encoding: Option<String>,
        #[serde(default)]
        summary_index: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        item_id: Option<String>,
    },
    #[serde(rename = "reasoning.trace.final")]
    ReasoningTraceFinal {
        session_id: String,
        turn_id: String,
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encoding: Option<String>,
        #[serde(default)]
        summary_index: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        item_id: Option<String>,
    },
    #[serde(rename = "turn.context_window.updated")]
    TurnContextWindowUpdated {
        session_id: String,
        turn_id: String,
        context_window: Value,
    },
    #[serde(rename = "turn.completed")]
    TurnCompleted {
        session_id: String,
        turn_id: String,
        status: CrpTurnStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context_window: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<CrpTurnError>,
    },
    #[serde(rename = "tool.started")]
    ToolStarted {
        session_id: String,
        turn_id: String,
        tool_call_id: String,
        tool_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_preview: Option<Value>,
    },
    #[serde(rename = "tool.output_delta")]
    ToolOutputDelta {
        session_id: String,
        turn_id: String,
        tool_call_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stream: Option<CrpToolOutputStream>,
        chunk: String,
    },
    #[serde(rename = "tool.completed")]
    ToolCompleted {
        session_id: String,
        turn_id: String,
        tool_call_id: String,
        tool_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_label: Option<String>,
        status: CrpToolStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_preview: Option<Value>,
    },
    #[serde(rename = "models.list")]
    ModelsList {
        models: Vec<CrpModelInfo>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        current_model_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        catalog_source: Option<String>,
    },
    #[serde(rename = "session.gap")]
    SessionGap {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    #[serde(rename = "session.notice")]
    SessionNotice {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_id: Option<String>,
        code: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        severity: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        transient: Option<bool>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CrpTurnStatus {
    Success,
    Error,
    Canceled,
    Interrupted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CrpTurnError {
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CrpToolStatus {
    Success,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CrpToolOutputStream {
    Stdout,
    Stderr,
    Stdin,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_output_delta_event_round_trips() {
        let envelope = CrpEventEnvelope {
            v: Some(CRP_VERSION),
            seq: 7,
            channel: CrpChannel::Data,
            event: CrpEvent::ToolOutputDelta {
                session_id: "session-1".to_string(),
                turn_id: "turn-1".to_string(),
                tool_call_id: "tool-1".to_string(),
                stream: Some(CrpToolOutputStream::Stdout),
                chunk: "hello".to_string(),
            },
        };

        let raw = serde_json::to_string(&envelope).expect("serialize event");
        let decoded: CrpEventEnvelope = serde_json::from_str(&raw).expect("deserialize event");

        assert!(matches!(
            decoded.event,
            CrpEvent::ToolOutputDelta {
                session_id,
                turn_id,
                tool_call_id,
                stream,
                chunk,
            } if session_id == "session-1"
                && turn_id == "turn-1"
                && tool_call_id == "tool-1"
                && stream == Some(CrpToolOutputStream::Stdout)
                && chunk == "hello"
        ));
    }

    #[test]
    fn session_authenticate_command_round_trips() {
        let envelope = CrpCommandEnvelope {
            v: Some(CRP_VERSION),
            command: CrpCommand::SessionAuthenticate {
                session_id: Some("session-1".to_string()),
                method_id: Some("device_code".to_string()),
            },
        };

        let raw = serde_json::to_string(&envelope).expect("serialize command");
        let decoded: CrpCommandEnvelope = serde_json::from_str(&raw).expect("deserialize command");

        assert!(matches!(
            decoded.command,
            CrpCommand::SessionAuthenticate {
                session_id,
                method_id,
            } if session_id.as_deref() == Some("session-1")
                && method_id.as_deref() == Some("device_code")
        ));
    }
}
