mod seam;

use std::{collections::HashSet, sync::Arc};

use chrono::{DateTime, Utc};
use ctx_history_capture_model::normalization::provider_value_text;
use ctx_history_capture_runtime::BaseEventLookup;
use ctx_history_core::{
    admit_optional_metadata_text, admit_optional_provider_call_id, admit_provider_declared_fact,
    derive_event_id, ActivityInvocation, ActivityJsonCapture, ActivityResult, ActivityTextCapture,
    AgentScope, CoreActivity, CoreContentPolicyStatus, CoreRecord, EventIdentityInput,
    LiteralFactKind, NativeItemKey, ProviderDeclaredFact, ProviderNativeSessionRelationship,
    SourceKey, TypedKey, CORE_ACTIVITY_REVISION, MAX_CORE_CONTENT_BYTES,
};
use serde_json::Value;
use sha2::{Digest, Sha256};

use ctx_history_jsonl::{
    fit_jsonl_activity, FallbackEventIdentityState, JsonlActivityObservedBytes,
    JsonlFamilyProjectionMode, JsonlFamilyProjector, JsonlFamilyWorkerContext, JsonlReader,
    JsonlRecordFraming, JsonlRecordRef, JsonlSourceIdentity,
};
use ctx_history_provider_runtime::{
    source_io::ProviderSourceRoot, CaptureError, ProviderBaseEventLookup, ProviderJsonlRuntime,
    ProviderRuntimeBinding, Result,
};

use crate::mux::normalization::{
    apply_mux_core_output_diagnostic, mux_core_event, mux_event_text, mux_event_type,
    mux_history_sequence, mux_message_timestamp_opt, mux_output_projection,
    mux_partial_event_index, mux_provider_event_id, mux_result_content, MuxMessageRow,
    MuxOutputProjection,
};

use super::{
    bound_stream, open_verified, optional_bound_stream, MuxBinding, MuxStreamKind,
    EVENT_IDENTITY_REVISION, LOGICAL_EVENT_KIND, MAX_EVENT_SEQUENCE_ORDINAL, PARSER_REVISION,
    PARTIAL_EVENT_SEQUENCE_BASE,
};
use seam::MuxArchiveSeam;

const NATIVE_ITEM_NAMESPACE: &str = "mux.record";
const FALLBACK_ITEM_NAMESPACE: &str = "mux.record.fallback";
const FALLBACK_FINGERPRINT_DOMAIN: &[u8] = b"ctx.mux.fallback-event-fingerprint.v1\0";

pub(super) struct MuxProjector<L: BaseEventLookup> {
    source: SourceKey,
    authority: Arc<ProviderSourceRoot>,
    binding: MuxBinding,
    fallback_identities: FallbackEventIdentityState<L, CaptureError>,
    seen_native_record_ids: HashSet<String>,
    next_history_sequence: u64,
    archive_seam: MuxArchiveSeam,
}

impl<L> MuxProjector<L>
where
    L: BaseEventLookup,
{
    pub(super) fn new(
        source: SourceKey,
        authority: Arc<ProviderSourceRoot>,
        binding: MuxBinding,
        mode: JsonlFamilyProjectionMode,
        base_event_lookup: Option<L>,
    ) -> Result<Self> {
        let fallback_identities = FallbackEventIdentityState::new(
            source.clone(),
            binding.session_id,
            LOGICAL_EVENT_KIND,
            FALLBACK_ITEM_NAMESPACE,
            EVENT_IDENTITY_REVISION,
            mode.into(),
            base_event_lookup,
        )?;
        Ok(Self {
            source,
            authority,
            binding,
            fallback_identities,
            seen_native_record_ids: HashSet::new(),
            next_history_sequence: 0,
            archive_seam: MuxArchiveSeam::new(),
        })
    }

    pub(super) fn project_record(
        &mut self,
        stream: MuxStreamKind,
        record: JsonlRecordRef<'_>,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        let bytes = record.bytes();
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(());
        }
        let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
            return Ok(());
        };
        if !value.is_object() {
            return Ok(());
        }
        if value
            .get("workspaceId")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .is_some_and(|owner| owner != self.binding.metadata.provider_session_id)
        {
            return Err(CaptureError::InvalidPayload(
                "Mux record changed its native session owner".to_owned(),
            ));
        }
        let evidence = record.evidence();
        let ordinal = evidence.physical_ordinal();
        if !stream.is_partial() && ordinal > MAX_EVENT_SEQUENCE_ORDINAL {
            return Err(CaptureError::InvalidPayload(
                "Mux source ordinal exceeds event identity capacity".to_owned(),
            ));
        }
        let history_sequence = mux_history_sequence(&value);
        if self
            .archive_seam
            .suppress_replayed_chat_row(stream, &value)?
        {
            return Ok(());
        }
        let output = mux_output_projection(&value);
        let content_omission = mux_output_content_omission(&value, output.as_ref());
        let event_sequence = if stream.is_partial() {
            PARTIAL_EVENT_SEQUENCE_BASE
                | (mux_partial_event_index(bytes) & MAX_EVENT_SEQUENCE_ORDINAL)
        } else {
            if self.next_history_sequence > MAX_EVENT_SEQUENCE_ORDINAL {
                return Err(CaptureError::InvalidPayload(
                    "Mux compound history exceeds event ordering capacity".to_owned(),
                ));
            }
            // Preserve a valid provider sequence when it remains available;
            // otherwise promote the row to the next free compound slot.
            let sequence = history_sequence
                .filter(|sequence| *sequence <= MAX_EVENT_SEQUENCE_ORDINAL)
                .filter(|sequence| *sequence >= self.next_history_sequence)
                .unwrap_or(self.next_history_sequence);
            self.next_history_sequence = sequence + 1;
            sequence
        };
        let native_record_id = mux_provider_event_id(&value, stream.is_partial());
        let (native_item_key, native_event_id) = match native_record_id {
            Some(native_record_id)
                if self.seen_native_record_ids.insert(native_record_id.clone()) =>
            {
                let native_event_id = TypedKey::utf8(native_record_id).map_err(contract)?;
                (
                    NativeItemKey::native_id(NATIVE_ITEM_NAMESPACE, native_event_id.clone())
                        .map_err(contract)?,
                    native_event_id,
                )
            }
            Some(_) | None => {
                let assignment = self
                    .fallback_identities
                    .assign(fallback_fingerprint(stream, bytes)?, None)?;
                (
                    assignment.native_item_key().clone(),
                    assignment.native_event_id().clone(),
                )
            }
        };
        let event_id = derive_event_id(EventIdentityInput {
            source: &self.source,
            session_id: self.binding.session_id,
            logical_item_kind: LOGICAL_EVENT_KIND,
            native_item_key: &native_item_key,
            subrecord_selector: None,
        })
        .map_err(contract)?;
        let row = MuxMessageRow { value };
        let occurred_at = mux_message_timestamp_opt(&row.value).unwrap_or_else(|| {
            self.binding
                .metadata
                .started_at
                .parse::<DateTime<Utc>>()
                .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
        });
        let mut event = mux_core_event(&row, occurred_at);
        if let Some(output) = output.as_ref() {
            apply_mux_core_output_diagnostic(&mut event, &row.value, output);
        }
        let body = match mux_exact_logical_content(&row.value) {
            Ok(body) => body,
            Err(_) if content_omission.is_some() => "Mux output content omitted".to_owned(),
            Err(error) => return Err(error),
        };
        if body.is_empty() {
            return Err(CaptureError::InvalidPayload(
                "Mux source-backed event has no exact lexical body".to_owned(),
            ));
        }
        let mut facts = Vec::new();
        if let Some(cwd) = &self.binding.metadata.cwd {
            if let Some(fact) =
                admit_provider_declared_fact(LiteralFactKind::SessionCwd, cwd.clone(), facts.len())
            {
                facts.push(fact);
            }
        }
        let activity = mux_activity(&row.value, facts);
        let mut record = CoreRecord::new_selected(
            event_id,
            self.binding.session_id,
            self.source.clone(),
            event_sequence,
            event.event_type.as_str(),
            PARSER_REVISION,
            body.clone(),
        )
        .map_err(contract)?;
        if !self.binding.metadata.lineage_ambiguous {
            if let Some(parent_session_id) = self.binding.parent_session_id {
                record.parent_session_id = Some(parent_session_id);
                record.session_relationship = Some(ProviderNativeSessionRelationship::Delegated);
                record.agent_scope = Some(AgentScope::Subagent);
            } else if self
                .binding
                .metadata
                .root_provider_session_id
                .as_deref()
                .is_none_or(|root| root == self.binding.metadata.provider_session_id)
            {
                record.agent_scope = Some(AgentScope::Primary);
            }
            record.root_session_id = self.binding.root_session_id;
        }
        record.provider_session_id = Some(self.binding.metadata.provider_session_id.clone());
        record.native_event_id = Some(native_event_id);
        record.occurred_at_unix_ms = Some(event.occurred_at.timestamp_millis());
        record.role = event.role.map(|role| role.as_str().to_owned());
        record.content.structured_content = Some(row.value);
        record.content.activity = activity;
        if let Some((kind, reason)) = content_omission {
            record.content.policy_status = CoreContentPolicyStatus::Omitted {
                reason: reason.to_owned(),
            };
            record.content.normalized_body = None;
            record.content.structured_content = None;
            let _ = kind;
        } else {
            fit_jsonl_activity(
                &body,
                record.content.structured_content.as_ref(),
                &mut record.content.activity,
                JsonlActivityObservedBytes::infer_from_present(),
                MAX_CORE_CONTENT_BYTES,
            );
            record
                .content
                .omit_provider_declared_facts_if_aggregate_exceeds_limit()
                .map_err(contract)?;
            record
                .content
                .omit_structured_content_if_aggregate_exceeds_limit()
                .map_err(contract)?;
        }
        record.validate_contract().map_err(contract)?;
        emit(record)
    }

    pub(super) fn project_bound_stream(
        &mut self,
        stream: MuxStreamKind,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        let bound = bound_stream(&self.binding, stream)?.clone();
        let source_file = open_verified(&self.authority, &bound)?;
        let path = self.authority.named_path().join(&bound.relative_path);
        let stream_variant = match stream {
            MuxStreamKind::Archive => "mux-bounded-archive-jsonl-v1",
            MuxStreamKind::Chat => "mux-bounded-chat-jsonl-v1",
            MuxStreamKind::Partial => "mux-bounded-partial-snapshot-v1",
        };
        let identity = JsonlSourceIdentity::new(
            "mux",
            PARSER_REVISION,
            stream_variant,
            self.source.exact_descriptor_digest(),
            path,
        );
        let mut reader = if stream.is_partial() {
            JsonlReader::open_whole_record(identity, source_file, None)?
        } else {
            JsonlReader::open_with_record_framing(
                identity,
                source_file,
                None,
                None,
                JsonlRecordFraming::ordinary(),
            )?
        };
        while reader
            .visit_page(&mut |record| self.project_record(stream, record, emit))?
            .is_some()
        {}
        if reader.outcome().is_none() {
            return Err(CaptureError::SystemInvariant(
                "Mux companion stream scan has no terminal evidence",
            ));
        }
        Ok(())
    }

    pub(super) fn finish(&self) -> Result<()> {
        self.fallback_identities.finish()
    }
}

fn mux_activity(value: &Value, facts: Vec<ProviderDeclaredFact>) -> Option<CoreActivity> {
    let dynamic_parts = value
        .get("parts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("dynamic-tool"))
        .collect::<Vec<_>>();
    let [part] = dynamic_parts.as_slice() else {
        return (!facts.is_empty()).then_some(CoreActivity {
            revision: CORE_ACTIVITY_REVISION,
            provider_call_id: None,
            invocation: None,
            result: None,
            facts,
        });
    };
    let call_ids = [
        "toolCallId",
        "tool_call_id",
        "callId",
        "call_id",
        "toolUseId",
        "tool_use_id",
        "id",
    ]
    .into_iter()
    .filter_map(|field| part.get(field).and_then(Value::as_str))
    .collect::<Vec<_>>();
    let provider_call_id = admit_optional_provider_call_id(match call_ids.as_slice() {
        [id] => Some((*id).to_owned()),
        _ => None,
    });
    let tool = ["toolName", "tool_name", "name"]
        .into_iter()
        .filter_map(|field| part.get(field).and_then(Value::as_str))
        .collect::<Vec<_>>();
    let tool = admit_optional_metadata_text(match tool.as_slice() {
        [tool] => Some((*tool).to_owned()),
        _ => None,
    });
    let invocation = if provider_call_id.is_some() {
        tool.map(|tool| ActivityInvocation {
            protocol: None,
            server: None,
            tool,
            arguments: part
                .get("input")
                .map_or(ActivityJsonCapture::Absent, |value| {
                    ActivityJsonCapture::Present {
                        value: value.clone(),
                    }
                }),
            started_at_unix_ms: None,
        })
    } else {
        None
    };
    let output_redacted = part.get("state").and_then(Value::as_str) == Some("output-redacted");
    let result = if provider_call_id.is_none() {
        None
    } else if output_redacted {
        Some(ActivityResult {
            status: None,
            completed_at_unix_ms: None,
            duration_ns: None,
            text: ActivityTextCapture::Unavailable,
            structured_content: ActivityJsonCapture::Unavailable,
        })
    } else {
        part.get("output").map(|output| ActivityResult {
            status: None,
            completed_at_unix_ms: None,
            duration_ns: None,
            text: output
                .as_str()
                .map_or(ActivityTextCapture::Absent, |value| {
                    ActivityTextCapture::Present {
                        value: value.to_owned(),
                    }
                }),
            structured_content: ActivityJsonCapture::Present {
                value: output.clone(),
            },
        })
    };
    if invocation.is_none() && result.is_none() && facts.is_empty() {
        return None;
    }
    Some(CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id,
        invocation,
        result,
        facts,
    })
}

pub(super) struct MuxJsonlProjector<B: ProviderRuntimeBinding> {
    inner: MuxProjector<ProviderBaseEventLookup<B>>,
}

impl<B> MuxJsonlProjector<B>
where
    B: ProviderRuntimeBinding,
{
    pub(super) fn new(
        source: SourceKey,
        authority: Arc<ProviderSourceRoot>,
        binding: MuxBinding,
        mode: JsonlFamilyProjectionMode,
        base_event_lookup: Option<ProviderBaseEventLookup<B>>,
    ) -> Result<Self> {
        Ok(Self {
            inner: MuxProjector::new(source, authority, binding, mode, base_event_lookup)?,
        })
    }
}

fn mux_output_content_omission(
    value: &Value,
    output: Option<&MuxOutputProjection>,
) -> Option<(&'static str, &'static str)> {
    output.filter(|output| !output.body_available)?;
    let explicitly_redacted = value
        .get("parts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|part| part.get("state").and_then(Value::as_str) == Some("output-redacted"));
    if explicitly_redacted {
        Some((
            "explicit_redaction",
            "Mux provider marked the tool output as redacted",
        ))
    } else {
        Some((
            "provider_private_framing",
            "Mux output framing contains no admitted textual or structured result",
        ))
    }
}

impl<B> JsonlFamilyProjector for MuxJsonlProjector<B>
where
    B: ProviderRuntimeBinding,
{
    type Runtime = ProviderJsonlRuntime<B>;

    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        _worker: &mut JsonlFamilyWorkerContext<Self::Runtime>,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        let stream = self.inner.binding.primary_stream;
        self.inner.project_record(stream, record, emit)
    }

    fn finish_projecting(
        &mut self,
        _worker: &mut JsonlFamilyWorkerContext<Self::Runtime>,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        let trailing_streams: &[MuxStreamKind] = match self.inner.binding.primary_stream {
            MuxStreamKind::Archive => &[MuxStreamKind::Chat, MuxStreamKind::Partial],
            MuxStreamKind::Chat => &[MuxStreamKind::Partial],
            MuxStreamKind::Partial => &[],
        };
        for stream in trailing_streams {
            if optional_bound_stream(&self.inner.binding, *stream).is_some() {
                self.inner.project_bound_stream(*stream, emit)?;
            }
        }
        self.inner.finish()
    }
}

fn fallback_fingerprint(stream: MuxStreamKind, bytes: &[u8]) -> Result<TypedKey> {
    let mut digest = Sha256::new();
    digest.update(FALLBACK_FINGERPRINT_DOMAIN);
    // Archive and active chat are one durable history stream. Keep their
    // fallback identity domain identical so rotation cannot churn event IDs;
    // the staged partial remains deliberately separate.
    digest.update([u8::from(stream.is_partial())]);
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
    TypedKey::bytes(digest.finalize().to_vec()).map_err(contract)
}

fn mux_exact_logical_content(value: &Value) -> Result<String> {
    let event_type = mux_event_type(value);
    if matches!(
        event_type,
        ctx_history_core::EventType::ToolOutput | ctx_history_core::EventType::CommandOutput
    ) {
        return mux_result_content(value).ok_or_else(|| {
            CaptureError::InvalidPayload("Mux exact output body is unavailable".to_owned())
        });
    }
    let mut rendered = Vec::new();
    if let Some(parts) = value.get("parts").and_then(Value::as_array) {
        for part in parts {
            match part.get("type").and_then(Value::as_str) {
                Some("text" | "reasoning") => {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        rendered.push(text.to_owned());
                    }
                }
                Some("dynamic-tool") => rendered.push(exact_tool_part_text(part)),
                Some("file") => {
                    if let Some(label) = exact_file_part_text(part) {
                        rendered.push(label);
                    }
                }
                _ => {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        rendered.push(text.to_owned());
                    }
                }
            }
        }
    }
    if !rendered.is_empty() {
        return Ok(rendered.join("\n"));
    }
    if let Some(text) = value
        .get("content")
        .or_else(|| value.get("message"))
        .and_then(provider_value_text)
    {
        return Ok(text);
    }
    Ok(mux_event_text(value, event_type))
}

fn exact_tool_part_text(part: &Value) -> String {
    let name = part
        .get("toolName")
        .or_else(|| part.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("tool");
    let state = part.get("state").and_then(Value::as_str);
    let prefix = if matches!(state, Some("output-available" | "output-redacted"))
        || part.get("output").is_some()
    {
        "tool output"
    } else {
        "tool call"
    };
    let mut text = format!("{prefix}: {name}");
    if let Some(input) = part.get("input") {
        text.push_str("\ninput: ");
        text.push_str(&exact_value_text(input));
    }
    if let Some(output) = part.get("output") {
        text.push_str("\noutput: ");
        text.push_str(&exact_value_text(output));
    }
    if let Some(nested) = part.get("nestedCalls").and_then(Value::as_array) {
        let names = nested
            .iter()
            .filter_map(|call| {
                call.get("toolName")
                    .or_else(|| call.get("name"))
                    .and_then(Value::as_str)
            })
            .collect::<Vec<_>>();
        if !names.is_empty() {
            text.push_str("\nnested tools: ");
            text.push_str(&names.join(", "));
        }
    }
    text
}

fn exact_value_text(value: &Value) -> String {
    provider_value_text(value)
        .or_else(|| serde_json::to_string(value).ok())
        .unwrap_or_else(|| value.to_string())
}

fn exact_file_part_text(part: &Value) -> Option<String> {
    let label = part
        .get("filename")
        .or_else(|| part.get("name"))
        .or_else(|| part.get("mediaType"))
        .or_else(|| part.get("mimeType"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| {
            part.get("url")
                .and_then(Value::as_str)
                .filter(|url| !url.starts_with("data:") && url.len() < 256)
                .map(str::to_owned)
        })?;
    Some(format!("file: {label}"))
}
fn contract(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[derive(Clone)]
    struct EmptyLookup;

    impl ctx_history_capture_runtime::BaseEventLookup for EmptyLookup {
        type Error = std::convert::Infallible;

        fn contains(&self, _event_id: uuid::Uuid) -> std::result::Result<bool, Self::Error> {
            Ok(false)
        }
    }

    fn project_relationship_fixture(parent: Option<&str>) -> CoreRecord {
        project_lineage_fixture(parent, None)
    }

    fn project_lineage_fixture(parent: Option<&str>, root: Option<&str>) -> CoreRecord {
        let temp = tempfile::tempdir().unwrap();
        let provider_session_id = if parent.is_some() || root.is_some() {
            "mux-child"
        } else {
            "mux-root"
        };
        project_metadata_fixture(
            temp.path(),
            crate::mux::metadata::MuxBoundedSessionMetadata {
                provider_session_id: provider_session_id.to_owned(),
                parent_provider_session_id: parent.map(str::to_owned),
                root_provider_session_id: root.map(str::to_owned),
                lineage_ambiguous: false,
                started_at: "2026-08-05T12:00:00Z".to_owned(),
                cwd: Some("/workspace/mux".to_owned()),
                model: Some("mux-test".to_owned()),
                metadata_revision: "mux-test-metadata-v1".to_owned(),
                metadata_failure: None,
            },
            [7; 32],
        )
    }

    fn project_metadata_fixture(
        authority_path: &Path,
        metadata: crate::mux::metadata::MuxBoundedSessionMetadata,
        source_revision_digest: [u8; 32],
    ) -> CoreRecord {
        let provider_session_id = metadata.provider_session_id.clone();
        let source = super::super::source_key(&provider_session_id).unwrap();
        let session_id = super::super::session_identity(&source, &provider_session_id).unwrap();
        let parent_session_id = metadata
            .parent_provider_session_id
            .as_deref()
            .map(|parent| {
                super::super::related_session_identity(
                    parent,
                    ctx_history_core::SourceAnchorScope::Unqualified,
                )
            })
            .transpose()
            .unwrap();
        let root_session_id = metadata
            .root_provider_session_id
            .as_deref()
            .map(|root| {
                super::super::related_session_identity(
                    root,
                    ctx_history_core::SourceAnchorScope::Unqualified,
                )
            })
            .transpose()
            .unwrap();
        let binding = MuxBinding {
            metadata,
            session_id,
            parent_session_id,
            root_session_id,
            primary_stream: MuxStreamKind::Chat,
            archive: None,
            chat: None,
            partial: None,
            metadata_file: None,
            source_revision_digest,
        };
        let authority = Arc::new(ProviderSourceRoot::open(authority_path).unwrap());
        let mut projector = MuxProjector::<EmptyLookup>::new(
            source,
            authority,
            binding,
            JsonlFamilyProjectionMode::Cold,
            None,
        )
        .unwrap();
        let value = serde_json::json!({
            "id": "mux-child-event",
            "workspaceId": provider_session_id,
            "role": "user",
            "createdAt": "2026-08-05T12:00:01Z",
            "parts": [{"type": "text", "text": "exact child-owned Mux event"}]
        });
        let bytes = serde_json::to_vec(&value).unwrap();
        let mut emitted = Vec::new();
        projector
            .project_record(
                MuxStreamKind::Chat,
                JsonlRecordRef::for_test(&bytes, 0),
                &mut |record| {
                    emitted.push(record);
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(emitted.len(), 1);
        emitted.pop().unwrap()
    }

    #[test]
    fn optional_dynamic_tool_metadata_abstains_without_losing_valid_result_content() {
        let oversized = "x".repeat(64 * 1024 + 1);
        let invalid_id = serde_json::json!({
            "parts": [{
                "type": "dynamic-tool",
                "toolCallId": oversized,
                "toolName": "shell",
                "output": {"exact": true},
            }]
        });
        assert_eq!(mux_activity(&invalid_id, Vec::new()), None);

        let invalid_tool = serde_json::json!({
            "parts": [{
                "type": "dynamic-tool",
                "toolCallId": "call-1",
                "toolName": "x".repeat(64 * 1024 + 1),
                "output": {"exact": true},
            }]
        });
        let activity = mux_activity(&invalid_tool, Vec::new()).unwrap();
        assert!(activity.invocation.is_none());
        assert_eq!(
            activity.result.unwrap().structured_content,
            ActivityJsonCapture::Present {
                value: serde_json::json!({"exact": true}),
            }
        );
    }

    #[test]
    fn delegated_tasks_are_unique_while_root_events_are_primary() {
        let child = project_relationship_fixture(Some("mux-parent"));
        assert_eq!(
            child.session_relationship,
            Some(ProviderNativeSessionRelationship::Delegated)
        );
        assert_eq!(child.agent_scope, Some(AgentScope::Subagent));
        assert!(child.parent_session_id.is_some());
        assert_eq!(child.root_session_id, None);
        assert_eq!(
            child.content.meaningful_text(),
            "exact child-owned Mux event"
        );
        assert!(child.native_event_id.is_some());

        let explicit_root = project_lineage_fixture(Some("mux-parent"), Some("mux-explicit-root"));
        assert_eq!(explicit_root.parent_session_id, child.parent_session_id);
        assert!(explicit_root.root_session_id.is_some());

        let root = project_relationship_fixture(None);
        assert_eq!(root.session_relationship, None);
        assert_eq!(root.agent_scope, Some(AgentScope::Primary));
        assert_eq!(root.parent_session_id, None);
        assert_eq!(root.root_session_id, None);
        assert_eq!(
            root.content.meaningful_text(),
            "exact child-owned Mux event"
        );
        assert!(root.native_event_id.is_some());

        let unresolved_child = project_lineage_fixture(None, Some("mux-foreign-root"));
        assert_eq!(unresolved_child.session_relationship, None);
        assert_eq!(unresolved_child.agent_scope, None);
        assert!(unresolved_child.root_session_id.is_some());
    }

    #[test]
    fn contradictory_lineage_aliases_omit_relationship_claim() {
        let temp = tempfile::tempdir().unwrap();
        let native = crate::mux::source::MuxSessionSource {
            session_dir: temp.path().join("mux-child"),
            archive_path: None,
            chat_path: None,
            partial_path: None,
            metadata_path: None,
            provider_session_id: "mux-child".to_owned(),
            parent_provider_session_id: None,
        };
        let metadata = crate::mux::metadata::mux_bounded_session_metadata_from_bytes(
            &native,
            "mux-test-metadata-v2",
            "2026-08-05T12:00:00Z".parse().unwrap(),
            Some(
                &serde_json::to_vec(&serde_json::json!({
                    "workspaceId": "mux-child",
                    "parentWorkspaceId": "mux-parent",
                    "parentTaskId": "contradictory-parent",
                    "rootWorkspaceId": "mux-parent",
                    "rootTaskId": "contradictory-root"
                }))
                .unwrap(),
            ),
        )
        .unwrap();
        assert!(metadata.lineage_ambiguous);
        assert_eq!(
            metadata.parent_provider_session_id.as_deref(),
            Some("mux-parent")
        );
        assert_eq!(
            metadata.root_provider_session_id.as_deref(),
            Some("mux-parent")
        );

        let record = project_metadata_fixture(temp.path(), metadata, [8; 32]);
        assert_eq!(record.session_relationship, None);
        assert_eq!(record.agent_scope, None);
        assert_eq!(record.parent_session_id, None);
        assert_eq!(record.root_session_id, None);
    }

    #[test]
    fn duplicate_lineage_keys_project_unknown_without_edges() {
        let temp = tempfile::tempdir().unwrap();
        let native = crate::mux::source::MuxSessionSource {
            session_dir: temp.path().join("mux-child"),
            archive_path: None,
            chat_path: None,
            partial_path: None,
            metadata_path: None,
            provider_session_id: "mux-child".to_owned(),
            parent_provider_session_id: None,
        };
        let metadata = crate::mux::metadata::mux_bounded_session_metadata_from_bytes(
            &native,
            "mux-test-metadata-v3",
            "2026-08-05T12:00:00Z".parse().unwrap(),
            Some(
                br#"{
                    "workspaceId": "mux-child",
                    "parentSessionId": "mux-parent",
                    "parentSessionId": "conflicting-parent",
                    "rootSessionId": "mux-root"
                }"#,
            ),
        )
        .unwrap();
        assert!(metadata.lineage_ambiguous);

        let record = project_metadata_fixture(temp.path(), metadata, [9; 32]);
        assert_eq!(record.session_relationship, None);
        assert_eq!(record.agent_scope, None);
        assert_eq!(record.parent_session_id, None);
        assert_eq!(record.root_session_id, None);
    }

    fn duplicate_then_depth_exhausted_metadata() -> Vec<u8> {
        let depth = 256;
        let mut raw = br#"{
            "workspaceId": "mux-child",
            "parentSessionId": "metadata-parent",
            "parentSessionId": "duplicate-parent",
            "unrelated":
        "#
        .to_vec();
        raw.extend(std::iter::repeat_n(b'[', depth));
        raw.extend_from_slice(b"null");
        raw.extend(std::iter::repeat_n(b']', depth));
        raw.push(b'}');
        raw
    }

    #[test]
    fn failed_raw_lineage_audit_projects_unknown_with_or_without_path_parent() {
        let malformed = br#"{
            "workspaceId": "mux-child",
            "parentSessionId": "metadata-parent",
            "parentSessionId": "duplicate-parent",
            "unrelated":
        "#
        .to_vec();
        let depth_exhausted = duplicate_then_depth_exhausted_metadata();

        for (failure_kind, raw) in [
            ("malformed", malformed.as_slice()),
            ("depth-exhausted", depth_exhausted.as_slice()),
        ] {
            for path_parent in [None, Some("mux-path-parent")] {
                let temp = tempfile::tempdir().unwrap();
                let native = crate::mux::source::MuxSessionSource {
                    session_dir: temp.path().join("mux-child"),
                    archive_path: None,
                    chat_path: None,
                    partial_path: None,
                    metadata_path: None,
                    provider_session_id: "mux-child".to_owned(),
                    parent_provider_session_id: path_parent.map(str::to_owned),
                };
                let metadata = crate::mux::metadata::mux_bounded_session_metadata_from_bytes(
                    &native,
                    &format!("mux-test-{failure_kind}"),
                    "2026-08-05T12:00:00Z".parse().unwrap(),
                    Some(raw),
                )
                .unwrap();
                assert!(metadata.lineage_ambiguous, "{failure_kind} {path_parent:?}");
                assert!(metadata.metadata_failure.is_some());
                assert_eq!(
                    metadata.parent_provider_session_id.as_deref(),
                    path_parent,
                    "{failure_kind}"
                );

                let record = project_metadata_fixture(temp.path(), metadata, [10; 32]);
                assert_eq!(record.agent_scope, None, "{failure_kind} {path_parent:?}");
                assert_eq!(record.session_relationship, None);
                assert_eq!(record.parent_session_id, None);
                assert_eq!(record.root_session_id, None);
                assert_eq!(
                    record.content.meaningful_text(),
                    "exact child-owned Mux event"
                );
            }
        }
    }

    #[test]
    fn provider_textual_result_over_16k_is_complete() {
        let tail = "mux_success_result_tail_complete";
        let output = format!("{} {tail}", "successful mux output ".repeat(800));
        assert!(output.len() > 16_000);
        let value = serde_json::json!({
            "role": "assistant",
            "parts": [{
                "type": "dynamic-tool",
                "toolName": "shell",
                "toolCallId": "complete-success",
                "state": "output-available",
                "output": output,
            }]
        });

        assert_eq!(mux_exact_logical_content(&value).unwrap(), output);
        assert!(
            mux_output_content_omission(&value, mux_output_projection(&value).as_ref()).is_none()
        );
    }

    #[test]
    fn explicit_redaction_has_truthful_omission_reason() {
        let value = serde_json::json!({
            "role": "assistant",
            "parts": [{
                "type": "dynamic-tool",
                "toolName": "shell",
                "toolCallId": "redacted",
                "state": "output-redacted",
            }]
        });
        assert_eq!(
            mux_output_content_omission(&value, mux_output_projection(&value).as_ref()),
            Some((
                "explicit_redaction",
                "Mux provider marked the tool output as redacted"
            ))
        );
    }
}
