use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    sync::Mutex,
};

use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, BatchHydrationRequest, BatchHydrationResult,
    CaptureProvider, EventHydrationRequest, EventIdentityInput, HydratedProviderRecord,
    HydrationFailure, HydrationFailureKind, LocatorRevisionPolicy, NativeItemKey,
    NativeRecordCoordinate, NativeSessionKey, ProjectionContractError, ScannedSourceCounts,
    SessionIdentityInput, SourceAnchor, SourceKey, SourceRecordLocator,
    SourceResolverContractError, StableEntityId, TypedKey,
};
use ctx_history_index::LexicalDocument;
use rusqlite::{limits::Limit, params_from_iter, Connection};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    nativepath::{
        scan_warp_source_backed_connection, WarpNativeEvent, WarpNativeMessageIdentity,
        WarpNativePage, WarpNativeSession, WarpNativeSink, WarpNativeSourceBackedScan,
    },
    schema::WarpSqliteSchema,
    warp_message_content_at,
};
use crate::{
    common::io::{OpenedProviderSourcePath, ProviderSourceDirectory, ProviderSourceRoot},
    complete_content::sqlite::sqlite_logical_record_digest,
    native_source::NativeSqliteValue,
    provider::source_backed::{
        family::document::{
            ChangedDocumentSink, CompleteDocumentTree, DocumentLeafFingerprint,
            DocumentSourceTerminal, ObservedDocumentLeaf, ReplacementDocumentTree,
        },
        hydration_failure, route_error, SourceBackedRouteError, SourceBackedRouteErrorKind,
        SourceBackedRouteResult,
    },
    provider_sources::{
        open_root_handle_sqlite_source_snapshot, retain_sqlite_source_directory_authority,
        SqliteLogicalSnapshot, SqliteSourceDirectoryAuthority, SqliteSourceEvidence,
        SqliteSourceReadSnapshot,
    },
    CaptureError, Result as CaptureResult, MAX_PROVIDER_SQLITE_VALUE_BYTES,
    WARP_SQLITE_SOURCE_FORMAT,
};

const WARP_SOURCE_ANCHOR_NAMESPACE: &str = "warp.selected-surface";
const WARP_NATIVE_SESSION_NAMESPACE: &str = "warp.conversation";
const WARP_NATIVE_ITEM_NAMESPACE: &str = "warp.task-message";
const WARP_LOGICAL_SESSION_KIND: &str = "warp-conversation";
const WARP_LOGICAL_ITEM_KIND: &str = "warp-task-message";
const WARP_SOURCE_SCHEMA_VARIANT: &str = "warp-agent-task-protobuf-v1";
const WARP_SOURCE_BACKED_PARSER_REVISION: &str = "warp-source-backed-logical-v1";
const WARP_TASK_MESSAGE_RELATION: &str = "agent_tasks.task-message.v2";
const WARP_SCHEMA_EVIDENCE: &[u8] = b"agent_conversations+agent_tasks+unique-task-id-v1";
const WARP_MISSING_TREE_DOMAIN: &[u8] = b"ctx.warp.missing-logical-tree.v1\0";
const HYDRATION_NATIVE_KEY_BATCH: usize = 256;

#[derive(Debug, Error)]
pub(crate) enum WarpSourceBackedErrorV0 {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    Resolver(#[from] SourceResolverContractError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("Warp selected surface key is empty")]
    EmptySurfaceKey,
    #[error("Warp source-backed scan count overflow")]
    CountOverflow,
    #[error("Warp source-backed parser counts do not match its emitted records")]
    ScanCountMismatch,
    #[error("Warp source-backed digest is not canonical lowercase SHA-256")]
    InvalidDigest,
    #[error("Warp source-backed parser emitted an empty lexical record")]
    EmptyLexicalRecord,
    #[error("locator is not a Warp task-message row")]
    InvalidLocator,
    #[error("Warp locator task row digest no longer matches")]
    StaleTaskRow,
}

pub(crate) type WarpSourceBackedResultV0<T> = Result<T, WarpSourceBackedErrorV0>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WarpSourceSelectionV0 {
    data_root: PathBuf,
    path: PathBuf,
    surface_key: String,
}

impl WarpSourceSelectionV0 {
    pub(crate) fn new(
        data_root: impl Into<PathBuf>,
        path: impl Into<PathBuf>,
        surface_key: impl Into<String>,
    ) -> WarpSourceBackedResultV0<Self> {
        let surface_key = surface_key.into();
        if surface_key.is_empty() {
            return Err(WarpSourceBackedErrorV0::EmptySurfaceKey);
        }
        Ok(Self {
            data_root: data_root.into(),
            path: path.into(),
            surface_key,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn surface_key(&self) -> &str {
        &self.surface_key
    }
}

type WarpEventHydrator = fn(
    &WarpSourceSelectionV0,
    &EventHydrationRequest,
) -> Result<HydratedProviderRecord, HydrationFailure>;

pub(crate) fn project_warp_source_backed_v0(
    selection: WarpSourceSelectionV0,
    hydrate_event: WarpEventHydrator,
) -> WarpSourceBackedResultV0<WarpReplacementTreeAdapter> {
    Ok(WarpReplacementTreeAdapter {
        source: warp_source_key(&selection)?,
        selection,
        hydrate_event,
    })
}

pub(crate) fn resolve_warp_locator_v0(
    selection: &WarpSourceSelectionV0,
    request: &EventHydrationRequest,
) -> Result<HydratedProviderRecord, HydrationFailure> {
    let batch = BatchHydrationRequest::new(vec![request.clone()])
        .map_err(|error| hydration_failure(HydrationFailureKind::InvalidLocator, error))?;
    hydrate_warp_group(selection, &batch)?
        .into_records()
        .into_iter()
        .next()
        .ok_or_else(|| {
            hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "Warp one-record hydration returned no record",
            )
        })
}

pub(crate) struct WarpReplacementTreeAdapter {
    selection: WarpSourceSelectionV0,
    source: SourceKey,
    hydrate_event: WarpEventHydrator,
}

pub(crate) struct WarpPresentAuthority {
    retained: RetainedWarpDirectory,
    physical_evidence: SqliteSourceEvidence,
    snapshot: Mutex<Option<SqliteSourceReadSnapshot>>,
}

pub(crate) enum WarpTreeAuthority {
    Present(Box<WarpPresentAuthority>),
    Missing(RetainedWarpDirectory),
}

impl ReplacementDocumentTree for WarpReplacementTreeAdapter {
    type Leaf = ();
    type TreeAuthority = WarpTreeAuthority;

    fn parser_revision(&self) -> &'static str {
        WARP_SOURCE_BACKED_PARSER_REVISION
    }

    fn owns_source(&self, source: &SourceKey) -> bool {
        self.source.exact_descriptor_eq(source)
    }

    fn discover_complete(
        &self,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        let retained =
            RetainedWarpDirectory::open(&self.selection.data_root, self.selection.path())
                .map_err(route_error)?;
        let Some(snapshot) = retained.open_snapshot()? else {
            let fingerprint = missing_tree_fingerprint(&self.source);
            return Ok(CompleteDocumentTree::new(
                fingerprint,
                Vec::new(),
                WarpTreeAuthority::Missing(retained),
            ));
        };
        let physical_evidence = snapshot.evidence().clone();
        let fingerprint = *physical_evidence.revision();
        Ok(CompleteDocumentTree::new(
            fingerprint,
            vec![ObservedDocumentLeaf::with_durable_replay(
                DocumentLeafFingerprint::new(fingerprint),
                (),
                false,
            )],
            WarpTreeAuthority::Present(Box::new(WarpPresentAuthority {
                retained,
                physical_evidence,
                snapshot: Mutex::new(Some(snapshot)),
            })),
        ))
    }

    fn scan_changed(
        &self,
        authority: &Self::TreeAuthority,
        _leaf: &Self::Leaf,
        sink: &mut ChangedDocumentSink<'_, '_>,
    ) -> SourceBackedRouteResult<DocumentSourceTerminal> {
        let WarpTreeAuthority::Present(authority) = authority else {
            return Err(internal_route_error(
                "Warp shared lifecycle requested a changed scan for a missing database",
            ));
        };
        let snapshot = take_warp_snapshot(authority)?;
        sink.begin_source(self.source.clone())?;
        let terminal = scan_warp_logical_snapshot(
            snapshot.connection().map_err(route_error)?,
            &self.source,
            self.selection.path(),
            sink,
        )
        .map_err(route_error)?;
        snapshot.revalidate().map_err(route_error)?;
        authority.retained.revalidate()?;
        restore_warp_snapshot(authority, snapshot)?;
        Ok(terminal)
    }

    fn revalidate_complete(
        &self,
        tree: &CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>,
    ) -> SourceBackedRouteResult<[u8; 32]> {
        let current = match &tree.authority {
            WarpTreeAuthority::Present(authority) => {
                let snapshot = take_warp_snapshot(authority)?;
                finish_warp_authority(authority, snapshot)?;
                tree.tree_fingerprint
            }
            WarpTreeAuthority::Missing(retained) => {
                if retained.open_snapshot()?.is_some() {
                    return Err(source_changed("Warp database appeared"));
                }
                retained.revalidate()?;
                tree.tree_fingerprint
            }
        };
        Ok(current)
    }

    fn hydrate_group(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        if request.events().len() == 1 {
            let record = (self.hydrate_event)(&self.selection, &request.events()[0])?;
            let result = BatchHydrationResult::new(vec![record])
                .map_err(|error| hydration_failure(HydrationFailureKind::InvalidLocator, error))?;
            result.validate_for_request(request)?;
            return Ok(result);
        }
        hydrate_warp_group(&self.selection, request)
    }
}

fn restore_warp_snapshot(
    authority: &WarpPresentAuthority,
    snapshot: SqliteSourceReadSnapshot,
) -> SourceBackedRouteResult<()> {
    let mut slot = authority
        .snapshot
        .lock()
        .map_err(|_| internal_route_error("Warp snapshot lock was poisoned"))?;
    if slot.replace(snapshot).is_some() {
        return Err(internal_route_error(
            "Warp snapshot slot was already occupied",
        ));
    }
    Ok(())
}

fn take_warp_snapshot(
    authority: &WarpPresentAuthority,
) -> SourceBackedRouteResult<SqliteSourceReadSnapshot> {
    authority
        .snapshot
        .lock()
        .map_err(|_| internal_route_error("Warp snapshot lock was poisoned"))?
        .take()
        .ok_or_else(|| internal_route_error("Warp snapshot was already consumed"))
}

fn finish_warp_authority(
    authority: &WarpPresentAuthority,
    snapshot: SqliteSourceReadSnapshot,
) -> SourceBackedRouteResult<()> {
    let evidence = snapshot.finish().map_err(route_error)?;
    authority.retained.revalidate()?;
    if evidence != authority.physical_evidence {
        return Err(source_changed("Warp database changed during its snapshot"));
    }
    Ok(())
}

pub(crate) struct RetainedWarpDirectory {
    root: ProviderSourceRoot,
    directory: ProviderSourceDirectory,
    sqlite: SqliteSourceDirectoryAuthority,
    leaf: OsString,
}

impl RetainedWarpDirectory {
    fn open(data_root: &Path, path: &Path) -> WarpSourceBackedResultV0<Self> {
        let parent = path.parent().ok_or_else(|| {
            CaptureError::InvalidPayload("Warp SQLite source has no parent directory".to_owned())
        })?;
        let leaf = path.file_name().map(OsString::from).ok_or_else(|| {
            CaptureError::InvalidPayload("Warp SQLite source has no leaf name".to_owned())
        })?;
        let root = ProviderSourceRoot::open(parent)?;
        let directory = root.directory()?;
        let authority_handle = directory.try_clone_authority_handle()?;
        let sqlite = retain_sqlite_source_directory_authority(data_root, &authority_handle, parent)
            .map_err(sqlite_access_error)?;
        let retained = Self {
            root,
            directory,
            sqlite,
            leaf,
        };
        retained.revalidate().map_err(|error| {
            WarpSourceBackedErrorV0::Capture(CaptureError::InvalidPayload(error.detail))
        })?;
        Ok(retained)
    }

    fn open_snapshot(&self) -> SourceBackedRouteResult<Option<SqliteSourceReadSnapshot>> {
        match self.directory.open_child(&self.leaf) {
            Ok(OpenedProviderSourcePath::File(file)) => {
                file.revalidate().map_err(route_error)?;
            }
            Ok(OpenedProviderSourcePath::Directory(_)) => {
                return Err(SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::InvalidSource,
                    "Warp SQLite leaf is a directory",
                ));
            }
            Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                self.revalidate()?;
                return Ok(None);
            }
            Err(error) => return Err(route_error(error)),
        }
        let snapshot = open_root_handle_sqlite_source_snapshot(&self.sqlite, &self.leaf)
            .map_err(route_error)?;
        self.revalidate()?;
        Ok(Some(snapshot))
    }

    fn revalidate(&self) -> SourceBackedRouteResult<()> {
        self.directory.revalidate().map_err(route_error)?;
        self.root.revalidate().map_err(route_error)
    }
}

fn scan_warp_logical_snapshot(
    connection: &Connection,
    source: &SourceKey,
    path: &Path,
    sink: &mut ChangedDocumentSink<'_, '_>,
) -> WarpSourceBackedResultV0<DocumentSourceTerminal> {
    let value_limit = i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES)
        .map_err(|_| WarpSourceBackedErrorV0::CountOverflow)?;
    connection.set_limit(Limit::SQLITE_LIMIT_LENGTH, value_limit);
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    let mut projection =
        WarpProjectionSink::new(source.clone(), path.to_string_lossy().into_owned(), sink);
    let native_scan = scan_warp_source_backed_connection(connection, &mut projection)?;
    let counts = scan_counts(&native_scan, &projection)?;
    let content_digest = parse_hex_digest(&native_scan.source_integrity_digest)?;
    let logical = SqliteLogicalSnapshot::new(
        WARP_SOURCE_BACKED_PARSER_REVISION,
        WARP_SCHEMA_EVIDENCE,
        content_digest,
        counts,
    );
    let certificate = logical.certify(source.clone())?;
    Ok(DocumentSourceTerminal {
        source: source.clone(),
        opening: certificate.observation().clone(),
        closing: certificate.observation().clone(),
        parser_revision: WARP_SOURCE_BACKED_PARSER_REVISION,
        content_digest,
        counts,
    })
}

struct WarpProjectionSink<'changed, 'sink, 'writer> {
    source: SourceKey,
    source_path: String,
    session_lineage: BTreeMap<String, WarpSessionLineage>,
    sink: &'changed mut ChangedDocumentSink<'sink, 'writer>,
    indexed_documents: u64,
    rejected_records: u64,
    ignored_records: u64,
}

struct WarpSessionLineage {
    parent_conversation_id: Option<String>,
    root_conversation_id: String,
}

impl<'changed, 'sink, 'writer> WarpProjectionSink<'changed, 'sink, 'writer> {
    fn new(
        source: SourceKey,
        source_path: String,
        sink: &'changed mut ChangedDocumentSink<'sink, 'writer>,
    ) -> Self {
        Self {
            source,
            source_path,
            session_lineage: BTreeMap::new(),
            sink,
            indexed_documents: 0,
            rejected_records: 0,
            ignored_records: 0,
        }
    }
}

impl WarpNativeSink for WarpProjectionSink<'_, '_, '_> {
    fn push_page(&mut self, page: WarpNativePage) -> CaptureResult<()> {
        let WarpNativePage {
            sessions,
            hierarchy_edges,
            events,
            rejections,
            ..
        } = page;
        self.rejected_records = checked_add(
            self.rejected_records,
            u64::try_from(rejections.len())
                .map_err(|_| CaptureError::SystemInvariant("Warp rejection count exceeds u64"))?,
        )
        .map_err(source_backed_capture_error)?;
        let ignored = sessions
            .len()
            .checked_add(hierarchy_edges.len())
            .and_then(|count| u64::try_from(count).ok())
            .ok_or(CaptureError::SystemInvariant(
                "Warp ignored count exceeds u64",
            ))?;
        self.ignored_records =
            self.ignored_records
                .checked_add(ignored)
                .ok_or(CaptureError::SystemInvariant(
                    "Warp ignored count overflowed",
                ))?;
        for session in sessions {
            let conversation_id = session.conversation_id.clone();
            if self
                .session_lineage
                .insert(conversation_id, WarpSessionLineage::from(session))
                .is_some()
            {
                return Err(CaptureError::SystemInvariant(
                    "Warp parser repeated a session",
                ));
            }
        }
        for event in events {
            let lineage = self
                .session_lineage
                .get(&event.identity.conversation_id)
                .ok_or(CaptureError::SystemInvariant(
                    "Warp event has no session lineage",
                ))?;
            let document = lexical_document(&self.source, &self.source_path, lineage, event)
                .map_err(source_backed_capture_error)?;
            self.sink
                .emit_document(document)
                .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
            self.indexed_documents =
                self.indexed_documents
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Warp indexed document count overflowed",
                    ))?;
        }
        Ok(())
    }
}

impl From<WarpNativeSession> for WarpSessionLineage {
    fn from(session: WarpNativeSession) -> Self {
        Self {
            parent_conversation_id: session.parent_conversation_id,
            root_conversation_id: session.root_conversation_id,
        }
    }
}

fn lexical_document(
    source: &SourceKey,
    source_path: &str,
    lineage: &WarpSessionLineage,
    event: WarpNativeEvent,
) -> WarpSourceBackedResultV0<LexicalDocument> {
    let session_id = warp_session_id(source, &event.identity.conversation_id)?;
    let parent_session_id = lineage
        .parent_conversation_id
        .as_deref()
        .map(|parent| warp_session_id(source, parent))
        .transpose()?;
    let root_session_id = warp_session_id(source, &lineage.root_conversation_id)?;
    let is_primary = parent_session_id.is_none();
    let message_key = match &event.identity.message {
        WarpNativeMessageIdentity::ProviderId(message_id) => TypedKey::composite(vec![
            TypedKey::utf8("provider-id")?,
            TypedKey::utf8(message_id)?,
        ])?,
        WarpNativeMessageIdentity::MessageOrdinal(ordinal) => TypedKey::composite(vec![
            TypedKey::utf8("ordinal")?,
            TypedKey::U64(u64::from(*ordinal)),
        ])?,
    };
    let item_key = NativeItemKey::composite(
        WARP_NATIVE_ITEM_NAMESPACE,
        vec![TypedKey::utf8(&event.identity.task_id)?, message_key],
    )?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: WARP_LOGICAL_ITEM_KIND,
        native_item_key: &item_key,
        subrecord_selector: None,
    })?;
    let record_digest = digest_bytes(event.source_record_digest.as_str())?;
    let locator = SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::ProviderSqlite {
            logical_relation: WARP_TASK_MESSAGE_RELATION.to_owned(),
            primary_key: TypedKey::composite(vec![
                TypedKey::utf8(event.identity.task_id.clone())?,
                TypedKey::U64(u64::from(event.native_order.message_ordinal)),
            ])?,
            row_version: Some(TypedKey::bytes(record_digest.to_vec())?),
        },
        LocatorRevisionPolicy::StableRecordEvidence,
        None,
        record_digest,
    )?;
    let body = if event.lexical_body.is_empty() {
        event.kind.to_owned()
    } else {
        event.lexical_body
    };
    if body.is_empty() {
        return Err(WarpSourceBackedErrorV0::EmptyLexicalRecord);
    }
    Ok(LexicalDocument {
        event_id,
        session_id,
        parent_session_id,
        root_session_id,
        source: source.clone(),
        locator,
        provider_session_id: Some(event.identity.conversation_id),
        branch: None,
        source_path: Some(source_path.to_owned()),
        agent_type: if is_primary {
            AgentType::Primary
        } else {
            AgentType::Subagent
        }
        .as_str()
        .to_owned(),
        is_primary,
        event_sequence: event.native_order.provider_event_index,
        occurred_at_unix_ms: event.occurred_at.map(|value| value.timestamp_millis()),
        event_type: event.event_type.as_str().to_owned(),
        role: event.role.map(|role| role.as_str().to_owned()),
        body,
        workspace: None,
        cwd: None,
        touched_files: Vec::new(),
    })
}

fn hydrate_warp_group(
    selection: &WarpSourceSelectionV0,
    request: &BatchHydrationRequest,
) -> Result<BatchHydrationResult, HydrationFailure> {
    if request.events().is_empty() {
        return BatchHydrationResult::new(Vec::new())
            .map_err(|error| hydration_failure(HydrationFailureKind::InvalidLocator, error));
    }
    let source = warp_source_key(selection).map_err(warp_hydration_error)?;
    let coordinates = request
        .events()
        .iter()
        .map(|event| {
            validate_warp_locator(&source, event.locator()).map(|(task_id, message_ordinal)| {
                WarpHydrationCoordinate {
                    task_id,
                    message_ordinal,
                    record_digest: *event.locator().record_digest(),
                }
            })
        })
        .collect::<WarpSourceBackedResultV0<Vec<_>>>()
        .map_err(warp_hydration_error)?;
    let retained = RetainedWarpDirectory::open(&selection.data_root, selection.path())
        .map_err(warp_hydration_error)?;
    let snapshot = retained
        .open_snapshot()
        .map_err(|error| hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error))?
        .ok_or_else(|| {
            hydration_failure(
                HydrationFailureKind::MissingRecord,
                "Warp selected database is missing",
            )
        })?;
    let hydrated = (|| {
        let connection = snapshot
            .connection()
            .map_err(warp_snapshot_hydration_error)?;
        WarpSqliteSchema::detect(connection).map_err(warp_hydration_error)?;
        let keys = coordinates
            .iter()
            .map(|coordinate| coordinate.task_id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let rows = load_task_value_batches(connection, &keys).map_err(warp_hydration_error)?;
        request
            .events()
            .iter()
            .zip(&coordinates)
            .map(|(event, coordinate)| {
                let values = rows.get(&coordinate.task_id).ok_or_else(|| {
                    hydration_failure(
                        HydrationFailureKind::MissingRecord,
                        "Warp task row is missing",
                    )
                })?;
                let digest = digest_bytes(sqlite_logical_record_digest(values).as_str())
                    .map_err(warp_hydration_error)?;
                if digest != coordinate.record_digest {
                    return Err(hydration_failure(
                        HydrationFailureKind::StaleRecordEvidence,
                        "Warp task row digest changed",
                    ));
                }
                let [
                    NativeSqliteValue::Text(conversation_id),
                    NativeSqliteValue::Text(task_id),
                    NativeSqliteValue::Blob(task),
                    _,
                ] = values.as_slice()
                else {
                    return Err(hydration_failure(
                        HydrationFailureKind::StaleRecordEvidence,
                        "Warp task row changed storage class",
                    ));
                };
                let content = warp_message_content_at(
                    task,
                    conversation_id,
                    task_id,
                    usize::try_from(coordinate.message_ordinal).map_err(|_| {
                        hydration_failure(
                            HydrationFailureKind::InvalidLocator,
                            "Warp message ordinal exceeds usize",
                        )
                    })?,
                )
                .map_err(warp_hydration_error)?
                .ok_or_else(|| {
                    hydration_failure(
                        HydrationFailureKind::MissingRecord,
                        "Warp task message is missing",
                    )
                })?;
                Ok(HydratedProviderRecord {
                    event_id: event.event_id(),
                    provider_bytes: content.text.into_bytes(),
                })
            })
            .collect::<Result<Vec<_>, HydrationFailure>>()
    })();
    snapshot.finish().map_err(warp_snapshot_hydration_error)?;
    retained
        .revalidate()
        .map_err(|error| hydration_failure(HydrationFailureKind::StaleSourceEvidence, error))?;
    let result = BatchHydrationResult::new(hydrated?)
        .map_err(|error| hydration_failure(HydrationFailureKind::InvalidLocator, error))?;
    result.validate_for_request(request)?;
    Ok(result)
}

struct WarpHydrationCoordinate {
    task_id: String,
    message_ordinal: u64,
    record_digest: [u8; 32],
}

fn validate_warp_locator(
    source: &SourceKey,
    locator: &SourceRecordLocator,
) -> WarpSourceBackedResultV0<(String, u64)> {
    locator.validate_contract()?;
    if !source.exact_descriptor_eq(locator.source())
        || locator.revision_policy() != LocatorRevisionPolicy::StableRecordEvidence
        || locator.certified_source_revision_digest().is_some()
    {
        return Err(WarpSourceBackedErrorV0::InvalidLocator);
    }
    let NativeRecordCoordinate::ProviderSqlite {
        logical_relation,
        primary_key,
        row_version,
    } = locator.coordinate()
    else {
        return Err(WarpSourceBackedErrorV0::InvalidLocator);
    };
    let TypedKey::Composite(parts) = primary_key else {
        return Err(WarpSourceBackedErrorV0::InvalidLocator);
    };
    let [TypedKey::Utf8(task_id), TypedKey::U64(message_ordinal)] = parts.as_slice() else {
        return Err(WarpSourceBackedErrorV0::InvalidLocator);
    };
    if logical_relation != WARP_TASK_MESSAGE_RELATION
        || row_version.as_ref() != Some(&TypedKey::Bytes(locator.record_digest().to_vec()))
    {
        return Err(WarpSourceBackedErrorV0::InvalidLocator);
    }
    Ok((task_id.clone(), *message_ordinal))
}

fn load_task_value_batches(
    connection: &Connection,
    keys: &[String],
) -> WarpSourceBackedResultV0<BTreeMap<String, Vec<NativeSqliteValue>>> {
    let mut loaded = BTreeMap::new();
    for batch in keys.chunks(HYDRATION_NATIVE_KEY_BATCH) {
        let placeholders = std::iter::repeat_n("?", batch.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "select cast(conversation_id as text), cast(task_id as text), task, \
                    cast(last_modified_at as text) \
             from agent_tasks where task_id in ({placeholders}) order by task_id collate binary"
        );
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(batch.iter()), |row| {
            let task_id = row.get::<_, String>(1)?;
            Ok((
                task_id.clone(),
                vec![
                    NativeSqliteValue::Text(row.get(0)?),
                    NativeSqliteValue::Text(task_id),
                    NativeSqliteValue::Blob(row.get(2)?),
                    NativeSqliteValue::Text(row.get(3)?),
                ],
            ))
        })?;
        for row in rows {
            let (task_id, values) = row?;
            if loaded.insert(task_id, values).is_some() {
                return Err(WarpSourceBackedErrorV0::StaleTaskRow);
            }
        }
    }
    Ok(loaded)
}

fn warp_session_id(
    source: &SourceKey,
    conversation_id: &str,
) -> WarpSourceBackedResultV0<StableEntityId> {
    let session_key = NativeSessionKey::native_id(
        WARP_NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(conversation_id)?,
    )?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: WARP_LOGICAL_SESSION_KIND,
        native_session_key: &session_key,
    })?)
}

fn warp_source_key(selection: &WarpSourceSelectionV0) -> WarpSourceBackedResultV0<SourceKey> {
    let anchor = SourceAnchor::provider_native(
        WARP_SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(selection.surface_key())?,
    )?;
    Ok(SourceKey::derive(
        CaptureProvider::Warp.as_str(),
        WARP_SQLITE_SOURCE_FORMAT,
        WARP_SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )?)
}

fn scan_counts(
    native_scan: &WarpNativeSourceBackedScan,
    sink: &WarpProjectionSink<'_, '_, '_>,
) -> WarpSourceBackedResultV0<ScannedSourceCounts> {
    let retained_records = sink.indexed_documents;
    if retained_records != native_scan.counters.retained_events
        || u64::try_from(sink.session_lineage.len())
            .map_err(|_| WarpSourceBackedErrorV0::CountOverflow)?
            != native_scan.counters.sessions_retained
        || sink.ignored_records
            != native_scan
                .counters
                .sessions_retained
                .checked_add(native_scan.counters.hierarchy_edges)
                .ok_or(WarpSourceBackedErrorV0::CountOverflow)?
    {
        return Err(WarpSourceBackedErrorV0::ScanCountMismatch);
    }
    let complete_records = retained_records
        .checked_add(sink.rejected_records)
        .and_then(|count| count.checked_add(sink.ignored_records))
        .ok_or(WarpSourceBackedErrorV0::CountOverflow)?;
    let conversation_bytes = native_scan
        .counters
        .conversation_rows
        .checked_mul((b"conversation\0".len() + 32) as u64)
        .ok_or(WarpSourceBackedErrorV0::CountOverflow)?;
    let task_bytes = native_scan
        .counters
        .task_rows
        .checked_mul((b"task\0".len() + 32) as u64)
        .ok_or(WarpSourceBackedErrorV0::CountOverflow)?;
    Ok(ScannedSourceCounts {
        complete_records,
        retained_records,
        rejected_records: sink.rejected_records,
        ignored_records: sink.ignored_records,
        indexed_documents: retained_records,
        certified_bytes: conversation_bytes
            .checked_add(task_bytes)
            .ok_or(WarpSourceBackedErrorV0::CountOverflow)?,
    })
}

fn missing_tree_fingerprint(source: &SourceKey) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(WARP_MISSING_TREE_DOMAIN);
    digest.update(source.exact_descriptor_digest());
    digest.finalize().into()
}

fn checked_add(left: u64, right: u64) -> WarpSourceBackedResultV0<u64> {
    left.checked_add(right)
        .ok_or(WarpSourceBackedErrorV0::CountOverflow)
}

fn parse_hex_digest(value: &str) -> WarpSourceBackedResultV0<[u8; 32]> {
    digest_bytes(value)
}

fn digest_bytes(value: &str) -> WarpSourceBackedResultV0<[u8; 32]> {
    if value.len() != 64
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_hexdigit() || byte.is_ascii_uppercase())
    {
        return Err(WarpSourceBackedErrorV0::InvalidDigest);
    }
    let mut digest = [0_u8; 32];
    for (index, slot) in digest.iter_mut().enumerate() {
        let offset = index * 2;
        *slot = u8::from_str_radix(&value[offset..offset + 2], 16)
            .map_err(|_| WarpSourceBackedErrorV0::InvalidDigest)?;
    }
    Ok(digest)
}

fn source_changed(detail: &str) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::SourceChanged, detail)
}

fn internal_route_error(detail: &str) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, detail)
}

fn sqlite_access_error(error: crate::provider_sources::SqliteSourceAccessError) -> CaptureError {
    CaptureError::SystemIo {
        operation: "accessing a retained Warp SQLite source",
        source: io::Error::other(error),
    }
}

fn warp_snapshot_hydration_error(error: impl std::fmt::Display) -> HydrationFailure {
    hydration_failure(HydrationFailureKind::StaleSourceEvidence, error)
}

fn warp_hydration_error(error: impl std::fmt::Display) -> HydrationFailure {
    hydration_failure(HydrationFailureKind::StaleRecordEvidence, error)
}

fn source_backed_capture_error(error: WarpSourceBackedErrorV0) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}
