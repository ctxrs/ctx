use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, CoreRecord, EventIdentityInput,
    EventType, NativeItemKey, NativeSessionKey, SessionIdentityInput, SourceAnchor, SourceKey,
    StableEntityId, TypedKey,
};
use ctx_history_index::BaseEventIdentityLookup;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    common::io::{OpenedProviderSourceFile, ProviderSourceRoot},
    provider::{
        normalization::provider_local_preview,
        source_backed::{
            family::jsonl::{
                JsonlFamilyAdapter, JsonlFamilyAppendMode, JsonlFamilyInventory, JsonlFamilyLeaf,
                JsonlFamilyProjectionMode, JsonlFamilyProjector, JsonlFamilyWorkerContext,
                JsonlRecordRef,
            },
            FallbackEventIdentityState,
        },
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
const PARSER_REVISION: &str = "junie-source-backed-v5";
const EVENT_IDENTITY_REVISION: &str = "junie-content-occurrence-v1";
const FALLBACK_FINGERPRINT_DOMAIN: &[u8] = b"ctx.junie.fallback-event-fingerprint.v1\0";
const METADATA_TEXT_MAX_CHARS: usize = 2_048;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JunieBinding {
    provider_session_id: String,
    session_id: StableEntityId,
    meta: JunieIndexMeta,
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

    fn event_identity_revision(&self) -> &'static str {
        EVENT_IDENTITY_REVISION
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::Replacement
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
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
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
        base_event_lookup: Option<BaseEventIdentityLookup>,
        mode: JsonlFamilyProjectionMode,
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
        if checkpoint.is_some() {
            return Err(CaptureError::InvalidPayload(
                "Junie adapter does not accept provider checkpoint state".to_owned(),
            ));
        }
        let binding = decode_binding(leaf)?;
        let workspace = binding
            .meta
            .project_dir
            .as_deref()
            .map(|value| provider_local_preview(value, METADATA_TEXT_MAX_CHARS).0);
        let projection = JunieProjection::new(&binding.meta, imported_at);
        let fallback_identities = FallbackEventIdentityState::new(
            leaf.source().clone(),
            binding.session_id,
            LOGICAL_EVENT_KIND,
            "junie.event.fallback",
            EVENT_IDENTITY_REVISION,
            mode.into(),
            base_event_lookup,
        )?;
        Ok(Box::new(JunieProjector {
            source: leaf.source().clone(),
            binding,
            workspace,
            projection,
            fallback_identities,
        }))
    }
}

struct JunieProjector {
    source: SourceKey,
    binding: JunieBinding,
    workspace: Option<String>,
    projection: JunieProjection,
    fallback_identities: FallbackEventIdentityState,
}

impl JsonlFamilyProjector for JunieProjector {
    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        _worker: &mut JsonlFamilyWorkerContext,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        let rows = self.projection.project(record)?;
        self.emit_rows(rows, emit)
    }

    fn finish_projecting(
        &mut self,
        _worker: &mut JsonlFamilyWorkerContext,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        let rows = self.projection.finish()?;
        self.emit_rows(rows, emit)?;
        self.fallback_identities.finish()
    }
}

impl JunieProjector {
    fn emit_rows(
        &mut self,
        rows: Vec<EventDraft>,
        emit: &mut dyn FnMut(CoreRecord) -> Result<()>,
    ) -> Result<()> {
        let cwd = self
            .projection
            .cwd()
            .map(|value| provider_local_preview(value, METADATA_TEXT_MAX_CHARS).0)
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
    let structured_content = if row.event_type == EventType::Message {
        let model = row.body.get("model").cloned();
        let usage = row.body.get("usage").cloned();
        (model.is_some() || usage.is_some()).then(|| {
            serde_json::json!({
                "model": model,
                "usage": usage,
            })
        })
    } else {
        Some(serde_json::json!({
            "provider_native_event": row.body,
            "file_path": row.file_change.map(|change| change.path),
        }))
    };
    let mut record = CoreRecord::new_selected(
        event_id,
        binding.session_id,
        binding.session_id,
        source.clone(),
        row.event_index,
        row.event_type.as_str(),
        AgentType::Primary.as_str(),
        true,
        PARSER_REVISION,
        body,
    )
    .map_err(contract)?;
    record.provider_session_id = Some(binding.provider_session_id.clone());
    record.native_event_id = Some(native_event_id);
    record.occurred_at_unix_ms = Some(row.occurred_at.timestamp_millis());
    record.role = row.role.map(|role| role.as_str().to_owned());
    record.workspace = workspace.map(str::to_owned);
    record.cwd = cwd.map(str::to_owned);
    record.content.structured_content = structured_content;
    record.validate_contract().map_err(contract)?;
    Ok(record)
}

fn event_fingerprint(row: &EventDraft) -> Result<TypedKey> {
    let role = row.role.map(|role| role.as_str());
    let file_change = row.file_change.as_ref().map(|change| change.path.as_str());
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
