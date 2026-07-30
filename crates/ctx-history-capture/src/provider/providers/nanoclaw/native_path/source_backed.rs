use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

use ctx_history_core::{
    derive_event_id, derive_session_id, BatchHydrationRequest, BatchHydrationResult,
    CaptureProvider, EventIdentityInput, HydratedProviderRecord, HydrationFailure,
    HydrationFailureKind, LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate,
    NativeSessionKey, ProjectionContractError, ScannedSourceCounts, SessionIdentityInput,
    SourceAnchor, SourceKey, SourceObservation, SourceRecordLocator, SourceResolverContractError,
    TypedKey,
};
use ctx_history_index::LexicalDocument;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    complete_content::sqlite::{CompleteContentSqliteBoundError, CompleteContentSqliteQueryBudget},
    native_source::{NativeLocator, NativeSourceError},
    provider::{
        providers::nanoclaw::{
            complete_content::resolve_source_backed_exact,
            position::decode_nanoclaw_message_locator,
            project::NanoClawSourceBackedProject,
            projection::nanoclaw_core_event,
            rows::{nanoclaw_logical_record_digest_bytes, nanoclaw_message_digest_values},
            source::{NanoClawNativeScanner, NanoClawNativeUnit},
            NANOCLAW_MESSAGE_LOCATOR_KIND,
        },
        source_backed::{
            family::document::{
                ChangedDocumentSink, CompleteDocumentTree, DocumentLeafFingerprint,
                DocumentSourceTerminal, ObservedDocumentLeaf, ReplacementDocumentTree,
            },
            hydration_failure, SourceBackedRouteError, SourceBackedRouteErrorKind,
            SourceBackedRouteResult,
        },
        sqlite::sqlite_schema_fingerprint,
    },
    CaptureError, NANOCLAW_SOURCE_FORMAT,
};

const NANOCLAW_SOURCE_SCHEMA_VARIANT: &str = "nanoclaw-compound-project-v1";
const NANOCLAW_SOURCE_REVISION_KIND: &str = "nanoclaw-compound-project-snapshot-v1";
const NANOCLAW_SOURCE_BACKED_PARSER_REVISION: &str = "nanoclaw-source-backed-v1";
const NANOCLAW_LOGICAL_SESSION_KIND: &str = "nanoclaw-session";
const NANOCLAW_NATIVE_SESSION_NAMESPACE: &str = "nanoclaw.project-session";
const NANOCLAW_LOGICAL_EVENT_KIND: &str = "nanoclaw-message";
const NANOCLAW_NATIVE_EVENT_NAMESPACE: &str = "nanoclaw.project-message";

#[derive(Debug, Error)]
pub(crate) enum NanoClawSourceBackedError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    Resolver(#[from] SourceResolverContractError),
    #[error(transparent)]
    NativeSource(#[from] NativeSourceError),
    #[error("NanoClaw source-backed scan counters overflowed")]
    CountOverflow,
    #[error("NanoClaw source-backed scanner emitted inconsistent counts")]
    CountMismatch,
    #[error("NanoClaw source-backed locator does not name this compound project")]
    InvalidProjectMessageLocator,
    #[error("NanoClaw compound source evidence is stale")]
    StaleCompoundSourceEvidence,
    #[error("NanoClaw project message no longer exists")]
    MissingProjectMessage,
    #[error("NanoClaw project message digest no longer matches the certified locator")]
    StaleProjectMessageEvidence,
    #[error("NanoClaw exact project-message query exceeded its bound")]
    ExactQueryBoundExceeded,
}

pub(crate) type NanoClawSourceBackedResult<T> = Result<T, NanoClawSourceBackedError>;

#[derive(Debug, Clone)]
pub(crate) struct NanoClawDocumentLeaf {
    source: SourceKey,
}

pub(crate) struct NanoClawDocumentTreeAuthority {
    project: Mutex<NanoClawSourceBackedProject>,
}

type NanoClawDocumentTree =
    CompleteDocumentTree<NanoClawDocumentLeaf, NanoClawDocumentTreeAuthority>;

#[derive(Debug, Clone)]
pub(crate) struct NanoClawDocumentTreeAdapter {
    path: PathBuf,
    source: SourceKey,
}

impl NanoClawDocumentTreeAdapter {
    pub(crate) fn new(
        path: PathBuf,
        catalog_lineage: [u8; 32],
    ) -> NanoClawSourceBackedResult<Self> {
        Ok(Self {
            path,
            source: nanoclaw_source_key(catalog_lineage)?,
        })
    }
}

#[cfg(test)]
std::thread_local! {
    static BEFORE_SOURCE_BACKED_FINISH: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) struct NanoClawSourceBackedFinishHook;

#[cfg(test)]
impl Drop for NanoClawSourceBackedFinishHook {
    fn drop(&mut self) {
        BEFORE_SOURCE_BACKED_FINISH.with(|installed| {
            installed.borrow_mut().take();
        });
    }
}

#[cfg(test)]
pub(crate) fn set_before_source_backed_finish_hook(
    hook: impl FnOnce() + 'static,
) -> NanoClawSourceBackedFinishHook {
    BEFORE_SOURCE_BACKED_FINISH.with(|installed| {
        *installed.borrow_mut() = Some(Box::new(hook));
    });
    NanoClawSourceBackedFinishHook
}

#[cfg(test)]
fn run_before_source_backed_finish_hook() {
    BEFORE_SOURCE_BACKED_FINISH.with(|installed| {
        if let Some(hook) = installed.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_source_backed_finish_hook() {}

impl ReplacementDocumentTree for NanoClawDocumentTreeAdapter {
    type Leaf = NanoClawDocumentLeaf;
    type TreeAuthority = NanoClawDocumentTreeAuthority;

    fn parser_revision(&self) -> &'static str {
        NANOCLAW_SOURCE_BACKED_PARSER_REVISION
    }

    fn owns_source(&self, source: &SourceKey) -> bool {
        self.source.exact_descriptor_eq(source)
    }

    fn discover_complete(&self) -> SourceBackedRouteResult<NanoClawDocumentTree> {
        let project =
            NanoClawSourceBackedProject::open(&self.path).map_err(nanoclaw_route_capture_error)?;
        let physical_fingerprint = project.physical_fingerprint();
        let tree_fingerprint = nanoclaw_tree_fingerprint(physical_fingerprint, &self.source);
        Ok(CompleteDocumentTree::new(
            tree_fingerprint,
            vec![ObservedDocumentLeaf::with_durable_replay(
                DocumentLeafFingerprint::new(physical_fingerprint),
                NanoClawDocumentLeaf {
                    source: self.source.clone(),
                },
                false,
            )],
            NanoClawDocumentTreeAuthority {
                project: Mutex::new(project),
            },
        ))
    }

    fn scan_changed(
        &self,
        authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
        sink: &mut ChangedDocumentSink<'_, '_>,
    ) -> SourceBackedRouteResult<DocumentSourceTerminal> {
        if !leaf.source.exact_descriptor_eq(&self.source) {
            return Err(nanoclaw_changed(
                "NanoClaw document leaf changed catalog lineage",
            ));
        }
        let mut project = authority
            .project
            .lock()
            .map_err(|_| nanoclaw_internal("NanoClaw document authority lock was poisoned"))?;
        scan_nanoclaw_project(&mut project, &leaf.source, sink)
    }

    fn revalidate_complete(
        &self,
        tree: &NanoClawDocumentTree,
    ) -> SourceBackedRouteResult<[u8; 32]> {
        let project = tree
            .authority
            .project
            .lock()
            .map_err(|_| nanoclaw_internal("NanoClaw document authority lock was poisoned"))?;
        let snapshot = project.snapshot();
        if !snapshot
            .revalidate()
            .map_err(nanoclaw_route_capture_error)?
        {
            return Err(nanoclaw_changed(
                "NanoClaw compound project changed before publication",
            ));
        }
        Ok(nanoclaw_tree_fingerprint(
            snapshot.physical_fingerprint(),
            &self.source,
        ))
    }

    fn hydrate_group(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        let records = hydrate_nanoclaw_group(&self.path, &self.source, request)
            .map_err(nanoclaw_hydration_failure)?;
        BatchHydrationResult::new(records)
            .map_err(|error| hydration_failure(HydrationFailureKind::InvalidLocator, error))
    }
}

fn scan_nanoclaw_project(
    project: &mut NanoClawSourceBackedProject,
    source: &SourceKey,
    sink: &mut ChangedDocumentSink<'_, '_>,
) -> SourceBackedRouteResult<DocumentSourceTerminal> {
    let central = project.connection().map_err(nanoclaw_route_capture_error)?;
    let user_version = central
        .query_row("pragma user_version", [], |row| row.get(0))
        .map_err(CaptureError::from)
        .map_err(nanoclaw_route_capture_error)?;
    let schema_fingerprint =
        sqlite_schema_fingerprint(central).map_err(nanoclaw_route_capture_error)?;
    let revision = project
        .snapshot()
        .source_backed_revision_evidence(user_version, &schema_fingerprint)
        .map_err(nanoclaw_route_capture_error)?;
    let opening = SourceObservation::new(
        source.clone(),
        NANOCLAW_SOURCE_REVISION_KIND,
        revision.clone(),
    )
    .map_err(nanoclaw_route_contract_error)?;
    let source_revision_digest = Sha256::digest(&revision).into();
    let source_path = project.root_path().display().to_string();

    let mut scanner = NanoClawNativeScanner::new(central, project.snapshot())
        .map_err(nanoclaw_route_capture_error)?;
    sink.begin_source(source.clone())?;
    let mut complete_records = 0_u64;
    let mut retained_records = 0_u64;
    let mut rejected_records = 0_u64;
    let mut ignored_records = 0_u64;
    let mut indexed_documents = 0_u64;
    loop {
        let page = scanner.next_page().map_err(nanoclaw_route_capture_error)?;
        let terminal = page.terminal;
        for unit in page.units {
            complete_records = checked_add(complete_records, 1).map_err(nanoclaw_route_error)?;
            match unit {
                NanoClawNativeUnit::Session { .. } => {
                    ignored_records =
                        checked_add(ignored_records, 1).map_err(nanoclaw_route_error)?;
                }
                NanoClawNativeUnit::Message {
                    ordinal,
                    source: message_source,
                    session,
                    message,
                    locator,
                    ..
                } => {
                    let document = nanoclaw_lexical_document(
                        &source,
                        source_revision_digest,
                        ordinal,
                        message_source.label(),
                        &source_path,
                        &session,
                        &message,
                        locator,
                    )
                    .map_err(nanoclaw_route_error)?;
                    sink.emit_document(document)?;
                    retained_records =
                        checked_add(retained_records, 1).map_err(nanoclaw_route_error)?;
                    indexed_documents =
                        checked_add(indexed_documents, 1).map_err(nanoclaw_route_error)?;
                }
                NanoClawNativeUnit::Rejection { .. } => {
                    rejected_records =
                        checked_add(rejected_records, 1).map_err(nanoclaw_route_error)?;
                }
            }
        }
        if terminal {
            break;
        }
    }

    let prefix_digest = scanner.prefix_digest_bytes();
    let certified_bytes = scanner.prefix_bytes();
    scanner.finish().map_err(nanoclaw_route_capture_error)?;
    run_before_source_backed_finish_hook();
    project.finish().map_err(nanoclaw_route_capture_error)?;
    let classified = retained_records
        .checked_add(rejected_records)
        .and_then(|value| value.checked_add(ignored_records))
        .ok_or_else(|| nanoclaw_route_error(NanoClawSourceBackedError::CountOverflow))?;
    if classified != complete_records || indexed_documents != retained_records {
        return Err(nanoclaw_route_error(
            NanoClawSourceBackedError::CountMismatch,
        ));
    }
    let closing = SourceObservation::new(
        source.clone(),
        NANOCLAW_SOURCE_REVISION_KIND,
        project
            .snapshot()
            .source_backed_revision_evidence(user_version, &schema_fingerprint)
            .map_err(nanoclaw_route_capture_error)?,
    )
    .map_err(nanoclaw_route_contract_error)?;
    Ok(DocumentSourceTerminal {
        source: source.clone(),
        opening,
        closing,
        parser_revision: NANOCLAW_SOURCE_BACKED_PARSER_REVISION,
        content_digest: prefix_digest,
        counts: ScannedSourceCounts {
            complete_records,
            retained_records,
            rejected_records,
            ignored_records,
            indexed_documents,
            certified_bytes,
        },
    })
}

fn hydrate_nanoclaw_group(
    path: &Path,
    source: &SourceKey,
    request: &BatchHydrationRequest,
) -> NanoClawSourceBackedResult<Vec<HydratedProviderRecord>> {
    if request.events().iter().any(|event| {
        event.locator().validate_contract().is_err()
            || !source.exact_descriptor_eq(event.locator().source())
    }) {
        return Err(NanoClawSourceBackedError::InvalidProjectMessageLocator);
    }
    let mut project = NanoClawSourceBackedProject::open(path)?;
    let central = project.connection()?;
    let user_version = central
        .query_row("pragma user_version", [], |row| row.get(0))
        .map_err(CaptureError::from)?;
    let schema_fingerprint = sqlite_schema_fingerprint(central)?;
    let revision = project
        .snapshot()
        .source_backed_revision_evidence(user_version, &schema_fingerprint)?;
    let current_revision_digest: [u8; 32] = Sha256::digest(&revision).into();
    let mut records = Vec::with_capacity(request.events().len());
    for event in request.events() {
        let locator = event.locator();
        let native_locator = project_message_locator(source, locator)?;
        if locator.certified_source_revision_digest() != Some(&current_revision_digest) {
            return Err(NanoClawSourceBackedError::StaleCompoundSourceEvidence);
        }
        let record = resolve_source_backed_exact(
            central,
            project.snapshot(),
            &native_locator,
            CompleteContentSqliteQueryBudget::new(),
        )
        .map_err(map_exact_route_error)?
        .ok_or(NanoClawSourceBackedError::MissingProjectMessage)?;
        if nanoclaw_logical_record_digest_bytes(&record.values) != *locator.record_digest() {
            return Err(NanoClawSourceBackedError::StaleProjectMessageEvidence);
        }
        records.push(HydratedProviderRecord {
            event_id: event.event_id(),
            provider_bytes: record.text.into_bytes(),
        });
    }
    project.finish()?;
    Ok(records)
}

fn nanoclaw_tree_fingerprint(physical: [u8; 32], source: &SourceKey) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"ctx-nanoclaw-document-tree-v1\0");
    digest.update(physical);
    digest.update(source.identity().digest());
    digest.finalize().into()
}

fn nanoclaw_route_error(error: NanoClawSourceBackedError) -> SourceBackedRouteError {
    let kind = match &error {
        NanoClawSourceBackedError::Capture(CaptureError::SourceChangedDuringCapture) => {
            SourceBackedRouteErrorKind::SourceChanged
        }
        NanoClawSourceBackedError::Capture(CaptureError::Io(error))
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            SourceBackedRouteErrorKind::Unavailable
        }
        _ => SourceBackedRouteErrorKind::InvalidSource,
    };
    SourceBackedRouteError::new(kind, error.to_string())
}

fn nanoclaw_route_capture_error(error: CaptureError) -> SourceBackedRouteError {
    nanoclaw_route_error(error.into())
}

fn nanoclaw_route_contract_error(error: impl std::fmt::Display) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::InvalidSource, error.to_string())
}

fn nanoclaw_changed(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::SourceChanged, detail)
}

fn nanoclaw_internal(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, detail)
}

fn nanoclaw_hydration_failure(error: NanoClawSourceBackedError) -> HydrationFailure {
    let kind = match &error {
        NanoClawSourceBackedError::InvalidProjectMessageLocator
        | NanoClawSourceBackedError::Resolver(_)
        | NanoClawSourceBackedError::NativeSource(_)
        | NanoClawSourceBackedError::ExactQueryBoundExceeded => {
            HydrationFailureKind::InvalidLocator
        }
        NanoClawSourceBackedError::StaleCompoundSourceEvidence
        | NanoClawSourceBackedError::Capture(CaptureError::SourceChangedDuringCapture)
        | NanoClawSourceBackedError::Capture(CaptureError::InvalidProviderTranscriptPath {
            ..
        }) => HydrationFailureKind::StaleSourceEvidence,
        NanoClawSourceBackedError::MissingProjectMessage => HydrationFailureKind::MissingRecord,
        NanoClawSourceBackedError::StaleProjectMessageEvidence => {
            HydrationFailureKind::StaleRecordEvidence
        }
        NanoClawSourceBackedError::Capture(CaptureError::Io(_))
        | NanoClawSourceBackedError::Capture(CaptureError::ProviderSource { .. }) => {
            HydrationFailureKind::TemporarilyUnavailable
        }
        NanoClawSourceBackedError::Projection(_)
        | NanoClawSourceBackedError::CountOverflow
        | NanoClawSourceBackedError::CountMismatch
        | NanoClawSourceBackedError::Capture(_) => HydrationFailureKind::StaleSourceEvidence,
    };
    HydrationFailure {
        kind,
        detail: error.to_string(),
    }
}

pub(crate) fn nanoclaw_source_key(
    catalog_lineage: [u8; 32],
) -> NanoClawSourceBackedResult<SourceKey> {
    Ok(SourceKey::derive(
        CaptureProvider::NanoClaw.as_str(),
        NANOCLAW_SOURCE_FORMAT,
        NANOCLAW_SOURCE_SCHEMA_VARIANT,
        1,
        SourceAnchor::CatalogLineage(catalog_lineage),
    )?)
}

#[allow(clippy::too_many_arguments)]
fn nanoclaw_lexical_document(
    source: &SourceKey,
    source_revision_digest: [u8; 32],
    ordinal: u64,
    message_source: &str,
    source_path: &str,
    session: &super::super::rows::NanoClawSessionRow,
    message: &super::super::rows::NanoClawMessageRow,
    native_locator: NativeLocator,
) -> NanoClawSourceBackedResult<LexicalDocument> {
    let native_session_key = NativeSessionKey::composite(
        NANOCLAW_NATIVE_SESSION_NAMESPACE,
        vec![
            TypedKey::utf8(&session.agent_group_id)?,
            TypedKey::utf8(&session.id)?,
        ],
    )?;
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: NANOCLAW_LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })?;
    let native_item_key = NativeItemKey::composite(
        NANOCLAW_NATIVE_EVENT_NAMESPACE,
        vec![
            TypedKey::utf8(message_source)?,
            TypedKey::utf8(&message.id)?,
        ],
    )?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: NANOCLAW_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let seq = message
        .seq
        .map(|value| {
            u64::try_from(value).map_err(|_| {
                NanoClawSourceBackedError::Capture(CaptureError::InvalidPayload(
                    "NanoClaw source-backed message seq must be nonnegative".to_owned(),
                ))
            })
        })
        .transpose()?;
    let (event, exact_text) =
        nanoclaw_core_event(session, message, seq, chrono::DateTime::UNIX_EPOCH);
    let mut body = exact_text;
    if body.is_empty() {
        body = format!("NanoClaw {message_source} message");
    }
    let record_digest =
        nanoclaw_logical_record_digest_bytes(&nanoclaw_message_digest_values(message));
    let locator = SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::ProviderNative {
            namespace: NANOCLAW_MESSAGE_LOCATOR_KIND.to_owned(),
            coordinate: TypedKey::bytes(native_locator.value().to_vec())?,
        },
        LocatorRevisionPolicy::ExactSourceRevision,
        Some(source_revision_digest),
        record_digest,
    )?;
    let provider_session_id = session
        .thread_id
        .clone()
        .filter(|thread| !thread.is_empty())
        .unwrap_or_else(|| session.id.clone());
    let agent_type = session
        .agent_provider
        .clone()
        .filter(|agent| !agent.is_empty())
        .unwrap_or_else(|| CaptureProvider::NanoClaw.as_str().to_owned());
    Ok(LexicalDocument {
        event_id,
        session_id,
        parent_session_id: None,
        root_session_id: session_id,
        source: source.clone(),
        locator,
        provider_session_id: Some(provider_session_id),
        branch: None,
        source_path: Some(source_path.to_owned()),
        agent_type,
        is_primary: true,
        event_sequence: ordinal,
        occurred_at_unix_ms: Some(event.occurred_at.timestamp_millis()),
        event_type: event.event_type.as_str().to_owned(),
        role: event.role.map(|role| role.as_str().to_owned()),
        body,
        workspace: session.agent_group_folder.clone(),
        cwd: session.agent_group_folder.clone(),
        touched_files: Vec::new(),
    })
}

fn project_message_locator(
    source: &SourceKey,
    locator: &SourceRecordLocator,
) -> NanoClawSourceBackedResult<NativeLocator> {
    locator.validate_contract()?;
    if !source.exact_descriptor_eq(locator.source())
        || locator.revision_policy() != LocatorRevisionPolicy::ExactSourceRevision
    {
        return Err(NanoClawSourceBackedError::InvalidProjectMessageLocator);
    }
    let NativeRecordCoordinate::ProviderNative {
        namespace,
        coordinate,
    } = locator.coordinate()
    else {
        return Err(NanoClawSourceBackedError::InvalidProjectMessageLocator);
    };
    let TypedKey::Bytes(value) = coordinate else {
        return Err(NanoClawSourceBackedError::InvalidProjectMessageLocator);
    };
    if namespace != NANOCLAW_MESSAGE_LOCATOR_KIND {
        return Err(NanoClawSourceBackedError::InvalidProjectMessageLocator);
    }
    let native_locator = NativeLocator::new(namespace.clone(), value.clone())?;
    decode_nanoclaw_message_locator(&native_locator)
        .map_err(|_| NanoClawSourceBackedError::InvalidProjectMessageLocator)?;
    Ok(native_locator)
}

fn checked_add(value: u64, increment: u64) -> NanoClawSourceBackedResult<u64> {
    value
        .checked_add(increment)
        .ok_or(NanoClawSourceBackedError::CountOverflow)
}

fn map_exact_route_error(error: CompleteContentSqliteBoundError) -> NanoClawSourceBackedError {
    match error {
        CompleteContentSqliteBoundError::Capture(error) => error.into(),
        CompleteContentSqliteBoundError::ContentTooLarge => {
            NanoClawSourceBackedError::ExactQueryBoundExceeded
        }
    }
}
