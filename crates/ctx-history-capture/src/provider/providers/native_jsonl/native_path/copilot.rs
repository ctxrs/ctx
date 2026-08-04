use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use ctx_history_core::{
    CaptureProvider, McpToolCallAttribution, MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES,
};
use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{value::RawValue, Value};
use sha2::{Digest, Sha256};

use crate::{
    common::io::OpenedProviderSourceFile, CaptureError, Result, COPILOT_CLI_SOURCE_FORMAT,
};

pub(super) const COPILOT_DIRECT_NATIVE_JSONL_PARSER_REVISION: &str =
    "copilot-cli-direct-native-jsonl-v4-bound-mcp-tool-call-attribution";

const COPILOT_LINKAGE_SCAN_CHUNK_BYTES: usize = 1024 * 1024;
pub(super) const COPILOT_LINKAGE_MAX_LINE_BYTES: usize = 1024 * 1024;
pub(super) const COPILOT_LINKAGE_MAX_CALL_ID_BYTES: usize = 1024;
pub(super) const COPILOT_LINKAGE_MAX_DISTINCT_IDS: usize = 4096;
pub(super) const COPILOT_LINKAGE_MAX_TOTAL_CANDIDATES: usize = 8192;
pub(super) const COPILOT_LINKAGE_MAX_CANDIDATES_PER_ID: usize = 8;
pub(super) const COPILOT_LINKAGE_MAX_RETAINED_BYTES: usize = 4 * 1024 * 1024;

const COPILOT_LINKAGE_MAX_EVENT_TYPE_BYTES: usize = 64;
const COPILOT_PROJECTION_BINDING_DOMAIN: &[u8] = b"ctx-copilot-attribution-projection-v1\0";

pub(super) struct CopilotMcpToolCallAttributions {
    completions: BTreeMap<u64, CopilotBoundMcpToolCallAttribution>,
    expected_starts: BTreeMap<u64, [u8; 32]>,
    verified_starts: BTreeSet<u64>,
    expected_projection: Option<CopilotProjectionBindings>,
    projected_records: u64,
    projected_hasher: Sha256,
}

impl CopilotMcpToolCallAttributions {
    pub(super) fn new() -> Self {
        Self {
            completions: BTreeMap::new(),
            expected_starts: BTreeMap::new(),
            verified_starts: BTreeSet::new(),
            expected_projection: None,
            projected_records: 0,
            projected_hasher: new_copilot_projection_hasher(),
        }
    }

    fn insert(
        &mut self,
        start_ordinal: u64,
        start_record_digest: [u8; 32],
        completion_ordinal: u64,
        completion_record_digest: [u8; 32],
        attribution: McpToolCallAttribution,
    ) -> bool {
        if self
            .expected_starts
            .insert(start_ordinal, start_record_digest)
            .is_some()
        {
            return false;
        }
        self.completions
            .insert(
                completion_ordinal,
                CopilotBoundMcpToolCallAttribution {
                    attribution,
                    start_ordinal,
                    completion_record_digest,
                },
            )
            .is_none()
    }

    pub(super) fn observe_projected_record(&mut self, ordinal: u64, record_digest: [u8; 32]) {
        if self.expected_projection.is_some() {
            update_copilot_projection_hasher(&mut self.projected_hasher, ordinal, record_digest);
            self.projected_records = self.projected_records.saturating_add(1);
        }
        let Some(expected) = self.expected_starts.get(&ordinal) else {
            return;
        };
        if *expected == record_digest {
            self.verified_starts.insert(ordinal);
        } else {
            self.verified_starts.remove(&ordinal);
        }
    }

    pub(super) fn attribution_for_projected_completion(
        &self,
        ordinal: u64,
        record_digest: [u8; 32],
    ) -> Option<McpToolCallAttribution> {
        let bound = self.completions.get(&ordinal)?;
        (bound.completion_record_digest == record_digest
            && self.verified_starts.contains(&bound.start_ordinal))
        .then(|| bound.attribution.clone())
    }

    pub(super) fn projected_records_match(&self) -> bool {
        let Some(expected) = self.expected_projection else {
            return true;
        };
        if self.projected_records == 0 {
            return true;
        }
        let projected_digest: [u8; 32] = self.projected_hasher.clone().finalize().into();
        [expected.full, expected.after_identity_probe]
            .into_iter()
            .any(|binding| {
                self.projected_records == binding.records && projected_digest == binding.digest
            })
    }

    fn bind_projection(&mut self, binding: CopilotProjectionBindings) {
        if !self.completions.is_empty() {
            self.expected_projection = Some(binding);
        }
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.completions.len()
    }

    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.completions.is_empty()
    }
}

struct CopilotBoundMcpToolCallAttribution {
    attribution: McpToolCallAttribution,
    start_ordinal: u64,
    completion_record_digest: [u8; 32],
}

#[derive(Clone, Copy)]
struct CopilotProjectionBinding {
    records: u64,
    digest: [u8; 32],
}

#[derive(Clone, Copy)]
struct CopilotProjectionBindings {
    full: CopilotProjectionBinding,
    after_identity_probe: CopilotProjectionBinding,
}

fn new_copilot_projection_hasher() -> Sha256 {
    let mut hasher = Sha256::new();
    hasher.update(COPILOT_PROJECTION_BINDING_DOMAIN);
    hasher
}

fn update_copilot_projection_hasher(hasher: &mut Sha256, ordinal: u64, record_digest: [u8; 32]) {
    hasher.update(ordinal.to_le_bytes());
    hasher.update(record_digest);
}

#[cfg(test)]
type CopilotTestHook = Box<dyn FnOnce()>;
#[cfg(test)]
type CopilotRecordTestHook = (u64, CopilotTestHook);

#[cfg(test)]
thread_local! {
    static AFTER_COPILOT_LINKAGE_RECORD_HOOK:
        std::cell::RefCell<Option<CopilotRecordTestHook>> = const {
            std::cell::RefCell::new(None)
        };
    static AFTER_COPILOT_LINKAGE_PLAN_HOOK: std::cell::RefCell<Option<CopilotTestHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn set_after_copilot_linkage_record_hook(ordinal: u64, hook: impl FnOnce() + 'static) {
    AFTER_COPILOT_LINKAGE_RECORD_HOOK.with(|slot| {
        let previous = slot.replace(Some((ordinal, Box::new(hook))));
        assert!(
            previous.is_none(),
            "Copilot linkage-record test hooks must not be nested"
        );
    });
}

#[cfg(test)]
fn run_after_copilot_linkage_record_hook(ordinal: u64) {
    let hook = AFTER_COPILOT_LINKAGE_RECORD_HOOK.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot
            .as_ref()
            .is_some_and(|(expected_ordinal, _)| *expected_ordinal == ordinal)
        {
            slot.take().map(|(_, hook)| hook)
        } else {
            None
        }
    });
    if let Some(hook) = hook {
        hook();
    }
}

#[cfg(test)]
pub(super) fn set_after_copilot_linkage_plan_hook(hook: impl FnOnce() + 'static) {
    AFTER_COPILOT_LINKAGE_PLAN_HOOK.with(|slot| {
        let previous = slot.replace(Some(Box::new(hook)));
        assert!(
            previous.is_none(),
            "Copilot linkage-plan test hooks must not be nested"
        );
    });
}

#[cfg(test)]
fn run_after_copilot_linkage_plan_hook() {
    let hook = AFTER_COPILOT_LINKAGE_PLAN_HOOK.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

const PARSER_REVISION: &str = "direct-native-jsonl-parser-v4";

pub(crate) const fn copilot_source_backed_adapter() -> super::DirectJsonlFamilyAdapter {
    super::DirectJsonlFamilyAdapter::new(
        CaptureProvider::CopilotCli,
        COPILOT_CLI_SOURCE_FORMAT,
        "copilot-cli-direct-native-jsonl-v1",
        PARSER_REVISION,
    )
}

pub(super) fn copilot_event_identity(value: &Value) -> Option<&str> {
    value
        .get("id")
        .and_then(Value::as_str)
        .filter(|event_id| !event_id.trim().is_empty())
}

pub(super) fn copilot_mcp_tool_call_attributions(
    source_file: &OpenedProviderSourceFile,
) -> Result<CopilotMcpToolCallAttributions> {
    copilot_mcp_tool_call_attributions_with_limits(source_file, CopilotLinkageLimits::DEFAULT)
}

#[derive(Debug, Clone, Copy)]
pub(super) struct CopilotLinkageLimits {
    pub(super) max_line_bytes: usize,
    pub(super) max_call_id_bytes: usize,
    pub(super) max_distinct_ids: usize,
    pub(super) max_total_candidates: usize,
    pub(super) max_candidates_per_id: usize,
    pub(super) max_retained_bytes: usize,
}

impl CopilotLinkageLimits {
    pub(super) const DEFAULT: Self = Self {
        max_line_bytes: COPILOT_LINKAGE_MAX_LINE_BYTES,
        max_call_id_bytes: COPILOT_LINKAGE_MAX_CALL_ID_BYTES,
        max_distinct_ids: COPILOT_LINKAGE_MAX_DISTINCT_IDS,
        max_total_candidates: COPILOT_LINKAGE_MAX_TOTAL_CANDIDATES,
        max_candidates_per_id: COPILOT_LINKAGE_MAX_CANDIDATES_PER_ID,
        max_retained_bytes: COPILOT_LINKAGE_MAX_RETAINED_BYTES,
    };
}

pub(super) fn copilot_mcp_tool_call_attributions_with_limits(
    source_file: &OpenedProviderSourceFile,
    limits: CopilotLinkageLimits,
) -> Result<CopilotMcpToolCallAttributions> {
    let frozen_length = source_file.file().metadata()?.len();
    let mut scanner = CopilotLinkageScanner::new(limits);
    let mut line = Vec::new();
    let mut oversized = false;
    let mut ordinal = 0_u64;
    let mut offset = 0_u64;

    while offset < frozen_length {
        let remaining = frozen_length.saturating_sub(offset);
        let length = usize::try_from(remaining.min(COPILOT_LINKAGE_SCAN_CHUNK_BYTES as u64))
            .map_err(|_| CaptureError::SystemInvariant("Copilot linkage scan length overflow"))?;
        let chunk = source_file.read_exact_range_allow_append(
            offset,
            length,
            COPILOT_LINKAGE_SCAN_CHUNK_BYTES,
        )?;
        offset = offset
            .checked_add(length as u64)
            .ok_or(CaptureError::SystemInvariant(
                "Copilot linkage scan offset overflow",
            ))?;

        let mut chunk_offset = 0_usize;
        while chunk_offset < chunk.len() {
            let rest = &chunk[chunk_offset..];
            let take = rest
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(rest.len(), |index| index.saturating_add(1));
            let segment = &rest[..take];
            if !scanner.disabled && !oversized {
                if line.len().saturating_add(segment.len())
                    > limits.max_line_bytes.saturating_add(2)
                {
                    line.clear();
                    oversized = true;
                } else {
                    line.extend_from_slice(segment);
                }
            }
            chunk_offset = chunk_offset.saturating_add(take);

            if segment.last() != Some(&b'\n') {
                continue;
            }
            if !scanner.disabled {
                if oversized {
                    scanner.disable();
                } else {
                    let record_digest = Sha256::digest(&line).into();
                    line.pop();
                    if line.last() == Some(&b'\r') {
                        line.pop();
                    }
                    if line.len() > limits.max_line_bytes {
                        scanner.disable();
                    } else {
                        scanner.observe_line(&line, ordinal, record_digest);
                    }
                }
            }
            #[cfg(test)]
            run_after_copilot_linkage_record_hook(ordinal);
            ordinal = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                "Copilot linkage scan ordinal overflow",
            ))?;
            line.clear();
            oversized = false;
        }
    }
    let plan = scanner.finish();
    #[cfg(test)]
    run_after_copilot_linkage_plan_hook();
    source_file.revalidate_same_object()?;
    Ok(plan)
}

struct CopilotLinkageScanner {
    limits: CopilotLinkageLimits,
    calls: BTreeMap<String, CopilotCallEvidence>,
    total_candidates: usize,
    retained_bytes: usize,
    disabled: bool,
    projection_records: u64,
    projection_hasher: Sha256,
    full_projection_records: u64,
    full_projection_hasher: Sha256,
}

impl CopilotLinkageScanner {
    fn new(limits: CopilotLinkageLimits) -> Self {
        Self {
            limits,
            calls: BTreeMap::new(),
            total_candidates: 0,
            retained_bytes: 0,
            disabled: false,
            projection_records: 0,
            projection_hasher: new_copilot_projection_hasher(),
            full_projection_records: 0,
            full_projection_hasher: new_copilot_projection_hasher(),
        }
    }

    fn disable(&mut self) {
        self.calls.clear();
        self.disabled = true;
    }

    fn observe_line(&mut self, bytes: &[u8], ordinal: u64, record_digest: [u8; 32]) {
        if self.disabled {
            return;
        }
        update_copilot_projection_hasher(&mut self.full_projection_hasher, ordinal, record_digest);
        self.full_projection_records = self.full_projection_records.saturating_add(1);
        if ordinal != 0 {
            update_copilot_projection_hasher(&mut self.projection_hasher, ordinal, record_digest);
            self.projection_records = self.projection_records.saturating_add(1);
        }
        match parse_copilot_linkage_line(bytes, self.limits) {
            CopilotParsedLine::Irrelevant => {}
            CopilotParsedLine::DisableSession => self.disable(),
            CopilotParsedLine::Candidate(candidate) => {
                self.observe_candidate(candidate, ordinal, record_digest);
            }
        }
    }

    fn observe_candidate(
        &mut self,
        candidate: CopilotCandidate,
        ordinal: u64,
        record_digest: [u8; 32],
    ) {
        self.total_candidates = self.total_candidates.saturating_add(1);
        if self.total_candidates > self.limits.max_total_candidates {
            self.disable();
            return;
        }
        let Some(tool_call_id) = candidate.tool_call_id else {
            return;
        };
        let is_new_id = !self.calls.contains_key(&tool_call_id);
        if is_new_id && self.calls.len() == self.limits.max_distinct_ids {
            self.disable();
            return;
        }
        self.retained_bytes = self.retained_bytes.saturating_add(candidate.retained_bytes);
        if self.retained_bytes > self.limits.max_retained_bytes {
            self.disable();
            return;
        }
        let evidence = self.calls.entry(tool_call_id).or_default();
        evidence.candidates = evidence.candidates.saturating_add(1);
        if evidence.candidates > self.limits.max_candidates_per_id {
            self.disable();
            return;
        }
        match candidate.kind {
            CopilotCandidateKind::Start(attribution) => {
                evidence.start.observe(CopilotStartEvidence {
                    ordinal,
                    record_digest,
                    attribution,
                });
            }
            CopilotCandidateKind::Completion { exact } => {
                evidence.completion.observe(CopilotCompletionEvidence {
                    ordinal,
                    record_digest,
                    exact,
                });
            }
        }
    }

    fn finish(self) -> CopilotMcpToolCallAttributions {
        if self.disabled {
            return CopilotMcpToolCallAttributions::new();
        }
        let projection_binding = CopilotProjectionBindings {
            full: CopilotProjectionBinding {
                records: self.full_projection_records,
                digest: self.full_projection_hasher.finalize().into(),
            },
            after_identity_probe: CopilotProjectionBinding {
                records: self.projection_records,
                digest: self.projection_hasher.finalize().into(),
            },
        };
        let mut attributions = CopilotMcpToolCallAttributions::new();
        for evidence in self.calls.into_values() {
            let (
                CopilotOccurrence::Unique(CopilotStartEvidence {
                    ordinal: start_ordinal,
                    record_digest: start_record_digest,
                    attribution: Some(attribution),
                }),
                CopilotOccurrence::Unique(CopilotCompletionEvidence {
                    ordinal: completion_ordinal,
                    record_digest: completion_record_digest,
                    exact: true,
                }),
            ) = (evidence.start, evidence.completion)
            else {
                continue;
            };
            if start_ordinal >= completion_ordinal {
                continue;
            }
            if !attributions.insert(
                start_ordinal,
                start_record_digest,
                completion_ordinal,
                completion_record_digest,
                attribution,
            ) {
                return CopilotMcpToolCallAttributions::new();
            }
        }
        attributions.bind_projection(projection_binding);
        attributions
    }
}

#[derive(Default)]
struct CopilotCallEvidence {
    start: CopilotOccurrence<CopilotStartEvidence>,
    completion: CopilotOccurrence<CopilotCompletionEvidence>,
    candidates: usize,
}

struct CopilotStartEvidence {
    ordinal: u64,
    record_digest: [u8; 32],
    attribution: Option<McpToolCallAttribution>,
}

struct CopilotCompletionEvidence {
    ordinal: u64,
    record_digest: [u8; 32],
    exact: bool,
}

#[derive(Default)]
enum CopilotOccurrence<T> {
    #[default]
    Missing,
    Unique(T),
    Ambiguous,
}

impl<T> CopilotOccurrence<T> {
    fn observe(&mut self, value: T) {
        *self = match std::mem::replace(self, Self::Missing) {
            Self::Missing => Self::Unique(value),
            Self::Unique(_) | Self::Ambiguous => Self::Ambiguous,
        };
    }
}

struct CopilotCandidate {
    tool_call_id: Option<String>,
    retained_bytes: usize,
    kind: CopilotCandidateKind,
}

enum CopilotCandidateKind {
    Start(Option<McpToolCallAttribution>),
    Completion { exact: bool },
}

enum CopilotParsedLine {
    Irrelevant,
    Candidate(CopilotCandidate),
    DisableSession,
}

fn parse_copilot_linkage_line(bytes: &[u8], limits: CopilotLinkageLimits) -> CopilotParsedLine {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let Ok(envelope) = CopilotRawEnvelope::deserialize(&mut deserializer) else {
        return CopilotParsedLine::DisableSession;
    };
    if deserializer.end().is_err() {
        return CopilotParsedLine::DisableSession;
    }
    if envelope.event_type.duplicate {
        return CopilotParsedLine::DisableSession;
    }
    let event_type = match exact_bounded_string(
        envelope.event_type.value,
        COPILOT_LINKAGE_MAX_EVENT_TYPE_BYTES,
    ) {
        ExactBoundedString::Exact(event_type) => event_type,
        ExactBoundedString::Invalid | ExactBoundedString::Exceeded => {
            return CopilotParsedLine::Irrelevant;
        }
    };
    let event_kind = match event_type.as_str() {
        "tool.execution_start" => CopilotEventKind::Start,
        "tool.execution_complete" => CopilotEventKind::Completion,
        _ => return CopilotParsedLine::Irrelevant,
    };
    if envelope.data.duplicate {
        return CopilotParsedLine::DisableSession;
    }
    let Some(raw_data) = envelope.data.value else {
        return invalid_candidate(event_kind);
    };
    let mut deserializer = serde_json::Deserializer::from_str(raw_data.get());
    let Ok(data) = CopilotRawData::deserialize(&mut deserializer) else {
        return CopilotParsedLine::DisableSession;
    };
    if deserializer.end().is_err() {
        return CopilotParsedLine::DisableSession;
    }
    if !data.object {
        return invalid_candidate(event_kind);
    }
    if data.tool_call_id.duplicate {
        return CopilotParsedLine::DisableSession;
    }
    let tool_call_id = match exact_bounded_string(data.tool_call_id.value, limits.max_call_id_bytes)
    {
        ExactBoundedString::Exact(tool_call_id) => Some(tool_call_id),
        ExactBoundedString::Invalid => None,
        ExactBoundedString::Exceeded => return CopilotParsedLine::DisableSession,
    };
    let mut retained_bytes = tool_call_id.as_ref().map_or(0, String::len);

    let kind = match event_kind {
        CopilotEventKind::Start => {
            let server = exact_bounded_string(
                data.mcp_server_name.value,
                MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES,
            );
            let tool = exact_bounded_string(
                data.mcp_tool_name.value,
                MAX_MCP_TOOL_CALL_ATTRIBUTION_COMPONENT_BYTES,
            );
            if matches!(server, ExactBoundedString::Exceeded)
                || matches!(tool, ExactBoundedString::Exceeded)
            {
                return CopilotParsedLine::DisableSession;
            }
            let attribution = match (server, tool) {
                (ExactBoundedString::Exact(server), ExactBoundedString::Exact(tool))
                    if !data.mcp_server_name.duplicate && !data.mcp_tool_name.duplicate =>
                {
                    retained_bytes = retained_bytes
                        .saturating_add(server.len())
                        .saturating_add(tool.len());
                    let attribution = McpToolCallAttribution { server, tool };
                    attribution.validate_contract().ok().map(|()| attribution)
                }
                (ExactBoundedString::Exact(server), ExactBoundedString::Invalid) => {
                    retained_bytes = retained_bytes.saturating_add(server.len());
                    None
                }
                (ExactBoundedString::Invalid, ExactBoundedString::Exact(tool)) => {
                    retained_bytes = retained_bytes.saturating_add(tool.len());
                    None
                }
                _ => None,
            };
            CopilotCandidateKind::Start(attribution)
        }
        CopilotEventKind::Completion => {
            let exact = !data.success.duplicate && exact_bool(data.success.value).is_some();
            CopilotCandidateKind::Completion { exact }
        }
    };
    CopilotParsedLine::Candidate(CopilotCandidate {
        tool_call_id,
        retained_bytes,
        kind,
    })
}

#[derive(Clone, Copy)]
enum CopilotEventKind {
    Start,
    Completion,
}

fn invalid_candidate(event_kind: CopilotEventKind) -> CopilotParsedLine {
    let kind = match event_kind {
        CopilotEventKind::Start => CopilotCandidateKind::Start(None),
        CopilotEventKind::Completion => CopilotCandidateKind::Completion { exact: false },
    };
    CopilotParsedLine::Candidate(CopilotCandidate {
        tool_call_id: None,
        retained_bytes: 0,
        kind,
    })
}

enum ExactBoundedString {
    Exact(String),
    Invalid,
    Exceeded,
}

fn exact_bounded_string(raw: Option<&RawValue>, maximum_bytes: usize) -> ExactBoundedString {
    let Some(raw) = raw else {
        return ExactBoundedString::Invalid;
    };
    let encoded = raw.get().as_bytes();
    if encoded.first() != Some(&b'"') {
        return ExactBoundedString::Invalid;
    }
    if encoded.len() > maximum_bytes.saturating_mul(6).saturating_add(2) {
        return ExactBoundedString::Exceeded;
    }
    let Ok(value) = serde_json::from_str::<String>(raw.get()) else {
        return ExactBoundedString::Invalid;
    };
    if value.len() > maximum_bytes {
        ExactBoundedString::Exceeded
    } else if value.is_empty() {
        ExactBoundedString::Invalid
    } else {
        ExactBoundedString::Exact(value)
    }
}

fn exact_bool(raw: Option<&RawValue>) -> Option<bool> {
    serde_json::from_str(raw?.get()).ok()
}

#[derive(Default)]
struct CopilotRawField<'a> {
    value: Option<&'a RawValue>,
    duplicate: bool,
}

impl<'a> CopilotRawField<'a> {
    fn observe(&mut self, value: &'a RawValue) {
        if self.value.is_some() {
            self.duplicate = true;
        } else {
            self.value = Some(value);
        }
    }
}

#[derive(Default)]
struct CopilotRawEnvelope<'a> {
    event_type: CopilotRawField<'a>,
    data: CopilotRawField<'a>,
}

impl<'de> Deserialize<'de> for CopilotRawEnvelope<'de> {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(CopilotRawEnvelopeVisitor)
    }
}

struct CopilotRawEnvelopeVisitor;

impl<'de> Visitor<'de> for CopilotRawEnvelopeVisitor {
    type Value = CopilotRawEnvelope<'de>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Copilot session event JSON value")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut envelope = CopilotRawEnvelope::default();
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "type" => envelope.event_type.observe(map.next_value::<&RawValue>()?),
                "data" => envelope.data.observe(map.next_value::<&RawValue>()?),
                _ => {
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }
        Ok(envelope)
    }

    fn visit_bool<E>(self, _value: bool) -> std::result::Result<Self::Value, E> {
        Ok(CopilotRawEnvelope::default())
    }

    fn visit_i64<E>(self, _value: i64) -> std::result::Result<Self::Value, E> {
        Ok(CopilotRawEnvelope::default())
    }

    fn visit_u64<E>(self, _value: u64) -> std::result::Result<Self::Value, E> {
        Ok(CopilotRawEnvelope::default())
    }

    fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E> {
        Ok(CopilotRawEnvelope::default())
    }

    fn visit_str<E>(self, _value: &str) -> std::result::Result<Self::Value, E> {
        Ok(CopilotRawEnvelope::default())
    }

    fn visit_string<E>(self, _value: String) -> std::result::Result<Self::Value, E> {
        Ok(CopilotRawEnvelope::default())
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(CopilotRawEnvelope::default())
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(CopilotRawEnvelope::default())
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<serde::de::IgnoredAny>()?.is_some() {}
        Ok(CopilotRawEnvelope::default())
    }
}

#[derive(Default)]
struct CopilotRawData<'a> {
    object: bool,
    tool_call_id: CopilotRawField<'a>,
    mcp_server_name: CopilotRawField<'a>,
    mcp_tool_name: CopilotRawField<'a>,
    success: CopilotRawField<'a>,
}

impl<'de> Deserialize<'de> for CopilotRawData<'de> {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(CopilotRawDataVisitor)
    }
}

struct CopilotRawDataVisitor;

impl<'de> Visitor<'de> for CopilotRawDataVisitor {
    type Value = CopilotRawData<'de>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Copilot tool event data value")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut data = CopilotRawData {
            object: true,
            ..CopilotRawData::default()
        };
        while let Some(key) = map.next_key::<String>()? {
            let value = match key.as_str() {
                "toolCallId" | "mcpServerName" | "mcpToolName" | "success" => {
                    map.next_value::<&RawValue>()?
                }
                _ => {
                    map.next_value::<serde::de::IgnoredAny>()?;
                    continue;
                }
            };
            match key.as_str() {
                "toolCallId" => data.tool_call_id.observe(value),
                "mcpServerName" => data.mcp_server_name.observe(value),
                "mcpToolName" => data.mcp_tool_name.observe(value),
                "success" => data.success.observe(value),
                _ => unreachable!("Copilot raw data key was filtered above"),
            }
        }
        Ok(data)
    }

    fn visit_bool<E>(self, _value: bool) -> std::result::Result<Self::Value, E> {
        Ok(CopilotRawData::default())
    }

    fn visit_i64<E>(self, _value: i64) -> std::result::Result<Self::Value, E> {
        Ok(CopilotRawData::default())
    }

    fn visit_u64<E>(self, _value: u64) -> std::result::Result<Self::Value, E> {
        Ok(CopilotRawData::default())
    }

    fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E> {
        Ok(CopilotRawData::default())
    }

    fn visit_str<E>(self, _value: &str) -> std::result::Result<Self::Value, E> {
        Ok(CopilotRawData::default())
    }

    fn visit_string<E>(self, _value: String) -> std::result::Result<Self::Value, E> {
        Ok(CopilotRawData::default())
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(CopilotRawData::default())
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(CopilotRawData::default())
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<serde::de::IgnoredAny>()?.is_some() {}
        Ok(CopilotRawData::default())
    }
}
