use std::{
    collections::HashSet,
    fs, io,
    marker::PhantomData,
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::{provider::source_backed::IndexBaseEventLookup, JsonlProviderRuntime};
use chrono::{DateTime, Utc};
use ctx_history_core::{
    admit_optional_metadata_text, admit_optional_provider_call_id, admit_provider_declared_fact,
    derive_event_id, derive_native_session_id, ActivityInvocation, ActivityJsonCapture,
    ActivityResult, ActivityTextCapture, AgentScope, CaptureProvider, CoreActivity, CoreRecord,
    EventIdentityInput, EventType, LiteralFactKind, NativeItemKey, ProviderDeclaredFact,
    SourceAnchorScope, SourceKey, StableEntityId, TypedKey, CORE_ACTIVITY_REVISION,
    MAX_CORE_CONTENT_BYTES,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    common::io::{OpenedProviderSourceFile, ProviderSourceRoot},
    provider::source_backed::{
        family::jsonl::{
            JsonlFamilyAdapter, JsonlFamilyAppendMode, JsonlFamilyInventory, JsonlFamilyLeaf,
            JsonlFamilyProjectionMode, JsonlFamilyProjector, JsonlFamilyWorkerContext,
            JsonlOversizedRecordPolicy, JsonlRecordRef, JsonlRecordRejections,
            SourceBackedRecordRejectionDrafts,
        },
        FallbackEventIdentityState,
    },
    CaptureError, Result, JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
};

use super::super::session_tree::{
    bounded_junie_index_meta, junie_provider_session_id, visit_junie_session_event_paths,
    JunieIndexMeta,
};
use super::projection::{EventDraft, JunieProjection};

const SOURCE_ANCHOR_NAMESPACE: &str = "junie.session-events";
const NATIVE_SESSION_NAMESPACE: &str = "junie.session";
const LOGICAL_SESSION_KIND: &str = "junie-session";
const LOGICAL_EVENT_KIND: &str = "junie-event";
const SOURCE_SCHEMA_VARIANT: &str = "junie-session-events-v2";
const PARSER_REVISION: &str =
    "junie-source-backed-v8-optional-activity-admission-record-rejections";
const EVENT_IDENTITY_REVISION: &str = "junie-content-occurrence-v2";
const FALLBACK_FINGERPRINT_DOMAIN: &[u8] = b"ctx.junie.fallback-event-fingerprint.v1\0";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JunieBinding {
    provider_session_id: String,
    session_id: StableEntityId,
    meta: JunieIndexMeta,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct JunieJsonlAdapter<R> {
    source_anchor_scope: SourceAnchorScope,
    runtime: PhantomData<fn() -> R>,
}

pub(crate) fn junie_jsonl_adapter<R: JsonlProviderRuntime>(
) -> Arc<dyn JsonlFamilyAdapter<Runtime = R>> {
    junie_jsonl_adapter_with_source_root_lineage(None)
}

pub(crate) fn junie_jsonl_adapter_with_source_root_lineage<R: JsonlProviderRuntime>(
    source_root_lineage: Option<[u8; 32]>,
) -> Arc<dyn JsonlFamilyAdapter<Runtime = R>> {
    Arc::new(JunieJsonlAdapter {
        source_anchor_scope: source_root_lineage
            .map_or(SourceAnchorScope::Unqualified, SourceAnchorScope::Lineage),
        runtime: PhantomData,
    })
}

impl<R: JsonlProviderRuntime> JsonlFamilyAdapter for JunieJsonlAdapter<R> {
    type Runtime = R;

    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Junie
    }

    fn source_format(&self) -> &'static str {
        JUNIE_SESSION_EVENTS_SOURCE_FORMAT
    }

    fn schema_variant(&self) -> &'static str {
        SOURCE_SCHEMA_VARIANT
    }

    fn parser_revision(&self) -> &'static str {
        PARSER_REVISION
    }

    fn event_identity_revision(&self) -> &'static str {
        EVENT_IDENTITY_REVISION
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::Replacement
    }

    fn oversized_record_policy(&self) -> JsonlOversizedRecordPolicy {
        JsonlOversizedRecordPolicy::RejectRecord
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory> {
        let metadata = match fs::symlink_metadata(root) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return JsonlFamilyInventory::missing(self.provider(), root);
            }
            Err(error) => return Err(error.into()),
        };
        if metadata.file_type().is_symlink() {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: root.to_path_buf(),
                reason: "Junie transcript roots must not be symbolic links",
            });
        }
        let absolute = std::path::absolute(root)?;
        let authority_path = if metadata.is_file() {
            absolute
                .parent()
                .ok_or(CaptureError::InvalidProviderTranscriptPath {
                    path: absolute.clone(),
                    reason: "Junie events file has no authority directory",
                })?
                .to_path_buf()
        } else {
            absolute
        };
        let authority = Arc::new(ProviderSourceRoot::open(&authority_path)?);
        let mut leaves = Vec::new();
        let mut sources = HashSet::new();
        let visit = visit_junie_session_event_paths(root, &mut |session, _| {
            let provider_session_id = junie_provider_session_id(&session)?;
            let source = source_key_scoped(&provider_session_id, self.source_anchor_scope)?;
            if !sources.insert(source.exact_descriptor_digest()) {
                return Err(CaptureError::InvalidPayload(format!(
                    "Junie native session {provider_session_id:?} resolves more than once"
                )));
            }
            let meta = bounded_junie_index_meta(&session.index_meta);
            let binding = JunieBinding {
                session_id: session_identity(&source, &provider_session_id)?,
                provider_session_id,
                meta,
            };
            let relative_path = relative_to_authority(&authority, &session.events_path)?;
            leaves.push(JsonlFamilyLeaf::observe(
                source,
                session.events_path,
                Arc::clone(&authority),
                relative_path,
                TypedKey::bytes(serde_json::to_vec(&binding)?).map_err(contract)?,
            )?);
            Ok(())
        })?;
        if visit.rejection_count != 0 {
            return Err(CaptureError::InvalidPayload(format!(
                "Junie session-tree discovery rejected {} index entries",
                visit.rejection_count
            )));
        }
        JsonlFamilyInventory::present(self.provider(), root, authority, leaves)
    }

    fn projector(
        &self,
        leaf: &JsonlFamilyLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
        imported_at: DateTime<Utc>,
    ) -> Result<Box<dyn JsonlFamilyProjector<Runtime = R>>> {
        self.projector_with_provider_checkpoint(
            leaf,
            source_file,
            imported_at,
            None,
            None,
            JsonlFamilyProjectionMode::Cold,
        )
    }

    fn projector_with_provider_checkpoint(
        &self,
        leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        imported_at: DateTime<Utc>,
        checkpoint: Option<&TypedKey>,
        base_event_lookup: Option<IndexBaseEventLookup<R>>,
        mode: JsonlFamilyProjectionMode,
    ) -> Result<Box<dyn JsonlFamilyProjector<Runtime = R>>> {
        if checkpoint.is_some() {
            return Err(CaptureError::InvalidPayload(
                "Junie adapter does not accept provider checkpoint state".to_owned(),
            ));
        }
        let binding = decode_binding(leaf)?;
        let workspace = binding.meta.project_dir.clone();
        let projection = JunieProjection::new(&binding.meta, imported_at);
        let fallback_identities = FallbackEventIdentityState::<R>::new(
            leaf.source().clone(),
            binding.session_id,
            LOGICAL_EVENT_KIND,
            "junie.event.fallback",
            EVENT_IDENTITY_REVISION,
            mode.into(),
            base_event_lookup,
        )?;
        Ok(Box::new(JunieProjector::<R> {
            source: leaf.source().clone(),
            binding,
            workspace,
            projection,
            fallback_identities,
            rejections: JsonlRecordRejections::new(
                leaf.source().clone(),
                CaptureProvider::Junie,
                leaf.source_path().display().to_string(),
            ),
        }))
    }
}

struct JunieProjector<R: JsonlProviderRuntime> {
    source: SourceKey,
    binding: JunieBinding,
    workspace: Option<String>,
    projection: JunieProjection,
    fallback_identities: FallbackEventIdentityState<R>,
    rejections: JsonlRecordRejections,
}

impl<R: JsonlProviderRuntime> JsonlFamilyProjector for JunieProjector<R> {
    type Runtime = R;

    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        _worker: &mut JsonlFamilyWorkerContext<R>,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        let rejected_before = self.projection.rejected_records();
        let rows = self.projection.project(record)?;
        let rejected_after = self.projection.rejected_records();
        debug_assert!(
            rejected_after == rejected_before
                || rejected_after == rejected_before.saturating_add(1)
        );
        if rejected_after > rejected_before {
            self.rejections.malformed(
                record,
                "Junie record could not be projected within its structural bounds",
            );
        }
        self.emit_rows(rows, emit)
    }

    fn finish_projecting(
        &mut self,
        _worker: &mut JsonlFamilyWorkerContext<R>,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        let rows = self.projection.finish()?;
        self.emit_rows(rows, emit)?;
        self.fallback_identities.finish()
    }

    fn rejected_records(&self) -> u64 {
        self.rejections.count()
    }

    fn take_record_rejections(&mut self) -> SourceBackedRecordRejectionDrafts {
        self.rejections.take_drafts()
    }
}

impl<R: JsonlProviderRuntime> JunieProjector<R> {
    fn emit_rows(
        &mut self,
        rows: Vec<EventDraft>,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        let cwd = self
            .projection
            .cwd()
            .map(str::to_owned)
            .or_else(|| self.workspace.clone());
        for row in rows {
            let assignment = self
                .fallback_identities
                .assign(event_fingerprint(&row)?, None)?;
            emit(core_record(
                &self.source,
                &self.binding,
                self.workspace.as_deref(),
                cwd.as_deref(),
                assignment.native_item_key().clone(),
                assignment.native_event_id().clone(),
                row,
            )?)?;
        }
        Ok(())
    }
}

#[cfg(test)]
fn source_key(provider_session_id: &str) -> Result<SourceKey> {
    source_key_scoped(provider_session_id, SourceAnchorScope::Unqualified)
}

fn source_key_scoped(
    provider_session_id: &str,
    source_anchor_scope: SourceAnchorScope,
) -> Result<SourceKey> {
    SourceKey::derive_provider_native_scoped(
        CaptureProvider::Junie.as_str(),
        JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
        SOURCE_SCHEMA_VARIANT,
        1,
        SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(provider_session_id).map_err(contract)?,
        source_anchor_scope,
    )
    .map_err(contract)
}

fn session_identity(source: &SourceKey, provider_session_id: &str) -> Result<StableEntityId> {
    derive_native_session_id(
        source,
        LOGICAL_SESSION_KIND,
        NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(provider_session_id).map_err(contract)?,
    )
    .map_err(contract)
}

fn core_record(
    source: &SourceKey,
    binding: &JunieBinding,
    workspace: Option<&str>,
    cwd: Option<&str>,
    native_item_key: NativeItemKey,
    native_event_id: TypedKey,
    row: EventDraft,
) -> Result<CoreRecord> {
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id: binding.session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .map_err(contract)?;
    let body = row.text.clone();
    if body.is_empty() {
        return Err(CaptureError::InvalidPayload(
            "Junie source-backed event has no exact lexical text".to_owned(),
        ));
    }
    let mut facts = Vec::new();
    if let Some(workspace) = workspace {
        push_fact(&mut facts, LiteralFactKind::Workspace, workspace.to_owned());
    }
    if let Some(cwd) = cwd {
        push_fact(&mut facts, LiteralFactKind::SessionCwd, cwd.to_owned());
    }
    if let Some(change) = &row.file_change {
        for path in &change.paths {
            push_fact(&mut facts, LiteralFactKind::File, path.clone());
        }
    }
    let activity = junie_activity(&row, facts)?;
    let mut record = CoreRecord::new_selected(
        event_id,
        binding.session_id,
        source.clone(),
        row.event_index,
        row.event_type.as_str(),
        PARSER_REVISION,
        body.clone(),
    )
    .map_err(contract)?;
    record.agent_scope = Some(AgentScope::Primary);
    record.provider_session_id = Some(binding.provider_session_id.clone());
    record.native_event_id = Some(native_event_id);
    record.occurred_at_unix_ms = Some(row.occurred_at.timestamp_millis());
    record.role = row.role.map(|role| role.as_str().to_owned());
    record.content.structured_content = Some(row.body);
    record.content.activity = activity;
    ctx_history_jsonl::fit_jsonl_activity(
        &body,
        record.content.structured_content.as_ref(),
        &mut record.content.activity,
        ctx_history_jsonl::JsonlActivityObservedBytes::infer_from_present(),
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
    record.validate_contract().map_err(contract)?;
    Ok(record)
}

fn event_fingerprint(row: &EventDraft) -> Result<TypedKey> {
    let role = row.role.map(|role| role.as_str());
    let file_change = row.file_change.as_ref().map(|change| &change.paths);
    let canonical = serde_json::to_vec(&serde_json::json!({
        "event_type": row.event_type.as_str(),
        "role": role,
        "text": row.text,
        "body": row.body,
        "file_change": file_change,
    }))?;
    let mut digest = Sha256::new();
    digest.update(FALLBACK_FINGERPRINT_DOMAIN);
    digest.update((canonical.len() as u64).to_be_bytes());
    digest.update(canonical);
    TypedKey::bytes(digest.finalize().to_vec()).map_err(contract)
}

fn junie_activity(
    row: &EventDraft,
    facts: Vec<ProviderDeclaredFact>,
) -> Result<Option<CoreActivity>> {
    let provider_call_id = admit_optional_provider_call_id(
        row.body
            .pointer("/provider_native_tool_result/call_id")
            .or_else(|| row.body.get("provider_step_id"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
    );
    let invocation = if provider_call_id.is_some() && row.event_type == EventType::ToolCall {
        admit_optional_metadata_text(
            row.body
                .get("tool_name")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
        )
        .map(|tool| ActivityInvocation {
            protocol: None,
            server: None,
            tool,
            arguments: ActivityJsonCapture::Present {
                value: row.body.clone(),
            },
            started_at_unix_ms: None,
        })
    } else {
        None
    };
    let result = (provider_call_id.is_some()
        && matches!(
            row.event_type,
            EventType::ToolOutput | EventType::CommandOutput
        ))
    .then(|| ActivityResult {
        status: None,
        completed_at_unix_ms: None,
        duration_ns: None,
        text: ActivityTextCapture::NormalizedBody,
        structured_content: ActivityJsonCapture::Present {
            value: row.body.clone(),
        },
    });
    if invocation.is_none() && result.is_none() && facts.is_empty() {
        return Ok(None);
    }
    Ok(Some(CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id,
        invocation,
        result,
        facts,
    }))
}

fn push_fact(facts: &mut Vec<ProviderDeclaredFact>, kind: LiteralFactKind, value: String) {
    if let Some(fact) = admit_provider_declared_fact(kind, value, facts.len()) {
        facts.push(fact);
    }
}

fn decode_binding(leaf: &JsonlFamilyLeaf) -> Result<JunieBinding> {
    let TypedKey::Bytes(bytes) = leaf.binding() else {
        return Err(CaptureError::InvalidPayload(
            "Junie family binding is malformed".to_owned(),
        ));
    };
    Ok(serde_json::from_slice(bytes)?)
}

fn relative_to_authority(authority: &ProviderSourceRoot, path: &Path) -> Result<PathBuf> {
    path.strip_prefix(authority.named_path())
        .map(Path::to_path_buf)
        .map_err(|_| CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Junie source escaped its retained authority",
        })
}

fn contract(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(event_type: EventType, body: serde_json::Value) -> EventDraft {
        EventDraft {
            event_index: 0,
            event_type,
            role: None,
            occurred_at: DateTime::<Utc>::UNIX_EPOCH,
            text: "body".to_owned(),
            body,
            file_change: None,
        }
    }

    #[test]
    fn source_and_session_identities_are_root_scoped() {
        let released = source_key("same-session").unwrap();
        let compatibility =
            source_key_scoped("same-session", SourceAnchorScope::Unqualified).unwrap();
        let first = source_key_scoped("same-session", SourceAnchorScope::Lineage([1; 32])).unwrap();
        let second =
            source_key_scoped("same-session", SourceAnchorScope::Lineage([2; 32])).unwrap();

        assert!(released.exact_descriptor_eq(&compatibility));
        assert_ne!(first.identity(), second.identity());
        assert_ne!(
            session_identity(&first, "same-session").unwrap(),
            session_identity(&second, "same-session").unwrap()
        );
    }

    #[test]
    fn unadmitted_call_id_withholds_linkage_and_empty_activity() {
        let oversized = "x".repeat(64 * 1024 + 1);
        let call = row(
            EventType::ToolCall,
            serde_json::json!({"provider_step_id": oversized, "tool_name": "Bash"}),
        );
        assert_eq!(junie_activity(&call, Vec::new()).unwrap(), None);

        let output = row(
            EventType::CommandOutput,
            serde_json::json!({
                "provider_native_tool_result": {"call_id": oversized},
            }),
        );
        let facts = vec![ProviderDeclaredFact {
            kind: LiteralFactKind::Command,
            value: "true".to_owned(),
        }];
        let activity = junie_activity(&output, facts.clone()).unwrap().unwrap();
        assert!(activity.provider_call_id.is_none());
        assert!(activity.result.is_none());
        assert_eq!(activity.facts, facts);
    }
}
