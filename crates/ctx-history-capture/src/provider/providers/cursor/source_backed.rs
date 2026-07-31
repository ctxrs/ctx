//! Thin Cursor adapter for the shared certified-append JSONL family.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::Path,
    sync::Arc,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, CoreRecord, EventIdentityInput,
    EventType, NativeItemKey, NativeSessionKey, PositionStability, RepositoryAbstention,
    RepositoryAbstentionReason, RepositoryEvidenceKind, RepositoryFileObservationKind,
    SessionIdentityInput, SourceAnchor, SourceKey, StableEntityId, SubrecordSelector, TypedKey,
};
use serde::{Deserialize, Serialize};

use super::{
    discover_cursor_transcripts,
    parser::project_cursor_jsonl_record,
    projection::{CursorEventBody, CursorNativeEvent},
};
use crate::{
    common::io::OpenedProviderSourceFile,
    provider::source_backed::family::jsonl::{
        JsonlFamilyAdapter, JsonlFamilyAppendMode, JsonlFamilyInventory, JsonlFamilyLeaf,
        JsonlFamilyProjector, JsonlRecordRef,
    },
    CaptureError, Result, CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
};

const SOURCE_ANCHOR_NAMESPACE: &str = "cursor.session";
const NATIVE_SESSION_NAMESPACE: &str = "cursor.session";
const NATIVE_EVENT_POSITION_KIND: &str = "cursor.physical-ordinal";
const NATIVE_SUBRECORD_POSITION_KIND: &str = "cursor.part-ordinal";
const LOGICAL_SESSION_KIND: &str = "cursor-session";
const LOGICAL_EVENT_KIND: &str = "cursor-event";
const SOURCE_SCHEMA_VARIANT: &str = "cursor-agent-transcript-jsonl-v1";
const PARSER_REVISION: &str = "cursor-shared-jsonl-v3-repository-attribution";
const EVENT_SEQUENCE_PARTS: u64 = u16::MAX as u64 + 1;
const MAX_CURSOR_TOOL_CONTEXTS: usize = 256;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CursorBinding {
    native_session_id: String,
}

#[derive(Debug, Clone, Copy)]
struct CursorJsonlAdapter;

pub(crate) fn cursor_jsonl_adapter() -> Arc<dyn JsonlFamilyAdapter> {
    Arc::new(CursorJsonlAdapter)
}

impl JsonlFamilyAdapter for CursorJsonlAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Cursor
    }

    fn source_format(&self) -> &'static str {
        CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        SOURCE_SCHEMA_VARIANT
    }

    fn parser_revision(&self) -> &'static str {
        PARSER_REVISION
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::CertifiedSuffix
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        match fs::symlink_metadata(root) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return JsonlFamilyInventory::missing(self.provider(), root);
            }
            Err(error) => return Err(error.into()),
        }
        let inventory = discover_cursor_transcripts(root);
        if !inventory.completed {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: root.to_path_buf(),
                reason: "Cursor transcript inventory could not be completed",
            });
        }
        let authority = Arc::new(
            inventory
                .authority()
                .ok_or(CaptureError::InvalidProviderTranscriptPath {
                    path: root.to_path_buf(),
                    reason: "Cursor discovery has no retained source authority",
                })?
                .clone(),
        );
        let mut native_sessions = BTreeSet::new();
        let mut leaves = Vec::with_capacity(inventory.transcripts.len());
        for transcript in inventory.transcripts {
            if transcript.authority().named_path() != authority.named_path()
                || transcript.authority().authority_fingerprint()
                    != authority.authority_fingerprint()
            {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            let native_session_id = transcript.native_session_id().to_owned();
            if !native_sessions.insert(native_session_id.clone()) {
                return Err(CaptureError::InvalidPayload(format!(
                    "Cursor native session ID {native_session_id:?} resolves more than once"
                )));
            }
            let source = source_key(&native_session_id)?;
            let binding = CursorBinding { native_session_id };
            leaves.push(JsonlFamilyLeaf::observe(
                source,
                transcript.path().to_path_buf(),
                Arc::clone(&authority),
                transcript.authority_relative_path().to_path_buf(),
                TypedKey::bytes(serde_json::to_vec(&binding)?).map_err(contract)?,
            )?);
        }
        JsonlFamilyInventory::present(self.provider(), root, authority, leaves)
    }

    fn projector(
        &self,
        leaf: &JsonlFamilyLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
        let binding = decode_binding(leaf)?;
        validate_binding(leaf, &binding, source_file.as_ref())?;
        let session_id = session_id(leaf.source(), &binding.native_session_id)?;
        Ok(Box::new(CursorProjector {
            source: leaf.source().clone(),
            native_session_id: binding.native_session_id,
            session_id,
            repository_attributor: crate::repository_attribution::RepositoryAttributor::default(),
            tool_contexts: BTreeMap::new(),
            linkage_capacity_exceeded: false,
        }))
    }
}

struct CursorProjector {
    source: SourceKey,
    native_session_id: String,
    session_id: StableEntityId,
    repository_attributor: crate::repository_attribution::RepositoryAttributor,
    tool_contexts: BTreeMap<String, CursorToolContextState>,
    linkage_capacity_exceeded: bool,
}

#[derive(Debug, Clone)]
struct CursorToolContext {
    command: Option<String>,
    declared_workdir: Option<String>,
    input_paths: Vec<String>,
}

#[derive(Debug, Clone)]
enum CursorToolContextState {
    Exact(CursorToolContext),
    Ambiguous,
}

impl JsonlFamilyProjector for CursorProjector {
    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        let evidence = record.evidence();
        let Some(events) = project_cursor_jsonl_record(
            record.bytes(),
            evidence.physical_ordinal(),
            evidence.physical_ordinal(),
            evidence.byte_start(),
            evidence.byte_end_exclusive(),
        )?
        else {
            return Ok(());
        };
        for event in events {
            let attribution = self.attribution_for_event(&event);
            if let Some(document) = core_record(
                &self.source,
                self.session_id,
                &self.native_session_id,
                event,
                attribution,
            )? {
                emit(document)?;
            }
        }
        Ok(())
    }
}

impl CursorProjector {
    fn attribution_for_event(
        &mut self,
        event: &CursorNativeEvent,
    ) -> ctx_history_core::CoreRecordAnnotation {
        let activity_at_unix_ms = event
            .occurred_at
            .map(|occurred_at| occurred_at.timestamp_millis());
        let structured_content = match &event.body {
            CursorEventBody::Text { .. } | CursorEventBody::None => None,
            body => serde_json::to_value(body).ok(),
        };
        let mut input = crate::repository_attribution::AttributionInput {
            activity_at_unix_ms,
            structured_content,
            ..crate::repository_attribution::AttributionInput::default()
        };
        let mut adapter_abstentions = Vec::new();
        match &event.body {
            CursorEventBody::ToolCall {
                call_id,
                command,
                declared_workdir,
                input_paths,
                ambiguous_native_fields,
                ..
            } => {
                let context = CursorToolContext {
                    command: command.clone(),
                    declared_workdir: declared_workdir.clone(),
                    input_paths: input_paths.clone(),
                };
                apply_cursor_context(&mut input, &context);
                if *ambiguous_native_fields {
                    adapter_abstentions.push((
                        RepositoryEvidenceKind::ProviderNativeResult,
                        RepositoryAbstentionReason::Ambiguous,
                        "cursor_tool_native_fields_are_ambiguous",
                    ));
                }
                if let Some(call_id) = call_id.as_ref() {
                    if self.tool_contexts.contains_key(call_id) {
                        self.tool_contexts
                            .insert(call_id.clone(), CursorToolContextState::Ambiguous);
                    } else if self.tool_contexts.len() < MAX_CURSOR_TOOL_CONTEXTS {
                        let state = if *ambiguous_native_fields {
                            CursorToolContextState::Ambiguous
                        } else {
                            CursorToolContextState::Exact(context)
                        };
                        self.tool_contexts.insert(call_id.clone(), state);
                    } else {
                        self.linkage_capacity_exceeded = true;
                    }
                } else {
                    adapter_abstentions.push((
                        RepositoryEvidenceKind::ProviderNativeResult,
                        RepositoryAbstentionReason::ProviderOutputUnjoined,
                        "cursor_tool_call_has_no_exact_result_link_id",
                    ));
                }
            }
            CursorEventBody::ToolOutput {
                call_id,
                ambiguous_linkage,
            } => {
                let context = if *ambiguous_linkage {
                    None
                } else {
                    call_id
                        .as_ref()
                        .and_then(|call_id| self.tool_contexts.remove(call_id))
                        .and_then(|state| match state {
                            CursorToolContextState::Exact(context) => Some(context),
                            CursorToolContextState::Ambiguous => None,
                        })
                };
                if let Some(context) = context {
                    apply_cursor_context(&mut input, &context);
                    input
                        .outcome_abstentions
                        .extend(cursor_outcome_abstentions(&context));
                } else {
                    let (reason, detail) = if self.linkage_capacity_exceeded {
                        (
                            RepositoryAbstentionReason::LinkageCapacityExceeded,
                            "cursor_tool_result_linkage_capacity_exceeded",
                        )
                    } else {
                        (
                            RepositoryAbstentionReason::ProviderOutputUnjoined,
                            "cursor_tool_result_has_no_exact_unique_call_link",
                        )
                    };
                    adapter_abstentions.push((
                        RepositoryEvidenceKind::ProviderNativeResult,
                        reason,
                        detail,
                    ));
                }
            }
            CursorEventBody::None | CursorEventBody::Text { .. } => {}
        }
        let mut annotation = self.repository_attributor.attribute(input);
        append_adapter_abstentions(&mut annotation, adapter_abstentions);
        annotation
    }
}

fn append_adapter_abstentions(
    annotation: &mut ctx_history_core::CoreRecordAnnotation,
    abstentions: Vec<(
        RepositoryEvidenceKind,
        RepositoryAbstentionReason,
        &'static str,
    )>,
) {
    for (evidence_kind, reason, detail) in abstentions {
        let abstention = RepositoryAbstention {
            evidence_kind,
            reason,
            detail: Some(detail.to_owned()),
            association_policy_revision: crate::repository_attribution::ASSOCIATION_POLICY_REVISION,
        };
        if !annotation.repository_abstentions.contains(&abstention) {
            annotation.repository_abstentions.push(abstention);
        }
    }
}

fn apply_cursor_context(
    input: &mut crate::repository_attribution::AttributionInput,
    context: &CursorToolContext,
) {
    input.command = context.command.clone();
    input.declared_tool_workdir = context.declared_workdir.clone();
    input
        .file_observations
        .extend(context.input_paths.iter().cloned().map(|path| {
            crate::repository_attribution::UnscopedFileObservation {
                path,
                prior_path: None,
                kind: RepositoryFileObservationKind::Unknown,
            }
        }));
}

fn cursor_outcome_abstentions(
    context: &CursorToolContext,
) -> Vec<(RepositoryAbstentionReason, &'static str)> {
    let Some(command) = context.command.as_deref() else {
        return Vec::new();
    };
    let base = context
        .declared_workdir
        .as_deref()
        .and_then(|path| crate::repository_attribution::lexical_absolute(path, None));
    let plan = match base.as_deref() {
        Some(base) => crate::repository_attribution::bounded_outcome_plan(command, base),
        None => {
            let provisional =
                crate::repository_attribution::bounded_outcome_plan(command, Path::new("/"));
            if matches!(
                provisional,
                crate::repository_attribution::BoundedOutcomePlanDisposition::Planned(_)
            ) {
                return vec![(
                    RepositoryAbstentionReason::OutcomeRepositoryUnbound,
                    "cursor_outcome_command_has_no_bounded_base",
                )];
            }
            provisional
        }
    };
    match plan {
        crate::repository_attribution::BoundedOutcomePlanDisposition::Unrecognized => Vec::new(),
        crate::repository_attribution::BoundedOutcomePlanDisposition::Abstained {
            reason,
            detail,
            ..
        } => vec![(reason, detail)],
        crate::repository_attribution::BoundedOutcomePlanDisposition::Planned(plan) => {
            let rewrite = matches!(
                plan.operation,
                crate::repository_attribution::BoundedOutcomeOperation::Commit {
                    rewrites_history: true,
                    ..
                }
            );
            if rewrite {
                vec![(
                    RepositoryAbstentionReason::HistoryRewriteUnlinked,
                    "cursor_result_has_no_exact_structured_replacement_lineage",
                )]
            } else {
                vec![(
                    RepositoryAbstentionReason::OutcomeResultInadmissible,
                    "cursor_result_has_no_exact_structured_repository_outcome",
                )]
            }
        }
    }
}

fn core_record(
    source: &SourceKey,
    session_id: StableEntityId,
    native_session_id: &str,
    event: CursorNativeEvent,
    annotation: ctx_history_core::CoreRecordAnnotation,
) -> Result<Option<CoreRecord>> {
    let text = match &event.body {
        CursorEventBody::Text { text }
            if event.event_type == EventType::Message && event.complete_content_ref.is_some() =>
        {
            text.clone()
        }
        CursorEventBody::ToolCall {
            tool_name,
            command,
            input_paths,
            ..
        } => {
            let mut body = format!(
                "Cursor {} tool call",
                tool_name.as_deref().unwrap_or("native")
            );
            if let Some(command) = command {
                body.push('\n');
                body.push_str(command);
            }
            for path in input_paths {
                body.push('\n');
                body.push_str(path);
            }
            body
        }
        CursorEventBody::ToolOutput { call_id, .. } => call_id.as_deref().map_or_else(
            || "Cursor tool result".to_owned(),
            |call_id| format!("Cursor tool result {call_id}"),
        ),
        CursorEventBody::None | CursorEventBody::Text { .. } => return Ok(None),
    };
    if text.is_empty() {
        return Ok(None);
    }
    let part_ordinal = event.native_order.part_ordinal;
    if part_ordinal > u32::from(u16::MAX) {
        return Err(CaptureError::InvalidPayload(
            "Cursor record exceeds the stable event-sequence part bound".to_owned(),
        ));
    }
    let event_id = event_id(
        source,
        session_id,
        event.native_order.semantic_ordinal,
        part_ordinal,
    )?;
    let native_event_key = TypedKey::composite(vec![
        TypedKey::U64(event.native_order.semantic_ordinal),
        TypedKey::U64(u64::from(part_ordinal)),
    ])
    .map_err(contract)?;
    let event_sequence = event
        .native_order
        .semantic_ordinal
        .checked_mul(EVENT_SEQUENCE_PARTS)
        .and_then(|base| base.checked_add(u64::from(part_ordinal)))
        .ok_or(CaptureError::SystemInvariant(
            "Cursor event sequence overflowed",
        ))?;
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        session_id,
        source.clone(),
        event_sequence,
        event.event_type.as_str(),
        AgentType::Primary.as_str(),
        true,
        PARSER_REVISION,
        text,
    )
    .map_err(contract)?;
    record.provider_session_id = Some(native_session_id.to_owned());
    record.native_event_id = Some(native_event_key);
    record.occurred_at_unix_ms = event
        .occurred_at
        .map(|occurred_at| occurred_at.timestamp_millis());
    record.role = Some(event.role.as_str().to_owned());
    record.content.structured_content = annotation.structured_content;
    record.metadata = annotation.metadata;
    record.repository_candidate_evidence = annotation.repository_candidate_evidence;
    record.repository_bindings = annotation.repository_bindings;
    record.repository_abstentions = annotation.repository_abstentions;
    record.repository_file_observations = annotation.repository_file_observations;
    record.repository_vcs_observations = annotation.repository_vcs_observations;
    record.validate_contract().map_err(contract)?;
    Ok(Some(record))
}

fn source_key(native_session_id: &str) -> Result<SourceKey> {
    let anchor = SourceAnchor::provider_native(
        SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(native_session_id).map_err(contract)?,
    )
    .map_err(contract)?;
    SourceKey::derive(
        CaptureProvider::Cursor.as_str(),
        CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
        SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )
    .map_err(contract)
}

fn session_id(source: &SourceKey, native_session_id: &str) -> Result<StableEntityId> {
    let native_session_key = NativeSessionKey::native_id(
        NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(native_session_id).map_err(contract)?,
    )
    .map_err(contract)?;
    derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })
    .map_err(contract)
}

fn event_id(
    source: &SourceKey,
    session_id: StableEntityId,
    semantic_ordinal: u64,
    part_ordinal: u32,
) -> Result<StableEntityId> {
    let native_item_key = NativeItemKey::certified_position(
        NATIVE_EVENT_POSITION_KIND,
        TypedKey::U64(semantic_ordinal),
        PositionStability::AppendStable,
    )
    .map_err(contract)?;
    let subrecord = SubrecordSelector::certified_position(
        NATIVE_SUBRECORD_POSITION_KIND,
        TypedKey::U64(u64::from(part_ordinal)),
        PositionStability::StableSlot,
    )
    .map_err(contract)?;
    derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: Some(&subrecord),
    })
    .map_err(contract)
}

fn validate_binding(
    leaf: &JsonlFamilyLeaf,
    binding: &CursorBinding,
    _source_file: &OpenedProviderSourceFile,
) -> Result<()> {
    if !source_key(&binding.native_session_id)?.exact_descriptor_eq(leaf.source()) {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    Ok(())
}

fn decode_binding(leaf: &JsonlFamilyLeaf) -> Result<CursorBinding> {
    let TypedKey::Bytes(bytes) = leaf.binding() else {
        return Err(contract("Cursor family binding is malformed"));
    };
    Ok(serde_json::from_slice(bytes)?)
}

fn contract(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(format!("Cursor source-backed contract is invalid: {error}"))
}

#[cfg(test)]
mod repository_tests {
    use std::{fs, path::PathBuf, process::Command};

    use tempfile::TempDir;

    use super::*;

    fn repository(temp: &TempDir) -> PathBuf {
        let path = temp.path().join("repo");
        fs::create_dir(&path).unwrap();
        assert!(Command::new("git")
            .args(["init", "-q"])
            .current_dir(&path)
            .status()
            .unwrap()
            .success());
        fs::create_dir(path.join("src")).unwrap();
        fs::write(path.join("src/lib.rs"), "pub fn native() {}\n").unwrap();
        path
    }

    fn event(value: &str, ordinal: u64) -> CursorNativeEvent {
        project_cursor_jsonl_record(value.as_bytes(), ordinal, ordinal, 0, value.len() as u64)
            .unwrap()
            .unwrap()
            .into_iter()
            .next()
            .unwrap()
    }

    fn projector() -> CursorProjector {
        let native_session_id = "cursor-native-contract-test".to_owned();
        let source = source_key(&native_session_id).unwrap();
        let session_id = session_id(&source, &native_session_id).unwrap();
        CursorProjector {
            source,
            native_session_id,
            session_id,
            repository_attributor: crate::repository_attribution::RepositoryAttributor::default(),
            tool_contexts: BTreeMap::new(),
            linkage_capacity_exceeded: false,
        }
    }

    fn has_reason(
        annotation: &ctx_history_core::CoreRecordAnnotation,
        reason: RepositoryAbstentionReason,
    ) -> bool {
        annotation
            .repository_abstentions
            .iter()
            .any(|abstention| abstention.reason == reason)
    }

    #[test]
    fn cursor_exact_native_tool_fields_and_result_id_bind_without_fabricating_outcomes() {
        let temp = TempDir::new().unwrap();
        let repo = repository(&temp);
        let call = serde_json::json!({
            "timestamp": "2026-07-31T12:00:00Z",
            "role": "assistant",
            "message": {"role": "assistant", "content": [{
                "type": "tool_use",
                "id": "call-1",
                "name": "run_shell_command",
                "input": {
                    "command": "git commit -m bounded",
                    "workdir": repo,
                    "path": "src/lib.rs"
                }
            }]}
        })
        .to_string();
        let result = r#"{"timestamp":"2026-07-31T12:00:01Z","role":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call-1","content":"untrusted prose oid deadbeef"}]}}"#;
        let mut projector = projector();

        let call_annotation = projector.attribution_for_event(&event(&call, 1));
        assert_eq!(call_annotation.repository_bindings.len(), 1);
        assert_eq!(
            call_annotation
                .repository_candidate_evidence
                .declared_tool_workdir
                .as_deref(),
            Some(repo.to_string_lossy().as_ref())
        );
        assert_eq!(
            call_annotation.repository_file_observations[0].relative_path,
            "src/lib.rs"
        );

        let result_annotation = projector.attribution_for_event(&event(result, 2));
        assert_eq!(result_annotation.repository_bindings.len(), 1);
        assert!(result_annotation.repository_vcs_observations.is_empty());
        assert!(has_reason(
            &result_annotation,
            RepositoryAbstentionReason::OutcomeResultInadmissible
        ));
        assert!(!has_reason(
            &result_annotation,
            RepositoryAbstentionReason::ProviderOutputUnjoined
        ));
    }

    #[test]
    fn cursor_dynamic_ambiguous_and_rewrite_evidence_abstains_with_typed_reasons() {
        let temp = TempDir::new().unwrap();
        let repo = repository(&temp);
        let mut projector = projector();
        let dynamic = serde_json::json!({
            "role": "assistant",
            "message": {"role": "assistant", "content": [{
                "type": "tool_use",
                "id": "dynamic",
                "name": "run_shell_command",
                "input": {"command": "cd $REPO && git status", "path": "$REPO/src/lib.rs"}
            }]}
        })
        .to_string();
        let dynamic_annotation = projector.attribution_for_event(&event(&dynamic, 1));
        assert!(dynamic_annotation.repository_bindings.is_empty());
        assert!(has_reason(
            &dynamic_annotation,
            RepositoryAbstentionReason::DynamicPath
        ));

        let rewrite = serde_json::json!({
            "role": "assistant",
            "message": {"role": "assistant", "content": [{
                "type": "tool_use",
                "id": "rewrite",
                "name": "run_shell_command",
                "input": {"command": "git commit --amend --no-edit", "workdir": repo}
            }]}
        })
        .to_string();
        projector.attribution_for_event(&event(&rewrite, 2));
        let rewrite_result = r#"{"role":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"rewrite","content":"success without structured replacement lineage"}]}}"#;
        let rewrite_annotation = projector.attribution_for_event(&event(rewrite_result, 3));
        assert!(rewrite_annotation.repository_vcs_observations.is_empty());
        assert!(has_reason(
            &rewrite_annotation,
            RepositoryAbstentionReason::HistoryRewriteUnlinked
        ));

        let pr = serde_json::json!({
            "role": "assistant",
            "message": {"role": "assistant", "content": [{
                "type": "tool_use",
                "id": "pr",
                "name": "run_shell_command",
                "input": {
                    "command": "gh pr create --title bounded --body bounded",
                    "workdir": repo
                }
            }]}
        })
        .to_string();
        projector.attribution_for_event(&event(&pr, 4));
        let pr_result = r#"{"role":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"pr","content":"URL prose is not structured outcome authority"}]}}"#;
        let pr_annotation = projector.attribution_for_event(&event(pr_result, 5));
        assert!(pr_annotation.repository_vcs_observations.is_empty());
        assert!(has_reason(
            &pr_annotation,
            RepositoryAbstentionReason::OutcomeResultInadmissible
        ));

        let ambiguous_result = r#"{"role":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"rewrite","tool_use_id":"other","content":"ignored"}]}}"#;
        let ambiguous_annotation = projector.attribution_for_event(&event(ambiguous_result, 6));
        assert!(has_reason(
            &ambiguous_annotation,
            RepositoryAbstentionReason::ProviderOutputUnjoined
        ));
    }

    #[test]
    fn cursor_synthetic_native_contract_does_not_establish_real_history_parity() {
        let mut projector = projector();
        let relative_only = r#"{"role":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"call","name":"write_file","input":{"path":"src/unproven.rs"}}]}}"#;
        let annotation = projector.attribution_for_event(&event(relative_only, 1));
        assert!(annotation.repository_bindings.is_empty());
        assert!(annotation.repository_vcs_observations.is_empty());
        assert!(has_reason(
            &annotation,
            RepositoryAbstentionReason::UnscopedFileActivity
        ));
    }
}
