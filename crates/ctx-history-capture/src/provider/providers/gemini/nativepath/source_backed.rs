use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

mod projection;

use chrono::{DateTime, Utc};
use ctx_history_core::{
    CaptureProvider, CoreRecord, CoreRecordError, ProjectionContractError, RepositoryAbstention,
    RepositoryAbstentionReason, RepositoryEvidenceKind, RepositoryFileObservationKind, SourceKey,
    StableEntityId, TypedKey,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::dto::{GeminiEventBody, GeminiTranscriptLayout};
use super::parser::{read_gemini_session_header, GeminiBorrowedRecordParser};
use super::{
    discover_gemini_transcripts, GeminiFileObservation, GeminiScanError, GeminiSession,
    GeminiTranscriptSource,
};
use crate::{
    common::io::{OpenedProviderSourceFile, ProviderSourceRoot},
    provider::source_backed::{
        executable_route,
        family::jsonl::{
            jsonl_family_driver, JsonlFamilyAdapter, JsonlFamilyAppendMode, JsonlFamilyInventory,
            JsonlFamilyLeaf, JsonlFamilyProjector, JsonlRecordRef,
        },
        SourceBackedCoordinatorResult, SourceBackedProviderRegistry, SourceBackedRouteSelection,
        SourceBackedSelectorAuthority,
    },
    repository_attribution::{linked_outcome_evidence, LinkedOutcomeInput},
    CaptureError, OutputOutcome, GEMINI_CLI_SOURCE_FORMAT,
};
use projection::{gemini_event_id, gemini_session_id, gemini_source_key, project_event};

const GEMINI_SOURCE_ANCHOR_NAMESPACE: &str = "gemini.session";
const GEMINI_NATIVE_SESSION_NAMESPACE: &str = "gemini.session";
const GEMINI_NATIVE_EVENT_NAMESPACE: &str = "gemini.event";
const GEMINI_LOGICAL_SESSION_KIND: &str = "gemini-session";
const GEMINI_LOGICAL_EVENT_KIND: &str = "gemini-event";
const GEMINI_SOURCE_SCHEMA_VARIANT: &str = "gemini-nativepath-jsonl-v0";
const GEMINI_SOURCE_BACKED_PARSER_REVISION: &str =
    "gemini-nativepath-source-backed-v0-p8-p6-core-result-linkage";
const MAX_GEMINI_LEXICAL_METADATA_CHARS: usize = 8 * 1024;
const MAX_GEMINI_REPOSITORY_FIELD_CHARS: usize = 64 * 1024;
const MAX_GEMINI_TOOL_CONTEXTS: usize = 256;

pub(crate) mod registration {
    use super::*;
    use crate::ProviderSource;

    pub(crate) fn register(
        registry: &mut SourceBackedProviderRegistry,
        source: ProviderSource,
        selection: SourceBackedRouteSelection,
    ) -> SourceBackedCoordinatorResult<()> {
        let driver = jsonl_family_driver(gemini_jsonl_adapter(), source.path.clone());
        registry.register(executable_route(
            source,
            selection,
            SourceBackedSelectorAuthority::DiscoveredWinner,
            driver,
        )?);
        Ok(())
    }
}

#[derive(Debug, Error)]
pub(crate) enum GeminiSourceBackedError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Scan(#[from] GeminiScanError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    Core(#[from] CoreRecordError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub(crate) type GeminiSourceBackedResult<T> = Result<T, GeminiSourceBackedError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GeminiFamilyBinding {
    relative_path: PathBuf,
    layout: GeminiTranscriptLayout,
    observation: GeminiFileObservation,
    ordinary_file_token: [u8; 32],
    authority_relative_path: PathBuf,
    session: GeminiSession,
}

impl GeminiFamilyBinding {
    fn transcript(&self, leaf: &JsonlFamilyLeaf) -> GeminiTranscriptSource {
        GeminiTranscriptSource {
            path: leaf.source_path().to_path_buf(),
            relative_path: self.relative_path.clone(),
            layout: self.layout.clone(),
            observation: self.observation.clone(),
            ordinary_file_token: self.ordinary_file_token,
            authority_relative_path: self.authority_relative_path.clone(),
            authority: leaf.authority().as_ref().clone(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct GeminiJsonlAdapter;

fn gemini_jsonl_adapter() -> Arc<dyn JsonlFamilyAdapter> {
    Arc::new(GeminiJsonlAdapter)
}

impl JsonlFamilyAdapter for GeminiJsonlAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Gemini
    }

    fn source_format(&self) -> &'static str {
        GEMINI_CLI_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        GEMINI_SOURCE_SCHEMA_VARIANT
    }

    fn parser_revision(&self) -> &'static str {
        GEMINI_SOURCE_BACKED_PARSER_REVISION
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::Replacement
    }

    fn discover(&self, root: &Path) -> crate::Result<JsonlFamilyInventory> {
        let metadata = match fs::symlink_metadata(root) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return JsonlFamilyInventory::missing(self.provider(), root);
            }
            Err(error) => return Err(error.into()),
        };
        let discovery = discover_gemini_transcripts(root)?;
        if !discovery.completed_inventory {
            return Err(CaptureError::InvalidPayload(
                "Gemini discovery did not produce a complete inventory".to_owned(),
            ));
        }
        let authority = shared_authority(root, &metadata, &discovery.transcripts)?;
        let mut leaves = Vec::with_capacity(discovery.transcripts.len());
        for transcript in discovery.transcripts {
            if transcript.authority.named_path() != authority.named_path()
                || transcript.authority.authority_fingerprint() != authority.authority_fingerprint()
            {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            let session = read_gemini_session_header(&transcript).map_err(capture_scan_error)?;
            let source = gemini_source_key(&session.native_session_id).map_err(capture_error)?;
            let binding = GeminiFamilyBinding {
                relative_path: transcript.relative_path.clone(),
                layout: transcript.layout.clone(),
                observation: transcript.observation.clone(),
                ordinary_file_token: transcript.ordinary_file_token,
                authority_relative_path: transcript.authority_relative_path.clone(),
                session,
            };
            leaves.push(JsonlFamilyLeaf::observe(
                source,
                transcript.path,
                Arc::clone(&authority),
                transcript.authority_relative_path,
                TypedKey::bytes(serde_json::to_vec(&binding)?).map_err(contract_error)?,
            )?);
        }
        JsonlFamilyInventory::present(self.provider(), root, authority, leaves)
    }

    fn projector(
        &self,
        leaf: &JsonlFamilyLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> crate::Result<Box<dyn JsonlFamilyProjector>> {
        let binding = decode_binding(leaf)?;
        if source_file.ordinary_file_token() != binding.ordinary_file_token {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let expected_source =
            gemini_source_key(&binding.session.native_session_id).map_err(capture_error)?;
        if !expected_source.exact_descriptor_eq(leaf.source()) {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let session_id = gemini_session_id(leaf.source(), &binding.session.native_session_id)
            .map_err(capture_error)?;
        let parent_session_id = binding
            .session
            .parent_native_session_id
            .as_deref()
            .map(|parent_native_session_id| {
                let parent_source =
                    gemini_source_key(parent_native_session_id).map_err(capture_error)?;
                gemini_session_id(&parent_source, parent_native_session_id).map_err(capture_error)
            })
            .transpose()?;
        let root_session_id = parent_session_id.unwrap_or(session_id);
        let transcript = binding.transcript(leaf);
        Ok(Box::new(GeminiProjector {
            parser: GeminiBorrowedRecordParser::new(transcript, binding.session.clone()),
            source: leaf.source().clone(),
            session: binding.session,
            session_id,
            parent_session_id,
            root_session_id,
            source_file,
            authority: Arc::clone(leaf.authority()),
            repository_attributor: crate::repository_attribution::RepositoryAttributor::default(),
            tool_contexts: BTreeMap::new(),
            linkage_capacity_exceeded: false,
            native_item_ids: GeminiSourceNativeItemIds::default(),
            emitted_event_digests: BTreeSet::new(),
        }))
    }
}

struct GeminiProjector {
    parser: GeminiBorrowedRecordParser,
    source: SourceKey,
    session: GeminiSession,
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
    root_session_id: StableEntityId,
    source_file: Arc<OpenedProviderSourceFile>,
    authority: Arc<ProviderSourceRoot>,
    repository_attributor: crate::repository_attribution::RepositoryAttributor,
    tool_contexts: BTreeMap<String, GeminiToolContextState>,
    linkage_capacity_exceeded: bool,
    native_item_ids: GeminiSourceNativeItemIds,
    emitted_event_digests: BTreeSet<[u8; 32]>,
}

#[derive(Debug, Default)]
pub(super) struct GeminiSourceNativeItemIds {
    header_seen: bool,
    ids: BTreeSet<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiSourceNativeItemProbe {
    id: Option<String>,
    session_id: Option<String>,
}

impl GeminiSourceNativeItemIds {
    fn candidate(&mut self, payload: &[u8]) -> Option<String> {
        let Ok(probe) = serde_json::from_slice::<GeminiSourceNativeItemProbe>(payload) else {
            return None;
        };
        if probe
            .session_id
            .as_deref()
            .is_some_and(|session_id| !session_id.trim().is_empty())
        {
            self.header_seen = true;
            return None;
        }
        if !self.header_seen {
            return None;
        }
        probe.id.filter(|id| !id.trim().is_empty())
    }

    fn contains(&self, id: &str) -> bool {
        self.ids.contains(id)
    }

    fn remember(&mut self, id: Option<String>) {
        if let Some(id) = id {
            self.ids.insert(id);
        }
    }

    #[cfg(test)]
    pub(super) fn admit(&mut self, payload: &[u8]) -> bool {
        let candidate = self.candidate(payload);
        if candidate.as_deref().is_some_and(|id| self.contains(id)) {
            return false;
        }
        self.remember(candidate);
        true
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct GeminiToolContext {
    origin_call_id: Option<String>,
    origin_event_sequence: Option<u64>,
    command: Option<String>,
    command_too_large: bool,
    declared_workdir: Option<String>,
    file_paths: Vec<String>,
    ambiguous_native_fields: bool,
}

#[derive(Debug, Clone)]
enum GeminiToolContextState {
    Exact(GeminiToolContext),
    Ambiguous,
}

impl JsonlFamilyProjector for GeminiProjector {
    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        emit: &mut dyn FnMut(CoreRecord) -> crate::Result<()>,
    ) -> crate::Result<()> {
        let native_item_id = self.native_item_ids.candidate(record.bytes());
        if native_item_id
            .as_deref()
            .is_some_and(|id| self.native_item_ids.contains(id))
        {
            return Ok(());
        }
        let evidence = record.evidence();
        let events = self
            .parser
            .project(
                record.bytes(),
                evidence.physical_ordinal(),
                evidence.byte_start(),
                evidence.byte_end_exclusive(),
                evidence.record_digest(),
            )
            .map_err(capture_scan_error)?;
        if !events.is_empty() {
            self.native_item_ids.remember(native_item_id);
        }
        for event in events {
            let event_id =
                gemini_event_id(&self.source, self.session_id, &event).map_err(capture_error)?;
            if !self.emitted_event_digests.insert(event_id.digest()) {
                continue;
            }
            let annotation = self.attribution_for_event(&event);
            emit(
                project_event(
                    &self.source,
                    self.session_id,
                    self.parent_session_id,
                    self.root_session_id,
                    &self.session,
                    event,
                    annotation,
                )
                .map_err(capture_error)?,
            )?;
        }
        Ok(())
    }

    fn finish(&mut self) -> crate::Result<()> {
        self.parser.finish().map_err(capture_scan_error)?;
        self.source_file.revalidate_leaf()?;
        self.authority.revalidate()
    }
}

impl GeminiProjector {
    fn attribution_for_event(
        &mut self,
        event: &super::GeminiRetainedEvent,
    ) -> ctx_history_core::CoreRecordAnnotation {
        gemini_attribution_for_event(
            &mut self.repository_attributor,
            &self.session,
            &mut self.tool_contexts,
            &mut self.linkage_capacity_exceeded,
            event,
        )
    }
}

fn gemini_attribution_for_event(
    repository_attributor: &mut crate::repository_attribution::RepositoryAttributor,
    session: &GeminiSession,
    tool_contexts: &mut BTreeMap<String, GeminiToolContextState>,
    linkage_capacity_exceeded: &mut bool,
    event: &super::GeminiRetainedEvent,
) -> ctx_history_core::CoreRecordAnnotation {
    let structured_content = gemini_structured_content(event);
    let mut input = crate::repository_attribution::AttributionInput {
        activity_at_unix_ms: event
            .occurred_at
            .or(session.started_at)
            .map(|timestamp| timestamp.timestamp_millis()),
        session_cwd: session.cwd.clone(),
        structured_content,
        ..crate::repository_attribution::AttributionInput::default()
    };
    let mut adapter_abstentions = Vec::new();
    if session.cwd_ambiguous {
        input.provider_native_context_ambiguous = true;
        adapter_abstentions.push((
            RepositoryEvidenceKind::SessionCwd,
            RepositoryAbstentionReason::Ambiguous,
            "gemini_header_has_multiple_workspace_directories",
        ));
    }
    match &event.body {
        GeminiEventBody::ToolCall { calls } => {
            let contexts = calls
                .iter()
                .map(gemini_tool_call_context)
                .collect::<Vec<_>>();
            let combined = combine_gemini_tool_contexts(&contexts, &event.safe_file_touches);
            apply_gemini_context(&mut input, &combined);
            if combined.ambiguous_native_fields {
                input.provider_native_context_ambiguous = true;
                adapter_abstentions.push((
                    RepositoryEvidenceKind::DeclaredToolWorkdir,
                    RepositoryAbstentionReason::Ambiguous,
                    "gemini_tool_calls_do_not_share_one_exact_repository_context",
                ));
            }
            for (call, mut context) in calls.iter().zip(contexts) {
                let Some(call_id) = call
                    .id
                    .as_deref()
                    .filter(|call_id| call_id.chars().count() <= MAX_GEMINI_REPOSITORY_FIELD_CHARS)
                else {
                    adapter_abstentions.push((
                        RepositoryEvidenceKind::ProviderNativeResult,
                        RepositoryAbstentionReason::ProviderOutputUnjoined,
                        "gemini_tool_call_has_no_exact_result_link_id",
                    ));
                    continue;
                };
                context.origin_call_id = Some(call_id.to_owned());
                context.origin_event_sequence = gemini_event_sequence(event);
                if tool_contexts.contains_key(call_id) {
                    tool_contexts.insert(call_id.to_owned(), GeminiToolContextState::Ambiguous);
                } else if tool_contexts.len() < MAX_GEMINI_TOOL_CONTEXTS {
                    tool_contexts
                        .insert(call_id.to_owned(), GeminiToolContextState::Exact(context));
                } else {
                    *linkage_capacity_exceeded = true;
                }
            }
        }
        GeminiEventBody::OutputDiagnostic {
            result,
            call_id,
            command,
            command_too_large,
            declared_workdir,
            file_paths,
            ambiguous_native_fields,
            outcome,
            ..
        } => {
            let direct = GeminiToolContext {
                command: command.clone(),
                command_too_large: *command_too_large,
                declared_workdir: declared_workdir.clone(),
                file_paths: file_paths.clone(),
                ambiguous_native_fields: *ambiguous_native_fields,
                ..GeminiToolContext::default()
            };
            let linked = call_id
                .as_ref()
                .and_then(|call_id| tool_contexts.remove(call_id));
            let (context, linkage_exact) = match linked {
                Some(GeminiToolContextState::Exact(linked)) => {
                    merge_gemini_result_context(direct, linked)
                }
                Some(GeminiToolContextState::Ambiguous) => (direct, false),
                None => (direct, false),
            };
            apply_gemini_context(&mut input, &context);
            if context.ambiguous_native_fields {
                input.provider_native_context_ambiguous = true;
                adapter_abstentions.push((
                    RepositoryEvidenceKind::DeclaredToolWorkdir,
                    RepositoryAbstentionReason::Ambiguous,
                    "gemini_result_repository_fields_are_ambiguous",
                ));
            }
            if !linkage_exact {
                let (reason, detail) = if *linkage_capacity_exceeded {
                    (
                        RepositoryAbstentionReason::LinkageCapacityExceeded,
                        "gemini_tool_result_linkage_capacity_exceeded",
                    )
                } else {
                    (
                        RepositoryAbstentionReason::ProviderOutputUnjoined,
                        "gemini_result_has_no_exact_unique_call_link",
                    )
                };
                adapter_abstentions.push((
                    RepositoryEvidenceKind::ProviderNativeResult,
                    reason,
                    detail,
                ));
            }
            let result_outcome = match outcome.as_str() {
                "success" => OutputOutcome::Success,
                "failure" => OutputOutcome::Failure,
                "timeout" => OutputOutcome::Timeout,
                _ => OutputOutcome::Unknown,
            };
            if linkage_exact && result_outcome == OutputOutcome::Success {
                if let (
                    Some(command),
                    Some(origin_call_id),
                    Some(result_call_id),
                    Some(origin_event_sequence),
                    Some(result),
                ) = (
                    context.command.as_deref(),
                    context.origin_call_id.as_deref(),
                    call_id.as_deref(),
                    context.origin_event_sequence,
                    result.as_ref(),
                ) {
                    let structured_oid = result
                        .pointer("/gitOperation/commit/sha")
                        .and_then(serde_json::Value::as_str);
                    let output_workdir = result
                        .get("cwd")
                        .or_else(|| result.get("workdir"))
                        .and_then(serde_json::Value::as_str);
                    if let Some(linked) = linked_outcome_evidence(LinkedOutcomeInput {
                        provider: "gemini",
                        command,
                        session_cwd: input.session_cwd.as_deref(),
                        declared_workdir: context.declared_workdir.as_deref(),
                        origin_call_id,
                        result_call_id,
                        origin_event_sequence,
                        continuation_call_id_sha256: &[],
                        result_record_sha256: event.source_record.record_digest,
                        observed_at_unix_ms: input.activity_at_unix_ms.unwrap_or(0),
                        result_outcome,
                        result_output: result,
                        structured_commit_oid: structured_oid,
                        output_repository_path: output_workdir,
                    }) {
                        input.provider_native_repository_aliases =
                            linked.provider_native_repository_aliases;
                        input.outcome_operation_repository_path =
                            linked.outcome_operation_repository_path;
                        input.outcome_output_repository_path =
                            linked.outcome_output_repository_path;
                        input.outcome_observations = linked.outcomes;
                        input.outcome_abstentions = linked.abstentions;
                    }
                }
            }
            input.outcome_abstentions.extend(gemini_outcome_abstentions(
                &context,
                result_outcome,
                linkage_exact,
                result.is_some(),
            ));
        }
        GeminiEventBody::Message { .. }
        | GeminiEventBody::StateNotice { .. }
        | GeminiEventBody::RewindNotice { .. } => {}
    }
    let mut annotation = repository_attributor.attribute(input);
    append_adapter_abstentions(&mut annotation, adapter_abstentions);
    annotation
}

fn gemini_structured_content(event: &super::GeminiRetainedEvent) -> Option<serde_json::Value> {
    if matches!(&event.body, GeminiEventBody::Message { .. }) && event.safe_file_touches.is_empty()
    {
        return None;
    }
    let details = match &event.body {
        GeminiEventBody::Message { .. } => None,
        GeminiEventBody::OutputDiagnostic {
            result,
            call_id,
            tool_name,
            command,
            command_too_large,
            declared_workdir,
            file_paths,
            ambiguous_native_fields,
            outcome,
            exit_code,
            duration_ms,
        } => Some(serde_json::json!({
            "kind": "output_diagnostic",
            "complete_result": result.as_ref().map(|_| serde_json::json!({
                "location": "normalized_body",
                "retained_body_sha256": hex_digest(event.body_sha256),
                "source_record_sha256": hex_digest(event.source_record.record_digest),
            })),
            "call_id": call_id,
            "tool_name": tool_name,
            "command": command,
            "declared_workdir": declared_workdir,
            "file_paths": file_paths,
            "ambiguous_native_fields": ambiguous_native_fields,
            "command_too_large": command_too_large,
            "outcome": outcome,
            "exit_code": exit_code,
            "duration_ms": duration_ms,
        })),
        body => serde_json::to_value(body).ok(),
    };
    Some(serde_json::json!({
        "details": details,
        "file_touches": event.safe_file_touches,
    }))
}

fn hex_digest(digest: [u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
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
            association_policy_revision:
                ctx_history_core::CORE_REPOSITORY_ASSOCIATION_POLICY_REVISION,
        };
        if !annotation.repository_abstentions.contains(&abstention) {
            annotation.repository_abstentions.push(abstention);
        }
    }
}

fn gemini_tool_call_context(call: &super::dto::GeminiToolCall) -> GeminiToolContext {
    let mut context = GeminiToolContext::default();
    let Some(args) = call.args.as_ref() else {
        return context;
    };
    let Some(args) = args.as_object() else {
        context.ambiguous_native_fields = true;
        return context;
    };
    context.command = exact_json_command(
        args.get("command"),
        &mut context.command_too_large,
        &mut context.ambiguous_native_fields,
    );
    context.declared_workdir =
        exact_json_string(args.get("dir_path"), &mut context.ambiguous_native_fields);
    for key in ["path", "file_path", "filePath"] {
        if let Some(path) = exact_json_string(args.get(key), &mut context.ambiguous_native_fields) {
            context.file_paths.push(path);
        }
    }
    context
}

fn exact_json_command(
    value: Option<&serde_json::Value>,
    too_large: &mut bool,
    invalid: &mut bool,
) -> Option<String> {
    match value {
        Some(serde_json::Value::String(value))
            if value.len() > crate::repository_attribution::MAX_COMMAND_BYTES =>
        {
            *too_large = true;
            None
        }
        Some(serde_json::Value::String(value)) => Some(value.clone()),
        Some(_) => {
            *invalid = true;
            None
        }
        None => None,
    }
}

fn exact_json_string(value: Option<&serde_json::Value>, invalid: &mut bool) -> Option<String> {
    match value {
        Some(serde_json::Value::String(value))
            if value.chars().count() <= MAX_GEMINI_REPOSITORY_FIELD_CHARS =>
        {
            Some(value.clone())
        }
        Some(serde_json::Value::String(_)) => {
            *invalid = true;
            None
        }
        Some(_) => {
            *invalid = true;
            None
        }
        None => None,
    }
}

fn combine_gemini_tool_contexts(
    contexts: &[GeminiToolContext],
    file_paths: &[String],
) -> GeminiToolContext {
    let (mut command, mut command_ambiguous) =
        common_gemini_field(contexts, |context| context.command.as_deref());
    let command_too_large = contexts.iter().any(|context| context.command_too_large);
    if command_too_large {
        command_ambiguous |= contexts.iter().any(|context| context.command.is_some());
        command = None;
    }
    let (declared_workdir, workdir_ambiguous) =
        common_gemini_field(contexts, |context| context.declared_workdir.as_deref());
    GeminiToolContext {
        command,
        command_too_large,
        declared_workdir,
        file_paths: file_paths.to_vec(),
        ambiguous_native_fields: command_ambiguous
            || workdir_ambiguous
            || contexts
                .iter()
                .any(|context| context.ambiguous_native_fields),
        ..GeminiToolContext::default()
    }
}

fn common_gemini_field(
    contexts: &[GeminiToolContext],
    select: impl Fn(&GeminiToolContext) -> Option<&str>,
) -> (Option<String>, bool) {
    let Some(first) = contexts.first().and_then(&select) else {
        return (
            None,
            contexts
                .iter()
                .skip(1)
                .any(|context| select(context).is_some()),
        );
    };
    if contexts
        .iter()
        .all(|context| select(context) == Some(first))
    {
        (Some(first.to_owned()), false)
    } else {
        (None, true)
    }
}

fn merge_gemini_result_context(
    mut direct: GeminiToolContext,
    linked: GeminiToolContext,
) -> (GeminiToolContext, bool) {
    let mut exact = !direct.ambiguous_native_fields && !linked.ambiguous_native_fields;
    direct.origin_call_id = linked.origin_call_id;
    direct.origin_event_sequence = linked.origin_event_sequence;
    let direct_command = direct.command.take();
    match (
        direct.command_too_large,
        linked.command_too_large,
        direct_command,
        linked.command,
    ) {
        (true, true, _, _) | (true, false, _, None) | (false, true, None, _) => {
            direct.command_too_large = true;
        }
        (true, false, _, Some(_)) | (false, true, Some(_), _) => {
            direct.command_too_large = true;
            exact = false;
        }
        (false, false, None, Some(linked)) => direct.command = Some(linked),
        (false, false, Some(direct_command), Some(linked)) => {
            exact &= direct_command == linked;
            direct.command = Some(direct_command);
        }
        (false, false, Some(direct_command), None) => direct.command = Some(direct_command),
        (false, false, None, None) => {}
    }
    match (&direct.declared_workdir, linked.declared_workdir) {
        (None, Some(linked)) => direct.declared_workdir = Some(linked),
        (Some(direct_workdir), Some(linked)) if direct_workdir != &linked => exact = false,
        _ => {}
    }
    for path in linked.file_paths {
        if !direct.file_paths.contains(&path) {
            direct.file_paths.push(path);
        }
    }
    direct.ambiguous_native_fields |= !exact;
    (direct, exact)
}

fn apply_gemini_context(
    input: &mut crate::repository_attribution::AttributionInput,
    context: &GeminiToolContext,
) {
    input.command = context.command.clone();
    input.command_disposition = if context.command_too_large {
        crate::repository_attribution::CommandEvidenceDisposition::CommandTooLarge
    } else {
        crate::repository_attribution::CommandEvidenceDisposition::Analyze
    };
    input.declared_tool_workdir = context.declared_workdir.clone();
    input
        .file_observations
        .extend(context.file_paths.iter().cloned().map(|path| {
            crate::repository_attribution::UnscopedFileObservation {
                path,
                prior_path: None,
                kind: RepositoryFileObservationKind::Unknown,
            }
        }));
}

fn gemini_outcome_abstentions(
    context: &GeminiToolContext,
    outcome: OutputOutcome,
    linkage_exact: bool,
    has_exact_result: bool,
) -> Vec<(RepositoryAbstentionReason, &'static str)> {
    let Some(command) = context.command.as_deref() else {
        return Vec::new();
    };
    if !crate::repository_attribution::bounded_outcome_evidence_relevant(command) {
        return Vec::new();
    }
    if !linkage_exact {
        return vec![(
            RepositoryAbstentionReason::ProviderOutputUnjoined,
            "gemini_repository_outcome_has_no_exact_result_link",
        )];
    }
    if outcome != OutputOutcome::Success {
        return vec![(
            RepositoryAbstentionReason::OutcomeResultInadmissible,
            "recognized_gemini_outcome_command_did_not_succeed",
        )];
    }
    (!has_exact_result)
        .then_some((
            RepositoryAbstentionReason::OutcomeResultInadmissible,
            "gemini_result_has_no_exact_value",
        ))
        .into_iter()
        .collect()
}

fn gemini_event_sequence(event: &super::GeminiRetainedEvent) -> Option<u64> {
    event
        .native_order
        .raw_ordinal
        .checked_mul(u64::from(u32::MAX) + 1)
        .and_then(|sequence| sequence.checked_add(u64::from(event.native_order.sub_ordinal)))
}

fn shared_authority(
    root: &Path,
    metadata: &fs::Metadata,
    transcripts: &[GeminiTranscriptSource],
) -> crate::Result<Arc<ProviderSourceRoot>> {
    if let Some(transcript) = transcripts.first() {
        return Ok(Arc::new(transcript.authority.clone()));
    }
    let authority_path = if metadata.is_file() {
        root.parent()
            .ok_or(CaptureError::InvalidProviderTranscriptPath {
                path: root.to_path_buf(),
                reason: "Gemini transcript file has no parent authority",
            })?
    } else {
        root
    };
    Ok(Arc::new(ProviderSourceRoot::open(authority_path)?))
}

fn decode_binding(leaf: &JsonlFamilyLeaf) -> crate::Result<GeminiFamilyBinding> {
    let TypedKey::Bytes(bytes) = leaf.binding() else {
        return Err(CaptureError::InvalidPayload(
            "Gemini family leaf binding is malformed".to_owned(),
        ));
    };
    Ok(serde_json::from_slice(bytes)?)
}

#[cfg(test)]
pub(super) fn project_gemini_test_events(
    source: &GeminiTranscriptSource,
    events: Vec<super::GeminiRetainedEvent>,
) -> GeminiSourceBackedResult<Vec<CoreRecord>> {
    let session = read_gemini_session_header(source)?;
    let source_key = gemini_source_key(&session.native_session_id)?;
    let session_id = gemini_session_id(&source_key, &session.native_session_id)?;
    let parent_session_id = session
        .parent_native_session_id
        .as_deref()
        .map(|parent_native_session_id| {
            let parent_source = gemini_source_key(parent_native_session_id)?;
            gemini_session_id(&parent_source, parent_native_session_id)
        })
        .transpose()?;
    let root_session_id = parent_session_id.unwrap_or(session_id);
    let mut repository_attributor = crate::repository_attribution::RepositoryAttributor::default();
    let mut tool_contexts = BTreeMap::new();
    let mut linkage_capacity_exceeded = false;
    let mut emitted_event_digests = BTreeSet::new();
    events
        .into_iter()
        .filter_map(|event| {
            let event_id = match gemini_event_id(&source_key, session_id, &event) {
                Ok(event_id) => event_id,
                Err(error) => return Some(Err(error)),
            };
            if !emitted_event_digests.insert(event_id.digest()) {
                return None;
            }
            let annotation = gemini_attribution_for_event(
                &mut repository_attributor,
                &session,
                &mut tool_contexts,
                &mut linkage_capacity_exceeded,
                &event,
            );
            Some(project_event(
                &source_key,
                session_id,
                parent_session_id,
                root_session_id,
                &session,
                event,
                annotation,
            ))
        })
        .collect()
}

fn capture_scan_error(error: GeminiScanError) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

fn capture_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

fn contract_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}
