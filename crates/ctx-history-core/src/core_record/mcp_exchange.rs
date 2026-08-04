use serde::{Deserialize, Serialize};

use super::{
    validation::validate_text, CoreRecordError, CoreRecordResult,
    MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES,
};

/// Revision of the provider-neutral, content-policy-governed MCP exchange shape.
pub const CORE_MCP_EXCHANGE_REVISION: u32 = 1;

/// Maximum decoded UTF-8 size of a provider-native MCP call identifier.
pub const MAX_MCP_EXCHANGE_CALL_ID_BYTES: usize = 64 * 1024;

/// Event-local MCP invocation and/or terminal response content.
///
/// Providers preserve their native event granularity. A combined terminal such
/// as Codex may carry both members, while providers with separate call/result
/// events publish one member on each record and link them with
/// `provider_call_id` inside the same source session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpExchangeContent {
    pub provider_call_id: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present_option"
    )]
    pub invocation: Option<McpInvocationContent>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present_option"
    )]
    pub response: Option<McpTerminalResponseContent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpInvocationContent {
    pub server: String,
    pub tool: String,
    pub arguments: McpJsonCapture,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpTerminalResponseContent {
    pub status: McpTerminalStatus,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present_option"
    )]
    pub failure_kind: Option<McpFailureKind>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present_option"
    )]
    pub duration_ns: Option<u64>,
    pub text: McpTextCapture,
    pub payload: McpJsonCapture,
}

/// Complete JSON capture or an explicit reason that no complete value exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "capture_status", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpJsonCapture {
    Present {
        value: serde_json::Value,
    },
    Absent,
    Unavailable,
    Omitted {
        reason: McpPayloadOmissionReason,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "deserialize_present_option"
        )]
        observed_encoded_bytes: Option<u64>,
    },
}

/// Location/disposition of the normalized textual response channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "capture_status", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpTextCapture {
    NormalizedBody,
    Absent,
    Unavailable,
    Omitted {
        reason: McpPayloadOmissionReason,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "deserialize_present_option"
        )]
        observed_encoded_bytes: Option<u64>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpPayloadOmissionReason {
    SizeLimit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpTerminalStatus {
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpFailureKind {
    ToolReported,
    Invocation,
    Unknown,
}

fn deserialize_present_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

impl McpExchangeContent {
    pub(super) fn validate_contract(&self, normalized_body: Option<&str>) -> CoreRecordResult<()> {
        validate_text(
            "mcp_exchange.provider_call_id",
            &self.provider_call_id,
            MAX_MCP_EXCHANGE_CALL_ID_BYTES,
        )?;
        if self.invocation.is_none() && self.response.is_none() {
            return Err(CoreRecordError::InvalidMcpExchange);
        }
        if let Some(invocation) = &self.invocation {
            invocation.validate_contract()?;
        }
        if let Some(response) = &self.response {
            response.validate_contract(normalized_body)?;
        }
        Ok(())
    }
}

impl McpInvocationContent {
    fn validate_contract(&self) -> CoreRecordResult<()> {
        validate_text(
            "mcp_exchange.invocation.server",
            &self.server,
            MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES,
        )?;
        validate_text(
            "mcp_exchange.invocation.tool",
            &self.tool,
            MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES,
        )?;
        if matches!(
            &self.arguments,
            McpJsonCapture::Present { value } if !value.is_object()
        ) {
            return Err(CoreRecordError::InvalidMcpExchange);
        }
        Ok(())
    }
}

impl McpTerminalResponseContent {
    fn validate_contract(&self, normalized_body: Option<&str>) -> CoreRecordResult<()> {
        if (self.status == McpTerminalStatus::Failed) != self.failure_kind.is_some() {
            return Err(CoreRecordError::InvalidMcpExchange);
        }
        if matches!(self.text, McpTextCapture::NormalizedBody)
            && normalized_body.is_none_or(str::is_empty)
        {
            return Err(CoreRecordError::InvalidMcpExchange);
        }
        Ok(())
    }
}
