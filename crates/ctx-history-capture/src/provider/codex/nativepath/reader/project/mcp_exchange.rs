use std::{borrow::Cow, collections::HashSet, fmt};

use serde::{
    de::{MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer,
};
use serde_json::{value::RawValue, Map, Value};

use super::mcp::BoundedStringProbe;
use super::*;

pub(super) struct ProjectedMcpExchange {
    content: ctx_history_core::McpExchangeContent,
    arguments_observed_encoded_bytes: Option<u64>,
    payload_observed_encoded_bytes: Option<u64>,
    strict_discovery_payload: bool,
}

impl ProjectedMcpExchange {
    /// Retains as much exact typed exchange content as fits beside the selected
    /// normalized body. Larger JSON channels become explicit omissions; the
    /// normalized body itself is never shortened or replaced.
    pub(super) fn fit_selected_body(mut self, normalized_body: &str) -> Option<Self> {
        while !selected_content_fits(normalized_body, None, Some(&self.content)) {
            match (
                self.arguments_omission_savings(),
                self.payload_omission_savings(),
            ) {
                (Some(arguments), Some(payload)) if arguments >= payload => self.omit_arguments(),
                (Some(_), Some(_)) => self.omit_payload(),
                (Some(_), None) => self.omit_arguments(),
                (None, Some(_)) => self.omit_payload(),
                (None, None) => return None,
            }
        }
        Some(self)
    }

    pub(super) fn content(&self) -> &ctx_history_core::McpExchangeContent {
        &self.content
    }

    pub(super) fn into_content(self) -> ctx_history_core::McpExchangeContent {
        self.content
    }

    pub(super) fn discovery_exclusion(
        &self,
        source_unique_terminal: bool,
    ) -> Option<ctx_history_core::CoreDiscoveryExclusion> {
        let invocation = self.content.invocation.as_ref();
        let linked_invocation =
            source_unique_terminal
                .then_some(invocation)
                .flatten()
                .map(|invocation| {
                    crate::provider::ctx_retrieval::classify_mcp_invocation(
                        &invocation.server,
                        &invocation.tool,
                    )
                });
        let terminal_status = self
            .content
            .response
            .as_ref()
            .map(|response| match response.status {
                ctx_history_core::McpTerminalStatus::Succeeded => {
                    crate::provider::ctx_retrieval::ResultTerminalStatus::Succeeded
                }
                ctx_history_core::McpTerminalStatus::Failed
                | ctx_history_core::McpTerminalStatus::Cancelled
                | ctx_history_core::McpTerminalStatus::TimedOut => {
                    crate::provider::ctx_retrieval::ResultTerminalStatus::Failed
                }
                ctx_history_core::McpTerminalStatus::Unknown => {
                    crate::provider::ctx_retrieval::ResultTerminalStatus::Unknown
                }
            })
            .unwrap_or(crate::provider::ctx_retrieval::ResultTerminalStatus::Unknown);
        let atom = if self.strict_discovery_payload {
            crate::provider::ctx_retrieval::ResultAtom::Payload
        } else {
            crate::provider::ctx_retrieval::ResultAtom::Unknown
        };
        let contribution = crate::provider::ctx_retrieval::classify_linked_result(
            linked_invocation,
            terminal_status,
            [atom],
        );
        crate::provider::ctx_retrieval::discovery_exclusion_for([contribution])
    }

    fn omit_arguments(&mut self) {
        if let Some(invocation) = self.content.invocation.as_mut() {
            invocation.arguments = ctx_history_core::McpJsonCapture::Omitted {
                reason: ctx_history_core::McpPayloadOmissionReason::SizeLimit,
                observed_encoded_bytes: self.arguments_observed_encoded_bytes,
            };
        }
    }

    fn omit_payload(&mut self) {
        if let Some(response) = self.content.response.as_mut() {
            response.payload = ctx_history_core::McpJsonCapture::Omitted {
                reason: ctx_history_core::McpPayloadOmissionReason::SizeLimit,
                observed_encoded_bytes: self.payload_observed_encoded_bytes,
            };
        }
    }

    fn arguments_omission_savings(&self) -> Option<usize> {
        omission_savings(
            &self.content.invocation.as_ref()?.arguments,
            self.arguments_observed_encoded_bytes,
        )
    }

    fn payload_omission_savings(&self) -> Option<usize> {
        omission_savings(
            &self.content.response.as_ref()?.payload,
            self.payload_observed_encoded_bytes,
        )
    }
}

fn omission_savings(
    capture: &ctx_history_core::McpJsonCapture,
    observed_encoded_bytes: Option<u64>,
) -> Option<usize> {
    if !matches!(capture, ctx_history_core::McpJsonCapture::Present { .. }) {
        return None;
    }
    let present = encoded_json_len(capture)?;
    let omitted = encoded_json_len(&ctx_history_core::McpJsonCapture::Omitted {
        reason: ctx_history_core::McpPayloadOmissionReason::SizeLimit,
        observed_encoded_bytes,
    })?;
    Some(present.saturating_sub(omitted))
}

pub(super) fn project_mcp_exchange(record: &[u8], payload: &Value) -> Option<ProjectedMcpExchange> {
    if payload.get("type").and_then(Value::as_str) != Some("mcp_tool_call_end") {
        return None;
    }
    let expected_call_id = payload.get("call_id").and_then(Value::as_str)?;
    let strict_discovery_payload = std::str::from_utf8(record)
        .ok()
        .and_then(exact_json_value)
        .is_some_and(|record| strict_mcp_retrieval_payload(&record, expected_call_id));
    let evidence = serde_json::from_slice::<McpExchangeEnvelope<'_>>(record).ok()?;
    evidence.project(expected_call_id, strict_discovery_payload)
}

fn strict_mcp_retrieval_payload(record: &Value, expected_call_id: &str) -> bool {
    let Some(record) = record.as_object() else {
        return false;
    };
    if !only_members(record, &["timestamp", "type", "payload"])
        || record.get("type").and_then(Value::as_str) != Some("event_msg")
        || record
            .get("timestamp")
            .is_some_and(|timestamp| !timestamp.as_str().is_some_and(|value| !value.is_empty()))
    {
        return false;
    }
    let Some(payload) = record.get("payload").and_then(Value::as_object) else {
        return false;
    };
    if !only_members(
        payload,
        &["type", "call_id", "invocation", "duration", "result"],
    ) || payload.get("type").and_then(Value::as_str) != Some("mcp_tool_call_end")
        || payload.get("call_id").and_then(Value::as_str) != Some(expected_call_id)
    {
        return false;
    }
    let Some(invocation) = payload.get("invocation").and_then(Value::as_object) else {
        return false;
    };
    if !only_members(invocation, &["server", "tool", "arguments"])
        || !invocation
            .get("server")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty())
        || !invocation
            .get("tool")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty())
    {
        return false;
    }
    let Some(duration) = payload.get("duration").and_then(Value::as_object) else {
        return false;
    };
    if !only_members(duration, &["secs", "nanos"])
        || duration.get("secs").and_then(Value::as_u64).is_none()
        || !duration
            .get("nanos")
            .and_then(Value::as_u64)
            .is_some_and(|nanos| nanos < 1_000_000_000)
    {
        return false;
    }
    let Some(result) = payload.get("result").and_then(Value::as_object) else {
        return false;
    };
    if result.len() != 1 {
        return false;
    }
    let Some(ok) = result.get("Ok").and_then(Value::as_object) else {
        return false;
    };
    if !only_members(ok, &["content", "isError"])
        || ok
            .get("isError")
            .is_some_and(|is_error| is_error.as_bool() != Some(false))
    {
        return false;
    }
    let Some(content) = ok.get("content").and_then(Value::as_array) else {
        return false;
    };
    let mut saw_payload = false;
    for block in content {
        let Some(block) = block.as_object() else {
            return false;
        };
        if !only_members(block, &["type", "text"])
            || block.get("type").and_then(Value::as_str) != Some("text")
        {
            return false;
        }
        let Some(text) = block.get("text").and_then(Value::as_str) else {
            return false;
        };
        saw_payload |= !text.is_empty();
    }
    saw_payload
}

fn only_members(object: &Map<String, Value>, allowed: &[&str]) -> bool {
    object.keys().all(|key| allowed.contains(&key.as_str()))
}

pub(super) fn selected_content_fits(
    normalized_body: &str,
    structured_content: Option<&Value>,
    exchange: Option<&ctx_history_core::McpExchangeContent>,
) -> bool {
    normalized_body
        .len()
        .checked_add(
            structured_content
                .and_then(encoded_json_len)
                .unwrap_or_default(),
        )
        .and_then(|bytes| {
            bytes.checked_add(exchange.and_then(encoded_json_len).unwrap_or_default())
        })
        .is_some_and(|bytes| bytes <= ctx_history_core::MAX_CORE_CONTENT_BYTES)
}

#[derive(Default)]
struct McpExchangeEnvelope<'a> {
    record_type: Option<String>,
    payload: Option<McpExchangePayload<'a>>,
    ambiguous: bool,
}

impl McpExchangeEnvelope<'_> {
    fn project(
        self,
        expected_call_id: &str,
        strict_discovery_payload: bool,
    ) -> Option<ProjectedMcpExchange> {
        if self.ambiguous || self.record_type.as_deref() != Some("event_msg") {
            return None;
        }
        self.payload?
            .project(expected_call_id, strict_discovery_payload)
    }
}

impl<'de> Deserialize<'de> for McpExchangeEnvelope<'de> {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(McpExchangeEnvelopeVisitor)
    }
}

struct McpExchangeEnvelopeVisitor;

impl<'de> Visitor<'de> for McpExchangeEnvelopeVisitor {
    type Value = McpExchangeEnvelope<'de>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Codex MCP terminal envelope")
    }

    fn visit_map<M>(self, mut map: M) -> std::result::Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut envelope = McpExchangeEnvelope::default();
        let mut saw_record_type = false;
        let mut saw_payload = false;
        while let Some(key) = map.next_key::<Cow<'de, str>>()? {
            match key.as_ref() {
                "type" => {
                    envelope.ambiguous |= saw_record_type;
                    saw_record_type = true;
                    envelope.record_type = map.next_value::<BoundedStringProbe<64>>()?.value;
                }
                "payload" => {
                    envelope.ambiguous |= saw_payload;
                    saw_payload = true;
                    envelope.payload = Some(map.next_value::<McpExchangePayload<'de>>()?);
                }
                _ => {
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }
        Ok(envelope)
    }
}

#[derive(Default)]
struct McpExchangePayload<'a> {
    item_type: Option<String>,
    call_id: Option<String>,
    invocation: Option<McpExchangeInvocation<'a>>,
    duration: Option<McpDurationProbe>,
    result: Option<McpResultProbe<'a>>,
    duplicate_item_type: bool,
    duplicate_call_id: bool,
    duplicate_invocation: bool,
    duplicate_duration: bool,
    duplicate_result: bool,
}

impl McpExchangePayload<'_> {
    fn project(
        self,
        expected_call_id: &str,
        strict_discovery_payload: bool,
    ) -> Option<ProjectedMcpExchange> {
        if self.duplicate_item_type
            || self.duplicate_call_id
            || self.item_type.as_deref() != Some("mcp_tool_call_end")
            || self.call_id.as_deref() != Some(expected_call_id)
        {
            return None;
        }

        let (invocation, arguments_observed_encoded_bytes) = if self.duplicate_invocation {
            (None, None)
        } else {
            match self.invocation.and_then(McpExchangeInvocation::project) {
                Some((invocation, observed_encoded_bytes)) => {
                    (Some(invocation), observed_encoded_bytes)
                }
                None => (None, None),
            }
        };
        let duration_ns = (!self.duplicate_duration)
            .then_some(self.duration)
            .flatten()
            .and_then(McpDurationProbe::duration_ns);
        let projected_result = if self.duplicate_result {
            ProjectedMcpResult::unavailable()
        } else {
            self.result
                .and_then(McpResultProbe::project)
                .unwrap_or_else(ProjectedMcpResult::unavailable)
        };
        let response = ctx_history_core::McpTerminalResponseContent {
            status: projected_result.status,
            failure_kind: projected_result.failure_kind,
            duration_ns,
            text: ctx_history_core::McpTextCapture::NormalizedBody,
            payload: projected_result.payload,
        };
        Some(ProjectedMcpExchange {
            content: ctx_history_core::McpExchangeContent {
                provider_call_id: expected_call_id.to_owned(),
                invocation,
                response: Some(response),
            },
            arguments_observed_encoded_bytes,
            payload_observed_encoded_bytes: projected_result.observed_encoded_bytes,
            strict_discovery_payload,
        })
    }
}

impl<'de> Deserialize<'de> for McpExchangePayload<'de> {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(McpExchangePayloadVisitor)
    }
}

struct McpExchangePayloadVisitor;

impl<'de> Visitor<'de> for McpExchangePayloadVisitor {
    type Value = McpExchangePayload<'de>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Codex MCP terminal payload")
    }

    fn visit_map<M>(self, mut map: M) -> std::result::Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut payload = McpExchangePayload::default();
        let mut saw_item_type = false;
        let mut saw_call_id = false;
        let mut saw_invocation = false;
        let mut saw_duration = false;
        let mut saw_result = false;
        while let Some(key) = map.next_key::<Cow<'de, str>>()? {
            match key.as_ref() {
                "type" => {
                    payload.duplicate_item_type |= saw_item_type;
                    saw_item_type = true;
                    payload.item_type = map.next_value::<BoundedStringProbe<64>>()?.value;
                }
                "call_id" => {
                    payload.duplicate_call_id |= saw_call_id;
                    saw_call_id = true;
                    payload.call_id = map
                        .next_value::<BoundedStringProbe<MAX_CODEX_TOOL_CALL_ID_BYTES>>()?
                        .value;
                }
                "invocation" => {
                    payload.duplicate_invocation |= saw_invocation;
                    saw_invocation = true;
                    payload.invocation = Some(map.next_value::<McpExchangeInvocation<'de>>()?);
                }
                "duration" => {
                    payload.duplicate_duration |= saw_duration;
                    saw_duration = true;
                    payload.duration = Some(map.next_value()?);
                }
                "result" => {
                    payload.duplicate_result |= saw_result;
                    saw_result = true;
                    payload.result = Some(map.next_value::<McpResultProbe<'de>>()?);
                }
                _ => {
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }
        Ok(payload)
    }
}

#[derive(Default)]
struct McpExchangeInvocation<'a> {
    server: Option<String>,
    tool: Option<String>,
    arguments: Option<&'a RawValue>,
    object: bool,
    duplicate_server: bool,
    duplicate_tool: bool,
    duplicate_arguments: bool,
}

impl McpExchangeInvocation<'_> {
    fn project(self) -> Option<(ctx_history_core::McpInvocationContent, Option<u64>)> {
        if !self.object || self.duplicate_server || self.duplicate_tool {
            return None;
        }
        let server = self.server.filter(|server| !server.is_empty())?;
        let tool = self.tool.filter(|tool| !tool.is_empty())?;
        let (arguments, observed_encoded_bytes) = if self.duplicate_arguments {
            (ctx_history_core::McpJsonCapture::Unavailable, None)
        } else if let Some(raw) = self.arguments {
            let observed = u64::try_from(raw.get().len()).ok();
            let capture = exact_json_value(raw.get())
                .filter(Value::is_object)
                .map(|value| ctx_history_core::McpJsonCapture::Present { value })
                .unwrap_or(ctx_history_core::McpJsonCapture::Unavailable);
            (capture, observed)
        } else {
            (ctx_history_core::McpJsonCapture::Absent, None)
        };
        Some((
            ctx_history_core::McpInvocationContent {
                server,
                tool,
                arguments,
            },
            observed_encoded_bytes,
        ))
    }
}

impl<'de> Deserialize<'de> for McpExchangeInvocation<'de> {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(McpExchangeInvocationVisitor)
    }
}

struct McpExchangeInvocationVisitor;

impl<'de> Visitor<'de> for McpExchangeInvocationVisitor {
    type Value = McpExchangeInvocation<'de>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Codex MCP invocation object")
    }

    fn visit_map<M>(self, mut map: M) -> std::result::Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut invocation = McpExchangeInvocation {
            object: true,
            ..McpExchangeInvocation::default()
        };
        let mut saw_server = false;
        let mut saw_tool = false;
        let mut saw_arguments = false;
        while let Some(key) = map.next_key::<Cow<'de, str>>()? {
            match key.as_ref() {
                "server" => {
                    invocation.duplicate_server |= saw_server;
                    saw_server = true;
                    invocation.server = map
                        .next_value::<BoundedStringProbe<
                            { ctx_history_core::MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES },
                        >>()?
                        .value;
                }
                "tool" => {
                    invocation.duplicate_tool |= saw_tool;
                    saw_tool = true;
                    invocation.tool = map
                        .next_value::<BoundedStringProbe<
                            { ctx_history_core::MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES },
                        >>()?
                        .value;
                }
                "arguments" => {
                    invocation.duplicate_arguments |= saw_arguments;
                    saw_arguments = true;
                    invocation.arguments = Some(map.next_value::<&'de RawValue>()?);
                }
                _ => {
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }
        Ok(invocation)
    }

    fn visit_bool<E>(self, _value: bool) -> std::result::Result<Self::Value, E> {
        Ok(McpExchangeInvocation::default())
    }

    fn visit_i64<E>(self, _value: i64) -> std::result::Result<Self::Value, E> {
        Ok(McpExchangeInvocation::default())
    }

    fn visit_u64<E>(self, _value: u64) -> std::result::Result<Self::Value, E> {
        Ok(McpExchangeInvocation::default())
    }

    fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E> {
        Ok(McpExchangeInvocation::default())
    }

    fn visit_borrowed_str<E>(self, _value: &'de str) -> std::result::Result<Self::Value, E> {
        Ok(McpExchangeInvocation::default())
    }

    fn visit_str<E>(self, _value: &str) -> std::result::Result<Self::Value, E> {
        Ok(McpExchangeInvocation::default())
    }

    fn visit_string<E>(self, _value: String) -> std::result::Result<Self::Value, E> {
        Ok(McpExchangeInvocation::default())
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(McpExchangeInvocation::default())
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(McpExchangeInvocation::default())
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<serde::de::IgnoredAny>()?.is_some() {}
        Ok(McpExchangeInvocation::default())
    }
}

#[derive(Default)]
struct McpDurationProbe {
    secs: Option<u64>,
    nanos: Option<u64>,
    object: bool,
    ambiguous: bool,
}

impl McpDurationProbe {
    fn duration_ns(self) -> Option<u64> {
        if !self.object || self.ambiguous || self.nanos? >= 1_000_000_000 {
            return None;
        }
        self.secs?
            .checked_mul(1_000_000_000)?
            .checked_add(self.nanos?)
    }
}

impl<'de> Deserialize<'de> for McpDurationProbe {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(McpDurationProbeVisitor)
    }
}

struct McpDurationProbeVisitor;

impl<'de> Visitor<'de> for McpDurationProbeVisitor {
    type Value = McpDurationProbe;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Codex MCP duration object")
    }

    fn visit_map<M>(self, mut map: M) -> std::result::Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut duration = McpDurationProbe {
            object: true,
            ..McpDurationProbe::default()
        };
        let mut saw_secs = false;
        let mut saw_nanos = false;
        while let Some(key) = map.next_key::<Cow<'de, str>>()? {
            match key.as_ref() {
                "secs" => {
                    duration.ambiguous |= saw_secs;
                    saw_secs = true;
                    duration.secs = map.next_value::<U64Probe>()?.0;
                }
                "nanos" => {
                    duration.ambiguous |= saw_nanos;
                    saw_nanos = true;
                    duration.nanos = map.next_value::<U64Probe>()?.0;
                }
                _ => {
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }
        Ok(duration)
    }

    fn visit_bool<E>(self, _value: bool) -> std::result::Result<Self::Value, E> {
        Ok(McpDurationProbe::default())
    }

    fn visit_i64<E>(self, _value: i64) -> std::result::Result<Self::Value, E> {
        Ok(McpDurationProbe::default())
    }

    fn visit_u64<E>(self, _value: u64) -> std::result::Result<Self::Value, E> {
        Ok(McpDurationProbe::default())
    }

    fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E> {
        Ok(McpDurationProbe::default())
    }

    fn visit_borrowed_str<E>(self, _value: &'de str) -> std::result::Result<Self::Value, E> {
        Ok(McpDurationProbe::default())
    }

    fn visit_str<E>(self, _value: &str) -> std::result::Result<Self::Value, E> {
        Ok(McpDurationProbe::default())
    }

    fn visit_string<E>(self, _value: String) -> std::result::Result<Self::Value, E> {
        Ok(McpDurationProbe::default())
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(McpDurationProbe::default())
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(McpDurationProbe::default())
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<serde::de::IgnoredAny>()?.is_some() {}
        Ok(McpDurationProbe::default())
    }
}

struct U64Probe(Option<u64>);

impl<'de> Deserialize<'de> for U64Probe {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(U64ProbeVisitor)
    }
}

struct U64ProbeVisitor;

impl<'de> Visitor<'de> for U64ProbeVisitor {
    type Value = U64Probe;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON unsigned integer")
    }

    fn visit_u64<E>(self, value: u64) -> std::result::Result<Self::Value, E> {
        Ok(U64Probe(Some(value)))
    }

    fn visit_i64<E>(self, _value: i64) -> std::result::Result<Self::Value, E> {
        Ok(U64Probe(None))
    }

    fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E> {
        Ok(U64Probe(None))
    }

    fn visit_bool<E>(self, _value: bool) -> std::result::Result<Self::Value, E> {
        Ok(U64Probe(None))
    }

    fn visit_borrowed_str<E>(self, _value: &'de str) -> std::result::Result<Self::Value, E> {
        Ok(U64Probe(None))
    }

    fn visit_str<E>(self, _value: &str) -> std::result::Result<Self::Value, E> {
        Ok(U64Probe(None))
    }

    fn visit_string<E>(self, _value: String) -> std::result::Result<Self::Value, E> {
        Ok(U64Probe(None))
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(U64Probe(None))
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(U64Probe(None))
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<serde::de::IgnoredAny>()?.is_some() {}
        Ok(U64Probe(None))
    }

    fn visit_map<M>(self, mut map: M) -> std::result::Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        while map
            .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
            .is_some()
        {}
        Ok(U64Probe(None))
    }
}

#[derive(Clone, Copy)]
enum McpResultVariant {
    Ok,
    Err,
}

#[derive(Default)]
struct McpResultProbe<'a> {
    selected: Option<(McpResultVariant, &'a RawValue)>,
    object: bool,
    members: usize,
}

impl McpResultProbe<'_> {
    fn project(self) -> Option<ProjectedMcpResult> {
        if !self.object || self.members != 1 {
            return None;
        }
        let (variant, raw) = self.selected?;
        let observed_encoded_bytes = u64::try_from(raw.get().len()).ok();
        let exact_value = exact_json_value(raw.get());
        let payload = exact_value
            .map(|value| ctx_history_core::McpJsonCapture::Present { value })
            .unwrap_or(ctx_history_core::McpJsonCapture::Unavailable);
        let (status, failure_kind) = match variant {
            McpResultVariant::Err => (
                ctx_history_core::McpTerminalStatus::Failed,
                Some(ctx_history_core::McpFailureKind::Invocation),
            ),
            McpResultVariant::Ok => match exact_ok_is_error(raw.get()) {
                Some(true) => (
                    ctx_history_core::McpTerminalStatus::Failed,
                    Some(ctx_history_core::McpFailureKind::ToolReported),
                ),
                Some(false) => (ctx_history_core::McpTerminalStatus::Succeeded, None),
                None => (ctx_history_core::McpTerminalStatus::Unknown, None),
            },
        };
        Some(ProjectedMcpResult {
            status,
            failure_kind,
            payload,
            observed_encoded_bytes,
        })
    }
}

impl<'de> Deserialize<'de> for McpResultProbe<'de> {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(McpResultProbeVisitor)
    }
}

struct McpResultProbeVisitor;

impl<'de> Visitor<'de> for McpResultProbeVisitor {
    type Value = McpResultProbe<'de>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Codex MCP Ok/Err result wrapper")
    }

    fn visit_map<M>(self, mut map: M) -> std::result::Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut result = McpResultProbe {
            object: true,
            ..McpResultProbe::default()
        };
        while let Some(key) = map.next_key::<Cow<'de, str>>()? {
            let raw = map.next_value::<&'de RawValue>()?;
            result.members = result.members.saturating_add(1);
            if result.members == 1 {
                result.selected = match key.as_ref() {
                    "Ok" => Some((McpResultVariant::Ok, raw)),
                    "Err" => Some((McpResultVariant::Err, raw)),
                    _ => None,
                };
            }
        }
        Ok(result)
    }

    fn visit_bool<E>(self, _value: bool) -> std::result::Result<Self::Value, E> {
        Ok(McpResultProbe::default())
    }

    fn visit_i64<E>(self, _value: i64) -> std::result::Result<Self::Value, E> {
        Ok(McpResultProbe::default())
    }

    fn visit_u64<E>(self, _value: u64) -> std::result::Result<Self::Value, E> {
        Ok(McpResultProbe::default())
    }

    fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E> {
        Ok(McpResultProbe::default())
    }

    fn visit_borrowed_str<E>(self, _value: &'de str) -> std::result::Result<Self::Value, E> {
        Ok(McpResultProbe::default())
    }

    fn visit_str<E>(self, _value: &str) -> std::result::Result<Self::Value, E> {
        Ok(McpResultProbe::default())
    }

    fn visit_string<E>(self, _value: String) -> std::result::Result<Self::Value, E> {
        Ok(McpResultProbe::default())
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(McpResultProbe::default())
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(McpResultProbe::default())
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<serde::de::IgnoredAny>()?.is_some() {}
        Ok(McpResultProbe::default())
    }
}

struct ProjectedMcpResult {
    status: ctx_history_core::McpTerminalStatus,
    failure_kind: Option<ctx_history_core::McpFailureKind>,
    payload: ctx_history_core::McpJsonCapture,
    observed_encoded_bytes: Option<u64>,
}

impl ProjectedMcpResult {
    fn unavailable() -> Self {
        Self {
            status: ctx_history_core::McpTerminalStatus::Unknown,
            failure_kind: None,
            payload: ctx_history_core::McpJsonCapture::Unavailable,
            observed_encoded_bytes: None,
        }
    }
}

fn exact_ok_is_error(input: &str) -> Option<bool> {
    let probe = serde_json::from_str::<McpOkErrorProbe>(input).ok()?;
    if !probe.object || probe.ambiguous {
        return None;
    }
    if probe.saw_is_error {
        probe.is_error
    } else {
        Some(false)
    }
}

#[derive(Default)]
struct McpOkErrorProbe {
    is_error: Option<bool>,
    saw_is_error: bool,
    object: bool,
    ambiguous: bool,
}

impl<'de> Deserialize<'de> for McpOkErrorProbe {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(McpOkErrorProbeVisitor)
    }
}

struct McpOkErrorProbeVisitor;

impl<'de> Visitor<'de> for McpOkErrorProbeVisitor {
    type Value = McpOkErrorProbe;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Codex MCP Ok result")
    }

    fn visit_map<M>(self, mut map: M) -> std::result::Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut probe = McpOkErrorProbe {
            object: true,
            ..McpOkErrorProbe::default()
        };
        while let Some(key) = map.next_key::<Cow<'de, str>>()? {
            if key == "isError" {
                probe.ambiguous |= probe.saw_is_error;
                probe.saw_is_error = true;
                probe.is_error = map.next_value::<BoolProbe>()?.0;
            } else {
                map.next_value::<serde::de::IgnoredAny>()?;
            }
        }
        Ok(probe)
    }

    fn visit_bool<E>(self, _value: bool) -> std::result::Result<Self::Value, E> {
        Ok(McpOkErrorProbe::default())
    }

    fn visit_i64<E>(self, _value: i64) -> std::result::Result<Self::Value, E> {
        Ok(McpOkErrorProbe::default())
    }

    fn visit_u64<E>(self, _value: u64) -> std::result::Result<Self::Value, E> {
        Ok(McpOkErrorProbe::default())
    }

    fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E> {
        Ok(McpOkErrorProbe::default())
    }

    fn visit_borrowed_str<E>(self, _value: &'de str) -> std::result::Result<Self::Value, E> {
        Ok(McpOkErrorProbe::default())
    }

    fn visit_str<E>(self, _value: &str) -> std::result::Result<Self::Value, E> {
        Ok(McpOkErrorProbe::default())
    }

    fn visit_string<E>(self, _value: String) -> std::result::Result<Self::Value, E> {
        Ok(McpOkErrorProbe::default())
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(McpOkErrorProbe::default())
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(McpOkErrorProbe::default())
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<serde::de::IgnoredAny>()?.is_some() {}
        Ok(McpOkErrorProbe::default())
    }
}

struct BoolProbe(Option<bool>);

impl<'de> Deserialize<'de> for BoolProbe {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(BoolProbeVisitor)
    }
}

struct BoolProbeVisitor;

impl<'de> Visitor<'de> for BoolProbeVisitor {
    type Value = BoolProbe;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON boolean")
    }

    fn visit_bool<E>(self, value: bool) -> std::result::Result<Self::Value, E> {
        Ok(BoolProbe(Some(value)))
    }

    fn visit_i64<E>(self, _value: i64) -> std::result::Result<Self::Value, E> {
        Ok(BoolProbe(None))
    }

    fn visit_u64<E>(self, _value: u64) -> std::result::Result<Self::Value, E> {
        Ok(BoolProbe(None))
    }

    fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E> {
        Ok(BoolProbe(None))
    }

    fn visit_borrowed_str<E>(self, _value: &'de str) -> std::result::Result<Self::Value, E> {
        Ok(BoolProbe(None))
    }

    fn visit_str<E>(self, _value: &str) -> std::result::Result<Self::Value, E> {
        Ok(BoolProbe(None))
    }

    fn visit_string<E>(self, _value: String) -> std::result::Result<Self::Value, E> {
        Ok(BoolProbe(None))
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(BoolProbe(None))
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(BoolProbe(None))
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<serde::de::IgnoredAny>()?.is_some() {}
        Ok(BoolProbe(None))
    }

    fn visit_map<M>(self, mut map: M) -> std::result::Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        while map
            .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
            .is_some()
        {}
        Ok(BoolProbe(None))
    }
}

/// Parses one JSON value and rejects duplicate keys at every object depth.
fn exact_json_value(input: &str) -> Option<Value> {
    let mut deserializer = serde_json::Deserializer::from_str(input);
    let value = NoDuplicateJson::deserialize(&mut deserializer).ok()?.0;
    deserializer.end().ok()?;
    Some(value)
}

struct NoDuplicateJson(Value);

impl<'de> Deserialize<'de> for NoDuplicateJson {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(NoDuplicateJsonVisitor)
    }
}

struct NoDuplicateJsonVisitor;

impl<'de> Visitor<'de> for NoDuplicateJsonVisitor {
    type Value = NoDuplicateJson;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JSON without duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> std::result::Result<Self::Value, E> {
        Ok(NoDuplicateJson(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> std::result::Result<Self::Value, E> {
        Ok(NoDuplicateJson(Value::Number(value.into())))
    }

    fn visit_u64<E>(self, value: u64) -> std::result::Result<Self::Value, E> {
        Ok(NoDuplicateJson(Value::Number(value.into())))
    }

    fn visit_f64<E>(self, value: f64) -> std::result::Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        serde_json::Number::from_f64(value)
            .map(Value::Number)
            .map(NoDuplicateJson)
            .ok_or_else(|| E::custom("non-finite JSON number"))
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E> {
        Ok(NoDuplicateJson(Value::String(value.to_owned())))
    }

    fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E> {
        Ok(NoDuplicateJson(Value::String(value)))
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(NoDuplicateJson(Value::Null))
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(NoDuplicateJson(Value::Null))
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element::<NoDuplicateJson>()? {
            values.push(value.0);
        }
        Ok(NoDuplicateJson(Value::Array(values)))
    }

    fn visit_map<A>(self, mut object: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut keys = HashSet::new();
        let mut values = Map::new();
        while let Some(key) = object.next_key::<String>()? {
            if !keys.insert(key.clone()) {
                return Err(serde::de::Error::custom("duplicate JSON object key"));
            }
            let value = object.next_value::<NoDuplicateJson>()?;
            values.insert(key, value.0);
        }
        Ok(NoDuplicateJson(Value::Object(values)))
    }
}

#[cfg(test)]
mod tests {
    use super::exact_ok_is_error;

    #[test]
    fn ok_is_error_absence_defaults_to_success_but_invalid_or_duplicate_is_unknown() {
        assert_eq!(exact_ok_is_error(r#"{"content":[]}"#), Some(false));
        assert_eq!(exact_ok_is_error(r#"{"isError":false}"#), Some(false));
        assert_eq!(exact_ok_is_error(r#"{"isError":true}"#), Some(true));
        assert_eq!(exact_ok_is_error(r#"{"isError":"false"}"#), None);
        assert_eq!(exact_ok_is_error(r#"{"isError":null}"#), None);
        assert_eq!(
            exact_ok_is_error(r#"{"isError":false,"isError":true}"#),
            None
        );
    }
}
