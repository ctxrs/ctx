//! One-pass replacement-only source-backed ingestion for Auggie documents.

use std::{
    collections::HashSet,
    io,
    marker::PhantomData,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use ctx_history_core::{
    derive_event_id, derive_session_id, AgentScope, CoreActivity, CoreRecord, CoreRecordError,
    EventIdentityInput, LiteralFactKind, NativeItemKey, NativeSessionKey, ProjectionContractError,
    ProviderDeclaredFact, ProviderNativeSessionRelationship, ScannedSourceCounts,
    SessionIdentityInput, SourceAnchorScope, SourceKey, SourceObservation, StableEntityId,
    TypedKey, CORE_ACTIVITY_REVISION,
};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    model::{
        ParsedAuggieEvent, ParsedAuggieSession, ParsedAuggieSource, AUGGIE_MAX_DISCOVERED_FILES,
        AUGGIE_PARSER_REVISION,
    },
    normalized_auggie_authority_path,
    parse::parse_opened_auggie_source,
    source::{invalid_source_path, AuggieFileStamp},
};
use crate::{
    common::io::{
        open_provider_source_path, OpenedProviderSourcePath, ProviderSourceDirectory,
        ProviderSourceRoot,
    },
    provider::providers::auggie::AuggieLineageClaim,
    provider::source_backed::{
        family::document::{
            ChangedDocumentSink, CompleteDocumentTree, DocumentLeafExecutionPolicy,
            DocumentLeafFingerprint, DocumentSourceTerminal, ObservedDocumentLeaf,
            ReplacementDocumentTree,
        },
        route_error, SourceBackedRouteError, SourceBackedRouteErrorKind, SourceBackedRouteResult,
    },
    CaptureError, ProviderAdapterContext, AUGGIE_SESSION_JSON_SOURCE_FORMAT,
};

const AUGGIE_SOURCE_ANCHOR_NAMESPACE: &str = "auggie.session";
const AUGGIE_NATIVE_SESSION_NAMESPACE: &str = "auggie.session";
const AUGGIE_NATIVE_EVENT_NAMESPACE: &str = "auggie.request-part";
const AUGGIE_EVENT_POSITION_KIND: &str = "auggie.chat-history-position";
const AUGGIE_LOGICAL_SESSION_KIND: &str = "auggie-session";
const AUGGIE_LOGICAL_EVENT_KIND: &str = "auggie-message";
const AUGGIE_SOURCE_SCHEMA_VARIANT: &str = "auggie-structured-session-v1";
const AUGGIE_SOURCE_REVISION_KIND: &str = "auggie-ordinary-file-observation-v1";

#[derive(Debug, Error)]
pub(crate) enum AuggieSourceBackedError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    CoreRecord(#[from] CoreRecordError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("Auggie source contains duplicate stable event identity {0}")]
    DuplicateEventIdentity(StableEntityId),
    #[error("Auggie source-backed event has no meaningful normalized content")]
    MissingNormalizedContent,
}

pub(crate) type AuggieSourceBackedResult<T> = Result<T, AuggieSourceBackedError>;

#[derive(Debug, Clone)]
pub struct AuggieSourceBackedRoot {
    path: PathBuf,
}

impl AuggieSourceBackedRoot {
    pub fn explicit(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuggieSourceBackedInventoryStatus {
    Complete,
    Unavailable,
}

#[derive(Debug)]
pub(crate) struct AuggieSourceBackedInventory {
    pub(crate) status: AuggieSourceBackedInventoryStatus,
    tree: Option<AuggieDocumentTree>,
}

impl AuggieSourceBackedInventory {
    fn into_complete_tree(self) -> Option<AuggieDocumentTree> {
        if self.status == AuggieSourceBackedInventoryStatus::Complete {
            self.tree
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum AuggieTreeSelection {
    ExplicitFile,
    SelectedDirectory,
    DirectSessionsChild,
}

impl AuggieTreeSelection {
    fn tag(self) -> u8 {
        match self {
            Self::ExplicitFile => 1,
            Self::SelectedDirectory => 2,
            Self::DirectSessionsChild => 3,
        }
    }
}

/// Handle-free identity captured while one admitted leaf was open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuggieDocumentLeaf {
    canonical_path: PathBuf,
    authority_relative_path: PathBuf,
    len: u64,
    modified: SystemTime,
    readonly: bool,
    device: Option<u64>,
    inode: Option<u64>,
    authority_fingerprint: [u8; 32],
}

impl AuggieDocumentLeaf {
    fn from_opened(
        canonical_path: PathBuf,
        authority_relative_path: PathBuf,
        stamp: &AuggieFileStamp,
    ) -> Self {
        Self {
            canonical_path,
            authority_relative_path,
            len: stamp.len,
            modified: stamp.modified,
            readonly: stamp.readonly,
            device: stamp.device,
            inode: stamp.inode,
            authority_fingerprint: stamp.authority_fingerprint(),
        }
    }

    fn matches(&self, stamp: &AuggieFileStamp) -> bool {
        self.canonical_path == stamp.canonical_path
            && self.len == stamp.len
            && self.modified == stamp.modified
            && self.readonly == stamp.readonly
            && self.device == stamp.device
            && self.inode == stamp.inode
            && self.authority_fingerprint == stamp.authority_fingerprint()
    }
}

#[derive(Debug)]
pub enum AuggieTreeAuthority {
    File {
        root: ProviderSourceRoot,
        selected: AuggieDocumentLeaf,
    },
    Directory {
        directory: ProviderSourceDirectory,
        selection_tag: u8,
        routes: Vec<AuggieDocumentLeaf>,
    },
}

impl AuggieTreeAuthority {
    fn open_leaf(&self, leaf: &AuggieDocumentLeaf) -> AuggieSourceBackedResult<AuggieFileStamp> {
        let opened = match self {
            Self::File { root, .. } => root.open_file(&leaf.authority_relative_path)?,
            Self::Directory { directory, .. } => {
                let opened = directory.open_child(leaf.authority_relative_path.as_os_str())?;
                let OpenedProviderSourcePath::File(opened) = opened else {
                    return Err(invalid_source_path(
                        &leaf.canonical_path,
                        "Auggie observed document leaf became a directory",
                    )
                    .into());
                };
                opened
            }
        };
        let stamp = AuggieFileStamp::from_opened(leaf.canonical_path.clone(), opened)?;
        if !leaf.matches(&stamp) {
            return Err(CaptureError::SourceChangedDuringCapture.into());
        }
        Ok(stamp)
    }
}

type AuggieDocumentTree = CompleteDocumentTree<AuggieDocumentLeaf, AuggieTreeAuthority>;

fn discover_auggie_source_backed_unfenced(
    root: &AuggieSourceBackedRoot,
) -> AuggieSourceBackedResult<AuggieSourceBackedInventory> {
    let selected = normalized_auggie_authority_path(&root.path)?;
    let opened = match open_provider_source_path(&selected) {
        Ok(opened) => opened,
        Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(AuggieSourceBackedInventory {
                status: AuggieSourceBackedInventoryStatus::Unavailable,
                tree: None,
            });
        }
        Err(error) => return Err(error.into()),
    };
    let tree = match opened {
        OpenedProviderSourcePath::File(opened) => {
            drop(opened);
            complete_auggie_file_tree(selected)?
        }
        OpenedProviderSourcePath::Directory(directory) => {
            let (directory, selection) =
                match directory.open_child(std::ffi::OsStr::new("sessions")) {
                    Ok(OpenedProviderSourcePath::Directory(child)) => {
                        (child, AuggieTreeSelection::DirectSessionsChild)
                    }
                    Ok(OpenedProviderSourcePath::File(opened)) => {
                        drop(opened);
                        return Err(invalid_source_path(
                            &selected.join("sessions"),
                            "Auggie sessions selection must be a directory",
                        )
                        .into());
                    }
                    Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                        (directory, AuggieTreeSelection::SelectedDirectory)
                    }
                    Err(error) => return Err(error.into()),
                };
            complete_auggie_directory_tree(directory, selection)?
        }
    };
    Ok(AuggieSourceBackedInventory {
        status: AuggieSourceBackedInventoryStatus::Complete,
        tree: Some(tree),
    })
}

fn complete_auggie_file_tree(path: PathBuf) -> AuggieSourceBackedResult<AuggieDocumentTree> {
    if !is_json_path(&path) {
        return Err(invalid_source_path(
            &path,
            "explicit Auggie source files must have a .json extension",
        )
        .into());
    }
    let file_name = path
        .file_name()
        .ok_or_else(|| invalid_source_path(&path, "Auggie source file has no final component"))?;
    let parent = path
        .parent()
        .ok_or_else(|| invalid_source_path(&path, "Auggie source file has no authority parent"))?;
    let root = ProviderSourceRoot::open(parent)?;
    let relative_path = PathBuf::from(file_name);
    let opened = root.open_file(&relative_path)?;
    let stamp = AuggieFileStamp::from_opened(path.clone(), opened)?;
    let selected = AuggieDocumentLeaf::from_opened(path.clone(), relative_path, &stamp);
    drop(stamp);
    let leaves = vec![observed_auggie_leaf(selected.clone())];
    let tree_fingerprint = auggie_tree_fingerprint(
        AuggieTreeSelection::ExplicitFile.tag(),
        selected.authority_fingerprint,
        std::slice::from_ref(&selected),
    );
    Ok(CompleteDocumentTree::new(
        tree_fingerprint,
        leaves,
        AuggieTreeAuthority::File { root, selected },
    ))
}

fn complete_auggie_directory_tree(
    directory: ProviderSourceDirectory,
    selection: AuggieTreeSelection,
) -> AuggieSourceBackedResult<AuggieDocumentTree> {
    let authority = directory.authority_root();
    let selected_path = authority.named_path().join(directory.relative_path());
    let entries = directory.entries(AUGGIE_MAX_DISCOVERED_FILES.saturating_add(1))?;
    let mut physical_sources = HashSet::<[u8; 32]>::new();
    let mut routes = Vec::new();
    let mut leaves = Vec::new();
    for name in entries {
        let path = selected_path.join(&name);
        match directory.open_child(&name)? {
            OpenedProviderSourcePath::File(opened) if is_json_path(&path) => {
                let stamp = AuggieFileStamp::from_opened(path.clone(), opened)?;
                let leaf = AuggieDocumentLeaf::from_opened(path, PathBuf::from(&name), &stamp);
                routes.push(leaf.clone());
                if physical_sources.insert(leaf.authority_fingerprint) {
                    leaves.push(observed_auggie_leaf(leaf));
                }
            }
            OpenedProviderSourcePath::File(_) | OpenedProviderSourcePath::Directory(_) => {}
        }
        if routes.len() > AUGGIE_MAX_DISCOVERED_FILES {
            return Err(invalid_source_path(
                &selected_path,
                "Auggie source-backed discovery exceeds the file bound",
            )
            .into());
        }
    }
    let authority_fingerprint = directory.authority_fingerprint();
    let selection_tag = selection.tag();
    let tree_fingerprint = auggie_tree_fingerprint(selection_tag, authority_fingerprint, &routes);
    Ok(CompleteDocumentTree::new(
        tree_fingerprint,
        leaves,
        AuggieTreeAuthority::Directory {
            directory,
            selection_tag,
            routes,
        },
    ))
}

fn observed_auggie_leaf(leaf: AuggieDocumentLeaf) -> ObservedDocumentLeaf<AuggieDocumentLeaf> {
    let mut digest = Sha256::new();
    digest.update(b"ctx.auggie-document-physical-leaf-v1\0");
    digest.update(leaf.authority_fingerprint);
    ObservedDocumentLeaf::new(DocumentLeafFingerprint::new(digest.finalize().into()), leaf)
}

fn auggie_tree_fingerprint(
    selection_tag: u8,
    authority_fingerprint: [u8; 32],
    routes: &[AuggieDocumentLeaf],
) -> [u8; 32] {
    let mut fingerprints = routes
        .iter()
        .map(|route| {
            let mut digest = Sha256::new();
            digest.update(b"ctx.auggie-document-route-v1\0");
            let path = route.canonical_path.as_os_str().as_encoded_bytes();
            digest.update((path.len() as u64).to_be_bytes());
            digest.update(path);
            digest.update(route.authority_fingerprint);
            <[u8; 32]>::from(digest.finalize())
        })
        .collect::<Vec<_>>();
    fingerprints.sort_unstable();
    let mut digest = Sha256::new();
    digest.update(b"ctx.auggie-document-tree-v1\0");
    digest.update([selection_tag]);
    digest.update(authority_fingerprint);
    digest.update((fingerprints.len() as u64).to_be_bytes());
    for fingerprint in fingerprints {
        digest.update(fingerprint);
    }
    digest.finalize().into()
}

fn scope_auggie_document_fingerprint(
    fingerprint: [u8; 32],
    source_anchor_scope: SourceAnchorScope,
    domain: &[u8],
) -> [u8; 32] {
    let SourceAnchorScope::Lineage(root_lineage) = source_anchor_scope else {
        return fingerprint;
    };
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update(root_lineage);
    digest.update(fingerprint);
    digest.finalize().into()
}

fn scope_auggie_document_tree(
    tree: &mut AuggieDocumentTree,
    source_anchor_scope: SourceAnchorScope,
) {
    for leaf in &mut tree.leaves {
        leaf.fingerprint = DocumentLeafFingerprint::new(scope_auggie_document_fingerprint(
            leaf.fingerprint.as_bytes(),
            source_anchor_scope,
            b"ctx.auggie-document-root-scoped-leaf-v1\0",
        ));
    }
    tree.tree_fingerprint = scope_auggie_document_fingerprint(
        tree.tree_fingerprint,
        source_anchor_scope,
        b"ctx.auggie-document-root-scoped-tree-v1\0",
    );
}

fn revalidate_auggie_tree(tree: &AuggieDocumentTree) -> AuggieSourceBackedResult<[u8; 32]> {
    let (selection_tag, authority_fingerprint) = match &tree.authority {
        AuggieTreeAuthority::File { root, selected, .. } => {
            let stamp = tree.authority.open_leaf(selected)?;
            if !stamp.revalidate()? || !selected.matches(&stamp) {
                return Err(CaptureError::SourceChangedDuringCapture.into());
            }
            drop(stamp);
            root.revalidate()?;
            (
                AuggieTreeSelection::ExplicitFile.tag(),
                selected.authority_fingerprint,
            )
        }
        AuggieTreeAuthority::Directory {
            directory,
            selection_tag,
            routes,
            ..
        } => {
            for route in routes {
                let stamp = tree.authority.open_leaf(route)?;
                if !stamp.revalidate()? || !route.matches(&stamp) {
                    return Err(CaptureError::SourceChangedDuringCapture.into());
                }
                drop(stamp);
            }
            directory.revalidate()?;
            directory.authority_root().revalidate()?;
            (*selection_tag, directory.authority_fingerprint())
        }
    };
    Ok(auggie_tree_fingerprint(
        selection_tag,
        authority_fingerprint,
        match &tree.authority {
            AuggieTreeAuthority::File { selected, .. } => std::slice::from_ref(selected),
            AuggieTreeAuthority::Directory { routes, .. } => routes,
        },
    ))
}

#[derive(Debug)]
pub struct AuggieDocumentTreeAdapter<B = ()> {
    root: AuggieSourceBackedRoot,
    context: ProviderAdapterContext,
    source_anchor_scope: SourceAnchorScope,
    _binding: PhantomData<fn() -> B>,
}

impl<B> AuggieDocumentTreeAdapter<B> {
    pub fn new(root: AuggieSourceBackedRoot, context: ProviderAdapterContext) -> Self {
        Self::new_scoped(root, context, SourceAnchorScope::Unqualified)
    }

    pub fn new_scoped(
        root: AuggieSourceBackedRoot,
        context: ProviderAdapterContext,
        source_anchor_scope: SourceAnchorScope,
    ) -> Self {
        Self {
            root,
            context,
            source_anchor_scope,
            _binding: PhantomData,
        }
    }
}

impl<B> ReplacementDocumentTree for AuggieDocumentTreeAdapter<B>
where
    B: crate::ProviderRuntimeBinding,
{
    type Lifecycle = B::CaptureLifecycleSink;
    type Spool = B::DocumentRecordSpool;
    type RouteControl = crate::ProviderRouteControlExpectation;
    type Leaf = AuggieDocumentLeaf;
    type TreeAuthority = AuggieTreeAuthority;

    fn parser_revision(&self) -> &'static str {
        AUGGIE_PARSER_REVISION
    }

    fn owns_source(&self, source: &SourceKey) -> bool {
        owns_auggie_source(source)
    }

    fn leaf_execution_policy(&self) -> DocumentLeafExecutionPolicy {
        DocumentLeafExecutionPolicy::Serial
    }

    fn discover_complete(&self) -> SourceBackedRouteResult<AuggieDocumentTree> {
        let inventory = discover_auggie_source_backed_unfenced(&self.root).map_err(route_error)?;
        let mut tree = inventory.into_complete_tree().ok_or_else(|| {
            SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Unavailable,
                "Auggie selected route inventory is temporarily unavailable",
            )
        })?;
        scope_auggie_document_tree(&mut tree, self.source_anchor_scope);
        Ok(tree)
    }

    fn scan_changed(
        &self,
        authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
        sink: &mut ChangedDocumentSink<'_, '_, B>,
    ) -> SourceBackedRouteResult<DocumentSourceTerminal> {
        scan_changed_auggie_document::<B>(
            authority,
            leaf,
            &self.context,
            self.source_anchor_scope,
            sink,
        )
    }

    fn revalidate_complete(&self, tree: &AuggieDocumentTree) -> SourceBackedRouteResult<[u8; 32]> {
        revalidate_auggie_tree(tree)
            .map(|fingerprint| {
                scope_auggie_document_fingerprint(
                    fingerprint,
                    self.source_anchor_scope,
                    b"ctx.auggie-document-root-scoped-tree-v1\0",
                )
            })
            .map_err(route_error)
    }
}

fn scan_changed_auggie_document<B>(
    authority: &AuggieTreeAuthority,
    leaf: &AuggieDocumentLeaf,
    context: &ProviderAdapterContext,
    source_anchor_scope: SourceAnchorScope,
    sink: &mut ChangedDocumentSink<'_, '_, B>,
) -> SourceBackedRouteResult<DocumentSourceTerminal>
where
    B: crate::ProviderRuntimeBinding,
{
    let stamp = authority.open_leaf(leaf).map_err(route_error)?;
    let parsed = parse_opened_auggie_source(stamp, context).map_err(route_error)?;
    let ParsedAuggieSource {
        stamp,
        content_digest,
        session,
        events,
        complete_records,
        ignored_records,
        rejected_records,
    } = parsed;
    if !leaf.matches(&stamp) {
        return Err(route_error(CaptureError::SourceChangedDuringCapture));
    }
    let source = auggie_source_key_scoped(&session.provider_session_id, source_anchor_scope)
        .map_err(route_error)?;
    let session_id =
        auggie_session_id(&source, &session.provider_session_id).map_err(route_error)?;
    let observation = auggie_source_observation(&source, &stamp).map_err(route_error)?;
    let certified_bytes = stamp.len;
    drop(stamp);
    sink.begin_source(source.clone())?;
    let mut event_ids = HashSet::with_capacity(events.len());
    let mut indexed_documents = 0_u64;
    for event in events {
        let document = auggie_core_record(
            &source,
            session_id,
            source_anchor_scope,
            &session,
            content_digest,
            event,
        )
        .map_err(route_error)?;
        if !event_ids.insert(document.event_id) {
            return Err(route_error(
                AuggieSourceBackedError::DuplicateEventIdentity(document.event_id),
            ));
        }
        sink.emit_core_record(document)?;
        indexed_documents = indexed_documents
            .checked_add(1)
            .ok_or_else(|| route_error("too many Auggie messages"))?;
    }
    Ok(DocumentSourceTerminal {
        source,
        opening: observation.clone(),
        closing: observation,
        parser_revision: AUGGIE_PARSER_REVISION,
        content_digest,
        counts: ScannedSourceCounts {
            complete_records,
            retained_records: indexed_documents,
            rejected_records,
            ignored_records,
            indexed_documents,
            certified_bytes,
        },
    })
}

fn is_json_path(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()) == Some("json")
}

fn owns_auggie_source(source: &SourceKey) -> bool {
    source.provider() == ctx_history_core::CaptureProvider::Auggie.as_str()
        && source.source_format() == AUGGIE_SESSION_JSON_SOURCE_FORMAT
        && source.schema_variant() == AUGGIE_SOURCE_SCHEMA_VARIANT
        && source.provider_identity_version() == 1
}

#[cfg(test)]
fn auggie_source_key(native_session_id: &str) -> AuggieSourceBackedResult<SourceKey> {
    auggie_source_key_scoped(native_session_id, SourceAnchorScope::Unqualified)
}

fn auggie_source_key_scoped(
    native_session_id: &str,
    source_anchor_scope: SourceAnchorScope,
) -> AuggieSourceBackedResult<SourceKey> {
    Ok(SourceKey::derive_provider_native_scoped(
        ctx_history_core::CaptureProvider::Auggie.as_str(),
        AUGGIE_SESSION_JSON_SOURCE_FORMAT,
        AUGGIE_SOURCE_SCHEMA_VARIANT,
        1,
        AUGGIE_SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(native_session_id)?,
        source_anchor_scope,
    )?)
}

fn auggie_session_id(
    source: &SourceKey,
    native_session_id: &str,
) -> AuggieSourceBackedResult<StableEntityId> {
    let native_session_key = NativeSessionKey::native_id(
        AUGGIE_NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(native_session_id)?,
    )?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: AUGGIE_LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })?)
}

fn auggie_core_record(
    source: &SourceKey,
    session_id: StableEntityId,
    source_anchor_scope: SourceAnchorScope,
    session: &ParsedAuggieSession,
    content_digest: [u8; 32],
    parsed: ParsedAuggieEvent,
) -> AuggieSourceBackedResult<CoreRecord> {
    let (agent_scope, parent_session_id, root_session_id, session_relationship) =
        match auggie_lineage_evidence(session) {
            AuggieLineageEvidence::Root => (Some(AgentScope::Primary), None, None, None),
            AuggieLineageEvidence::Child { parent, root } => (
                Some(AgentScope::Subagent),
                Some(related_auggie_session_id(parent, source_anchor_scope)?),
                root.map(|root| related_auggie_session_id(root, source_anchor_scope))
                    .transpose()?,
                Some(ProviderNativeSessionRelationship::Delegated),
            ),
            AuggieLineageEvidence::Unknown => (None, None, None, None),
        };
    let native_item_key = if let Some(native_event_id) = parsed.native_event_id.as_deref() {
        NativeItemKey::native_id(
            AUGGIE_NATIVE_EVENT_NAMESPACE,
            TypedKey::utf8(native_event_id)?,
        )?
    } else {
        NativeItemKey::revision_scoped_position(
            AUGGIE_EVENT_POSITION_KIND,
            TypedKey::composite(vec![
                TypedKey::U64(u64::try_from(parsed.chat_index).map_err(|_| {
                    CaptureError::InvalidPayload("Auggie chat history index exceeds u64".to_owned())
                })?),
                TypedKey::utf8(parsed.message_kind)?,
            ])?,
            TypedKey::bytes(content_digest.to_vec())?,
        )?
    };
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: AUGGIE_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let chat_index = u64::try_from(parsed.chat_index).map_err(|_| {
        CaptureError::InvalidPayload("Auggie chat history index exceeds u64".to_owned())
    })?;
    let object_key = TypedKey::composite(vec![
        TypedKey::utf8(&parsed.provider_event_hash)?,
        TypedKey::U64(chat_index),
        TypedKey::utf8(parsed.message_kind)?,
    ])?;
    let body = (!parsed.text.is_empty())
        .then_some(parsed.text)
        .ok_or(AuggieSourceBackedError::MissingNormalizedContent)?;
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        source.clone(),
        parsed.provider_event_index,
        parsed.event_type.as_str(),
        AUGGIE_PARSER_REVISION,
        body,
    )?;
    record.agent_scope = agent_scope;
    record.parent_session_id = parent_session_id;
    record.root_session_id = root_session_id;
    record.session_relationship = session_relationship;
    record.provider_session_id = Some(session.provider_session_id.clone());
    record.native_event_id = Some(object_key);
    record.occurred_at_unix_ms = Some(parsed.occurred_at.timestamp_millis());
    record.role = Some(parsed.role.as_str().to_owned());
    record.content.activity = session.cwd.as_ref().map(|cwd| CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id: None,
        invocation: None,
        result: None,
        facts: vec![ProviderDeclaredFact {
            kind: LiteralFactKind::SessionCwd,
            value: cwd.clone(),
        }],
    });
    record.validate_contract()?;
    Ok(record)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuggieLineageEvidence<'a> {
    Root,
    Child {
        parent: &'a str,
        root: Option<&'a str>,
    },
    Unknown,
}

fn auggie_lineage_evidence(session: &ParsedAuggieSession) -> AuggieLineageEvidence<'_> {
    let native_session_id = session.provider_session_id.as_str();
    let parent = &session.parent_session_claim;
    let root = &session.root_session_claim;
    match (parent, root) {
        (AuggieLineageClaim::Absent, AuggieLineageClaim::Absent) => AuggieLineageEvidence::Root,
        (AuggieLineageClaim::Absent, AuggieLineageClaim::Exact(root))
            if root == native_session_id =>
        {
            AuggieLineageEvidence::Root
        }
        (AuggieLineageClaim::Exact(parent), AuggieLineageClaim::Absent)
            if parent != native_session_id =>
        {
            AuggieLineageEvidence::Child { parent, root: None }
        }
        (AuggieLineageClaim::Exact(parent), AuggieLineageClaim::Exact(root))
            if parent != native_session_id && root != native_session_id =>
        {
            AuggieLineageEvidence::Child {
                parent,
                root: Some(root),
            }
        }
        _ => AuggieLineageEvidence::Unknown,
    }
}

fn related_auggie_session_id(
    native_session_id: &str,
    source_anchor_scope: SourceAnchorScope,
) -> AuggieSourceBackedResult<StableEntityId> {
    let source = auggie_source_key_scoped(native_session_id, source_anchor_scope)?;
    auggie_session_id(&source, native_session_id)
}

fn auggie_source_observation(
    source: &SourceKey,
    stamp: &AuggieFileStamp,
) -> AuggieSourceBackedResult<SourceObservation> {
    Ok(SourceObservation::new(
        source.clone(),
        AUGGIE_SOURCE_REVISION_KIND,
        auggie_stamp_revision(stamp),
    )?)
}

fn auggie_stamp_revision(stamp: &AuggieFileStamp) -> Vec<u8> {
    let mut revision = Vec::with_capacity(42);
    revision.extend_from_slice(&stamp.len.to_be_bytes());
    let (sign, seconds, nanos) = system_time_parts(stamp.modified);
    revision.push(sign);
    revision.extend_from_slice(&seconds.to_be_bytes());
    revision.extend_from_slice(&nanos.to_be_bytes());
    revision.push(u8::from(stamp.readonly));
    revision.extend_from_slice(&stamp.device.unwrap_or_default().to_be_bytes());
    revision.extend_from_slice(&stamp.inode.unwrap_or_default().to_be_bytes());
    revision
}

fn system_time_parts(time: SystemTime) -> (u8, u64, u32) {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => (1, duration.as_secs(), duration.subsec_nanos()),
        Err(error) => {
            let duration = error.duration();
            (0, duration.as_secs(), duration.subsec_nanos())
        }
    }
}

#[cfg(test)]
#[path = "source_backed_tests.rs"]
mod tests;
