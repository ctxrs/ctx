use std::mem::size_of;

use chrono::{DateTime, Utc};
use ctx_history_core::{EventRole, EventType, FileChangeKind, RepositoryFileObservationKind};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::record::{
    CodexDecodedRecord, CodexRecordClass, CodexRecordProbe, CodexResultKind, CodexRetainedKind,
    CodexStructuralOutput,
};
use crate::provider::codex::events::{
    codex_command_preview, codex_command_text, codex_content_text, codex_local_preview,
    codex_message_body, codex_provider_event, codex_result_content, codex_tool_arguments_preview,
    codex_tool_arguments_text, codex_tool_name, CodexNativeEvent, CodexToolCallContext,
};
use crate::{
    provider::codex::repository::{
        repository_tool_evidence, CodexRepositoryResultEvidence, CodexRepositoryToolEvidence,
    },
    repository_attribution::UnscopedFileObservation,
    CaptureError, OutputOutcome, OutputOutcomeMetadata, Result as CaptureResult,
    CODEX_SESSION_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS, PROVIDER_MAX_TEXT_CHARS,
};

const OWNED_ALLOCATION_OVERHEAD_BYTES: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CodexSessionRow {
    pub(crate) native_session_id: String,
    pub(crate) parent_native_session_id: Option<String>,
    pub(crate) root_native_session_id: Option<String>,
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) cwd: Option<String>,
    pub(crate) originator: Option<String>,
    pub(crate) cli_version: Option<String>,
    pub(crate) source_kind: Option<String>,
    pub(crate) external_agent_id: Option<String>,
    pub(crate) role_hint: Option<String>,
    pub(crate) model_provider: Option<String>,
    pub(crate) git: Option<CodexSessionGitMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CodexSessionGitMetadata {
    pub(crate) commit_hash: Option<String>,
    pub(crate) branch: Option<String>,
    pub(crate) repository_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CodexEventRow {
    pub(crate) provider_event: CodexNativeEvent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexSourceBackedRowV0 {
    pub(crate) raw_ordinal: u64,
    pub(crate) provider_event_identity: Option<CodexProviderEventIdentityV0>,
    pub(crate) occurred_at: DateTime<Utc>,
    pub(crate) event_type: EventType,
    pub(crate) role: Option<EventRole>,
    pub(crate) session_cwd: Option<String>,
    pub(crate) lexical_body: String,
    pub(crate) touched_paths: Vec<String>,
    pub(crate) repository_tools: Vec<CodexRepositoryToolEvidence>,
    pub(crate) repository_result: Option<CodexRepositoryResultEvidence>,
    pub(crate) repository_files: Vec<UnscopedFileObservation>,
}

impl CodexSourceBackedRowV0 {
    pub(crate) fn estimated_owned_bytes(&self) -> Option<usize> {
        let path_slots = self
            .touched_paths
            .capacity()
            .checked_mul(size_of::<String>())?;
        let path_bytes = self
            .touched_paths
            .iter()
            .try_fold(0_usize, |total, path| total.checked_add(path.capacity()))?;
        let repository_bytes = self
            .repository_tools
            .iter()
            .fold(0_usize, |total, evidence| {
                total
                    .saturating_add(
                        serde_json::to_vec(&evidence.structured_content)
                            .map_or(0, |value| value.len()),
                    )
                    .saturating_add(evidence.command.as_ref().map_or(0, String::capacity))
                    .saturating_add(
                        evidence
                            .declared_workdir
                            .as_ref()
                            .map_or(0, String::capacity),
                    )
                    .saturating_add(
                        evidence
                            .file_observations
                            .iter()
                            .map(|observation| observation.path.capacity())
                            .sum::<usize>(),
                    )
            });
        let repository_result_bytes = self.repository_result.as_ref().map_or(0, |evidence| {
            evidence.command.capacity()
                + evidence
                    .declared_workdir
                    .as_ref()
                    .map_or(0, String::capacity)
                + evidence
                    .outcome_operation_repository_path
                    .as_ref()
                    .map_or(0, String::capacity)
                + evidence
                    .outcome_output_repository_path
                    .as_ref()
                    .map_or(0, String::capacity)
                + serde_json::to_vec(&evidence.structured_content)
                    .map_or(0, |encoded| encoded.len())
                + serde_json::to_vec(&evidence.provider_native_repository_aliases)
                    .map_or(0, |encoded| encoded.len())
                + serde_json::to_vec(&evidence.outcomes).map_or(0, |encoded| encoded.len())
        });
        let repository_file_bytes =
            self.repository_files
                .iter()
                .try_fold(0_usize, |total, observation| {
                    total
                        .checked_add(observation.path.capacity())?
                        .checked_add(observation.prior_path.as_ref().map_or(0, String::capacity))
                })?;
        let allocation_count = 3_usize
            .checked_add(self.touched_paths.len())?
            .checked_add(self.repository_files.len())?;
        size_of::<Self>()
            .checked_add(
                self.provider_event_identity
                    .as_ref()
                    .map_or(0, |identity| identity.value.capacity()),
            )?
            .checked_add(self.lexical_body.capacity())?
            .checked_add(self.session_cwd.as_ref().map_or(0, String::capacity))?
            .checked_add(path_slots)?
            .checked_add(path_bytes)?
            .checked_add(repository_bytes)?
            .checked_add(repository_result_bytes)?
            .checked_add(repository_file_bytes)?
            .checked_add(allocation_count.checked_mul(OWNED_ALLOCATION_OVERHEAD_BYTES)?)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodexProviderEventIdentityKindV0 {
    Id,
    CallId,
}

impl CodexProviderEventIdentityKindV0 {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::CallId => "call_id",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexProviderEventIdentityV0 {
    pub(crate) kind: CodexProviderEventIdentityKindV0,
    pub(crate) value: String,
}

pub(super) struct CodexSourceBackedBuiltRowV0 {
    pub(super) row: CodexSourceBackedRowV0,
    pub(super) tool_context: Option<(String, CodexToolCallContext)>,
}

pub(super) enum CodexRetainedNonMaterialized {
    ValidUnmaterializable,
    Malformed,
}

pub(super) type CodexRetainedProjection =
    std::result::Result<CodexEventRow, CodexRetainedNonMaterialized>;

pub(super) fn build_event_row(
    raw_ordinal: u64,
    kind: CodexRetainedKind,
    retained: &CodexDecodedRecord,
) -> CaptureResult<CodexRetainedProjection> {
    let built = match kind {
        CodexRetainedKind::Message => build_message(&retained.payload),
        CodexRetainedKind::Reasoning => build_reasoning(&retained.payload),
        CodexRetainedKind::Compacted => build_compacted(&retained.payload),
        CodexRetainedKind::ToolCall => build_tool_call(&retained.payload),
    };
    let built = match built {
        BuiltBodyProjection::Materialized(built) => built,
        BuiltBodyProjection::ValidUnmaterializable => {
            return Ok(Err(CodexRetainedNonMaterialized::ValidUnmaterializable));
        }
        BuiltBodyProjection::Malformed => {
            return Ok(Err(CodexRetainedNonMaterialized::Malformed));
        }
    };
    let line_number = raw_ordinal
        .checked_add(1)
        .and_then(|line| usize::try_from(line).ok())
        .ok_or(CaptureError::SystemInvariant(
            "Codex NativePath raw ordinal exceeds platform limits",
        ))?;
    let provider_event = codex_provider_event(
        line_number,
        retained.occurred_at,
        built.event_type,
        built.role,
        built.body.clone(),
        json!({
            "source": "codex_session",
            "source_format": CODEX_SESSION_SOURCE_FORMAT,
            "line": line_number,
            "item_type": built.item_type,
            "tool": built.body.get("tool").and_then(Value::as_str),
            "source_record_ordinal": raw_ordinal,
            "source_record_subrecord_index": 0,
        }),
    );
    Ok(Ok(CodexEventRow { provider_event }))
}

pub(super) fn build_source_backed_event_row(
    raw_ordinal: u64,
    kind: CodexRetainedKind,
    retained: &CodexDecodedRecord,
) -> CaptureResult<std::result::Result<CodexSourceBackedBuiltRowV0, CodexRetainedNonMaterialized>> {
    let semantic = match source_backed_semantic_projection(kind, &retained.payload) {
        SourceBackedSemanticProjection::Materialized(semantic) => *semantic,
        SourceBackedSemanticProjection::ValidUnmaterializable => {
            return Ok(Err(CodexRetainedNonMaterialized::ValidUnmaterializable));
        }
        SourceBackedSemanticProjection::Malformed => {
            return Ok(Err(CodexRetainedNonMaterialized::Malformed));
        }
    };
    let repository_tools = repository_tool_evidence(&retained.payload);
    let mut tool_context = semantic.tool_context;
    if let (Some((call_id, context)), [evidence]) =
        (tool_context.as_mut(), repository_tools.as_slice())
    {
        context.tool_name.clone_from(&evidence.tool_name);
        context.exact_command.clone_from(&evidence.command);
        context
            .declared_workdir
            .clone_from(&evidence.declared_workdir);
        context
            .continuation_cell_id
            .clone_from(&evidence.continuation_cell_id);
        if evidence.command.is_some() {
            context.origin_call_id = Some(call_id.clone());
            context.origin_event_sequence = Some(raw_ordinal);
        }
    }
    Ok(Ok(CodexSourceBackedBuiltRowV0 {
        row: CodexSourceBackedRowV0 {
            raw_ordinal,
            provider_event_identity: provider_event_identity(&retained.payload),
            occurred_at: retained.occurred_at,
            event_type: semantic.event_type,
            role: semantic.role,
            session_cwd: None,
            lexical_body: semantic.lexical_body,
            touched_paths: Vec::new(),
            repository_tools,
            repository_result: None,
            repository_files: Vec::new(),
        },
        tool_context,
    }))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn build_source_backed_sparse_output_row(
    raw_ordinal: u64,
    provider_event_identity: Option<CodexProviderEventIdentityV0>,
    occurred_at: DateTime<Utc>,
    result_kind: CodexResultKind,
    context: Option<&CodexToolCallContext>,
    outcome: &OutputOutcomeMetadata,
    normalized_body: String,
    repository_result: Option<CodexRepositoryResultEvidence>,
    session_cwd: Option<String>,
) -> CaptureResult<Option<CodexSourceBackedRowV0>> {
    let diagnostic = matches!(
        outcome.outcome,
        crate::OutputOutcome::Failure | crate::OutputOutcome::Timeout
    );
    if !diagnostic && repository_result.is_none() {
        return Ok(None);
    }
    let tool_name = context
        .map(|context| context.tool_name.as_str())
        .unwrap_or_else(|| result_kind.item_type());
    let event_type = if crate::provider::codex::events::codex_is_command_tool(tool_name) {
        EventType::CommandOutput
    } else {
        EventType::ToolOutput
    };
    let lexical_body = if diagnostic {
        source_backed_lexical_body(
            EventType::ToolOutput,
            Some(EventRole::Tool),
            &normalized_body,
        )
    } else {
        "codex repository outcome result".to_owned()
    };
    Ok(Some(CodexSourceBackedRowV0 {
        raw_ordinal,
        provider_event_identity,
        occurred_at,
        event_type,
        role: Some(EventRole::Tool),
        session_cwd,
        lexical_body,
        touched_paths: Vec::new(),
        repository_tools: Vec::new(),
        repository_result,
        repository_files: Vec::new(),
    }))
}

pub(super) fn provider_event_identity(payload: &Value) -> Option<CodexProviderEventIdentityV0> {
    const MAX_PROVIDER_EVENT_ID_BYTES: usize = 64 * 1024;

    [
        (CodexProviderEventIdentityKindV0::Id, "id"),
        (CodexProviderEventIdentityKindV0::CallId, "call_id"),
    ]
    .into_iter()
    .find_map(|(kind, field)| {
        payload
            .get(field)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= MAX_PROVIDER_EVENT_ID_BYTES)
            .map(|value| CodexProviderEventIdentityV0 {
                kind,
                value: value.to_owned(),
            })
    })
}

pub(super) fn repository_file_kind(kind: Option<FileChangeKind>) -> RepositoryFileObservationKind {
    match kind {
        Some(FileChangeKind::Created) => RepositoryFileObservationKind::Created,
        Some(FileChangeKind::Read) => RepositoryFileObservationKind::Read,
        Some(FileChangeKind::Modified) => RepositoryFileObservationKind::Modified,
        Some(FileChangeKind::Deleted) => RepositoryFileObservationKind::Deleted,
        Some(FileChangeKind::Renamed) => RepositoryFileObservationKind::Renamed,
        _ => RepositoryFileObservationKind::Unknown,
    }
}

struct SourceBackedSemantic {
    event_type: EventType,
    role: Option<EventRole>,
    lexical_body: String,
    tool_context: Option<(String, CodexToolCallContext)>,
}

enum SourceBackedSemanticProjection {
    Materialized(Box<SourceBackedSemantic>),
    ValidUnmaterializable,
    Malformed,
}

/// The shared admission rule for Codex records with policy-selected text.
///
/// `Eligible` means the parser can emit complete normalized text immediately.
/// Known bookkeeping, encrypted/code-only, and ordinary non-diagnostic result
/// records are intentionally non-display. Exact repository-outcome result rows
/// use a fixed bounded display body and retain only their policy-selected
/// structured evidence. `ParserRevisionGap` is neither category: it prevents
/// publication when an admitted record reaches a newer or malformed shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CodexSourceBackedDocumentEligibility<T = ()> {
    Eligible(T),
    IntentionallyNonDisplay,
    ParserRevisionGap,
}

pub(super) fn source_backed_output_eligibility(
    result_kind: CodexResultKind,
    structural: &CodexStructuralOutput,
) -> CodexSourceBackedDocumentEligibility {
    if result_kind.is_eligible_output()
        && matches!(
            structural.outcome.outcome,
            OutputOutcome::Success | OutputOutcome::Failure | OutputOutcome::Timeout
        )
        && structural.has_exact_display_field
    {
        CodexSourceBackedDocumentEligibility::Eligible(())
    } else {
        CodexSourceBackedDocumentEligibility::IntentionallyNonDisplay
    }
}

fn source_backed_semantic_projection(
    kind: CodexRetainedKind,
    payload: &Value,
) -> SourceBackedSemanticProjection {
    match kind {
        CodexRetainedKind::Message => source_backed_message(payload),
        CodexRetainedKind::Reasoning => source_backed_reasoning(payload),
        CodexRetainedKind::Compacted => source_backed_compacted(payload),
        CodexRetainedKind::ToolCall => source_backed_tool_call(payload),
    }
}

pub(super) fn source_backed_display_text(
    probe: &CodexRecordProbe<'_>,
    payload: &Value,
) -> CodexSourceBackedDocumentEligibility<String> {
    match probe.class {
        CodexRecordClass::Retained(kind) => {
            match source_backed_semantic_projection(kind, payload) {
                SourceBackedSemanticProjection::Materialized(semantic) => {
                    CodexSourceBackedDocumentEligibility::Eligible(semantic.lexical_body)
                }
                SourceBackedSemanticProjection::ValidUnmaterializable => {
                    CodexSourceBackedDocumentEligibility::IntentionallyNonDisplay
                }
                SourceBackedSemanticProjection::Malformed => {
                    CodexSourceBackedDocumentEligibility::ParserRevisionGap
                }
            }
        }
        CodexRecordClass::ExcludedResult(result_kind) => {
            let Some(structural) = probe.output.as_ref() else {
                return if result_kind.is_eligible_output() {
                    CodexSourceBackedDocumentEligibility::ParserRevisionGap
                } else {
                    CodexSourceBackedDocumentEligibility::IntentionallyNonDisplay
                };
            };
            match source_backed_output_eligibility(result_kind, structural) {
                CodexSourceBackedDocumentEligibility::Eligible(()) => {
                    if structural.outcome.outcome == OutputOutcome::Success {
                        CodexSourceBackedDocumentEligibility::Eligible(
                            "codex repository outcome result".to_owned(),
                        )
                    } else {
                        match codex_result_content(payload) {
                            Some(content) => {
                                CodexSourceBackedDocumentEligibility::Eligible(content.into_owned())
                            }
                            None => CodexSourceBackedDocumentEligibility::ParserRevisionGap,
                        }
                    }
                }
                CodexSourceBackedDocumentEligibility::IntentionallyNonDisplay => {
                    CodexSourceBackedDocumentEligibility::IntentionallyNonDisplay
                }
                CodexSourceBackedDocumentEligibility::ParserRevisionGap => {
                    CodexSourceBackedDocumentEligibility::ParserRevisionGap
                }
            }
        }
        CodexRecordClass::SessionMeta
        | CodexRecordClass::TurnContext
        | CodexRecordClass::Ignored => {
            CodexSourceBackedDocumentEligibility::IntentionallyNonDisplay
        }
    }
}

fn source_backed_message(payload: &Value) -> SourceBackedSemanticProjection {
    let role_text = payload
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let role = match role_text {
        "user" => EventRole::User,
        "assistant" => EventRole::Assistant,
        "developer" | "system" => EventRole::System,
        _ => {
            return SourceBackedSemanticProjection::Malformed;
        }
    };
    let Some(text) = payload.get("content").and_then(codex_content_text) else {
        return SourceBackedSemanticProjection::Malformed;
    };
    SourceBackedSemanticProjection::Materialized(Box::new(SourceBackedSemantic {
        event_type: EventType::Message,
        role: Some(role),
        lexical_body: source_backed_lexical_body(EventType::Message, Some(role), &text),
        tool_context: None,
    }))
}

fn source_backed_reasoning(payload: &Value) -> SourceBackedSemanticProjection {
    let summary = payload
        .get("summary")
        .and_then(codex_content_text)
        .or_else(|| {
            payload
                .get("summary_text")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    let Some(summary) = summary else {
        return if is_encrypted_reasoning_without_plaintext(payload) {
            SourceBackedSemanticProjection::ValidUnmaterializable
        } else {
            SourceBackedSemanticProjection::Malformed
        };
    };
    SourceBackedSemanticProjection::Materialized(Box::new(SourceBackedSemantic {
        event_type: EventType::Summary,
        role: Some(EventRole::Assistant),
        lexical_body: source_backed_lexical_body(
            EventType::Summary,
            Some(EventRole::Assistant),
            &summary,
        ),
        tool_context: None,
    }))
}

fn source_backed_compacted(payload: &Value) -> SourceBackedSemanticProjection {
    let Some(text) = codex_content_text(payload) else {
        return if is_source_only_compacted(payload) {
            SourceBackedSemanticProjection::ValidUnmaterializable
        } else {
            SourceBackedSemanticProjection::Malformed
        };
    };
    SourceBackedSemanticProjection::Materialized(Box::new(SourceBackedSemantic {
        event_type: EventType::Summary,
        role: Some(EventRole::System),
        lexical_body: source_backed_lexical_body(
            EventType::Summary,
            Some(EventRole::System),
            &text,
        ),
        tool_context: None,
    }))
}

fn source_backed_tool_call(payload: &Value) -> SourceBackedSemanticProjection {
    let Some((text, tool_context)) = source_backed_tool_call_text(payload) else {
        return SourceBackedSemanticProjection::Malformed;
    };
    SourceBackedSemanticProjection::Materialized(Box::new(SourceBackedSemantic {
        event_type: EventType::ToolCall,
        role: Some(EventRole::Assistant),
        lexical_body: source_backed_lexical_body(
            EventType::ToolCall,
            Some(EventRole::Assistant),
            &text,
        ),
        tool_context,
    }))
}

fn source_backed_tool_call_text(
    payload: &Value,
) -> Option<(String, Option<(String, CodexToolCallContext)>)> {
    let item_type = payload.get("type").and_then(Value::as_str)?;
    let tool_name = codex_tool_name(payload, item_type);
    let call_id = payload.get("call_id").and_then(Value::as_str);
    let arguments = payload
        .get("arguments")
        .or_else(|| payload.get("input"))
        .or_else(|| payload.get("action"))
        .or_else(|| payload.get("execution"));
    let command_text = codex_command_text(&tool_name, arguments);
    let (arguments_text, _) = arguments
        .map(codex_tool_arguments_text)
        .unwrap_or_else(|| (String::new(), false));
    let repository_redacted = !repository_tool_evidence(payload).is_empty();
    let text = if repository_redacted {
        format!("{tool_name} tool call")
    } else {
        command_text
            .as_deref()
            .map(|command| format!("{tool_name}: {command}"))
            .unwrap_or_else(|| {
                if arguments_text.is_empty() {
                    format!("{tool_name} tool call")
                } else {
                    format!("{tool_name}: {arguments_text}")
                }
            })
    };
    let tool_context = call_id.map(|call_id| {
        (
            call_id.to_owned(),
            CodexToolCallContext {
                tool_name: tool_name.clone(),
                command_preview: (!repository_redacted)
                    .then(|| codex_command_preview(&tool_name, arguments))
                    .flatten(),
                arguments_preview: (!repository_redacted).then(|| {
                    arguments
                        .map(codex_tool_arguments_preview)
                        .map(|(preview, _, _)| preview)
                        .unwrap_or_default()
                }),
                ..CodexToolCallContext::default()
            },
        )
    });
    Some((text, tool_context))
}

pub(super) fn source_backed_lexical_body(
    event_type: EventType,
    role: Option<EventRole>,
    text: &str,
) -> String {
    let text = text.trim();
    if !text.is_empty() {
        return text.to_owned();
    }
    format!(
        "{} {}",
        event_type.as_str(),
        role.map(|role| role.as_str()).unwrap_or("event")
    )
}

pub(super) fn tool_context_from_row(row: &CodexEventRow) -> Option<(String, CodexToolCallContext)> {
    (row.provider_event.event_type == EventType::ToolCall).then_some(())?;
    let call_id = row
        .provider_event
        .payload
        .get("call_id")
        .and_then(Value::as_str)?
        .to_owned();
    let tool_name = row
        .provider_event
        .payload
        .get("tool")
        .and_then(Value::as_str)
        .unwrap_or("tool")
        .to_owned();
    let command_preview = row
        .provider_event
        .payload
        .get("command")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let arguments_preview = row
        .provider_event
        .payload
        .get("arguments_preview")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Some((
        call_id,
        CodexToolCallContext {
            tool_name,
            command_preview,
            arguments_preview,
            ..CodexToolCallContext::default()
        },
    ))
}

struct BuiltBody {
    event_type: EventType,
    role: Option<EventRole>,
    body: Value,
    item_type: String,
}

enum BuiltBodyProjection {
    Materialized(BuiltBody),
    ValidUnmaterializable,
    Malformed,
}

fn build_message(payload: &Value) -> BuiltBodyProjection {
    let Some((role, body)) = codex_message_body(payload) else {
        return BuiltBodyProjection::Malformed;
    };
    BuiltBodyProjection::Materialized(BuiltBody {
        event_type: EventType::Message,
        role: Some(role),
        body,
        item_type: "message".to_owned(),
    })
}

fn build_reasoning(payload: &Value) -> BuiltBodyProjection {
    let summary = payload
        .get("summary")
        .and_then(codex_content_text)
        .or_else(|| {
            payload
                .get("summary_text")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    let Some(summary) = summary else {
        return if is_encrypted_reasoning_without_plaintext(payload) {
            BuiltBodyProjection::ValidUnmaterializable
        } else {
            BuiltBodyProjection::Malformed
        };
    };
    let (summary, truncated) = codex_local_preview(&summary, PROVIDER_MAX_TEXT_CHARS);
    BuiltBodyProjection::Materialized(BuiltBody {
        event_type: EventType::Summary,
        role: Some(EventRole::Assistant),
        body: json!({
            "item_type": "reasoning",
            "summary": summary,
            "text": summary,
            "truncated": truncated,
            "encrypted_content_present": payload.get("encrypted_content").is_some(),
        }),
        item_type: "reasoning".to_owned(),
    })
}

fn is_encrypted_reasoning_without_plaintext(payload: &Value) -> bool {
    let Some(object) = payload.as_object() else {
        return false;
    };
    if object.get("type").and_then(Value::as_str) != Some("reasoning")
        || !object
            .get("encrypted_content")
            .and_then(Value::as_str)
            .is_some_and(|content| !content.is_empty())
    {
        return false;
    }
    let empty_summary = match object.get("summary") {
        None | Some(Value::Null) => true,
        Some(Value::Array(parts)) => parts.is_empty(),
        _ => false,
    };
    let empty_summary_text = matches!(object.get("summary_text"), None | Some(Value::Null));
    empty_summary && empty_summary_text
}

fn build_compacted(payload: &Value) -> BuiltBodyProjection {
    let Some(text) = codex_content_text(payload) else {
        return if is_source_only_compacted(payload) {
            BuiltBodyProjection::ValidUnmaterializable
        } else {
            BuiltBodyProjection::Malformed
        };
    };
    let (text, truncated) = codex_local_preview(&text, PROVIDER_MAX_TEXT_CHARS);
    BuiltBodyProjection::Materialized(BuiltBody {
        event_type: EventType::Summary,
        role: Some(EventRole::System),
        body: json!({
            "entry_type": "compacted",
            "text": text,
            "truncated": truncated,
        }),
        item_type: "compacted".to_owned(),
    })
}

fn is_source_only_compacted(payload: &Value) -> bool {
    let Some(object) = payload.as_object() else {
        return false;
    };
    object.get("message").is_some_and(Value::is_string)
        && object
            .get("replacement_history")
            .is_some_and(Value::is_array)
}

fn build_tool_call(payload: &Value) -> BuiltBodyProjection {
    let Some(item_type) = payload.get("type").and_then(Value::as_str) else {
        return BuiltBodyProjection::Malformed;
    };
    let tool_name = codex_tool_name(payload, item_type);
    let call_id = payload.get("call_id").and_then(Value::as_str);
    let arguments = payload
        .get("arguments")
        .or_else(|| payload.get("input"))
        .or_else(|| payload.get("action"))
        .or_else(|| payload.get("execution"));
    let command_preview = codex_command_preview(&tool_name, arguments);
    let (arguments_preview, arguments_truncated, raw_arguments_retained) = arguments
        .map(codex_tool_arguments_preview)
        .unwrap_or_else(|| (String::new(), false, false));
    let text = command_preview
        .as_deref()
        .map(|command| format!("{tool_name}: {command}"))
        .unwrap_or_else(|| {
            if arguments_preview.is_empty() {
                format!("{tool_name} tool call")
            } else {
                format!("{tool_name}: {arguments_preview}")
            }
        });
    let (text, text_truncated) = codex_local_preview(&text, PROVIDER_MAX_PREVIEW_CHARS);
    BuiltBodyProjection::Materialized(BuiltBody {
        event_type: EventType::ToolCall,
        role: Some(EventRole::Assistant),
        body: json!({
            "item_type": item_type,
            "tool": tool_name,
            "name": tool_name,
            "call_id": call_id,
            "command": command_preview,
            "arguments_preview": arguments_preview,
            "arguments_truncated": arguments_truncated,
            "raw_arguments_retained": raw_arguments_retained,
            "text": text,
            "truncated": text_truncated || arguments_truncated,
        }),
        item_type: item_type.to_owned(),
    })
}
