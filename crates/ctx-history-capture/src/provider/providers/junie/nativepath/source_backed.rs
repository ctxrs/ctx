use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, EventIdentityInput,
    LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate, NativeSessionKey,
    PositionStability, SessionIdentityInput, SourceAnchor, SourceKey, SourceRecordLocator,
    StableEntityId, TypedKey,
};
use ctx_history_index::LexicalDocument;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    common::io::{OpenedProviderSourceFile, ProviderSourceRoot},
    provider::{
        normalization::provider_local_preview,
        source_backed::family::jsonl::{
            JsonlFamilyAdapter, JsonlFamilyHydrator, JsonlFamilyInventory, JsonlFamilyLeaf,
            JsonlFamilyProjector, JsonlRecordRef,
        },
    },
    CaptureError, Result, JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
};

use super::super::{
    session_tree::{
        bounded_junie_index_meta, junie_provider_session_id, visit_junie_session_event_paths,
        JunieIndexMeta,
    },
    source::JunieSessionObservation,
};
use super::projection::{
    EventDraft, JunieProjection, RecordSetBinding, SourceBackedBinding, SourceBackedTarget,
    RECORD_SET_DIGEST_DOMAIN,
};

mod resolver;

use resolver::JunieHydrator;

const SOURCE_ANCHOR_NAMESPACE: &str = "junie.session-events";
const NATIVE_SESSION_NAMESPACE: &str = "junie.session";
const NATIVE_EVENT_POSITION_KIND: &str = "junie.normalized-event-index";
const LOGICAL_SESSION_KIND: &str = "junie-session";
const LOGICAL_EVENT_KIND: &str = "junie-event";
const SOURCE_SCHEMA_VARIANT: &str = "junie-session-events-v2";
const PARSER_REVISION: &str = "junie-source-backed-v3";
const RELATIVE_EVENTS_FILE: &str = "events.jsonl";
const RECORD_SET_COORDINATE_KIND: &str = "junie-record-set-coordinate-v2";
const USER_PROMPT_COORDINATE_KIND: &str = "junie-user-prompt-coordinate-v2";
const UNAVAILABLE_COORDINATE_NAMESPACE: &str = "junie.record-set-unavailable.v2";
const UNAVAILABLE_DIGEST_DOMAIN: &[u8] = b"ctx-junie-unavailable-record-set-v2\0";
const METADATA_TEXT_MAX_CHARS: usize = 2_048;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JunieBinding {
    provider_session_id: String,
    session_id: StableEntityId,
    meta: JunieIndexMeta,
    require_supported_events: bool,
    source_revision_digest: [u8; 32],
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct JunieJsonlAdapter;

pub(crate) fn junie_jsonl_adapter() -> Arc<dyn JsonlFamilyAdapter> {
    Arc::new(JunieJsonlAdapter)
}

impl JsonlFamilyAdapter for JunieJsonlAdapter {
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
            let source = source_key(&provider_session_id)?;
            if !sources.insert(source.exact_descriptor_digest()) {
                return Err(CaptureError::InvalidPayload(format!(
                    "Junie native session {provider_session_id:?} resolves more than once"
                )));
            }
            let observation = JunieSessionObservation::read(&session)?;
            let meta = bounded_junie_index_meta(&session.index_meta);
            let binding = JunieBinding {
                session_id: session_identity(&source, &provider_session_id)?,
                provider_session_id,
                meta,
                require_supported_events: session.require_supported_events,
                source_revision_digest: Sha256::digest(observation.source_revision().as_bytes())
                    .into(),
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
        _source_file: Arc<OpenedProviderSourceFile>,
        imported_at: DateTime<Utc>,
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
        let binding = decode_binding(leaf)?;
        let workspace = binding
            .meta
            .project_dir
            .as_deref()
            .map(|value| provider_local_preview(value, METADATA_TEXT_MAX_CHARS).0);
        let projection =
            JunieProjection::new(&binding.meta, binding.require_supported_events, imported_at);
        Ok(Box::new(JunieProjector {
            source: leaf.source().clone(),
            source_path: leaf.source_path().to_string_lossy().into_owned(),
            binding,
            workspace,
            projection,
        }))
    }

    fn hydrator(
        &self,
        leaf: &JsonlFamilyLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
    ) -> std::result::Result<Box<dyn JsonlFamilyHydrator>, ctx_history_core::HydrationFailure> {
        Ok(Box::new(JunieHydrator::new(
            leaf.source().clone(),
            decode_binding(leaf).map_err(resolver::unavailable)?,
            source_file,
        )))
    }
}

struct JunieProjector {
    source: SourceKey,
    source_path: String,
    binding: JunieBinding,
    workspace: Option<String>,
    projection: JunieProjection,
}

impl JsonlFamilyProjector for JunieProjector {
    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        emit: &mut dyn FnMut(LexicalDocument) -> Result<()>,
    ) -> Result<()> {
        let rows = self.projection.project(record)?;
        self.emit_rows(rows, emit)
    }

    fn finish_projecting(
        &mut self,
        emit: &mut dyn FnMut(LexicalDocument) -> Result<()>,
    ) -> Result<()> {
        let rows = self.projection.finish()?;
        self.emit_rows(rows, emit)
    }
}

impl JunieProjector {
    fn emit_rows(
        &self,
        rows: Vec<EventDraft>,
        emit: &mut dyn FnMut(LexicalDocument) -> Result<()>,
    ) -> Result<()> {
        let cwd = self
            .projection
            .cwd()
            .map(|value| provider_local_preview(value, METADATA_TEXT_MAX_CHARS).0)
            .or_else(|| self.workspace.clone());
        for row in rows {
            emit(lexical_document(
                &self.source,
                &self.binding,
                &self.source_path,
                self.workspace.as_deref(),
                cwd.as_deref(),
                row,
            )?)?;
        }
        Ok(())
    }
}

fn source_key(provider_session_id: &str) -> Result<SourceKey> {
    SourceKey::derive(
        CaptureProvider::Junie.as_str(),
        JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
        SOURCE_SCHEMA_VARIANT,
        1,
        SourceAnchor::provider_native(
            SOURCE_ANCHOR_NAMESPACE,
            TypedKey::utf8(provider_session_id).map_err(contract)?,
        )
        .map_err(contract)?,
    )
    .map_err(contract)
}

fn session_identity(source: &SourceKey, provider_session_id: &str) -> Result<StableEntityId> {
    let native_session_key = NativeSessionKey::native_id(
        NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(provider_session_id).map_err(contract)?,
    )
    .map_err(contract)?;
    derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })
    .map_err(contract)
}

fn lexical_document(
    source: &SourceKey,
    binding: &JunieBinding,
    source_path: &str,
    workspace: Option<&str>,
    cwd: Option<&str>,
    row: EventDraft,
) -> Result<LexicalDocument> {
    let native_item_key = NativeItemKey::certified_position(
        NATIVE_EVENT_POSITION_KIND,
        TypedKey::U64(row.event_index),
        PositionStability::AppendStable,
    )
    .map_err(contract)?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id: binding.session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .map_err(contract)?;
    let locator = source_locator(
        source,
        &binding.provider_session_id,
        row.event_index,
        binding.source_revision_digest,
        &row.source_backed_binding,
    )?;
    let body = lexical_body(&row);
    if body.is_empty() {
        return Err(CaptureError::InvalidPayload(
            "Junie source-backed event has no exact lexical text".to_owned(),
        ));
    }
    let touched_files = row
        .file_change
        .as_ref()
        .map(|change| vec![change.path.clone()])
        .unwrap_or_default();
    Ok(LexicalDocument {
        event_id,
        session_id: binding.session_id,
        parent_session_id: None,
        root_session_id: binding.session_id,
        source: source.clone(),
        locator,
        provider_session_id: Some(binding.provider_session_id.clone()),
        branch: None,
        source_path: Some(source_path.to_owned()),
        agent_type: AgentType::Primary.as_str().to_owned(),
        is_primary: true,
        event_sequence: row.event_index,
        occurred_at_unix_ms: Some(row.occurred_at.timestamp_millis()),
        event_type: row.event_type.as_str().to_owned(),
        role: row.role.map(|role| role.as_str().to_owned()),
        body,
        workspace: workspace.map(str::to_owned),
        cwd: cwd.map(str::to_owned),
        touched_files,
    })
}

fn lexical_body(row: &EventDraft) -> String {
    match &row.source_backed_binding.target {
        SourceBackedTarget::StepOutput { .. } => row
            .body
            .get("details")
            .and_then(Value::as_str)
            .map_or_else(|| row.text.clone(), str::to_owned),
        _ => row.text.clone(),
    }
}

fn source_locator(
    source: &SourceKey,
    provider_session_id: &str,
    event_sequence: u64,
    source_revision_digest: [u8; 32],
    binding: &SourceBackedBinding,
) -> Result<SourceRecordLocator> {
    if binding.target == SourceBackedTarget::UserPrompt
        && !binding.records.unavailable
        && binding.records.entries.len() == 1
    {
        let entry = &binding.records.entries[0];
        return SourceRecordLocator::new(
            source.clone(),
            NativeRecordCoordinate::Jsonl {
                byte_offset: entry.byte_start,
                byte_length: entry.byte_end_exclusive.saturating_sub(entry.byte_start),
                physical_ordinal: entry.ordinal,
                native_session_key: Some(TypedKey::utf8(provider_session_id).map_err(contract)?),
                native_event_key: Some(
                    TypedKey::composite(vec![
                        TypedKey::utf8(USER_PROMPT_COORDINATE_KIND).map_err(contract)?,
                        TypedKey::U64(event_sequence),
                    ])
                    .map_err(contract)?,
                ),
            },
            LocatorRevisionPolicy::StableRecordEvidence,
            Some(source_revision_digest),
            entry.payload_sha256,
        )
        .map_err(contract);
    }
    if binding.records.unavailable || binding.records.entries.is_empty() {
        let target = target_key(&binding.target)?;
        return SourceRecordLocator::new(
            source.clone(),
            NativeRecordCoordinate::ProviderNative {
                namespace: UNAVAILABLE_COORDINATE_NAMESPACE.to_owned(),
                coordinate: TypedKey::composite(vec![
                    target.clone(),
                    TypedKey::U64(event_sequence),
                ])
                .map_err(contract)?,
            },
            LocatorRevisionPolicy::StableRecordEvidence,
            Some(source_revision_digest),
            unavailable_digest(event_sequence, &target),
        )
        .map_err(contract);
    }
    let entries = binding
        .records
        .entries
        .iter()
        .map(|entry| {
            TypedKey::composite(vec![
                TypedKey::U64(entry.ordinal),
                TypedKey::U64(entry.byte_start),
                TypedKey::U64(entry.byte_end_exclusive),
                TypedKey::bytes(entry.payload_sha256.to_vec()).map_err(contract)?,
            ])
            .map_err(contract)
        })
        .collect::<Result<Vec<_>>>()?;
    SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::TreeRecord {
            relative_file_key: TypedKey::utf8(RELATIVE_EVENTS_FILE).map_err(contract)?,
            record_coordinate: TypedKey::composite(vec![
                TypedKey::utf8(RECORD_SET_COORDINATE_KIND).map_err(contract)?,
                TypedKey::U64(event_sequence),
                target_key(&binding.target)?,
                TypedKey::composite(entries).map_err(contract)?,
            ])
            .map_err(contract)?,
        },
        LocatorRevisionPolicy::StableRecordEvidence,
        Some(source_revision_digest),
        aggregate_digest(&binding.records),
    )
    .map_err(contract)
}

fn unavailable_digest(event_sequence: u64, target: &TypedKey) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(UNAVAILABLE_DIGEST_DOMAIN);
    digest.update(event_sequence.to_be_bytes());
    digest.update(format!("{target:?}").as_bytes());
    digest.finalize().into()
}

fn target_key(target: &SourceBackedTarget) -> Result<TypedKey> {
    let (tag, first, second) = match target {
        SourceBackedTarget::UserPrompt => (1, 0, 0),
        SourceBackedTarget::AssistantMessage => (2, 0, 0),
        SourceBackedTarget::StepCall { step_order } => (3, u64::from(*step_order), 0),
        SourceBackedTarget::StepOutput { step_order } => (4, u64::from(*step_order), 0),
        SourceBackedTarget::FileChange {
            step_order,
            change_index,
        } => (5, u64::from(*step_order), u64::from(*change_index)),
    };
    TypedKey::composite(vec![
        TypedKey::U64(tag),
        TypedKey::U64(first),
        TypedKey::U64(second),
    ])
    .map_err(contract)
}

fn aggregate_digest(binding: &RecordSetBinding) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(RECORD_SET_DIGEST_DOMAIN);
    digest.update((binding.entries.len() as u64).to_be_bytes());
    for entry in &binding.entries {
        digest.update(entry.ordinal.to_be_bytes());
        digest.update(entry.byte_start.to_be_bytes());
        digest.update(entry.byte_end_exclusive.to_be_bytes());
        digest.update(entry.payload_sha256);
    }
    digest.finalize().into()
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
mod tests;
