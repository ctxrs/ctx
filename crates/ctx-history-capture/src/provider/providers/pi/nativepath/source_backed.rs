//! Thin source-backed projection over Pi's bounded NativePath JSONL scanner.
//!
//! This module deliberately stops at provider-owned discovery, projection,
//! certification, and exact record hydration. The shared lifecycle
//! coordinator owns staging, append/rewrite choice, deletion, and publication.

use std::{
    collections::{HashMap, HashSet},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, CertifiedSource,
    CertifiedSourceInventory, ContentSourceResolver, EventHydrationRequest, EventIdentityInput,
    HydratedProviderRecord, HydrationFailure, HydrationFailureKind, LocatorRevisionPolicy,
    NativeItemKey, NativeRecordCoordinate, NativeSessionKey, PositionStability,
    ProjectionContractError, ScannedSourceCounts, SessionHydrationRequest, SessionIdentityInput,
    SourceAnchor, SourceFrontier, SourceInventoryObservation, SourceKey, SourceObservation,
    SourceRecordLocator, SourceResolverContractError, StableEntityId, TypedKey,
};
use ctx_history_index::LexicalDocument;
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    checkpoint::PiNativeCheckpoint,
    reader::{
        open_pi_native_session, open_pi_native_session_retained, PiNativeOpenOutcome,
        PiNativeOwnedPage, PiNativeProfile, PiNativeScanOptions, PiSourceLifecycle,
    },
    rows::{PiNativeCoreUnit, PiNativeEventRow, PiNativeFileTouchRow, PiNativeSessionRow},
    source::{discover_pi_sessions, PiFrozenSource, PiNativePathError},
};
use crate::{
    common::io::OpenedProviderSourceFile,
    provider::{
        importer::provider_path_identity,
        providers::pi::{pi_entry_text, PI_SOURCE_FORMAT},
    },
    CaptureError, ProviderAdapterContext, MAX_PROVIDER_JSONL_LINE_BYTES,
};

const PI_SOURCE_ANCHOR_NAMESPACE: &str = "pi.session";
const PI_NATIVE_SESSION_NAMESPACE: &str = "pi.session";
const PI_NATIVE_EVENT_NAMESPACE: &str = "pi.entry";
const PI_NATIVE_EVENT_POSITION_KIND: &str = "pi.jsonl.record-ordinal";
const PI_LOGICAL_SESSION_KIND: &str = "pi-session";
const PI_LOGICAL_EVENT_KIND: &str = "pi-event";
const PI_SOURCE_SCHEMA_VARIANT: &str = "pi-nativepath-jsonl-v1";
const PI_SOURCE_REVISION_KIND: &str = "pi-ordinary-file-observation-v1";
const PI_FRONTIER_KIND: &str = "pi-nativepath-checkpoint-v1";
const PI_PARSER_REVISION: &str = "pi-nativepath-source-backed-v1";
const PI_DISCOVERY_REVISION: &str = "pi-session-root-discovery-v1";
const PI_INVENTORY_REVISION_KIND: &str = "pi-session-root-snapshot-v1";
const PI_WINNING_ROOT_AUTHORITY: &str = "pi.winning-session-root";
const PI_EXPLICIT_ROOT_AUTHORITY: &str = "pi.explicit-session-root";
const PI_INVENTORY_DIGEST_DOMAIN: &[u8] = b"ctx-pi-source-inventory-v1\0";
const MAX_HYDRATED_PI_RECORD_BYTES: u64 = MAX_PROVIDER_JSONL_LINE_BYTES as u64 + 2;

#[derive(Debug, Error)]
pub(crate) enum PiSourceBackedError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Native(#[from] PiNativePathError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    ResolverContract(#[from] SourceResolverContractError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("historical Pi root {0:?} is accepted only when explicitly configured")]
    HistoricalRootRequiresExplicit(PathBuf),
    #[error("Pi source {0:?} was removed before it could be projected")]
    SourceDeleted(PathBuf),
    #[error("Pi source {0:?} has no valid session header")]
    MissingSessionHeader(PathBuf),
    #[error("Pi source {0:?} contains more than one session header")]
    MultipleSessionHeaders(PathBuf),
    #[error("Pi source session changed from {expected:?} to {actual:?}")]
    SessionChanged { expected: String, actual: String },
    #[error("Pi source does not match the supplied prior certificate")]
    PriorSourceMismatch,
    #[error("Pi source certificate has no NativePath checkpoint frontier")]
    MissingCheckpoint,
    #[error("Pi source certificate has an unsupported NativePath checkpoint")]
    InvalidCheckpoint,
    #[error("Pi source-backed scanner has not been fully drained")]
    ProjectionNotDrained,
    #[error("Pi Core-only source-backed scanner emitted a Pro page")]
    UnexpectedProPage,
    #[error("Pi source-backed scan counters do not reconcile")]
    ScanCountMismatch,
    #[error("Pi source-backed scan count overflow")]
    CountOverflow,
    #[error("Pi root changed while it was being projected")]
    InventoryChanged,
    #[error("Pi root contains duplicate native session {0:?}")]
    DuplicateNativeSession(String),
    #[error("Pi resolver received duplicate routes for one source")]
    DuplicateSourceRoute,
    #[error("locator is not a Pi NativePath JSONL record")]
    InvalidPiLocator,
    #[error("Pi locator source was not supplied to this resolver")]
    LocatorSourceNotFound,
    #[error("Pi locator byte range exceeds the bounded NativePath record size")]
    LocatorRangeTooLarge,
    #[error("Pi locator byte range ends after the provider source")]
    LocatorRangeMissing,
    #[error("Pi locator record digest no longer matches provider bytes")]
    LocatorDigestMismatch,
}

pub(crate) type PiSourceBackedResult<T> = Result<T, PiSourceBackedError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PiRootAuthority {
    Winning,
    Explicit,
}

/// One already-resolved provider root.
///
/// The caller performs Pi's normal root precedence resolution before using
/// `winning`. Historical OMP roots never enter that implicit winner set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PiSourceBackedRoot {
    path: PathBuf,
    authority: PiRootAuthority,
}

impl PiSourceBackedRoot {
    pub(crate) fn winning(path: impl Into<PathBuf>) -> PiSourceBackedResult<Self> {
        let path = path.into();
        if is_historical_omp_root(&path) {
            return Err(PiSourceBackedError::HistoricalRootRequiresExplicit(path));
        }
        Ok(Self {
            path,
            authority: PiRootAuthority::Winning,
        })
    }

    pub(crate) fn explicit(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            authority: PiRootAuthority::Explicit,
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    fn authority_namespace(&self) -> &'static str {
        match self.authority {
            PiRootAuthority::Winning => PI_WINNING_ROOT_AUTHORITY,
            PiRootAuthority::Explicit => PI_EXPLICIT_ROOT_AUTHORITY,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PiSourceRoute {
    pub(crate) source: SourceKey,
    pub(crate) path: PathBuf,
    pub(crate) source_revision: String,
    opened: Arc<OpenedProviderSourceFile>,
}

#[derive(Debug)]
pub(crate) struct PiSourceBackedPage {
    pub(crate) source: SourceKey,
    pub(crate) documents: Vec<LexicalDocument>,
}

#[derive(Clone, Debug)]
pub(crate) struct PiSourceBackedProjection {
    pub(crate) route: PiSourceRoute,
    pub(crate) lifecycle: PiSourceLifecycle,
    pub(crate) certificate: CertifiedSource,
    pub(crate) checkpoint: PiNativeCheckpoint,
}

#[derive(Clone, Debug)]
pub(crate) struct PiSourceBackedRootProjection {
    pub(crate) inventory: CertifiedSourceInventory,
    pub(crate) sources: Vec<PiSourceBackedProjection>,
}

/// Bounded pull scanner for one Pi JSONL source.
///
/// At most one scanner page and its lexical documents are retained. Calling
/// `finish` produces evidence only; it cannot publish any lifecycle action.
pub(crate) struct PiSourceBackedScanner {
    scanner: Box<super::reader::PiNativeScanner>,
    path: PathBuf,
    source_path: String,
    source_revision: String,
    opened: Arc<OpenedProviderSourceFile>,
    previous: Option<CertifiedSource>,
    source: Option<SourceKey>,
    session_id: Option<StableEntityId>,
    parent_session_id: Option<StableEntityId>,
    root_session_id: Option<StableEntityId>,
    provider_session_id: Option<String>,
    parent_provider_session_id: Option<String>,
    cwd: Option<String>,
    saw_header: bool,
    retained_delta: u64,
    rejected_delta: u64,
    indexed_delta: u64,
}

impl PiSourceBackedScanner {
    pub(crate) fn open(
        path: impl AsRef<Path>,
        mut context: ProviderAdapterContext,
        previous: Option<CertifiedSource>,
    ) -> PiSourceBackedResult<Self> {
        let path = path.as_ref().to_path_buf();
        let source_path = provider_path_identity(&path)?;
        context.source_path = Some(path.clone());
        let mut options = PiNativeScanOptions::new(context, PiNativeProfile::CoreOnly);
        if let Some(previous) = previous.as_ref() {
            options.resume.core = Some(decode_checkpoint(previous)?);
        }
        let PiNativeOpenOutcome::Ready(scanner) = open_pi_native_session(&path, options)? else {
            return Err(PiSourceBackedError::SourceDeleted(path));
        };
        Self::from_scanner(path, source_path, previous, scanner)
    }

    fn open_retained(
        path: &Path,
        opened: Arc<OpenedProviderSourceFile>,
        mut context: ProviderAdapterContext,
        previous: Option<CertifiedSource>,
    ) -> PiSourceBackedResult<Self> {
        let path = path.to_path_buf();
        let source_path = provider_path_identity(&path)?;
        context.source_path = Some(path.clone());
        let mut options = PiNativeScanOptions::new(context, PiNativeProfile::CoreOnly);
        if let Some(previous) = previous.as_ref() {
            options.resume.core = Some(decode_checkpoint(previous)?);
        }
        let PiNativeOpenOutcome::Ready(scanner) =
            open_pi_native_session_retained(&path, opened, options)?
        else {
            return Err(PiSourceBackedError::SourceDeleted(path));
        };
        Self::from_scanner(path, source_path, previous, scanner)
    }

    fn from_scanner(
        path: PathBuf,
        source_path: String,
        previous: Option<CertifiedSource>,
        scanner: Box<super::reader::PiNativeScanner>,
    ) -> PiSourceBackedResult<Self> {
        let opened = scanner.opened_source();
        let source_revision = scanner.source_revision().to_owned();
        let provider_session_id = scanner.provider_session_id().map(str::to_owned);
        let parent_provider_session_id = scanner.parent_provider_session_id().map(str::to_owned);
        let cwd = scanner.session_cwd().map(str::to_owned);
        let saw_header = provider_session_id.is_some();
        let mut scanner = Self {
            scanner,
            path,
            source_path,
            source_revision,
            opened,
            previous,
            source: None,
            session_id: None,
            parent_session_id: None,
            root_session_id: None,
            provider_session_id: None,
            parent_provider_session_id: None,
            cwd,
            saw_header,
            retained_delta: 0,
            rejected_delta: 0,
            indexed_delta: 0,
        };
        if let Some(provider_session_id) = provider_session_id {
            scanner.bind_session(&provider_session_id, parent_provider_session_id.as_deref())?;
        }
        Ok(scanner)
    }

    pub(crate) fn next_page(&mut self) -> PiSourceBackedResult<Option<PiSourceBackedPage>> {
        loop {
            let Some(page) = self.scanner.next_page()? else {
                return Ok(None);
            };
            let PiNativeOwnedPage::Core(page) = page else {
                return Err(PiSourceBackedError::UnexpectedProPage);
            };
            let documents = self.project_core_units(page.core.units)?;
            if documents.is_empty() {
                continue;
            }
            let source = self
                .source
                .clone()
                .ok_or_else(|| PiSourceBackedError::MissingSessionHeader(self.path.clone()))?;
            return Ok(Some(PiSourceBackedPage { source, documents }));
        }
    }

    pub(crate) fn finish(self) -> PiSourceBackedResult<PiSourceBackedProjection> {
        let outcome = self
            .scanner
            .outcome()
            .ok_or(PiSourceBackedError::ProjectionNotDrained)?;
        let lifecycle = outcome
            .core_lifecycle
            .ok_or(PiSourceBackedError::ProjectionNotDrained)?;
        let checkpoint = outcome
            .core_checkpoint
            .ok_or(PiSourceBackedError::MissingCheckpoint)?;
        let source = self
            .source
            .ok_or_else(|| PiSourceBackedError::MissingSessionHeader(self.path.clone()))?;
        self.scanner.revalidate_source()?;

        let base_counts = if matches!(
            lifecycle,
            PiSourceLifecycle::NoOp | PiSourceLifecycle::Append | PiSourceLifecycle::Relocate
        ) {
            self.previous
                .as_ref()
                .map(CertifiedSource::counts)
                .unwrap_or_default()
        } else {
            ScannedSourceCounts::default()
        };
        let complete_records =
            checked_add(base_counts.complete_records, outcome.stats.complete_records)?;
        let retained_records = checked_add(base_counts.retained_records, self.retained_delta)?;
        let rejected_records = checked_add(base_counts.rejected_records, self.rejected_delta)?;
        let indexed_documents = checked_add(base_counts.indexed_documents, self.indexed_delta)?;
        let classified = checked_add(retained_records, rejected_records)?;
        let ignored_records = complete_records
            .checked_sub(classified)
            .ok_or(PiSourceBackedError::ScanCountMismatch)?;
        if checkpoint.next_ordinal != complete_records || indexed_documents > retained_records {
            return Err(PiSourceBackedError::ScanCountMismatch);
        }
        let counts = ScannedSourceCounts {
            complete_records,
            retained_records,
            rejected_records,
            ignored_records,
            indexed_documents,
            certified_bytes: checkpoint.complete_offset,
        };
        let observation = SourceObservation::new(
            source.clone(),
            PI_SOURCE_REVISION_KIND,
            self.source_revision.as_bytes().to_vec(),
        )?;
        let frontier = SourceFrontier::new(
            PI_FRONTIER_KIND,
            TypedKey::bytes(serde_json::to_vec(&checkpoint)?)?,
            checkpoint.complete_offset,
            checkpoint.committed_prefix_sha256,
        )?;
        let certificate = CertifiedSource::certify_with_frontier(
            observation.clone(),
            observation,
            PI_PARSER_REVISION,
            checkpoint.committed_prefix_sha256,
            counts,
            Some(frontier),
        )?;
        Ok(PiSourceBackedProjection {
            route: PiSourceRoute {
                source,
                path: self.path,
                source_revision: self.source_revision,
                opened: self.opened,
            },
            lifecycle,
            certificate,
            checkpoint,
        })
    }

    fn project_core_units(
        &mut self,
        units: Vec<PiNativeCoreUnit>,
    ) -> PiSourceBackedResult<Vec<LexicalDocument>> {
        let mut events = Vec::new();
        let mut touches = HashMap::<u64, Vec<String>>::new();
        for unit in units {
            match unit {
                PiNativeCoreUnit::Session(row) => self.observe_session(row)?,
                PiNativeCoreUnit::Event(row) => {
                    self.retained_delta = checked_add(self.retained_delta, 1)?;
                    events.push(row);
                }
                PiNativeCoreUnit::FileTouch(row) => collect_touch(&mut touches, row),
                PiNativeCoreUnit::Rejection(_) => {
                    self.rejected_delta = checked_add(self.rejected_delta, 1)?;
                }
            }
        }
        let mut documents = Vec::with_capacity(events.len());
        for event in events {
            let event_touches = touches
                .remove(&event.provider_event_index)
                .unwrap_or_default();
            if let Some(document) = self.lexical_document(event, event_touches)? {
                self.indexed_delta = checked_add(self.indexed_delta, 1)?;
                documents.push(document);
            }
        }
        Ok(documents)
    }

    fn observe_session(&mut self, row: PiNativeSessionRow) -> PiSourceBackedResult<()> {
        if self.saw_header {
            return Err(PiSourceBackedError::MultipleSessionHeaders(
                self.path.clone(),
            ));
        }
        self.saw_header = true;
        self.cwd = row.cwd;
        self.bind_session(&row.provider_session_id, row.parent_session.as_deref())
    }

    fn bind_session(
        &mut self,
        provider_session_id: &str,
        parent_provider_session_id: Option<&str>,
    ) -> PiSourceBackedResult<()> {
        if let Some(expected) = self.provider_session_id.as_deref() {
            if expected != provider_session_id {
                return Err(PiSourceBackedError::SessionChanged {
                    expected: expected.to_owned(),
                    actual: provider_session_id.to_owned(),
                });
            }
            return Ok(());
        }
        let source = pi_source_key(provider_session_id)?;
        if self
            .previous
            .as_ref()
            .is_some_and(|previous| !previous.observation().source().exact_descriptor_eq(&source))
        {
            return Err(PiSourceBackedError::PriorSourceMismatch);
        }
        let session_id = pi_session_identity(&source, provider_session_id)?;
        let parent_session_id = parent_provider_session_id
            .map(pi_session_identity_for_native)
            .transpose()?;
        // Pi records only the immediate parent. In the absence of a complete
        // ancestry chain, that parent is the strongest provider-backed root
        // evidence; primary sessions are their own root.
        let root_session_id = parent_session_id.unwrap_or(session_id);
        self.source = Some(source);
        self.session_id = Some(session_id);
        self.parent_session_id = parent_session_id;
        self.root_session_id = Some(root_session_id);
        self.provider_session_id = Some(provider_session_id.to_owned());
        self.parent_provider_session_id = parent_provider_session_id.map(str::to_owned);
        Ok(())
    }

    fn lexical_document(
        &self,
        row: PiNativeEventRow,
        touched_files: Vec<String>,
    ) -> PiSourceBackedResult<Option<LexicalDocument>> {
        let source = self
            .source
            .as_ref()
            .ok_or_else(|| PiSourceBackedError::MissingSessionHeader(self.path.clone()))?;
        let session_id = self
            .session_id
            .ok_or_else(|| PiSourceBackedError::MissingSessionHeader(self.path.clone()))?;
        let root_session_id = self
            .root_session_id
            .ok_or_else(|| PiSourceBackedError::MissingSessionHeader(self.path.clone()))?;
        let provider_session_id = self
            .provider_session_id
            .as_deref()
            .ok_or_else(|| PiSourceBackedError::MissingSessionHeader(self.path.clone()))?;
        if row.provider_session_id != provider_session_id {
            return Err(PiSourceBackedError::SessionChanged {
                expected: provider_session_id.to_owned(),
                actual: row.provider_session_id,
            });
        }
        let body = lexical_body(&row);
        if body.is_empty() {
            return Ok(None);
        }
        let native_event_key = match row.cursor.as_deref() {
            Some(cursor) if !cursor.is_empty() => {
                NativeItemKey::native_id(PI_NATIVE_EVENT_NAMESPACE, TypedKey::utf8(cursor)?)?
            }
            _ => NativeItemKey::certified_position(
                PI_NATIVE_EVENT_POSITION_KIND,
                TypedKey::U64(row.locator.source_record_ordinal),
                PositionStability::AppendStable,
            )?,
        };
        let event_id = derive_event_id(EventIdentityInput {
            source,
            session_id,
            logical_item_kind: PI_LOGICAL_EVENT_KIND,
            native_item_key: &native_event_key,
            subrecord_selector: None,
        })?;
        let locator_event_key = row
            .cursor
            .as_deref()
            .filter(|cursor| !cursor.is_empty())
            .map(TypedKey::utf8)
            .transpose()?
            .unwrap_or(TypedKey::U64(row.locator.source_record_ordinal));
        let byte_length = row
            .locator
            .byte_end_exclusive
            .checked_sub(row.locator.byte_start)
            .ok_or(PiSourceBackedError::InvalidPiLocator)?;
        let locator = SourceRecordLocator::new(
            source.clone(),
            NativeRecordCoordinate::Jsonl {
                byte_offset: row.locator.byte_start,
                byte_length,
                physical_ordinal: row.locator.source_record_ordinal,
                native_session_key: Some(TypedKey::utf8(provider_session_id)?),
                native_event_key: Some(locator_event_key),
            },
            LocatorRevisionPolicy::StableRecordEvidence,
            None,
            row.locator.record_sha256,
        )?;
        let is_primary = self.parent_provider_session_id.is_none();
        Ok(Some(LexicalDocument {
            event_id,
            session_id,
            parent_session_id: self.parent_session_id,
            root_session_id,
            source: source.clone(),
            locator,
            provider_session_id: Some(provider_session_id.to_owned()),
            branch: None,
            source_path: Some(self.source_path.clone()),
            agent_type: if is_primary {
                AgentType::Primary
            } else {
                AgentType::Subagent
            }
            .as_str()
            .to_owned(),
            is_primary,
            event_sequence: row.provider_event_index,
            occurred_at_unix_ms: Some(row.occurred_at.timestamp_millis()),
            event_type: row.event_type.as_str().to_owned(),
            role: row.role.map(|role| role.as_str().to_owned()),
            body,
            workspace: None,
            cwd: self.cwd.clone(),
            touched_files,
        }))
    }
}

/// Cold-project one complete winning or explicit root.
///
/// Pages are handed to the caller as they are produced. The returned
/// certificates and complete inventory are evidence for the central
/// coordinator; this function performs no append/rewrite/deletion publication.
pub(crate) fn project_pi_source_backed_root_cold(
    root: &PiSourceBackedRoot,
    context: ProviderAdapterContext,
    mut emit: impl FnMut(PiSourceBackedPage),
) -> PiSourceBackedResult<PiSourceBackedRootProjection> {
    let opening = observe_root(root)?;
    let mut sources = Vec::with_capacity(opening.discovery.sessions.len());
    let mut native_sessions = HashSet::new();
    for path in &opening.discovery.sessions {
        let mut scanner = PiSourceBackedScanner::open_retained(
            path,
            opening.discovery.opened(path)?,
            context.clone(),
            None,
        )?;
        while let Some(page) = scanner.next_page()? {
            emit(page);
        }
        let projection = scanner.finish()?;
        let SourceAnchor::ProviderNative { key, .. } = projection.route.source.anchor() else {
            return Err(PiSourceBackedError::PriorSourceMismatch);
        };
        let TypedKey::Utf8(native_session) = key else {
            return Err(PiSourceBackedError::PriorSourceMismatch);
        };
        if !native_sessions.insert(native_session.clone()) {
            return Err(PiSourceBackedError::DuplicateNativeSession(
                native_session.clone(),
            ));
        }
        sources.push(projection);
    }
    let closing = observe_retained_root(root, &opening.discovery)?;
    if opening.discovery.sessions != closing.discovery.sessions {
        return Err(PiSourceBackedError::InventoryChanged);
    }
    let inventory = CertifiedSourceInventory::certify(
        opening.observation,
        closing.observation,
        PI_DISCOVERY_REVISION,
        sources
            .iter()
            .map(|projection| projection.route.source.clone())
            .collect(),
    )?;
    Ok(PiSourceBackedRootProjection { inventory, sources })
}

#[derive(Debug)]
struct PiRootObservation {
    discovery: super::source::PiDiscovery,
    observation: SourceInventoryObservation,
}

fn observe_root(root: &PiSourceBackedRoot) -> PiSourceBackedResult<PiRootObservation> {
    let discovery = discover_pi_sessions(root.path())?;
    observe_discovery(root, discovery)
}

fn observe_retained_root(
    root: &PiSourceBackedRoot,
    opening: &super::source::PiDiscovery,
) -> PiSourceBackedResult<PiRootObservation> {
    observe_discovery(root, opening.rediscover()?)
}

fn observe_discovery(
    root: &PiSourceBackedRoot,
    discovery: super::source::PiDiscovery,
) -> PiSourceBackedResult<PiRootObservation> {
    let root_identity = provider_path_identity(root.path())?;
    let mut digest = Sha256::new();
    digest.update(PI_INVENTORY_DIGEST_DOMAIN);
    digest.update((discovery.sessions.len() as u64).to_be_bytes());
    for path in &discovery.sessions {
        let (_, source) = PiFrozenSource::from_opened(path, discovery.opened(path)?)?;
        hash_inventory_field(&mut digest, provider_path_identity(path)?.as_bytes());
        hash_inventory_field(&mut digest, source.source_revision().as_bytes());
    }
    let observation = SourceInventoryObservation::new(
        CaptureProvider::Pi.as_str(),
        root.authority_namespace(),
        TypedKey::utf8(root_identity)?,
        PI_INVENTORY_REVISION_KIND,
        digest.finalize().to_vec(),
    )?;
    Ok(PiRootObservation {
        discovery,
        observation,
    })
}

fn hash_inventory_field(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
}

/// Invocation-local exact range resolver. Routes are supplied from projection,
/// so hydration never rediscovers or guesses a provider root.
#[derive(Debug)]
pub(crate) struct PiSourceBackedResolver {
    routes: HashMap<SourceKey, PiSourceRoute>,
}

impl PiSourceBackedResolver {
    pub(crate) fn new(
        routes: impl IntoIterator<Item = PiSourceRoute>,
    ) -> PiSourceBackedResult<Self> {
        let mut by_source = HashMap::<SourceKey, PiSourceRoute>::new();
        for route in routes {
            if let Some(existing) = by_source.get(&route.source) {
                if !existing.source.exact_descriptor_eq(&route.source)
                    || existing.path != route.path
                {
                    return Err(PiSourceBackedError::DuplicateSourceRoute);
                }
                continue;
            }
            by_source.insert(route.source.clone(), route);
        }
        Ok(Self { routes: by_source })
    }

    pub(crate) fn hydrate_message(
        &self,
        request: &EventHydrationRequest,
    ) -> Result<Option<String>, HydrationFailure> {
        let hydrated = self.hydrate_event(request)?;
        String::from_utf8(hydrated.provider_bytes)
            .map(Some)
            .map_err(|error| HydrationFailure {
                kind: HydrationFailureKind::InvalidLocator,
                detail: error.to_string(),
            })
    }

    fn hydrate_exact(
        &self,
        request: &EventHydrationRequest,
    ) -> PiSourceBackedResult<HydratedProviderRecord> {
        request.locator().validate_contract()?;
        let (byte_offset, byte_length, _, _) = validate_pi_locator(request.locator())?;
        if byte_length > MAX_HYDRATED_PI_RECORD_BYTES {
            return Err(PiSourceBackedError::LocatorRangeTooLarge);
        }
        let route = self
            .routes
            .get(request.locator().source())
            .ok_or(PiSourceBackedError::LocatorSourceNotFound)?;
        if !route.source.exact_descriptor_eq(request.locator().source()) {
            return Err(PiSourceBackedError::InvalidPiLocator);
        }
        let (file, source) = PiFrozenSource::from_opened(&route.path, Arc::clone(&route.opened))?;
        let range_end = byte_offset
            .checked_add(byte_length)
            .ok_or(PiSourceBackedError::LocatorRangeTooLarge)?;
        if range_end > source.len {
            return Err(PiSourceBackedError::LocatorRangeMissing);
        }
        let byte_length =
            usize::try_from(byte_length).map_err(|_| PiSourceBackedError::LocatorRangeTooLarge)?;
        let provider_bytes = route.opened.read_exact_range(
            byte_offset,
            byte_length,
            usize::try_from(MAX_HYDRATED_PI_RECORD_BYTES)
                .map_err(|_| PiSourceBackedError::LocatorRangeTooLarge)?,
        )?;
        source.fence(&file)?;
        let digest: [u8; 32] = Sha256::digest(json_record_bytes(&provider_bytes)).into();
        if &digest != request.locator().record_digest() {
            return Err(PiSourceBackedError::LocatorDigestMismatch);
        }
        let record = json_record_bytes(&provider_bytes);
        let value: Value = serde_json::from_slice(record)?;
        let entry_type = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let event_type = super::reader::pi_native_event_type(entry_type, value.get("message"));
        let display_text = pi_entry_text(&value, value.get("message"))
            .filter(|text| !text.trim().is_empty())
            .unwrap_or_else(|| event_type.as_str().to_owned());
        Ok(HydratedProviderRecord {
            event_id: request.event_id(),
            provider_bytes: display_text.into_bytes(),
        })
    }
}

impl ContentSourceResolver for PiSourceBackedResolver {
    fn hydrate_event(
        &self,
        request: &EventHydrationRequest,
    ) -> Result<HydratedProviderRecord, HydrationFailure> {
        self.hydrate_exact(request).map_err(hydration_failure)
    }

    fn hydrate_session(
        &self,
        request: &SessionHydrationRequest,
    ) -> Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        request
            .events()
            .iter()
            .map(|event| self.hydrate_event(event))
            .collect()
    }
}

fn validate_pi_locator(
    locator: &SourceRecordLocator,
) -> PiSourceBackedResult<(u64, u64, u64, String)> {
    if locator.source().provider() != CaptureProvider::Pi.as_str()
        || locator.source().source_format() != PI_SOURCE_FORMAT
        || locator.source().schema_variant() != PI_SOURCE_SCHEMA_VARIANT
        || locator.source().provider_identity_version() != 1
        || locator.revision_policy() != LocatorRevisionPolicy::StableRecordEvidence
        || locator.certified_source_revision_digest().is_some()
    {
        return Err(PiSourceBackedError::InvalidPiLocator);
    }
    let SourceAnchor::ProviderNative { namespace, key } = locator.source().anchor() else {
        return Err(PiSourceBackedError::InvalidPiLocator);
    };
    let TypedKey::Utf8(source_session) = key else {
        return Err(PiSourceBackedError::InvalidPiLocator);
    };
    if namespace != PI_SOURCE_ANCHOR_NAMESPACE {
        return Err(PiSourceBackedError::InvalidPiLocator);
    }
    let NativeRecordCoordinate::Jsonl {
        byte_offset,
        byte_length,
        physical_ordinal,
        native_session_key,
        native_event_key,
    } = locator.coordinate()
    else {
        return Err(PiSourceBackedError::InvalidPiLocator);
    };
    if native_session_key.as_ref() != Some(&TypedKey::Utf8(source_session.clone()))
        || !matches!(
            native_event_key,
            Some(TypedKey::Utf8(value)) if !value.is_empty()
        ) && native_event_key.as_ref() != Some(&TypedKey::U64(*physical_ordinal))
    {
        return Err(PiSourceBackedError::InvalidPiLocator);
    }
    Ok((
        *byte_offset,
        *byte_length,
        *physical_ordinal,
        source_session.clone(),
    ))
}

fn hydration_failure(error: PiSourceBackedError) -> HydrationFailure {
    let kind = match error {
        PiSourceBackedError::LocatorDigestMismatch => HydrationFailureKind::StaleRecordEvidence,
        PiSourceBackedError::LocatorRangeMissing => HydrationFailureKind::MissingRecord,
        PiSourceBackedError::InvalidPiLocator
        | PiSourceBackedError::ResolverContract(_)
        | PiSourceBackedError::LocatorRangeTooLarge => HydrationFailureKind::InvalidLocator,
        PiSourceBackedError::Native(PiNativePathError::SourceChanged) => {
            HydrationFailureKind::StaleSourceEvidence
        }
        _ => HydrationFailureKind::TemporarilyUnavailable,
    };
    HydrationFailure {
        kind,
        detail: error.to_string(),
    }
}

fn pi_source_key(native_session_id: &str) -> PiSourceBackedResult<SourceKey> {
    let anchor = SourceAnchor::provider_native(
        PI_SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(native_session_id)?,
    )?;
    Ok(SourceKey::derive(
        CaptureProvider::Pi.as_str(),
        PI_SOURCE_FORMAT,
        PI_SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )?)
}

fn pi_session_identity(
    source: &SourceKey,
    native_session_id: &str,
) -> PiSourceBackedResult<StableEntityId> {
    let native_session_key = NativeSessionKey::native_id(
        PI_NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(native_session_id)?,
    )?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: PI_LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })?)
}

fn pi_session_identity_for_native(native_session_id: &str) -> PiSourceBackedResult<StableEntityId> {
    let source = pi_source_key(native_session_id)?;
    pi_session_identity(&source, native_session_id)
}

fn decode_checkpoint(previous: &CertifiedSource) -> PiSourceBackedResult<PiNativeCheckpoint> {
    if previous.parser_revision() != PI_PARSER_REVISION {
        return Err(PiSourceBackedError::InvalidCheckpoint);
    }
    let frontier = previous
        .frontier()
        .ok_or(PiSourceBackedError::MissingCheckpoint)?;
    if frontier.checkpoint_kind() != PI_FRONTIER_KIND {
        return Err(PiSourceBackedError::InvalidCheckpoint);
    }
    let TypedKey::Bytes(bytes) = frontier.checkpoint() else {
        return Err(PiSourceBackedError::InvalidCheckpoint);
    };
    let checkpoint: PiNativeCheckpoint =
        serde_json::from_slice(bytes).map_err(|_| PiSourceBackedError::InvalidCheckpoint)?;
    if checkpoint.complete_offset != frontier.certified_prefix_bytes()
        || checkpoint.committed_prefix_sha256 != *frontier.certified_prefix_digest()
        || checkpoint.complete_offset != previous.counts().certified_bytes
        || checkpoint.next_ordinal != previous.counts().complete_records
    {
        return Err(PiSourceBackedError::InvalidCheckpoint);
    }
    Ok(checkpoint)
}

fn lexical_body(row: &PiNativeEventRow) -> String {
    if row.lexical_text.trim().is_empty() {
        row.event_type.as_str().to_owned()
    } else {
        row.lexical_text.clone()
    }
}

fn collect_touch(touches: &mut HashMap<u64, Vec<String>>, row: PiNativeFileTouchRow) {
    if let Some(event_index) = row.provider_event_index {
        touches.entry(event_index).or_default().push(row.path);
    }
}

fn checked_add(left: u64, right: u64) -> PiSourceBackedResult<u64> {
    left.checked_add(right)
        .ok_or(PiSourceBackedError::CountOverflow)
}

fn json_record_bytes(bytes: &[u8]) -> &[u8] {
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    bytes.strip_suffix(b"\r").unwrap_or(bytes)
}

fn is_historical_omp_root(path: &Path) -> bool {
    let components = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value),
            _ => None,
        })
        .collect::<Vec<_>>();
    components.len() >= 3
        && components[components.len() - 3] == ".omp"
        && components[components.len() - 2] == "agent"
        && components[components.len() - 1] == "sessions"
}
