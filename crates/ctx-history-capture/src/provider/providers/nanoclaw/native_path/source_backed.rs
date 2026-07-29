use std::path::Path;

use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CertifiedSource, EventIdentityInput,
    LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate, NativeSessionKey,
    ProjectionContractError, ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceKey,
    SourceObservation, SourceRecordLocator, SourceResolverContractError, TypedKey,
};
use ctx_history_index::LexicalDocument;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    complete_content::sqlite::{CompleteContentSqliteBoundError, CompleteContentSqliteQueryBudget},
    native_source::{NativeLocator, NativeSourceError},
    provider::{
        providers::nanoclaw::{
            complete_content::{resolve_source_backed_exact, NanoClawCompleteRecord},
            position::decode_nanoclaw_message_locator,
            project::NanoClawSourceBackedProject,
            projection::nanoclaw_core_event,
            rows::{nanoclaw_logical_record_digest_bytes, nanoclaw_message_digest_values},
            source::{NanoClawNativeScanner, NanoClawNativeUnit},
            NANOCLAW_MESSAGE_LOCATOR_KIND,
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

#[derive(Debug)]
pub(crate) struct NanoClawSourceBackedPage {
    pub(crate) documents: Vec<LexicalDocument>,
}

#[derive(Debug)]
pub(crate) struct NanoClawSourceBackedReceipt {
    pub(crate) source: CertifiedSource,
    // Emitted-page accounting remains part of the release scan receipt.
    #[allow(dead_code)]
    pub(crate) emitted_pages: u64,
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

/// Scans exactly one caller-selected NanoClaw project.
///
/// `catalog_lineage` is persisted shared-catalog authority. It deliberately
/// keeps canonical identity independent from the current project path.
pub(crate) fn scan_nanoclaw_source_backed<F>(
    path: &Path,
    catalog_lineage: [u8; 32],
    mut emit: F,
) -> NanoClawSourceBackedResult<NanoClawSourceBackedReceipt>
where
    F: FnMut(NanoClawSourceBackedPage) -> NanoClawSourceBackedResult<()>,
{
    let project = NanoClawSourceBackedProject::open(path)?;
    let central = project.connection()?;
    let user_version = central
        .query_row("pragma user_version", [], |row| row.get(0))
        .map_err(CaptureError::from)?;
    let schema_fingerprint = sqlite_schema_fingerprint(central)?;
    let revision = project
        .snapshot()
        .source_backed_revision_evidence(user_version, &schema_fingerprint)?;
    let source = nanoclaw_source_key(catalog_lineage)?;
    let opening = SourceObservation::new(
        source.clone(),
        NANOCLAW_SOURCE_REVISION_KIND,
        revision.clone(),
    )?;
    let source_revision_digest = Sha256::digest(&revision).into();
    let source_path = project.root_path().display().to_string();

    let mut scanner = NanoClawNativeScanner::new(central, project.snapshot())?;
    let mut complete_records = 0_u64;
    let mut retained_records = 0_u64;
    let mut rejected_records = 0_u64;
    let mut ignored_records = 0_u64;
    let mut indexed_documents = 0_u64;
    let mut pending_pages = Vec::new();
    loop {
        let page = scanner.next_page()?;
        let terminal = page.terminal;
        let mut documents = Vec::with_capacity(page.units.len());
        for unit in page.units {
            complete_records = checked_add(complete_records, 1)?;
            match unit {
                NanoClawNativeUnit::Session { .. } => {
                    ignored_records = checked_add(ignored_records, 1)?;
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
                    )?;
                    documents.push(document);
                    retained_records = checked_add(retained_records, 1)?;
                    indexed_documents = checked_add(indexed_documents, 1)?;
                }
                NanoClawNativeUnit::Rejection { .. } => {
                    rejected_records = checked_add(rejected_records, 1)?;
                }
            }
        }
        if !documents.is_empty() {
            pending_pages.push(NanoClawSourceBackedPage { documents });
        }
        if terminal {
            break;
        }
    }

    let prefix_digest = scanner.prefix_digest_bytes();
    let certified_bytes = scanner.prefix_bytes();
    scanner.finish()?;
    run_before_source_backed_finish_hook();
    let snapshot = project.finish()?;
    let classified = retained_records
        .checked_add(rejected_records)
        .and_then(|value| value.checked_add(ignored_records))
        .ok_or(NanoClawSourceBackedError::CountOverflow)?;
    if classified != complete_records || indexed_documents != retained_records {
        return Err(NanoClawSourceBackedError::CountMismatch);
    }
    let closing = SourceObservation::new(
        source,
        NANOCLAW_SOURCE_REVISION_KIND,
        snapshot.source_backed_revision_evidence(user_version, &schema_fingerprint)?,
    )?;
    let certificate = CertifiedSource::certify(
        opening,
        closing,
        NANOCLAW_SOURCE_BACKED_PARSER_REVISION,
        prefix_digest,
        ScannedSourceCounts {
            complete_records,
            retained_records,
            rejected_records,
            ignored_records,
            indexed_documents,
            certified_bytes,
        },
    )?;
    let mut emitted_pages = 0_u64;
    for page in pending_pages {
        emit(page)?;
        emitted_pages = checked_add(emitted_pages, 1)?;
    }
    Ok(NanoClawSourceBackedReceipt {
        source: certificate,
        emitted_pages,
    })
}

/// Resolves one source-backed project-message locator through NanoClaw's
/// existing exact compound-project route.
pub(crate) fn hydrate_nanoclaw_source_backed_exact(
    path: &Path,
    catalog_lineage: [u8; 32],
    locator: &SourceRecordLocator,
) -> NanoClawSourceBackedResult<NanoClawCompleteRecord> {
    let source = nanoclaw_source_key(catalog_lineage)?;
    let native_locator = project_message_locator(&source, locator)?;
    let project = NanoClawSourceBackedProject::open(path)?;
    let central = project.connection()?;
    let user_version = central
        .query_row("pragma user_version", [], |row| row.get(0))
        .map_err(CaptureError::from)?;
    let schema_fingerprint = sqlite_schema_fingerprint(central)?;
    let revision = project
        .snapshot()
        .source_backed_revision_evidence(user_version, &schema_fingerprint)?;
    let current_revision_digest: [u8; 32] = Sha256::digest(&revision).into();
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
    project.finish()?;
    Ok(record)
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
