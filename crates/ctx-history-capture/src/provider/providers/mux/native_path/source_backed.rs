//! Thin Mux adapter for the shared replacement-only JSONL family.

use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_session_id, CaptureProvider, NativeSessionKey, SessionIdentityInput, SourceAnchor,
    SourceKey, StableEntityId, TypedKey,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    common::io::{OpenedProviderSourceFile, ProviderSourceRoot},
    provider::{
        providers::mux::{
            metadata::{mux_bounded_session_metadata_from_bytes, MuxBoundedSessionMetadata},
            source::{visit_mux_session_sources, MuxSessionSource},
        },
        source_backed::family::jsonl::{
            observe_opened_file, JsonlFamilyAdapter, JsonlFamilyHydrator, JsonlFamilyInventory,
            JsonlFamilyLeaf, JsonlFamilyProjector, JsonlFileObservation,
        },
    },
    CaptureError, Result, MAX_PROVIDER_JSONL_LINE_BYTES, MUX_SOURCE_FORMAT,
};

mod projection;
mod resolver;

use projection::MuxProjector;
use resolver::MuxHydrator;

const SOURCE_ANCHOR_NAMESPACE: &str = "mux.session";
const NATIVE_SESSION_NAMESPACE: &str = "mux.session";
const LOGICAL_SESSION_KIND: &str = "mux-session";
const LOGICAL_EVENT_KIND: &str = "mux-event";
const SOURCE_SCHEMA_VARIANT: &str = "mux-session-tree-source-backed-v2";
const PARSER_REVISION: &str = "mux-source-backed-v4";
const COMPOUND_REVISION_DOMAIN: &[u8] = b"ctx.mux.compound-source.v3\0";
const PARTIAL_EVENT_SEQUENCE_BASE: u64 = 1_u64 << 62;
const MAX_EVENT_SEQUENCE_ORDINAL: u64 = (1_u64 << 47) - 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MuxStreamKind {
    Chat,
    Partial,
}

impl MuxStreamKind {
    fn is_partial(self) -> bool {
        self == Self::Partial
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MuxBoundFile {
    relative_path: PathBuf,
    observation: JsonlFileObservation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MuxBinding {
    metadata: MuxBoundedSessionMetadata,
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
    root_session_id: StableEntityId,
    primary_stream: MuxStreamKind,
    chat: Option<MuxBoundFile>,
    partial: Option<MuxBoundFile>,
    metadata_file: Option<MuxBoundFile>,
    source_revision_digest: [u8; 32],
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct MuxJsonlAdapter;

pub(crate) fn mux_jsonl_adapter() -> Arc<dyn JsonlFamilyAdapter> {
    Arc::new(MuxJsonlAdapter)
}

impl JsonlFamilyAdapter for MuxJsonlAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Mux
    }

    fn source_format(&self) -> &'static str {
        MUX_SOURCE_FORMAT
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
                reason: "Mux transcript roots must not be symbolic links",
            });
        }
        let absolute = std::path::absolute(root)?;
        let authority_path = if metadata.is_file() {
            absolute
                .parent()
                .ok_or(CaptureError::InvalidProviderTranscriptPath {
                    path: absolute.clone(),
                    reason: "Mux selected file has no authority directory",
                })?
                .to_path_buf()
        } else {
            absolute
        };
        let authority = Arc::new(ProviderSourceRoot::open(&authority_path)?);
        let mut native_sources = Vec::new();
        visit_mux_session_sources(root, &mut |source| {
            native_sources.push(source);
            Ok(())
        })?;
        native_sources.sort_by(|left, right| left.session_dir.cmp(&right.session_dir));

        let mut leaves = Vec::with_capacity(native_sources.len());
        let mut sources = HashSet::with_capacity(native_sources.len());
        for native in native_sources {
            let (source, binding) = bind_source(&authority, &native)?;
            if !sources.insert(source.exact_descriptor_digest()) {
                return Err(CaptureError::InvalidPayload(format!(
                    "Mux native session {:?} resolves more than once",
                    binding.metadata.provider_session_id
                )));
            }
            let primary = bound_stream(&binding, binding.primary_stream)?;
            let source_path = authority.named_path().join(&primary.relative_path);
            let binding_key = TypedKey::bytes(serde_json::to_vec(&binding)?).map_err(contract)?;
            leaves.push(if binding.primary_stream.is_partial() {
                JsonlFamilyLeaf::observe_whole_record(
                    source,
                    source_path,
                    Arc::clone(&authority),
                    primary.relative_path.clone(),
                    binding_key,
                )?
            } else {
                JsonlFamilyLeaf::observe(
                    source,
                    source_path,
                    Arc::clone(&authority),
                    primary.relative_path.clone(),
                    binding_key,
                )?
            });
        }
        JsonlFamilyInventory::present(self.provider(), root, authority, leaves)
    }

    fn projector(
        &self,
        leaf: &JsonlFamilyLeaf,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
    ) -> Result<Box<dyn JsonlFamilyProjector>> {
        Ok(Box::new(MuxProjector::new(
            leaf.source().clone(),
            Arc::clone(leaf.authority()),
            decode_binding(leaf)?,
        )))
    }

    fn hydrator(
        &self,
        leaf: &JsonlFamilyLeaf,
        source_file: Arc<OpenedProviderSourceFile>,
    ) -> std::result::Result<Box<dyn JsonlFamilyHydrator>, ctx_history_core::HydrationFailure> {
        Ok(Box::new(MuxHydrator::new(
            leaf.source().clone(),
            Arc::clone(leaf.authority()),
            decode_binding(leaf).map_err(resolver::unavailable)?,
            source_file,
        )?))
    }
}

fn bind_source(
    authority: &Arc<ProviderSourceRoot>,
    native: &MuxSessionSource,
) -> Result<(SourceKey, MuxBinding)> {
    let chat = observe_optional(authority, native.chat_path.as_deref())?;
    let partial = observe_optional(authority, native.partial_path.as_deref())?;
    let metadata_file = observe_optional(authority, native.metadata_path.as_deref())?;
    let metadata_bytes = metadata_file
        .as_ref()
        .map(|bound| {
            authority
                .open_file(&bound.relative_path)?
                .read_all_bounded(MAX_PROVIDER_JSONL_LINE_BYTES)
        })
        .transpose()?;
    let metadata_revision =
        compound_component_revision(metadata_file.as_ref(), metadata_bytes.as_deref())?;
    let metadata = mux_bounded_session_metadata_from_bytes(
        native,
        &metadata_revision,
        DateTime::<Utc>::UNIX_EPOCH,
        metadata_bytes.as_deref(),
    )?;
    let source = source_key(&metadata.provider_session_id)?;
    let session_id = session_identity(&source, &metadata.provider_session_id)?;
    let parent_session_id = metadata
        .parent_provider_session_id
        .as_deref()
        .map(related_session_identity)
        .transpose()?;
    let root_session_id = metadata
        .root_provider_session_id
        .as_deref()
        .map(related_session_identity)
        .transpose()?
        .or(parent_session_id)
        .unwrap_or(session_id);
    let primary_stream = if chat.is_some() {
        MuxStreamKind::Chat
    } else {
        MuxStreamKind::Partial
    };
    let source_revision_digest =
        compound_revision_digest(&chat, &partial, &metadata_file, metadata_bytes.as_deref())?;
    Ok((
        source,
        MuxBinding {
            metadata,
            session_id,
            parent_session_id,
            root_session_id,
            primary_stream,
            chat,
            partial,
            metadata_file,
            source_revision_digest,
        },
    ))
}

fn observe_optional(
    authority: &Arc<ProviderSourceRoot>,
    path: Option<&Path>,
) -> Result<Option<MuxBoundFile>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let absolute = std::path::absolute(path)?;
    let relative_path = relative_to_authority(authority, &absolute)?;
    let opened = authority.open_file(&relative_path)?;
    let observation = observe_opened_file(&absolute, &opened)?;
    drop(opened);
    Ok(Some(MuxBoundFile {
        relative_path,
        observation,
    }))
}

fn compound_component_revision(
    metadata: Option<&MuxBoundFile>,
    bytes: Option<&[u8]>,
) -> Result<String> {
    let mut digest = Sha256::new();
    digest.update(COMPOUND_REVISION_DOMAIN);
    digest.update(serde_json::to_vec(&metadata)?);
    if let Some(bytes) = bytes {
        digest.update(Sha256::digest(bytes));
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn compound_revision_digest(
    chat: &Option<MuxBoundFile>,
    partial: &Option<MuxBoundFile>,
    metadata: &Option<MuxBoundFile>,
    metadata_bytes: Option<&[u8]>,
) -> Result<[u8; 32]> {
    let mut digest = Sha256::new();
    digest.update(COMPOUND_REVISION_DOMAIN);
    digest.update(serde_json::to_vec(&(chat, partial, metadata))?);
    if let Some(bytes) = metadata_bytes {
        digest.update(Sha256::digest(bytes));
    }
    Ok(digest.finalize().into())
}

fn source_key(native_session_id: &str) -> Result<SourceKey> {
    SourceKey::derive(
        CaptureProvider::Mux.as_str(),
        MUX_SOURCE_FORMAT,
        SOURCE_SCHEMA_VARIANT,
        1,
        SourceAnchor::provider_native(
            SOURCE_ANCHOR_NAMESPACE,
            TypedKey::utf8(native_session_id).map_err(contract)?,
        )
        .map_err(contract)?,
    )
    .map_err(contract)
}

fn session_identity(source: &SourceKey, native_session_id: &str) -> Result<StableEntityId> {
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

fn related_session_identity(native_session_id: &str) -> Result<StableEntityId> {
    let source = source_key(native_session_id)?;
    session_identity(&source, native_session_id)
}

fn decode_binding(leaf: &JsonlFamilyLeaf) -> Result<MuxBinding> {
    let TypedKey::Bytes(bytes) = leaf.binding() else {
        return Err(CaptureError::InvalidPayload(
            "Mux family binding is malformed".to_owned(),
        ));
    };
    Ok(serde_json::from_slice(bytes)?)
}

fn bound_stream(binding: &MuxBinding, stream: MuxStreamKind) -> Result<&MuxBoundFile> {
    match stream {
        MuxStreamKind::Chat => binding.chat.as_ref(),
        MuxStreamKind::Partial => binding.partial.as_ref(),
    }
    .ok_or_else(|| CaptureError::InvalidPayload("Mux bound stream is absent".to_owned()))
}

fn open_verified(
    authority: &Arc<ProviderSourceRoot>,
    bound: &MuxBoundFile,
) -> Result<Arc<OpenedProviderSourceFile>> {
    let path = authority.named_path().join(&bound.relative_path);
    let opened = authority.open_file(&bound.relative_path)?;
    if observe_opened_file(&path, &opened)? != bound.observation {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    Ok(Arc::new(opened))
}

fn relative_to_authority(authority: &ProviderSourceRoot, path: &Path) -> Result<PathBuf> {
    path.strip_prefix(authority.named_path())
        .map(Path::to_path_buf)
        .map_err(|_| CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Mux compound leaf escaped its retained authority",
        })
}

fn contract(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

#[cfg(test)]
mod tests;
