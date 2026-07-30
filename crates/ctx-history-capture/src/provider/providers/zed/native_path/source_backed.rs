use std::path::{Path, PathBuf};

use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, BatchHydrationRequest, BatchHydrationResult,
    CaptureProvider, ContentSourceResolver, EventHydrationRequest, EventIdentityInput,
    HydratedProviderRecord, HydrationFailure, HydrationFailureKind, LocatorRevisionPolicy,
    NativeItemKey, NativeRecordCoordinate, NativeSessionKey, PositionStability,
    ProjectionContractError, SessionHydrationRequest, SessionIdentityInput, SourceAnchor,
    SourceObservation, SourceRecordLocator, SourceResolverContractError, StableEntityId, TypedKey,
};
#[cfg(test)]
use ctx_history_index::GenerationWriter;
use ctx_history_index::{IndexError, LexicalDocument};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub(crate) use super::query::scan_zed_native_snapshot;
use super::{
    acquire_immutable_snapshot, decode_complete_message_with_identity,
    dto::{
        ZedNativeEvent, ZedNativeMessageIdentity, ZedNativePage, ZedNativeSession, ZedNativeSink,
    },
    query::{
        hydrate_zed_thread_row, hydrate_zed_thread_rows, ZedThreadLineage, ZedThreadLineageResolver,
    },
    ZedNativePathError, ZedNativeResult, ZedSnapshotAcquisition,
};
#[cfg(test)]
use super::{
    record_zed_projected_document, reset_source_backed_work, source_backed_work,
    ZedSourceBackedWork,
};
use crate::{
    complete_content::CompleteContentBodyDigest, CaptureError, ZED_THREADS_SQLITE_SOURCE_FORMAT,
};

const ZED_SOURCE_ANCHOR_NAMESPACE: &str = "zed.selected-threads-database";
const ZED_SOURCE_ANCHOR_KEY: &str = "threads";
const ZED_NATIVE_SESSION_NAMESPACE: &str = "zed.thread";
const ZED_NATIVE_EVENT_NAMESPACE: &str = "zed.thread-message";
const ZED_NATIVE_EVENT_POSITION_KIND: &str = "zed.thread-message-ordinal";
const ZED_LOGICAL_SESSION_KIND: &str = "zed-thread";
const ZED_LOGICAL_EVENT_KIND: &str = "zed-thread-event";
const ZED_SOURCE_SCHEMA_VARIANT: &str = "zed-nativepath-sqlite-v0";
const ZED_SOURCE_REVISION_KIND: &str = "zed-logical-rows-v1";
const ZED_SQLITE_RELATION: &str = "threads";

#[derive(Debug, Error)]
pub(crate) enum ZedSourceBackedErrorV0 {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Index(#[from] IndexError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    Resolver(#[from] SourceResolverContractError),
    #[error(transparent)]
    Native(#[from] ZedNativePathError),
    #[error("Zed immutable SQLite snapshot could not be acquired")]
    SnapshotAcquisitionRace,
    #[error("Zed source-backed count overflow")]
    CountOverflow,
    #[error("Zed event {0:?} was emitted without its bounded session context")]
    MissingSessionContext(String),
    #[error("Zed retained thread {0:?} disappeared while resolving its native lineage")]
    MissingLineageThread(String),
    #[error("Zed source-backed parser emitted an empty lexical body")]
    MissingLexicalBody,
    #[error("Zed source-backed parser emitted an invalid SHA-256 digest")]
    InvalidDigest,
    #[error("locator is not a Zed threads SQLite row coordinate")]
    InvalidZedLocator,
    #[error("Zed locator source revision no longer matches the selected database")]
    LocatorSourceRevisionMismatch,
    #[error("Zed locator thread row no longer matches its certified digest")]
    LocatorRecordDigestMismatch,
    #[error("Zed locator thread row is missing")]
    LocatorRecordMissing,
    #[error("Zed locator message is missing")]
    LocatorMessageMissing,
    #[error("Zed locator message identity no longer matches the certified thread row")]
    LocatorMessageIdentityMismatch,
}

pub(crate) type ZedSourceBackedResultV0<T> = Result<T, ZedSourceBackedErrorV0>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ZedHydratedRecordV0 {
    pub(crate) provider_bytes: Vec<u8>,
    pub(crate) decoded_display_text: String,
}

#[derive(Debug)]
pub(crate) struct ZedLocatorResolverV0 {
    data_root: PathBuf,
    selected_database_path: PathBuf,
    source: ctx_history_core::SourceKey,
}

impl ZedLocatorResolverV0 {
    pub(crate) fn new(
        data_root: impl Into<PathBuf>,
        selected_database_path: impl Into<PathBuf>,
    ) -> ZedSourceBackedResultV0<Self> {
        Ok(Self {
            data_root: data_root.into(),
            selected_database_path: selected_database_path.into(),
            source: zed_source_key()?,
        })
    }

    pub(crate) fn hydrate(
        &self,
        locator: &SourceRecordLocator,
    ) -> ZedSourceBackedResultV0<ZedHydratedRecordV0> {
        let coordinate = validate_zed_locator(&self.source, locator)?;
        let mut snapshot = acquire_snapshot(&self.data_root, &self.selected_database_path)?;
        verify_snapshot_revision(&snapshot.snapshot_revision, &coordinate)?;
        let hydrated = hydrate_coordinate(snapshot.connection()?, &coordinate)?;
        snapshot.finish()?;
        Ok(hydrated)
    }
}

impl ContentSourceResolver for ZedLocatorResolverV0 {
    fn hydrate_event(
        &self,
        request: &EventHydrationRequest,
    ) -> Result<HydratedProviderRecord, HydrationFailure> {
        self.hydrate(request.locator())
            .map(|record| HydratedProviderRecord {
                event_id: request.event_id(),
                provider_bytes: record.provider_bytes,
            })
            .map_err(hydration_failure)
    }

    fn hydrate_session(
        &self,
        request: &SessionHydrationRequest,
    ) -> Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        let mut coordinates = Vec::with_capacity(request.events().len());
        for event in request.events() {
            coordinates.push(
                validate_zed_locator(&self.source, event.locator()).map_err(hydration_failure)?,
            );
        }
        let Some(first) = coordinates.first() else {
            return Ok(Vec::new());
        };
        if coordinates
            .iter()
            .any(|coordinate| coordinate.thread_id != first.thread_id)
        {
            return Err(hydration_failure(ZedSourceBackedErrorV0::InvalidZedLocator));
        }

        let mut snapshot = acquire_snapshot(&self.data_root, &self.selected_database_path)
            .map_err(hydration_failure)?;
        for coordinate in &coordinates {
            verify_snapshot_revision(&snapshot.snapshot_revision, coordinate)
                .map_err(hydration_failure)?;
        }
        let (row, row_digest) = hydrate_zed_thread_row(
            snapshot
                .connection()
                .map_err(ZedSourceBackedErrorV0::from)
                .map_err(hydration_failure)?,
            &first.thread_id,
        )
        .map_err(ZedSourceBackedErrorV0::from)
        .map_err(hydration_failure)?
        .ok_or(ZedSourceBackedErrorV0::LocatorRecordMissing)
        .map_err(hydration_failure)?;
        let row_digest_bytes = digest_bytes(&row_digest).map_err(hydration_failure)?;
        if coordinates
            .iter()
            .any(|coordinate| coordinate.record_digest != row_digest_bytes)
        {
            return Err(hydration_failure(
                ZedSourceBackedErrorV0::LocatorRecordDigestMismatch,
            ));
        }

        let hydrated = request
            .events()
            .iter()
            .zip(coordinates.iter())
            .map(|(event, coordinate)| {
                hydrate_decoded_row(&row, row_digest.clone(), coordinate)
                    .map(|record| HydratedProviderRecord {
                        event_id: event.event_id(),
                        provider_bytes: record.provider_bytes,
                    })
                    .map_err(hydration_failure)
            })
            .collect::<Result<Vec<_>, _>>()?;
        snapshot
            .finish()
            .map_err(ZedSourceBackedErrorV0::from)
            .map_err(hydration_failure)?;
        Ok(hydrated)
    }

    fn hydrate_batch(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        let mut coordinates = Vec::with_capacity(request.events().len());
        for event in request.events() {
            coordinates.push(
                validate_zed_locator(&self.source, event.locator()).map_err(hydration_failure)?,
            );
        }
        let mut snapshot = acquire_snapshot(&self.data_root, &self.selected_database_path)
            .map_err(hydration_failure)?;
        for coordinate in &coordinates {
            verify_snapshot_revision(&snapshot.snapshot_revision, coordinate)
                .map_err(hydration_failure)?;
        }
        let mut thread_ids = coordinates
            .iter()
            .map(|coordinate| coordinate.thread_id.clone())
            .collect::<Vec<_>>();
        thread_ids.sort_unstable();
        thread_ids.dedup();
        let rows = hydrate_zed_thread_rows(
            snapshot
                .connection()
                .map_err(ZedSourceBackedErrorV0::from)
                .map_err(hydration_failure)?,
            &thread_ids,
        )
        .map_err(ZedSourceBackedErrorV0::from)
        .map_err(hydration_failure)?;
        let mut hydrated = Vec::with_capacity(request.events().len());
        for (event, coordinate) in request.events().iter().zip(&coordinates) {
            let (row, row_digest) = rows
                .get(&coordinate.thread_id)
                .ok_or_else(|| hydration_failure(ZedSourceBackedErrorV0::LocatorRecordMissing))?;
            if digest_bytes(row_digest).map_err(hydration_failure)? != coordinate.record_digest {
                return Err(hydration_failure(
                    ZedSourceBackedErrorV0::LocatorRecordDigestMismatch,
                ));
            }
            let record = hydrate_decoded_row(row, row_digest.clone(), coordinate)
                .map_err(hydration_failure)?;
            hydrated.push(HydratedProviderRecord {
                event_id: event.event_id(),
                provider_bytes: record.provider_bytes,
            });
        }
        snapshot
            .finish()
            .map_err(ZedSourceBackedErrorV0::from)
            .map_err(hydration_failure)?;
        BatchHydrationResult::new(hydrated).map_err(|error| HydrationFailure {
            kind: HydrationFailureKind::InvalidLocator,
            detail: error.to_string(),
        })
    }
}

pub(crate) struct ZedSourceBackedSinkV0<'writer> {
    emit_document: Box<dyn FnMut(LexicalDocument) -> ZedSourceBackedResultV0<()> + 'writer>,
    lineage: ZedThreadLineageResolver,
    source: ctx_history_core::SourceKey,
    revision_digest: [u8; 32],
    source_path: String,
    last_session: Option<ZedSessionProjectionContextV0>,
    staged_documents: u64,
    failure: Option<ZedSourceBackedErrorV0>,
}

#[derive(Clone)]
struct ZedSessionProjectionContextV0 {
    session: ZedNativeSession,
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
    root_session_id: StableEntityId,
}

impl<'writer> ZedSourceBackedSinkV0<'writer> {
    #[cfg(test)]
    pub(crate) fn new(
        writer: &'writer mut GenerationWriter,
        connection: &rusqlite::Connection,
        source: ctx_history_core::SourceKey,
        revision_digest: [u8; 32],
        source_path: String,
    ) -> ZedSourceBackedResultV0<Self> {
        Self::with_emitter(
            connection,
            source,
            revision_digest,
            source_path,
            move |document| writer.add_document(document).map_err(Into::into),
        )
    }

    pub(crate) fn with_emitter(
        connection: &rusqlite::Connection,
        source: ctx_history_core::SourceKey,
        revision_digest: [u8; 32],
        source_path: String,
        emit_document: impl FnMut(LexicalDocument) -> ZedSourceBackedResultV0<()> + 'writer,
    ) -> ZedSourceBackedResultV0<Self> {
        Ok(Self {
            emit_document: Box::new(emit_document),
            lineage: ZedThreadLineageResolver::new(connection)?,
            source,
            revision_digest,
            source_path,
            last_session: None,
            staged_documents: 0,
            failure: None,
        })
    }

    pub(crate) fn take_failure(&mut self) -> Option<ZedSourceBackedErrorV0> {
        self.failure.take()
    }

    pub(crate) fn staged_documents(&self) -> u64 {
        self.staged_documents
    }

    fn project_session(
        &mut self,
        session: ZedNativeSession,
    ) -> ZedSourceBackedResultV0<ZedSessionProjectionContextV0> {
        let ZedThreadLineage {
            parent_thread_id,
            root_thread_id,
        } = self.lineage.resolve(&session.thread_id)?.ok_or_else(|| {
            ZedSourceBackedErrorV0::MissingLineageThread(session.thread_id.clone())
        })?;
        Ok(ZedSessionProjectionContextV0 {
            session_id: zed_session_identity(&self.source, &session.thread_id)?,
            parent_session_id: parent_thread_id
                .as_deref()
                .map(|thread_id| zed_session_identity(&self.source, thread_id))
                .transpose()?,
            root_session_id: zed_session_identity(&self.source, &root_thread_id)?,
            session,
        })
    }

    fn push_page_inner(&mut self, page: ZedNativePage) -> ZedSourceBackedResultV0<()> {
        let sessions = page
            .sessions
            .into_iter()
            .map(|session| self.project_session(session))
            .collect::<ZedSourceBackedResultV0<Vec<_>>>()?;
        for event in page.events {
            let session = sessions
                .iter()
                .find(|context| context.session.thread_id == event.identity.thread_id)
                .or_else(|| {
                    self.last_session
                        .as_ref()
                        .filter(|context| context.session.thread_id == event.identity.thread_id)
                })
                .ok_or_else(|| {
                    ZedSourceBackedErrorV0::MissingSessionContext(event.identity.thread_id.clone())
                })?;
            let document = zed_lexical_document(
                &self.source,
                self.revision_digest,
                &self.source_path,
                session,
                event,
            )?;
            (self.emit_document)(document)?;
            #[cfg(test)]
            record_zed_projected_document();
            self.staged_documents = self
                .staged_documents
                .checked_add(1)
                .ok_or(ZedSourceBackedErrorV0::CountOverflow)?;
        }
        if let Some(session) = sessions.last() {
            self.last_session = Some(session.clone());
        }
        Ok(())
    }
}

impl ZedNativeSink for ZedSourceBackedSinkV0<'_> {
    fn push_page(&mut self, page: ZedNativePage) -> ZedNativeResult<()> {
        if let Err(error) = self.push_page_inner(page) {
            self.failure = Some(error);
            return Err(ZedNativePathError::UnsupportedSchema(
                "Zed source-backed lexical sink rejected a bounded page".to_owned(),
            ));
        }
        Ok(())
    }
}

pub(crate) fn zed_source_key() -> ZedSourceBackedResultV0<ctx_history_core::SourceKey> {
    let anchor = SourceAnchor::provider_native(
        ZED_SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(ZED_SOURCE_ANCHOR_KEY)?,
    )?;
    Ok(ctx_history_core::SourceKey::derive(
        CaptureProvider::Zed.as_str(),
        ZED_THREADS_SQLITE_SOURCE_FORMAT,
        ZED_SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )?)
}

fn zed_session_identity(
    source: &ctx_history_core::SourceKey,
    thread_id: &str,
) -> ZedSourceBackedResultV0<StableEntityId> {
    let native_session_key =
        NativeSessionKey::native_id(ZED_NATIVE_SESSION_NAMESPACE, TypedKey::utf8(thread_id)?)?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: ZED_LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })?)
}

fn zed_lexical_document(
    source: &ctx_history_core::SourceKey,
    revision_digest: [u8; 32],
    source_path: &str,
    context: &ZedSessionProjectionContextV0,
    event: ZedNativeEvent,
) -> ZedSourceBackedResultV0<LexicalDocument> {
    if event.lexical_body.is_empty() {
        return Err(ZedSourceBackedErrorV0::MissingLexicalBody);
    }
    let session = &context.session;
    let session_id = context.session_id;
    let native_item_key = native_event_key(&event)?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: ZED_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let record_digest = digest_bytes(&event.record_digest)?;
    let row_version = TypedKey::composite(vec![
        TypedKey::bytes(record_digest.to_vec())?,
        locator_message_identity(&event.identity.message)?,
        TypedKey::U64(event.native_order.message_ordinal),
        TypedKey::U64(u64::from(event.native_order.sub_ordinal)),
    ])?;
    let locator = SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::ProviderSqlite {
            logical_relation: ZED_SQLITE_RELATION.to_owned(),
            primary_key: TypedKey::utf8(&event.identity.thread_id)?,
            row_version: Some(row_version),
        },
        LocatorRevisionPolicy::ExactSourceRevision,
        Some(revision_digest),
        record_digest,
    )?;
    let event_sequence = event
        .native_order
        .message_ordinal
        .checked_mul(2)
        .and_then(|value| value.checked_add(u64::from(event.native_order.sub_ordinal)))
        .ok_or(ZedSourceBackedErrorV0::CountOverflow)?;
    Ok(LexicalDocument {
        event_id,
        session_id,
        parent_session_id: context.parent_session_id,
        root_session_id: context.root_session_id,
        source: source.clone(),
        locator,
        provider_session_id: Some(session.thread_id.clone()),
        branch: None,
        source_path: Some(source_path.to_owned()),
        agent_type: if context.parent_session_id.is_some() {
            AgentType::Subagent
        } else {
            AgentType::Primary
        }
        .as_str()
        .to_owned(),
        is_primary: context.parent_session_id.is_none(),
        event_sequence,
        occurred_at_unix_ms: Some(event.occurred_at.timestamp_millis()),
        event_type: event.event_type.as_str().to_owned(),
        role: Some(event.role.as_str().to_owned()),
        body: event.lexical_body,
        workspace: session.folder_paths.first().cloned(),
        cwd: session.cwd.clone(),
        touched_files: event.safe_file_touches,
    })
}

fn native_event_key(event: &ZedNativeEvent) -> ZedSourceBackedResultV0<NativeItemKey> {
    let message_ordinal = event.native_order.message_ordinal;
    let sub_ordinal = u64::from(event.native_order.sub_ordinal);
    match &event.identity.message {
        ZedNativeMessageIdentity::ProviderId { value, .. } => Ok(NativeItemKey::composite(
            ZED_NATIVE_EVENT_NAMESPACE,
            vec![
                TypedKey::utf8(&event.identity.thread_id)?,
                TypedKey::utf8(value)?,
                TypedKey::U64(sub_ordinal),
            ],
        )?),
        ZedNativeMessageIdentity::MessageOrdinal(_) => Ok(NativeItemKey::certified_position(
            ZED_NATIVE_EVENT_POSITION_KIND,
            TypedKey::composite(vec![
                TypedKey::utf8(&event.identity.thread_id)?,
                TypedKey::U64(message_ordinal),
                TypedKey::U64(sub_ordinal),
            ])?,
            PositionStability::AppendStable,
        )?),
    }
}

fn locator_message_identity(
    identity: &ZedNativeMessageIdentity,
) -> ZedSourceBackedResultV0<TypedKey> {
    Ok(match identity {
        ZedNativeMessageIdentity::ProviderId { value, .. } => {
            TypedKey::composite(vec![TypedKey::Bool(true), TypedKey::utf8(value)?])?
        }
        ZedNativeMessageIdentity::MessageOrdinal(_) => {
            TypedKey::composite(vec![TypedKey::Bool(false), TypedKey::Null])?
        }
    })
}

pub(crate) fn source_observation(
    source: &ctx_history_core::SourceKey,
    snapshot_revision: &str,
) -> ZedSourceBackedResultV0<SourceObservation> {
    Ok(SourceObservation::new(
        source.clone(),
        ZED_SOURCE_REVISION_KIND,
        snapshot_revision.as_bytes().to_vec(),
    )?)
}

pub(crate) fn snapshot_revision_digest(snapshot_revision: &str) -> [u8; 32] {
    Sha256::digest(snapshot_revision.as_bytes()).into()
}

pub(crate) fn acquire_snapshot(
    data_root: &Path,
    path: &Path,
) -> ZedSourceBackedResultV0<super::ZedImmutableSqliteSnapshot> {
    match acquire_immutable_snapshot(data_root, path)? {
        ZedSnapshotAcquisition::Acquired(snapshot) => Ok(*snapshot),
        ZedSnapshotAcquisition::Incomplete => Err(ZedSourceBackedErrorV0::SnapshotAcquisitionRace),
    }
}

#[derive(Debug)]
struct ZedLocatorCoordinateV0 {
    thread_id: String,
    message_identity: ZedNativeMessageIdentity,
    message_ordinal: u64,
    sub_ordinal: u32,
    source_revision_digest: [u8; 32],
    record_digest: [u8; 32],
}

fn validate_zed_locator(
    source: &ctx_history_core::SourceKey,
    locator: &SourceRecordLocator,
) -> ZedSourceBackedResultV0<ZedLocatorCoordinateV0> {
    locator.validate_contract()?;
    if !source.exact_descriptor_eq(locator.source())
        || locator.revision_policy() != LocatorRevisionPolicy::ExactSourceRevision
    {
        return Err(ZedSourceBackedErrorV0::InvalidZedLocator);
    }
    let Some(source_revision_digest) = locator.certified_source_revision_digest().copied() else {
        return Err(ZedSourceBackedErrorV0::InvalidZedLocator);
    };
    let NativeRecordCoordinate::ProviderSqlite {
        logical_relation,
        primary_key,
        row_version,
    } = locator.coordinate()
    else {
        return Err(ZedSourceBackedErrorV0::InvalidZedLocator);
    };
    let TypedKey::Utf8(thread_id) = primary_key else {
        return Err(ZedSourceBackedErrorV0::InvalidZedLocator);
    };
    let Some(TypedKey::Composite(parts)) = row_version.as_ref() else {
        return Err(ZedSourceBackedErrorV0::InvalidZedLocator);
    };
    let [TypedKey::Bytes(row_digest), TypedKey::Composite(identity), TypedKey::U64(message_ordinal), TypedKey::U64(sub_ordinal)] =
        parts.as_slice()
    else {
        return Err(ZedSourceBackedErrorV0::InvalidZedLocator);
    };
    if logical_relation != ZED_SQLITE_RELATION
        || row_digest.as_slice() != locator.record_digest()
        || identity.len() != 2
    {
        return Err(ZedSourceBackedErrorV0::InvalidZedLocator);
    }
    let message_identity = match identity.as_slice() {
        [TypedKey::Bool(true), TypedKey::Utf8(value)] => ZedNativeMessageIdentity::ProviderId {
            value: value.clone(),
            message_ordinal: *message_ordinal,
        },
        [TypedKey::Bool(false), TypedKey::Null] => {
            ZedNativeMessageIdentity::MessageOrdinal(*message_ordinal)
        }
        _ => return Err(ZedSourceBackedErrorV0::InvalidZedLocator),
    };
    let sub_ordinal =
        u32::try_from(*sub_ordinal).map_err(|_| ZedSourceBackedErrorV0::InvalidZedLocator)?;
    Ok(ZedLocatorCoordinateV0 {
        thread_id: thread_id.clone(),
        message_identity,
        message_ordinal: *message_ordinal,
        sub_ordinal,
        source_revision_digest,
        record_digest: *locator.record_digest(),
    })
}

fn verify_snapshot_revision(
    snapshot_revision: &str,
    coordinate: &ZedLocatorCoordinateV0,
) -> ZedSourceBackedResultV0<()> {
    if snapshot_revision_digest(snapshot_revision) != coordinate.source_revision_digest {
        return Err(ZedSourceBackedErrorV0::LocatorSourceRevisionMismatch);
    }
    Ok(())
}

fn hydrate_coordinate(
    connection: &rusqlite::Connection,
    coordinate: &ZedLocatorCoordinateV0,
) -> ZedSourceBackedResultV0<ZedHydratedRecordV0> {
    let (row, row_digest) = hydrate_zed_thread_row(connection, &coordinate.thread_id)?
        .ok_or(ZedSourceBackedErrorV0::LocatorRecordMissing)?;
    if digest_bytes(&row_digest)? != coordinate.record_digest {
        return Err(ZedSourceBackedErrorV0::LocatorRecordDigestMismatch);
    }
    hydrate_decoded_row(&row, row_digest, coordinate)
}

fn hydrate_decoded_row(
    row: &super::super::thread::ZedThreadRow,
    row_digest: CompleteContentBodyDigest,
    coordinate: &ZedLocatorCoordinateV0,
) -> ZedSourceBackedResultV0<ZedHydratedRecordV0> {
    let decoded =
        decode_complete_message_with_identity(row, coordinate.message_ordinal, row_digest)?
            .ok_or(ZedSourceBackedErrorV0::LocatorMessageMissing)?;
    let expected_message_id = match &coordinate.message_identity {
        ZedNativeMessageIdentity::ProviderId { value, .. } => Some(value.as_str()),
        ZedNativeMessageIdentity::MessageOrdinal(_) => None,
    };
    if decoded.native_message_id.as_deref() != expected_message_id
        || decoded.native_message_ordinal != coordinate.message_ordinal
        || decoded.native_sub_ordinal != coordinate.sub_ordinal
    {
        return Err(ZedSourceBackedErrorV0::LocatorMessageIdentityMismatch);
    }
    let provider_bytes = decoded.message.complete_text.as_bytes().to_vec();
    Ok(ZedHydratedRecordV0 {
        provider_bytes,
        decoded_display_text: decoded.message.complete_text,
    })
}

fn digest_bytes(digest: &CompleteContentBodyDigest) -> ZedSourceBackedResultV0<[u8; 32]> {
    decode_sha256_hex(digest.as_str())
}

pub(crate) fn decode_sha256_hex(value: &str) -> ZedSourceBackedResultV0<[u8; 32]> {
    if value.len() != 64 {
        return Err(ZedSourceBackedErrorV0::InvalidDigest);
    }
    let mut digest = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_nibble(pair[0]).ok_or(ZedSourceBackedErrorV0::InvalidDigest)?;
        let low = hex_nibble(pair[1]).ok_or(ZedSourceBackedErrorV0::InvalidDigest)?;
        digest[index] = (high << 4) | low;
    }
    Ok(digest)
}

fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

fn hydration_failure(error: ZedSourceBackedErrorV0) -> HydrationFailure {
    let kind = match &error {
        ZedSourceBackedErrorV0::SnapshotAcquisitionRace
        | ZedSourceBackedErrorV0::Native(ZedNativePathError::Io(_))
        | ZedSourceBackedErrorV0::Capture(CaptureError::Io(_)) => {
            HydrationFailureKind::TemporarilyUnavailable
        }
        ZedSourceBackedErrorV0::LocatorSourceRevisionMismatch => {
            HydrationFailureKind::StaleSourceEvidence
        }
        ZedSourceBackedErrorV0::LocatorRecordDigestMismatch
        | ZedSourceBackedErrorV0::LocatorMessageIdentityMismatch => {
            HydrationFailureKind::StaleRecordEvidence
        }
        ZedSourceBackedErrorV0::LocatorRecordMissing
        | ZedSourceBackedErrorV0::LocatorMessageMissing => HydrationFailureKind::MissingRecord,
        ZedSourceBackedErrorV0::Native(ZedNativePathError::UnsupportedSchema(_)) => {
            HydrationFailureKind::UnsupportedParserRevision
        }
        _ => HydrationFailureKind::InvalidLocator,
    };
    HydrationFailure {
        kind,
        detail: error.to_string(),
    }
}

#[cfg(test)]
#[path = "source_backed_two_thread_tests.rs"]
mod two_thread_tests;

#[cfg(test)]
mod tests {
    use std::fs;

    use ctx_history_core::NativeRecordCoordinate;
    use rusqlite::{params, Connection};
    use serde_json::json;

    use super::*;

    #[test]
    fn source_backed_zed_cold_exact_and_replacement_preserve_stable_ids() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        let database = source.join("threads.db");
        create_database(&database, "cold exact sentinel");

        let event = project_root_document(&database);
        let cold_event_id = event.event_id;
        let cold_session_id = event.session_id;
        assert_eq!(event.parent_session_id, None);
        assert_eq!(event.root_session_id, event.session_id);
        assert_eq!(event.provider_session_id.as_deref(), Some("thread-1"));
        assert_eq!(event.branch, None);
        assert_eq!(
            event.source_path.as_deref(),
            Some(database.to_string_lossy().as_ref())
        );
        assert_eq!(event.agent_type, "primary");
        assert!(event.is_primary);
        assert!(matches!(
            event.locator.coordinate(),
            NativeRecordCoordinate::ProviderSqlite {
                logical_relation,
                primary_key: TypedKey::Utf8(thread_id),
                row_version: Some(TypedKey::Composite(_)),
            } if logical_relation == "threads" && thread_id == "thread-1"
        ));
        let resolver =
            ZedLocatorResolverV0::new(crate::test_provider_sqlite_data_root(), &database).unwrap();
        let hydrated = resolver.hydrate(&event.locator).unwrap();
        assert_eq!(hydrated.decoded_display_text, "cold exact sentinel");
        assert_eq!(hydrated.provider_bytes, b"cold exact sentinel");

        replace_thread(&database, "replacement exact sentinel");
        let stale = resolver.hydrate(&event.locator).unwrap_err();
        assert!(matches!(
            stale,
            ZedSourceBackedErrorV0::LocatorSourceRevisionMismatch
        ));

        let replacement_event = project_root_document(&database);
        assert_eq!(replacement_event.event_id, cold_event_id);
        assert_eq!(replacement_event.session_id, cold_session_id);
        assert_eq!(replacement_event.body, "replacement exact sentinel");
        let hydrated =
            ZedLocatorResolverV0::new(crate::test_provider_sqlite_data_root(), &database)
                .unwrap()
                .hydrate(&replacement_event.locator);
        assert_eq!(
            hydrated.unwrap().decoded_display_text,
            "replacement exact sentinel"
        );
    }

    #[test]
    fn source_backed_zed_resolves_native_thread_lineage() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        let database = source.join("threads.db");
        create_database(&database, "root lineage sentinel");
        insert_child_thread(&database, "child lineage sentinel");

        let snapshot =
            acquire_snapshot(crate::test_provider_sqlite_data_root(), &database).unwrap();
        let mut lineage = ZedThreadLineageResolver::new(snapshot.connection().unwrap()).unwrap();
        let child = lineage.resolve("a-child").unwrap().unwrap();
        assert_eq!(child.parent_thread_id.as_deref(), Some("thread-1"));
        assert_eq!(child.root_thread_id, "thread-1");
    }

    #[derive(Default)]
    struct CollectingSink {
        pages: Vec<ZedNativePage>,
    }

    impl ZedNativeSink for CollectingSink {
        fn push_page(&mut self, page: ZedNativePage) -> ZedNativeResult<()> {
            self.pages.push(page);
            Ok(())
        }
    }

    pub(super) fn project_root_document(path: &Path) -> LexicalDocument {
        let mut snapshot = acquire_snapshot(crate::test_provider_sqlite_data_root(), path).unwrap();
        let revision = snapshot.snapshot_revision.clone();
        let physical_locator = snapshot.physical_locator.clone();
        let mut sink = CollectingSink::default();
        let scan = scan_zed_native_snapshot(
            snapshot.connection().unwrap(),
            &physical_locator,
            &revision,
            &mut sink,
        )
        .unwrap();
        assert_eq!(scan.counters.native_thread_rows, 1);
        assert_eq!(scan.counters.sessions_retained, 1);
        assert_eq!(scan.counters.retained_events, 1);
        assert_eq!(scan.counters.rejected_threads, 0);
        assert!(scan.counters.certified_logical_bytes > 0);
        snapshot.finish().unwrap();

        let mut sessions = sink
            .pages
            .iter_mut()
            .flat_map(|page| page.sessions.drain(..));
        let session = sessions.next().unwrap();
        assert!(sessions.next().is_none());
        drop(sessions);
        let source = zed_source_key().unwrap();
        let session_id = zed_session_identity(&source, &session.thread_id).unwrap();
        let context = ZedSessionProjectionContextV0 {
            session,
            session_id,
            parent_session_id: None,
            root_session_id: session_id,
        };
        let mut events = sink.pages.into_iter().flat_map(|page| page.events);
        let event = events.next().unwrap();
        assert!(events.next().is_none());
        zed_lexical_document(
            &source,
            snapshot_revision_digest(&revision),
            &path.to_string_lossy(),
            &context,
            event,
        )
        .unwrap()
    }

    pub(super) fn create_database(path: &Path, text: &str) {
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(
                "PRAGMA user_version = 3;
                 CREATE TABLE threads (
                     id TEXT PRIMARY KEY,
                     summary TEXT NOT NULL,
                     updated_at TEXT NOT NULL,
                     data_type TEXT NOT NULL,
                     data BLOB NOT NULL,
                     parent_id TEXT,
                     folder_paths TEXT,
                     folder_paths_order TEXT,
                     created_at TEXT
                 );",
            )
            .unwrap();
        insert_thread(&connection, text);
    }

    fn insert_thread(connection: &Connection, text: &str) {
        let payload = serde_json::to_vec(&json!({
            "version": "0.3.0",
            "title": "Source-backed Zed thread",
            "updated_at": "2026-07-28T12:00:10Z",
            "messages": [{
                "User": {
                    "id": "message-1",
                    "content": [{"Text": text}]
                }
            }]
        }))
        .unwrap();
        connection
            .execute(
                "INSERT INTO threads (
                     id, summary, updated_at, data_type, data, parent_id,
                     folder_paths, folder_paths_order, created_at
                 ) VALUES (
                     'thread-1', 'source-backed fixture', '2026-07-28T12:00:10Z',
                     'json', ?1, NULL, '/workspace/zed', '0',
                     '2026-07-28T12:00:00Z'
                 )",
                params![payload],
            )
            .unwrap();
    }

    pub(super) fn replace_thread(path: &Path, text: &str) {
        let connection = Connection::open(path).unwrap();
        let payload = serde_json::to_vec(&json!({
            "version": "0.3.0",
            "title": "Source-backed Zed thread",
            "updated_at": "2026-07-28T12:00:11Z",
            "messages": [{
                "User": {
                    "id": "message-1",
                    "content": [{"Text": text}]
                }
            }]
        }))
        .unwrap();
        connection
            .execute(
                "UPDATE threads
                 SET data = ?1, updated_at = '2026-07-28T12:00:11Z'
                 WHERE id = 'thread-1'",
                params![payload],
            )
            .unwrap();
    }

    fn insert_child_thread(path: &Path, text: &str) {
        let connection = Connection::open(path).unwrap();
        let payload = serde_json::to_vec(&json!({
            "version": "0.3.0",
            "title": "Source-backed Zed child thread",
            "updated_at": "2026-07-28T12:00:12Z",
            "messages": [{
                "User": {
                    "id": "message-child",
                    "content": [{"Text": text}]
                }
            }]
        }))
        .unwrap();
        connection
            .execute(
                "INSERT INTO threads (
                     id, summary, updated_at, data_type, data, parent_id,
                     folder_paths, folder_paths_order, created_at
                 ) VALUES (
                     'a-child', 'source-backed child fixture', '2026-07-28T12:00:12Z',
                     'json', ?1, 'thread-1', '/workspace/zed', '0',
                     '2026-07-28T12:00:01Z'
                 )",
                params![payload],
            )
            .unwrap();
    }
}
