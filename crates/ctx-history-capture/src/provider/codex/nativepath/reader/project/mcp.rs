use std::{borrow::Cow, collections::BTreeMap, fmt, mem::size_of};

use super::*;

const MAX_CODEX_MCP_TERMINAL_AUTHORITIES: usize = 4 * 1024;
const MAX_MCP_RAW_CALL_IDS_PER_RECORD: usize = 8;
const MCP_TERMINAL_CALL_ID_DOMAIN: &[u8] = b"ctx/codex-nativepath/mcp-terminal-call-id/v1\0";
const RESULT_TERMINAL_CALL_ID_DOMAIN: &[u8] = b"ctx/codex-nativepath/result-terminal-call-id/v1\0";
const MCP_TERMINAL_AUTHORITY_ENTRY_OVERHEAD_BYTES: usize = 3 * size_of::<usize>();

pub(in super::super) fn mcp_terminal_candidate_evidence(
    record: &[u8],
) -> Option<McpRawRecordEvidence> {
    let evidence = serde_json::from_slice::<McpRawRecordEvidence>(record).ok()?;
    // Terminal uniqueness is source authority, not projection validity. Count
    // every bounded structural terminal occurrence before result or duration
    // validation so malformed same-call-ID evidence forces abstention too.
    evidence.is_terminal().then_some(evidence)
}

pub(super) fn project_mcp_tool_call_attribution(
    record: &[u8],
    payload: &Value,
    authority: &CodexMcpTerminalAuthority,
) -> Option<ctx_history_core::McpToolCallAttribution> {
    if payload.get("type").and_then(Value::as_str) != Some("mcp_tool_call_end") {
        return None;
    }
    let call_id = payload.get("call_id").and_then(Value::as_str)?;
    if !authority.is_unique(call_id) {
        return None;
    }
    let evidence = serde_json::from_slice::<McpRawRecordEvidence>(record).ok()?;
    evidence.attribution(call_id)
}

#[derive(Debug, Clone, Copy, Default)]
struct McpTerminalAuthorityState {
    candidates: u8,
    in_certified_prefix: bool,
    after_certified_prefix: bool,
}

#[derive(Debug, Default)]
pub(in super::super) struct CodexMcpTerminalAuthority {
    mcp_call_ids: BTreeMap<[u8; 32], McpTerminalAuthorityState>,
    result_call_ids: BTreeMap<[u8; 32], McpTerminalAuthorityState>,
    mcp_exhausted: bool,
    result_exhausted: bool,
}

impl CodexMcpTerminalAuthority {
    pub(in super::super) fn observe(
        &mut self,
        evidence: &McpRawRecordEvidence,
        in_certified_prefix: bool,
    ) {
        if self.mcp_exhausted || !evidence.is_terminal() {
            return;
        }
        if evidence.call_id_capacity_exceeded {
            self.exhaust_mcp();
            return;
        }
        for digest in &evidence.call_id_sha256 {
            if !self.mcp_call_ids.contains_key(digest)
                && self.mcp_call_ids.len() >= MAX_CODEX_MCP_TERMINAL_AUTHORITIES
            {
                self.exhaust_mcp();
                return;
            }
            let state = self.mcp_call_ids.entry(*digest).or_default();
            state.candidates = state.candidates.saturating_add(1).min(2);
            state.in_certified_prefix |= in_certified_prefix;
            state.after_certified_prefix |= !in_certified_prefix;
        }
    }

    pub(in super::super) fn observe_result_call_id(
        &mut self,
        call_id: &str,
        in_certified_prefix: bool,
    ) {
        if self.result_exhausted {
            return;
        }
        let digest = result_terminal_call_id_digest(call_id);
        if !self.result_call_ids.contains_key(&digest)
            && self.result_call_ids.len() >= MAX_CODEX_MCP_TERMINAL_AUTHORITIES
        {
            self.result_call_ids.clear();
            self.result_exhausted = true;
            return;
        }
        let state = self.result_call_ids.entry(digest).or_default();
        state.candidates = state.candidates.saturating_add(1).min(2);
        state.in_certified_prefix |= in_certified_prefix;
        state.after_certified_prefix |= !in_certified_prefix;
    }

    pub(super) fn is_unique(&self, call_id: &str) -> bool {
        !self.mcp_exhausted
            && self
                .mcp_call_ids
                .get(&mcp_terminal_call_id_digest(call_id))
                .is_some_and(|state| state.candidates == 1)
    }

    pub(super) fn is_unique_result(&self, call_id: &str) -> bool {
        !self.result_exhausted
            && self
                .result_call_ids
                .get(&result_terminal_call_id_digest(call_id))
                .is_some_and(|state| state.candidates == 1)
    }

    pub(in super::super) fn append_requires_replacement(&self) -> bool {
        self.mcp_exhausted
            || self.result_exhausted
            || self.mcp_call_ids.values().any(|state| {
                state.in_certified_prefix && state.after_certified_prefix && state.candidates > 1
            })
            || self.result_call_ids.values().any(|state| {
                state.in_certified_prefix && state.after_certified_prefix && state.candidates > 1
            })
    }

    pub(in super::super) fn entry_count(&self) -> usize {
        self.mcp_call_ids
            .len()
            .saturating_add(self.result_call_ids.len())
    }

    pub(in super::super) fn estimated_owned_bytes(&self) -> usize {
        size_of::<Self>().saturating_add(
            self.mcp_call_ids
                .len()
                .saturating_add(self.result_call_ids.len())
                .saturating_mul(
                    size_of::<([u8; 32], McpTerminalAuthorityState)>()
                        .saturating_add(MCP_TERMINAL_AUTHORITY_ENTRY_OVERHEAD_BYTES),
                ),
        )
    }

    fn exhaust_mcp(&mut self) {
        self.mcp_call_ids.clear();
        self.mcp_exhausted = true;
    }
}

fn mcp_terminal_call_id_digest(call_id: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(MCP_TERMINAL_CALL_ID_DOMAIN);
    hasher.update(call_id.as_bytes());
    hasher.finalize().into()
}

fn result_terminal_call_id_digest(call_id: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(RESULT_TERMINAL_CALL_ID_DOMAIN);
    hasher.update(call_id.as_bytes());
    hasher.finalize().into()
}

#[derive(Default)]
pub(in super::super) struct McpRawRecordEvidence {
    record_type: Option<String>,
    payload: Option<McpAttributionPayload>,
    call_id_sha256: Vec<[u8; 32]>,
    call_id_capacity_exceeded: bool,
    ambiguous: bool,
}

impl McpRawRecordEvidence {
    pub(super) fn is_terminal(&self) -> bool {
        self.record_type.as_deref() == Some("event_msg")
            && self
                .payload
                .as_ref()
                .and_then(|payload| payload.item_type.as_deref())
                == Some("mcp_tool_call_end")
    }

    fn attribution(self, call_id: &str) -> Option<ctx_history_core::McpToolCallAttribution> {
        if !self.is_terminal() || self.ambiguous || self.call_id_capacity_exceeded {
            return None;
        }
        let payload = self.payload?;
        if payload.ambiguous || payload.call_id.as_deref() != Some(call_id) {
            return None;
        }
        payload.invocation?.attribution()
    }

    fn merge_call_ids(&mut self, payload: &McpAttributionPayload) {
        self.call_id_capacity_exceeded |= payload.call_id_capacity_exceeded;
        if self.call_id_capacity_exceeded {
            return;
        }
        for digest in &payload.call_id_sha256 {
            if self.call_id_sha256.contains(digest) {
                continue;
            }
            if self.call_id_sha256.len() >= MAX_MCP_RAW_CALL_IDS_PER_RECORD {
                self.call_id_capacity_exceeded = true;
                return;
            }
            self.call_id_sha256.push(*digest);
        }
    }
}

impl<'de> serde::Deserialize<'de> for McpRawRecordEvidence {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(McpRawRecordEvidenceVisitor)
    }
}

struct McpRawRecordEvidenceVisitor;

impl<'de> serde::de::Visitor<'de> for McpRawRecordEvidenceVisitor {
    type Value = McpRawRecordEvidence;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Codex MCP terminal envelope")
    }

    fn visit_map<M>(self, mut map: M) -> std::result::Result<Self::Value, M::Error>
    where
        M: serde::de::MapAccess<'de>,
    {
        let mut evidence = McpRawRecordEvidence::default();
        let mut saw_record_type = false;
        let mut saw_payload = false;
        while let Some(key) = map.next_key::<Cow<'de, str>>()? {
            match key.as_ref() {
                "type" => {
                    evidence.ambiguous |= saw_record_type;
                    saw_record_type = true;
                    evidence.record_type = map.next_value::<BoundedStringProbe<64>>()?.value;
                }
                "payload" => {
                    evidence.ambiguous |= saw_payload;
                    saw_payload = true;
                    let payload = map.next_value::<McpAttributionPayload>()?;
                    evidence.merge_call_ids(&payload);
                    evidence.payload = Some(payload);
                }
                _ => {
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }
        Ok(evidence)
    }
}

#[derive(Default)]
struct McpAttributionPayload {
    item_type: Option<String>,
    call_id: Option<String>,
    call_id_sha256: Vec<[u8; 32]>,
    call_id_capacity_exceeded: bool,
    invocation: Option<McpInvocationProbe>,
    ambiguous: bool,
}

impl McpAttributionPayload {
    fn observe_call_id(&mut self, call_id: Option<String>) {
        if let Some(call_id) = call_id
            .as_deref()
            .filter(|call_id| !call_id.is_empty() && call_id.len() <= MAX_CODEX_TOOL_CALL_ID_BYTES)
        {
            let digest = mcp_terminal_call_id_digest(call_id);
            if !self.call_id_sha256.contains(&digest) {
                if self.call_id_sha256.len() >= MAX_MCP_RAW_CALL_IDS_PER_RECORD {
                    self.call_id_capacity_exceeded = true;
                } else {
                    self.call_id_sha256.push(digest);
                }
            }
        }
        self.call_id = call_id;
    }
}

impl<'de> serde::Deserialize<'de> for McpAttributionPayload {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(McpAttributionPayloadVisitor)
    }
}

struct McpAttributionPayloadVisitor;

impl<'de> serde::de::Visitor<'de> for McpAttributionPayloadVisitor {
    type Value = McpAttributionPayload;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Codex MCP terminal payload")
    }

    fn visit_map<M>(self, mut map: M) -> std::result::Result<Self::Value, M::Error>
    where
        M: serde::de::MapAccess<'de>,
    {
        let mut payload = McpAttributionPayload::default();
        let mut saw_item_type = false;
        let mut saw_call_id = false;
        let mut saw_invocation = false;
        let mut saw_duration = false;
        let mut saw_result = false;
        let mut saw_output = false;
        let mut saw_tools = false;
        while let Some(key) = map.next_key::<Cow<'de, str>>()? {
            match key.as_ref() {
                "type" => {
                    payload.ambiguous |= saw_item_type;
                    saw_item_type = true;
                    payload.item_type = map.next_value::<BoundedStringProbe<64>>()?.value;
                }
                "call_id" => {
                    payload.ambiguous |= saw_call_id;
                    saw_call_id = true;
                    let call_id = map
                        .next_value::<BoundedStringProbe<MAX_CODEX_TOOL_CALL_ID_BYTES>>()?
                        .value;
                    payload.observe_call_id(call_id);
                }
                "invocation" => {
                    payload.ambiguous |= saw_invocation;
                    saw_invocation = true;
                    payload.invocation = Some(map.next_value()?);
                }
                "duration" => {
                    payload.ambiguous |= saw_duration;
                    saw_duration = true;
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
                "result" => {
                    payload.ambiguous |= saw_result;
                    saw_result = true;
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
                "output" => {
                    payload.ambiguous |= saw_output;
                    saw_output = true;
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
                "tools" => {
                    payload.ambiguous |= saw_tools;
                    saw_tools = true;
                    map.next_value::<serde::de::IgnoredAny>()?;
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
struct McpInvocationProbe {
    server: Option<String>,
    tool: Option<String>,
    ambiguous: bool,
    object: bool,
}

impl McpInvocationProbe {
    fn attribution(self) -> Option<ctx_history_core::McpToolCallAttribution> {
        if self.ambiguous || !self.object {
            return None;
        }
        let server = self.server?;
        let tool = self.tool?;
        if server.is_empty()
            || tool.is_empty()
            || server.len() > ctx_history_core::MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES
            || tool.len() > ctx_history_core::MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES
        {
            return None;
        }
        Some(ctx_history_core::McpToolCallAttribution { server, tool })
    }
}

impl<'de> serde::Deserialize<'de> for McpInvocationProbe {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(McpInvocationProbeVisitor)
    }
}

struct McpInvocationProbeVisitor;

impl<'de> serde::de::Visitor<'de> for McpInvocationProbeVisitor {
    type Value = McpInvocationProbe;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Codex MCP invocation object")
    }

    fn visit_map<M>(self, mut map: M) -> std::result::Result<Self::Value, M::Error>
    where
        M: serde::de::MapAccess<'de>,
    {
        let mut invocation = McpInvocationProbe {
            object: true,
            ..McpInvocationProbe::default()
        };
        let mut saw_server = false;
        let mut saw_tool = false;
        while let Some(key) = map.next_key::<Cow<'de, str>>()? {
            match key.as_ref() {
                "server" if !saw_server => {
                    saw_server = true;
                    invocation.server = map
                        .next_value::<BoundedStringProbe<
                            { ctx_history_core::MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES },
                        >>()?
                        .value;
                }
                "tool" if !saw_tool => {
                    saw_tool = true;
                    invocation.tool = map
                        .next_value::<BoundedStringProbe<
                            { ctx_history_core::MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES },
                        >>()?
                        .value;
                }
                "server" => {
                    invocation.ambiguous = true;
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
                "tool" => {
                    invocation.ambiguous = true;
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
                _ => {
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }
        Ok(invocation)
    }

    fn visit_bool<E>(self, _value: bool) -> std::result::Result<Self::Value, E> {
        Ok(McpInvocationProbe::default())
    }

    fn visit_i64<E>(self, _value: i64) -> std::result::Result<Self::Value, E> {
        Ok(McpInvocationProbe::default())
    }

    fn visit_u64<E>(self, _value: u64) -> std::result::Result<Self::Value, E> {
        Ok(McpInvocationProbe::default())
    }

    fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E> {
        Ok(McpInvocationProbe::default())
    }

    fn visit_borrowed_str<E>(self, _value: &'de str) -> std::result::Result<Self::Value, E> {
        Ok(McpInvocationProbe::default())
    }

    fn visit_str<E>(self, _value: &str) -> std::result::Result<Self::Value, E> {
        Ok(McpInvocationProbe::default())
    }

    fn visit_string<E>(self, _value: String) -> std::result::Result<Self::Value, E> {
        Ok(McpInvocationProbe::default())
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(McpInvocationProbe::default())
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(McpInvocationProbe::default())
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: serde::de::SeqAccess<'de>,
    {
        while sequence.next_element::<serde::de::IgnoredAny>()?.is_some() {}
        Ok(McpInvocationProbe::default())
    }
}

#[derive(Default)]
pub(super) struct BoundedStringProbe<const MAX_BYTES: usize> {
    pub(super) value: Option<String>,
}

impl<'de, const MAX_BYTES: usize> serde::Deserialize<'de> for BoundedStringProbe<MAX_BYTES> {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(BoundedStringProbeVisitor::<MAX_BYTES>)
    }
}

struct BoundedStringProbeVisitor<const MAX_BYTES: usize>;

impl<'de, const MAX_BYTES: usize> serde::de::Visitor<'de> for BoundedStringProbeVisitor<MAX_BYTES> {
    type Value = BoundedStringProbe<MAX_BYTES>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded JSON string")
    }

    fn visit_borrowed_str<E>(self, value: &'de str) -> std::result::Result<Self::Value, E> {
        Ok(BoundedStringProbe {
            value: (value.len() <= MAX_BYTES).then(|| value.to_owned()),
        })
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E> {
        Ok(BoundedStringProbe {
            value: (value.len() <= MAX_BYTES).then(|| value.to_owned()),
        })
    }

    fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E> {
        Ok(BoundedStringProbe {
            value: (value.len() <= MAX_BYTES).then_some(value),
        })
    }

    fn visit_bool<E>(self, _value: bool) -> std::result::Result<Self::Value, E> {
        Ok(BoundedStringProbe::default())
    }

    fn visit_i64<E>(self, _value: i64) -> std::result::Result<Self::Value, E> {
        Ok(BoundedStringProbe::default())
    }

    fn visit_u64<E>(self, _value: u64) -> std::result::Result<Self::Value, E> {
        Ok(BoundedStringProbe::default())
    }

    fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E> {
        Ok(BoundedStringProbe::default())
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(BoundedStringProbe::default())
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(BoundedStringProbe::default())
    }

    fn visit_map<M>(self, mut map: M) -> std::result::Result<Self::Value, M::Error>
    where
        M: serde::de::MapAccess<'de>,
    {
        while map
            .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
            .is_some()
        {}
        Ok(BoundedStringProbe::default())
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: serde::de::SeqAccess<'de>,
    {
        while sequence.next_element::<serde::de::IgnoredAny>()?.is_some() {}
        Ok(BoundedStringProbe::default())
    }
}
