//! Provider-local source-backed ForgeCode projection.
//!
//! Discovery and parsing remain ForgeCode-owned. Publication, replacement,
//! deletion, and projection frontiers remain shared concerns: this module
//! emits bounded complete Core pages from one certified SQLite snapshot
//! without retaining publication state.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, CertifiedSource, CoreRecord,
    CoreRecordError, EventIdentityInput, NativeItemKey, NativeSessionKey, PositionStability,
    ProjectionContractError, ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceKey,
    StableEntityId, TypedKey,
};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    provider::source_backed::{
        family::document::{
            ChangedDocumentSink, CompleteDocumentTree, DocumentLeafFingerprint,
            DocumentSourceTerminal, ObservedDocumentLeaf, ReplacementDocumentTree,
        },
        route_error, SourceBackedRouteError, SourceBackedRouteErrorKind, SourceBackedRouteResult,
    },
    provider_sources::{SqliteLogicalSnapshot, SqliteSourceAccessError},
    CaptureError, ProviderAdapterContext, FORGECODE_SQLITE_SOURCE_FORMAT,
};

use super::source::{
    discover_forgecode_source, ForgeCodeConversationRow, ForgeCodeDiscovery, ForgeCodeFrontier,
    ForgeCodePage, ForgeCodeScanner, ForgeCodeSourceObservation, FORGECODE_NATIVE_PAGE_MAX_BYTES,
    FORGECODE_NATIVE_PARSER_REVISION, FORGECODE_NATIVE_POLICY_REVISION,
};

const FORGECODE_PROVIDER_ID: &str = "forgecode";
const FORGECODE_SOURCE_SCHEMA_VARIANT: &str = "conversations-messages-v1";
const FORGECODE_SELECTED_SOURCE_NAMESPACE: &str = "forgecode-selected-database-v1";
const FORGECODE_SELECTED_SOURCE_KEY: &str = "selected";
const FORGECODE_LOGICAL_SESSION_KIND: &str = "forgecode-conversation";
const FORGECODE_NATIVE_SESSION_NAMESPACE: &str = "forgecode-conversation-id-v1";
const FORGECODE_LOGICAL_EVENT_KIND: &str = "forgecode-message";
const FORGECODE_NATIVE_EVENT_POSITION_KIND: &str = "forgecode-message-index-v1";
const FORGECODE_RECORD_DIGEST_DOMAIN: &[u8] = b"ctx.forgecode.source-backed-scan-v0\0";
const FORGECODE_SOURCE_BACKED_PARSER_REVISION: &str =
    "forgecode-nativepath-source-backed-v0:parser=1;policy=6";

#[derive(Debug, Error)]
pub(crate) enum ForgeCodeSourceBackedErrorV0 {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    CoreRecord(#[from] CoreRecordError),
    #[error("ForgeCode source-backed scan was finished before a terminal page")]
    IncompleteScan,
    #[error("ForgeCode source-backed scan lost its conversation row")]
    MissingConversationRow,
    #[error("ForgeCode source-backed scan counters overflowed")]
    CountOverflow,
}

pub(crate) type ForgeCodeSourceBackedResultV0<T> = Result<T, ForgeCodeSourceBackedErrorV0>;

/// Selection authority is separate from physical location.
///
/// Automatic discovery owns one logical selected slot. A nonselected database
/// is importable only with catalog lineage assigned by the explicit/manual
/// caller, so its path never becomes public identity.
#[derive(Debug, Clone)]
pub(crate) struct ForgeCodeSourceSelectionV0 {
    path: PathBuf,
    data_root: PathBuf,
    authority: ForgeCodeSourceAuthorityV0,
}

#[derive(Debug, Clone, Copy)]
enum ForgeCodeSourceAuthorityV0 {
    Selected,
    ExplicitCatalogLineage([u8; 32]),
}

impl ForgeCodeSourceSelectionV0 {
    pub(crate) fn selected(data_root: &Path, path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            data_root: data_root.to_path_buf(),
            authority: ForgeCodeSourceAuthorityV0::Selected,
        }
    }

    pub(crate) fn explicit(
        data_root: &Path,
        path: impl Into<PathBuf>,
        catalog_lineage: [u8; 32],
    ) -> Self {
        Self {
            path: path.into(),
            data_root: data_root.to_path_buf(),
            authority: ForgeCodeSourceAuthorityV0::ExplicitCatalogLineage(catalog_lineage),
        }
    }

    fn source_key(&self) -> ForgeCodeSourceBackedResultV0<SourceKey> {
        let anchor = match self.authority {
            ForgeCodeSourceAuthorityV0::Selected => SourceAnchor::provider_native(
                FORGECODE_SELECTED_SOURCE_NAMESPACE,
                TypedKey::utf8(FORGECODE_SELECTED_SOURCE_KEY)?,
            )?,
            ForgeCodeSourceAuthorityV0::ExplicitCatalogLineage(lineage) => {
                SourceAnchor::CatalogLineage(lineage)
            }
        };
        Ok(SourceKey::derive(
            FORGECODE_PROVIDER_ID,
            FORGECODE_SQLITE_SOURCE_FORMAT,
            FORGECODE_SOURCE_SCHEMA_VARIANT,
            1,
            anchor,
        )?)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ForgeCodeSourceBackedSourceV0 {
    source: SourceKey,
    canonical_path: PathBuf,
}

pub(crate) struct ForgeCodeSourceBackedPageV0 {
    pub(crate) documents: Vec<CoreRecord>,
}

fn project_source_backed_page(
    source: &ForgeCodeSourceBackedSourceV0,
    content_digest: &mut Sha256,
    counts: &mut ScannedSourceCounts,
    last_observed_rowid: &mut Option<i64>,
    terminal_scan: &mut bool,
    page: ForgeCodePage,
) -> ForgeCodeSourceBackedResultV0<ForgeCodeSourceBackedPageV0> {
    observe_source_record(content_digest, counts, last_observed_rowid, &page)?;
    let terminal = page.terminal;
    let retained_bytes = page.retained_bytes;
    if retained_bytes > FORGECODE_NATIVE_PAGE_MAX_BYTES {
        return Err(CaptureError::InvalidPayload(
            "ForgeCode source-backed page exceeds its retained byte bound".to_owned(),
        )
        .into());
    }
    let ignored_records = ignored_output_records(&page)?;
    let direct_touches = direct_touches(&page);
    let row = page.row.as_ref();
    let mut documents = Vec::with_capacity(page.events.len());
    let provider_rejections = u64::try_from(page.rejections.len())
        .map_err(|_| ForgeCodeSourceBackedErrorV0::CountOverflow)?;
    let mut projection_rejections = 0_u64;
    for retained in page.events {
        let Some(row) = row else {
            return Err(ForgeCodeSourceBackedErrorV0::MissingConversationRow);
        };
        match core_record(source, row, retained, &direct_touches) {
            Ok(document) => documents.push(document),
            Err(_) => {
                projection_rejections = checked_add(projection_rejections, 1)?;
            }
        }
    }
    let retained_records =
        u64::try_from(documents.len()).map_err(|_| ForgeCodeSourceBackedErrorV0::CountOverflow)?;
    let rejected_records = checked_add(provider_rejections, projection_rejections)?;
    let complete_records = checked_add(
        checked_add(retained_records, rejected_records)?,
        ignored_records,
    )?;
    counts.complete_records = checked_add(counts.complete_records, complete_records)?;
    counts.retained_records = checked_add(counts.retained_records, retained_records)?;
    counts.rejected_records = checked_add(counts.rejected_records, rejected_records)?;
    counts.ignored_records = checked_add(counts.ignored_records, ignored_records)?;
    counts.indexed_documents = checked_add(counts.indexed_documents, retained_records)?;
    *terminal_scan = terminal;
    Ok(ForgeCodeSourceBackedPageV0 { documents })
}

fn observe_source_record(
    content_digest: &mut Sha256,
    counts: &mut ScannedSourceCounts,
    last_observed_rowid: &mut Option<i64>,
    page: &ForgeCodePage,
) -> ForgeCodeSourceBackedResultV0<()> {
    if let Some(row) = page.row.as_ref() {
        if *last_observed_rowid != Some(row.rowid) {
            content_digest.update([1]);
            content_digest.update(row.source_record_digest);
            counts.certified_bytes =
                checked_add(counts.certified_bytes, row.canonical_record_bytes)?;
            *last_observed_rowid = Some(row.rowid);
        }
        return Ok(());
    }
    let Some(rowid) = page.next_frontier.rowid else {
        return Ok(());
    };
    if *last_observed_rowid == Some(rowid) {
        return Ok(());
    }
    content_digest.update([2]);
    let mut certified_bytes = 1_u64;
    for failure in &page.rejections {
        let error_bytes = failure.error.as_bytes();
        content_digest.update((error_bytes.len() as u64).to_be_bytes());
        content_digest.update(error_bytes);
        certified_bytes = checked_add(
            certified_bytes,
            u64::try_from(error_bytes.len())
                .map_err(|_| ForgeCodeSourceBackedErrorV0::CountOverflow)?,
        )?;
    }
    counts.certified_bytes = checked_add(counts.certified_bytes, certified_bytes)?;
    *last_observed_rowid = Some(rowid);
    Ok(())
}

fn core_record(
    source: &ForgeCodeSourceBackedSourceV0,
    row: &ForgeCodeConversationRow,
    retained: super::source::ForgeCodeRetainedEvent,
    direct_touches: &BTreeMap<u64, Vec<String>>,
) -> ForgeCodeSourceBackedResultV0<CoreRecord> {
    let session_id = forgecode_session_id(&source.source, &row.conversation_id)?;
    let subrecord_index = retained
        .provider_event_index
        .checked_sub(1)
        .ok_or(ProjectionContractError::InvalidDerivedIdentity)?;
    let event_id = forgecode_event_id(&source.source, session_id, subrecord_index)?;
    let primary_key = TypedKey::composite(vec![
        TypedKey::utf8(&row.conversation_id)?,
        TypedKey::U64(subrecord_index),
    ])?;
    let lexical_text = retained
        .event
        .payload
        .get("text")
        .and_then(serde_json::Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| retained.event.event_type.as_str().replace('_', " "));
    let native_file_touches = direct_touches
        .get(&retained.provider_event_index)
        .filter(|touches| !touches.is_empty())
        .map(|touches| serde_json::json!(touches));
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        session_id,
        source.source.clone(),
        subrecord_index,
        retained.event.event_type.as_str(),
        AgentType::Primary.as_str(),
        true,
        FORGECODE_SOURCE_BACKED_PARSER_REVISION,
        lexical_text,
    )?;
    record.provider_session_id = Some(row.conversation_id.clone());
    record.native_event_id = Some(primary_key);
    record.occurred_at_unix_ms = Some(retained.event.occurred_at.timestamp_millis());
    record.role = retained.event.role.map(|role| role.as_str().to_owned());
    record.branch = forgecode_branch(row);
    record.workspace = Some(row.workspace_id.to_string());
    if let Some(native_file_touches) = native_file_touches {
        record.metadata.insert(
            "provider_native_file_touches".to_owned(),
            native_file_touches,
        );
    }
    record.validate_contract()?;
    Ok(record)
}

fn forgecode_branch(row: &ForgeCodeConversationRow) -> Option<String> {
    [
        "/branch",
        "/git_branch",
        "/gitBranch",
        "/repository/branch",
        "/workspace/branch",
    ]
    .into_iter()
    .find_map(|pointer| {
        row.context_metadata
            .pointer(pointer)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|branch| !branch.is_empty())
            .map(str::to_owned)
    })
}

fn direct_touches(page: &ForgeCodePage) -> BTreeMap<u64, Vec<String>> {
    let mut touches = BTreeMap::<u64, Vec<String>>::new();
    for touch in &page.touches {
        if let Some(event_index) = touch.provider_event_index {
            touches
                .entry(event_index)
                .or_default()
                .push(touch.path.clone());
        }
    }
    touches
}

fn ignored_output_records(page: &ForgeCodePage) -> ForgeCodeSourceBackedResultV0<u64> {
    let retained = page
        .events
        .iter()
        .filter_map(|event| event.provider_event_index.checked_sub(1))
        .filter_map(|index| u32::try_from(index).ok())
        .collect::<BTreeSet<_>>();
    page.outputs.iter().try_fold(0_u64, |count, output| {
        let retained_in_core = output
            .coordinate
            .source_record_subrecord_index
            .is_some_and(|index| retained.contains(&index));
        if retained_in_core {
            Ok(count)
        } else {
            checked_add(count, 1)
        }
    })
}

fn forgecode_session_id(
    source: &SourceKey,
    conversation_id: &str,
) -> Result<StableEntityId, ProjectionContractError> {
    let native_session_key = NativeSessionKey::native_id(
        FORGECODE_NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(conversation_id)?,
    )?;
    derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: FORGECODE_LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })
}

fn forgecode_event_id(
    source: &SourceKey,
    session_id: StableEntityId,
    subrecord_index: u64,
) -> Result<StableEntityId, ProjectionContractError> {
    let native_item_key = NativeItemKey::certified_position(
        FORGECODE_NATIVE_EVENT_POSITION_KIND,
        TypedKey::U64(subrecord_index),
        PositionStability::StableSlot,
    )?;
    derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: FORGECODE_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
}

fn forgecode_schema_evidence(native: &ForgeCodeSourceObservation) -> Vec<u8> {
    format!(
        "forgecode-logical-schema-v1;parser={FORGECODE_NATIVE_PARSER_REVISION};\
         policy={FORGECODE_NATIVE_POLICY_REVISION};user_version={};schema={}",
        native.user_version, native.schema_fingerprint
    )
    .into_bytes()
}

fn checked_add(left: u64, right: u64) -> ForgeCodeSourceBackedResultV0<u64> {
    left.checked_add(right)
        .ok_or(ForgeCodeSourceBackedErrorV0::CountOverflow)
}

pub(crate) struct ForgeCodeTreeAuthority {
    native: ForgeCodeSourceObservation,
    terminal_revalidate:
        Box<dyn Fn() -> Result<(), SqliteSourceAccessError> + Send + Sync + 'static>,
}

impl ReplacementDocumentTree for ForgeCodeSourceSelectionV0 {
    type Leaf = ForgeCodeSourceBackedSourceV0;
    type TreeAuthority = ForgeCodeTreeAuthority;

    fn parser_revision(&self) -> &'static str {
        FORGECODE_SOURCE_BACKED_PARSER_REVISION
    }

    fn owns_source(&self, source: &SourceKey) -> bool {
        source.provider() == CaptureProvider::ForgeCode.as_str()
            && source.source_format() == FORGECODE_SQLITE_SOURCE_FORMAT
            && source.schema_variant() == FORGECODE_SOURCE_SCHEMA_VARIANT
            && source.provider_identity_version() == 1
    }

    fn discover_complete(
        &self,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        let native =
            match discover_forgecode_source(&self.data_root, &self.path).map_err(route_error)? {
                ForgeCodeDiscovery::Live(native) => native,
                ForgeCodeDiscovery::Missing => {
                    return Err(SourceBackedRouteError::new(
                        SourceBackedRouteErrorKind::Unavailable,
                        "selected ForgeCode database is missing",
                    ));
                }
            };
        let leaf = ForgeCodeSourceBackedSourceV0 {
            source: self.source_key().map_err(route_error)?,
            canonical_path: native.canonical_path.clone(),
        };
        let mut fingerprint_hasher = Sha256::new();
        fingerprint_hasher.update(b"ctx-forgecode-document-leaf-v1\0");
        fingerprint_hasher.update(leaf.source.exact_descriptor_digest());
        fingerprint_hasher.update(native.logical_fingerprint);
        let fingerprint = DocumentLeafFingerprint::new(fingerprint_hasher.finalize().into());
        let terminal_revalidate = native
            .database
            .terminal_revalidator()
            .map_err(route_error)?;
        Ok(CompleteDocumentTree::new(
            fingerprint.as_bytes(),
            vec![ObservedDocumentLeaf::new(fingerprint, leaf)],
            ForgeCodeTreeAuthority {
                native,
                terminal_revalidate,
            },
        ))
    }

    fn scan_changed(
        &self,
        authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
        sink: &mut ChangedDocumentSink<'_, '_>,
    ) -> SourceBackedRouteResult<DocumentSourceTerminal> {
        sink.begin_source(leaf.source.clone())?;
        let context = ProviderAdapterContext {
            machine_id: "source-backed".to_owned(),
            source_path: Some(leaf.canonical_path.clone()),
            source_root: leaf.canonical_path.parent().map(Path::to_path_buf),
            imported_at: DateTime::<Utc>::UNIX_EPOCH,
        };
        let mut scanner = ForgeCodeScanner::new(
            authority.native.clone(),
            ForgeCodeFrontier::initial(),
            context,
            true,
        )
        .map_err(route_error)?;
        let mut content_digest = Sha256::new();
        content_digest.update(FORGECODE_RECORD_DIGEST_DOMAIN);
        let mut counts = ScannedSourceCounts::default();
        let mut last_observed_rowid = None;
        let mut terminal = false;
        let mut emitted_pages = 0_u64;
        let mut peak_buffered_documents = 0_u64;
        let mut sink_error = None;
        let streamed = scanner.stream_pages(|page| {
            let page = project_source_backed_page(
                leaf,
                &mut content_digest,
                &mut counts,
                &mut last_observed_rowid,
                &mut terminal,
                page,
            )
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
            peak_buffered_documents = peak_buffered_documents.max(
                u64::try_from(page.documents.len())
                    .map_err(|_| CaptureError::SystemInvariant("ForgeCode page count overflow"))?,
            );
            if !page.documents.is_empty() {
                emitted_pages =
                    emitted_pages
                        .checked_add(1)
                        .ok_or(CaptureError::SystemInvariant(
                            "ForgeCode emitted-page count overflow",
                        ))?;
            }
            for document in page.documents {
                if let Err(error) = sink.emit_core_record(document) {
                    let detail = error.to_string();
                    sink_error = Some(error);
                    return Err(CaptureError::InvalidPayload(detail));
                }
            }
            Ok(())
        });
        if let Some(error) = sink_error {
            return Err(error);
        }
        streamed.map_err(route_error)?;
        if !terminal {
            return Err(route_error(ForgeCodeSourceBackedErrorV0::IncompleteScan));
        }
        let decoded_rows = scanner.decoded_rows();
        if decoded_rows > counts.complete_records
            || peak_buffered_documents > 64
            || (counts.indexed_documents == 0) != (emitted_pages == 0)
        {
            return Err(forgecode_internal(
                "ForgeCode scan violated its one-pass bounded-page receipt",
            ));
        }
        scanner
            .source_database()
            .revalidate()
            .map_err(route_error)?;
        let logical = SqliteLogicalSnapshot::new(
            FORGECODE_SOURCE_BACKED_PARSER_REVISION,
            &forgecode_schema_evidence(&authority.native),
            content_digest.finalize().into(),
            counts,
        );
        let certificate = logical.certify(leaf.source.clone()).map_err(route_error)?;
        authority
            .native
            .database
            .finish_if_active(&authority.native.canonical_path)
            .map_err(route_error)?
            .ok_or_else(|| forgecode_changed("ForgeCode snapshot was already sealed"))?;
        Ok(forgecode_document_terminal(certificate))
    }

    fn revalidate_complete(
        &self,
        tree: &CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>,
    ) -> SourceBackedRouteResult<[u8; 32]> {
        tree.authority
            .native
            .database
            .finish_if_active(&tree.authority.native.canonical_path)
            .map_err(route_error)?;
        (tree.authority.terminal_revalidate)().map_err(route_error)?;
        Ok(tree.tree_fingerprint)
    }
}

fn forgecode_document_terminal(certificate: CertifiedSource) -> DocumentSourceTerminal {
    DocumentSourceTerminal {
        source: certificate.observation().source().clone(),
        opening: certificate.observation().clone(),
        closing: certificate.observation().clone(),
        parser_revision: FORGECODE_SOURCE_BACKED_PARSER_REVISION,
        content_digest: *certificate.content_digest(),
        counts: certificate.counts(),
    }
}

fn forgecode_changed(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::SourceChanged, detail)
}

fn forgecode_internal(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, detail)
}

#[cfg(test)]
mod tests;
