//! Thin Mux adapter for the shared replacement-only JSONL family.

use std::{
    collections::HashSet,
    fs, io,
    marker::PhantomData,
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_native_session_id, CaptureProvider, SourceAnchorScope, SourceKey, StableEntityId,
    TypedKey,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use ctx_history_jsonl::{
    observe_opened_file, JsonlFamilyAdapter, JsonlFamilyAppendMode, JsonlFamilyInventory,
    JsonlFamilyLeaf, JsonlFamilyProjectionMode, JsonlFamilyProjector, JsonlFamilyTerminalProof,
    JsonlFileObservation,
};
use ctx_history_provider_runtime::{
    source_io::{OpenedProviderSourceFile, ProviderSourceRoot},
    CaptureError, ProviderBaseEventLookup, ProviderJsonlRuntime, ProviderRuntimeBinding, Result,
};
use ctx_history_source_io::MAX_PROVIDER_JSONL_LINE_BYTES;

use crate::mux::{
    metadata::{mux_bounded_session_metadata_from_bytes, MuxBoundedSessionMetadata},
    source::{visit_mux_session_sources, MuxSessionSource},
    MUX_SOURCE_FORMAT,
};

mod projection;

use projection::MuxJsonlProjector;

const SOURCE_ANCHOR_NAMESPACE: &str = "mux.session";
const NATIVE_SESSION_NAMESPACE: &str = "mux.session";
const LOGICAL_SESSION_KIND: &str = "mux-session";
const LOGICAL_EVENT_KIND: &str = "mux-event";
const SOURCE_SCHEMA_VARIANT: &str = "mux-session-tree-source-backed-v2";
const PARSER_REVISION: &str = "mux-source-backed-v16-explicit-root-only";
const EVENT_IDENTITY_REVISION: &str = "mux-content-occurrence-v1";
const COMPOUND_REVISION_DOMAIN: &[u8] = b"ctx.mux.compound-source.v3\0";
const PARTIAL_EVENT_SEQUENCE_BASE: u64 = 1_u64 << 62;
const MAX_EVENT_SEQUENCE_ORDINAL: u64 = (1_u64 << 47) - 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MuxStreamKind {
    Archive,
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
    root_session_id: Option<StableEntityId>,
    primary_stream: MuxStreamKind,
    archive: Option<MuxBoundFile>,
    chat: Option<MuxBoundFile>,
    partial: Option<MuxBoundFile>,
    metadata_file: Option<MuxBoundFile>,
    source_revision_digest: [u8; 32],
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct MuxJsonlAdapter<B> {
    source_anchor_scope: SourceAnchorScope,
    binding: PhantomData<fn() -> B>,
}

pub(crate) fn mux_jsonl_adapter<B>(
) -> Arc<dyn JsonlFamilyAdapter<Runtime = ProviderJsonlRuntime<B>>>
where
    B: ProviderRuntimeBinding,
{
    mux_jsonl_adapter_with_source_root_lineage(None)
}

pub(crate) fn mux_jsonl_adapter_with_source_root_lineage<B>(
    source_root_lineage: Option<[u8; 32]>,
) -> Arc<dyn JsonlFamilyAdapter<Runtime = ProviderJsonlRuntime<B>>>
where
    B: ProviderRuntimeBinding,
{
    Arc::new(MuxJsonlAdapter {
        source_anchor_scope: source_root_lineage
            .map_or(SourceAnchorScope::Unqualified, SourceAnchorScope::Lineage),
        binding: PhantomData,
    })
}

impl<B> JsonlFamilyAdapter for MuxJsonlAdapter<B>
where
    B: ProviderRuntimeBinding,
{
    type Runtime = ProviderJsonlRuntime<B>;

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

    fn event_identity_revision(&self) -> &'static str {
        EVENT_IDENTITY_REVISION
    }

    fn append_mode(&self) -> JsonlFamilyAppendMode {
        JsonlFamilyAppendMode::Replacement
    }

    fn discover(&self, root: &Path) -> Result<JsonlFamilyInventory<CaptureError>> {
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
        let mut exact_dependencies = Vec::new();
        let mut sources = HashSet::with_capacity(native_sources.len());
        for native in native_sources {
            let (source, binding) = bind_source(&authority, &native, self.source_anchor_scope)?;
            if !sources.insert(source.exact_descriptor_digest()) {
                return Err(CaptureError::InvalidPayload(format!(
                    "Mux native session {:?} resolves more than once",
                    binding.metadata.provider_session_id
                )));
            }
            let primary = bound_stream(&binding, binding.primary_stream)?;
            for bound in [
                binding.archive.as_ref(),
                binding.chat.as_ref(),
                binding.partial.as_ref(),
                binding.metadata_file.as_ref(),
            ]
            .into_iter()
            .flatten()
            .filter(|bound| bound.relative_path != primary.relative_path)
            {
                exact_dependencies.push(exact_dependency(&authority, bound)?);
            }
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
            .map(|inventory| inventory.with_exact_dependencies(exact_dependencies))
    }

    fn projector(
        &self,
        leaf: &JsonlFamilyLeaf<CaptureError>,
        source_file: Arc<OpenedProviderSourceFile>,
        imported_at: DateTime<Utc>,
    ) -> Result<Box<dyn JsonlFamilyProjector<Runtime = ProviderJsonlRuntime<B>>>> {
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
        leaf: &JsonlFamilyLeaf<CaptureError>,
        _source_file: Arc<OpenedProviderSourceFile>,
        _imported_at: DateTime<Utc>,
        checkpoint: Option<&TypedKey>,
        base_event_lookup: Option<ProviderBaseEventLookup<B>>,
        mode: JsonlFamilyProjectionMode,
    ) -> Result<Box<dyn JsonlFamilyProjector<Runtime = ProviderJsonlRuntime<B>>>> {
        if checkpoint.is_some() {
            return Err(CaptureError::InvalidPayload(
                "Mux adapter does not accept provider checkpoint state".to_owned(),
            ));
        }
        Ok(Box::new(MuxJsonlProjector::<B>::new(
            leaf.source().clone(),
            Arc::clone(leaf.authority()),
            decode_binding(leaf)?,
            mode,
            base_event_lookup,
        )?))
    }
}

fn bind_source(
    authority: &Arc<ProviderSourceRoot>,
    native: &MuxSessionSource,
    source_anchor_scope: SourceAnchorScope,
) -> Result<(SourceKey, MuxBinding)> {
    let archive = observe_optional(authority, native.archive_path.as_deref())?;
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
    let source = source_key_scoped(&metadata.provider_session_id, source_anchor_scope)?;
    let session_id = session_identity(&source, &metadata.provider_session_id)?;
    let parent_session_id = metadata
        .parent_provider_session_id
        .as_deref()
        .map(|parent| related_session_identity(parent, source_anchor_scope))
        .transpose()?;
    let root_session_id = metadata
        .root_provider_session_id
        .as_deref()
        .map(|root| related_session_identity(root, source_anchor_scope))
        .transpose()?;
    let primary_stream = if archive.is_some() {
        MuxStreamKind::Archive
    } else if chat.is_some() {
        MuxStreamKind::Chat
    } else {
        MuxStreamKind::Partial
    };
    let source_revision_digest = compound_revision_digest(
        &archive,
        &chat,
        &partial,
        &metadata_file,
        metadata_bytes.as_deref(),
    )?;
    Ok((
        source,
        MuxBinding {
            metadata,
            session_id,
            parent_session_id,
            root_session_id,
            primary_stream,
            archive,
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
    archive: &Option<MuxBoundFile>,
    chat: &Option<MuxBoundFile>,
    partial: &Option<MuxBoundFile>,
    metadata: &Option<MuxBoundFile>,
    metadata_bytes: Option<&[u8]>,
) -> Result<[u8; 32]> {
    let mut digest = Sha256::new();
    digest.update(COMPOUND_REVISION_DOMAIN);
    digest.update(serde_json::to_vec(&(archive, chat, partial, metadata))?);
    if let Some(bytes) = metadata_bytes {
        digest.update(Sha256::digest(bytes));
    }
    Ok(digest.finalize().into())
}

#[cfg(test)]
fn source_key(native_session_id: &str) -> Result<SourceKey> {
    source_key_scoped(native_session_id, SourceAnchorScope::Unqualified)
}

fn source_key_scoped(
    native_session_id: &str,
    source_anchor_scope: SourceAnchorScope,
) -> Result<SourceKey> {
    SourceKey::derive_provider_native_scoped(
        CaptureProvider::Mux.as_str(),
        MUX_SOURCE_FORMAT,
        SOURCE_SCHEMA_VARIANT,
        1,
        SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(native_session_id).map_err(contract)?,
        source_anchor_scope,
    )
    .map_err(contract)
}

fn session_identity(source: &SourceKey, native_session_id: &str) -> Result<StableEntityId> {
    derive_native_session_id(
        source,
        LOGICAL_SESSION_KIND,
        NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(native_session_id).map_err(contract)?,
    )
    .map_err(contract)
}

fn related_session_identity(
    native_session_id: &str,
    source_anchor_scope: SourceAnchorScope,
) -> Result<StableEntityId> {
    let source = source_key_scoped(native_session_id, source_anchor_scope)?;
    session_identity(&source, native_session_id)
}

fn decode_binding(leaf: &JsonlFamilyLeaf<CaptureError>) -> Result<MuxBinding> {
    let TypedKey::Bytes(bytes) = leaf.binding() else {
        return Err(CaptureError::InvalidPayload(
            "Mux family binding is malformed".to_owned(),
        ));
    };
    Ok(serde_json::from_slice(bytes)?)
}

fn bound_stream(binding: &MuxBinding, stream: MuxStreamKind) -> Result<&MuxBoundFile> {
    optional_bound_stream(binding, stream)
        .ok_or_else(|| CaptureError::InvalidPayload("Mux bound stream is absent".to_owned()))
}

fn optional_bound_stream(binding: &MuxBinding, stream: MuxStreamKind) -> Option<&MuxBoundFile> {
    match stream {
        MuxStreamKind::Archive => binding.archive.as_ref(),
        MuxStreamKind::Chat => binding.chat.as_ref(),
        MuxStreamKind::Partial => binding.partial.as_ref(),
    }
}

fn exact_dependency(
    authority: &Arc<ProviderSourceRoot>,
    bound: &MuxBoundFile,
) -> Result<JsonlFamilyTerminalProof<CaptureError>> {
    let source_path = authority.named_path().join(&bound.relative_path);
    let opened = authority.open_file(&bound.relative_path)?;
    if observe_opened_file(&source_path, &opened)? != bound.observation {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    JsonlFamilyTerminalProof::exact_opened_path(
        source_path,
        Arc::clone(authority),
        bound.relative_path.clone(),
        &opened,
    )
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
