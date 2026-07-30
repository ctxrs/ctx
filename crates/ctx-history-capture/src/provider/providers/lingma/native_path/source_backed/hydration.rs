use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

use ctx_history_core::{
    derive_event_id, derive_session_id, BatchHydrationRequest, BatchHydrationResult,
    ContentSourceResolver, EventHydrationRequest, EventIdentityInput, HydratedProviderRecord,
    HydrationFailure, HydrationFailureKind, LocatorRevisionPolicy, NativeItemKey,
    NativeRecordCoordinate, NativeSessionKey, SessionHydrationRequest, SessionIdentityInput,
    SourceKey, SourceRecordLocator, SubrecordSelector, TypedKey,
};
use rusqlite::{params_from_iter, types::Value};
use sha2::{Digest, Sha256};

use crate::{provider_sources::SqliteSourceAccessError, CaptureError};

use super::super::{
    detect_schema, record_identity_set_read,
    records::{
        assistant_text, lingma_logical_record_sha256, native_values, row_from_native_values,
    },
    visit_raw_rows, LingmaRow, LINGMA_SET_READ_ROWS,
};
#[cfg(test)]
use super::identity::LingmaSourceBackedRecordV0;
use super::{
    discovery::{LingmaRootAuthorizedSource, LingmaSourceInventoryV0},
    LingmaSourceBackedErrorV0, LingmaSourceBackedResultV0, ASSISTANT_ERROR_COORDINATE,
    ASSISTANT_SUMMARY_COORDINATE, LOGICAL_EVENT_KIND, LOGICAL_RELATION, LOGICAL_SESSION_KIND,
    NATIVE_POSITION_KIND, NATIVE_REQUEST_NAMESPACE, NATIVE_SESSION_NAMESPACE,
    NATIVE_SUBRECORD_NAMESPACE, USER_PROMPT_COORDINATE,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LingmaBodyKind {
    UserPrompt,
    AssistantSummary,
    AssistantError,
}

impl LingmaBodyKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::UserPrompt => USER_PROMPT_COORDINATE,
            Self::AssistantSummary => ASSISTANT_SUMMARY_COORDINATE,
            Self::AssistantError => ASSISTANT_ERROR_COORDINATE,
        }
    }

    fn logical_text(self, row: &LingmaRow) -> Result<String, HydrationFailure> {
        match self {
            Self::UserPrompt if !row.chat_prompt.trim().is_empty() => Ok(row.chat_prompt.clone()),
            Self::UserPrompt => Err(hydration_failure(
                HydrationFailureKind::MissingRecord,
                "Lingma user-prompt subrecord has no meaningful text",
            )),
            Self::AssistantSummary | Self::AssistantError => {
                let expected = if self == Self::AssistantSummary {
                    "summary"
                } else {
                    "error_result"
                };
                assistant_text(row)
                    .filter(|(_, body_kind, _)| *body_kind == expected)
                    .map(|(text, _, _)| text)
                    .ok_or_else(|| {
                        hydration_failure(
                            HydrationFailureKind::MissingRecord,
                            "Lingma assistant subrecord is missing",
                        )
                    })
            }
        }
    }
}

#[derive(Debug, Clone)]
enum LingmaNativeIdentityCoordinate {
    Request {
        session_id: String,
        request_id: String,
    },
    Position {
        ordinal: u64,
        revision_scope: TypedKey,
    },
}

impl LingmaNativeIdentityCoordinate {
    fn validate_and_build(
        &self,
        row: &LingmaRow,
        current_record_scope: &TypedKey,
        evidence: &LingmaIdentityEvidence,
    ) -> Result<NativeItemKey, HydrationFailure> {
        match self {
            Self::Request {
                session_id,
                request_id,
            } => {
                if &row.session_id != session_id
                    || row.request_id.as_deref() != Some(request_id.as_str())
                    || request_id.trim().is_empty()
                {
                    return Err(hydration_failure(
                        HydrationFailureKind::InvalidLocator,
                        "Lingma request-native key does not match the reopened row",
                    ));
                }
                if evidence
                    .request_counts
                    .get(&(session_id.clone(), request_id.clone()))
                    != Some(&1)
                {
                    return Err(hydration_failure(
                        HydrationFailureKind::InvalidLocator,
                        "Lingma request-native key is not unique in the reopened source",
                    ));
                }
                NativeItemKey::composite(
                    NATIVE_REQUEST_NAMESPACE,
                    vec![
                        TypedKey::utf8(session_id.clone()).map_err(invalid_locator)?,
                        TypedKey::utf8(request_id.clone()).map_err(invalid_locator)?,
                    ],
                )
                .map_err(invalid_locator)
            }
            Self::Position {
                ordinal,
                revision_scope,
            } => {
                if revision_scope != current_record_scope {
                    return Err(hydration_failure(
                        HydrationFailureKind::InvalidLocator,
                        "Lingma position-native key has the wrong row revision scope",
                    ));
                }
                if evidence.position_ordinals.get(&row.rowid) != Some(ordinal) {
                    return Err(hydration_failure(
                        HydrationFailureKind::InvalidLocator,
                        "Lingma position-native key does not match the row ordinal",
                    ));
                }
                NativeItemKey::revision_scoped_position(
                    NATIVE_POSITION_KIND,
                    TypedKey::U64(*ordinal),
                    revision_scope.clone(),
                )
                .map_err(invalid_locator)
            }
        }
    }
}

#[derive(Default)]
struct LingmaIdentityEvidence {
    request_counts: BTreeMap<(String, String), u64>,
    position_ordinals: BTreeMap<i64, u64>,
}

fn load_identity_evidence(
    connection: &rusqlite::Connection,
    coordinates: &[LingmaCoordinate],
) -> Result<LingmaIdentityEvidence, HydrationFailure> {
    let request_identities = coordinates
        .iter()
        .filter_map(|coordinate| match &coordinate.native_identity {
            LingmaNativeIdentityCoordinate::Request {
                session_id,
                request_id,
            } => Some((session_id.clone(), request_id.clone())),
            LingmaNativeIdentityCoordinate::Position { .. } => None,
        })
        .collect::<BTreeSet<_>>();
    let position_rowids = coordinates
        .iter()
        .filter_map(|coordinate| {
            matches!(
                coordinate.native_identity,
                LingmaNativeIdentityCoordinate::Position { .. }
            )
            .then_some(coordinate.rowid)
        })
        .collect::<BTreeSet<_>>();
    Ok(LingmaIdentityEvidence {
        request_counts: load_request_identity_counts(connection, &request_identities)?,
        position_ordinals: load_position_ordinals(connection, &position_rowids)?,
    })
}

fn load_request_identity_counts(
    connection: &rusqlite::Connection,
    identities: &BTreeSet<(String, String)>,
) -> Result<BTreeMap<(String, String), u64>, HydrationFailure> {
    if identities.is_empty() {
        return Ok(BTreeMap::new());
    }
    let identities = identities.iter().cloned().collect::<Vec<_>>();
    let mut counts = BTreeMap::new();
    for identities in identities.chunks(LINGMA_SET_READ_ROWS) {
        record_identity_set_read();
        let values = std::iter::repeat_n("(?, ?)", identities.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "with requested(session_id, request_id) as (values {values})
             select requested.session_id, requested.request_id, count(c.rowid)
               from requested
               left join chat_record c
                 on cast(c.session_id as text) = requested.session_id
                and cast(c.request_id as text) = requested.request_id
              group by requested.session_id, requested.request_id
              order by requested.session_id, requested.request_id"
        );
        let parameters = identities.iter().flat_map(|(session_id, request_id)| {
            [
                Value::Text(session_id.clone()),
                Value::Text(request_id.clone()),
            ]
        });
        let mut statement = connection
            .prepare(&sql)
            .map_err(CaptureError::from)
            .map_err(map_capture_hydration)?;
        let rows = statement
            .query_map(params_from_iter(parameters), |row| {
                Ok((
                    (row.get::<_, String>(0)?, row.get::<_, String>(1)?),
                    row.get::<_, u64>(2)?,
                ))
            })
            .map_err(CaptureError::from)
            .map_err(map_capture_hydration)?;
        counts.extend(
            rows.collect::<rusqlite::Result<BTreeMap<_, _>>>()
                .map_err(CaptureError::from)
                .map_err(map_capture_hydration)?,
        );
    }
    if counts.len() != identities.len() {
        return Err(hydration_failure(
            HydrationFailureKind::StaleSourceEvidence,
            "Lingma identity evidence omitted a requested key",
        ));
    }
    Ok(counts)
}

fn load_position_ordinals(
    connection: &rusqlite::Connection,
    rowids: &BTreeSet<i64>,
) -> Result<BTreeMap<i64, u64>, HydrationFailure> {
    if rowids.is_empty() {
        return Ok(BTreeMap::new());
    }
    record_identity_set_read();
    let mut statement = connection
        .prepare("select rowid from chat_record order by rowid")
        .map_err(CaptureError::from)
        .map_err(map_capture_hydration)?;
    let mut rows = statement
        .query([])
        .map_err(CaptureError::from)
        .map_err(map_capture_hydration)?;
    let last_requested = rowids.last().copied().unwrap_or(i64::MAX);
    let mut ordinals = BTreeMap::new();
    let mut ordinal = 0_u64;
    while let Some(row) = rows
        .next()
        .map_err(CaptureError::from)
        .map_err(map_capture_hydration)?
    {
        let rowid = row
            .get::<_, i64>(0)
            .map_err(CaptureError::from)
            .map_err(map_capture_hydration)?;
        if rowids.contains(&rowid) {
            ordinals.insert(rowid, ordinal);
        }
        if rowid >= last_requested {
            break;
        }
        ordinal = ordinal.checked_add(1).ok_or_else(|| {
            hydration_failure(
                HydrationFailureKind::TemporarilyUnavailable,
                "Lingma row ordinal exceeds u64",
            )
        })?;
    }
    Ok(ordinals)
}

#[derive(Debug, Clone)]
struct LingmaCoordinate {
    rowid: i64,
    body_kind: LingmaBodyKind,
    row_digest: [u8; 32],
    native_identity: LingmaNativeIdentityCoordinate,
}

fn decode_lingma_locator(
    locator: &SourceRecordLocator,
) -> Result<LingmaCoordinate, HydrationFailure> {
    if locator.revision_policy() != LocatorRevisionPolicy::StableRecordEvidence
        || locator.certified_source_revision_digest().is_some()
    {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Lingma locator is not stable-record scoped",
        ));
    }
    let NativeRecordCoordinate::ProviderSqlite {
        logical_relation,
        primary_key,
        row_version,
    } = locator.coordinate()
    else {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Lingma locator is not a provider SQLite coordinate",
        ));
    };
    if logical_relation != LOGICAL_RELATION {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Lingma locator has an unsupported logical relation",
        ));
    }
    let Some(TypedKey::Bytes(row_digest)) = row_version else {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Lingma locator has no typed row version",
        ));
    };
    let row_digest: [u8; 32] = row_digest.as_slice().try_into().map_err(|_| {
        hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Lingma locator row version has an invalid length",
        )
    })?;
    let TypedKey::Composite(parts) = primary_key else {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Lingma locator primary key is not composite",
        ));
    };
    let [TypedKey::I64(rowid), TypedKey::Utf8(body_kind), TypedKey::Composite(native)] =
        parts.as_slice()
    else {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Lingma locator primary key has an invalid shape",
        ));
    };
    let body_kind = match body_kind.as_str() {
        USER_PROMPT_COORDINATE => LingmaBodyKind::UserPrompt,
        ASSISTANT_SUMMARY_COORDINATE => LingmaBodyKind::AssistantSummary,
        ASSISTANT_ERROR_COORDINATE => LingmaBodyKind::AssistantError,
        _ => {
            return Err(hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "Lingma locator addresses an unsupported logical body",
            ));
        }
    };
    let native_identity = match native.as_slice() {
        [TypedKey::Utf8(kind), TypedKey::Utf8(session_id), TypedKey::Utf8(request_id)]
            if kind == "request" =>
        {
            LingmaNativeIdentityCoordinate::Request {
                session_id: session_id.clone(),
                request_id: request_id.clone(),
            }
        }
        [TypedKey::Utf8(kind), TypedKey::U64(ordinal), revision_scope] if kind == "position" => {
            LingmaNativeIdentityCoordinate::Position {
                ordinal: *ordinal,
                revision_scope: revision_scope.clone(),
            }
        }
        _ => {
            return Err(hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "Lingma locator native key has an invalid shape",
            ));
        }
    };
    Ok(LingmaCoordinate {
        rowid: *rowid,
        body_kind,
        row_digest,
        native_identity,
    })
}

fn verify_record_digest(
    locator: &SourceRecordLocator,
    provider_bytes: &[u8],
) -> Result<(), HydrationFailure> {
    let digest: [u8; 32] = Sha256::digest(provider_bytes).into();
    if &digest == locator.record_digest() {
        Ok(())
    } else {
        Err(hydration_failure(
            HydrationFailureKind::StaleRecordEvidence,
            "Lingma logical text digest no longer matches",
        ))
    }
}

fn invalid_locator(error: impl std::fmt::Display) -> HydrationFailure {
    hydration_failure(HydrationFailureKind::InvalidLocator, error.to_string())
}

fn hydration_failure(kind: HydrationFailureKind, detail: impl Into<String>) -> HydrationFailure {
    HydrationFailure {
        kind,
        detail: detail.into(),
    }
}

fn map_capture_hydration(error: CaptureError) -> HydrationFailure {
    match error {
        CaptureError::SourceChangedDuringCapture => hydration_failure(
            HydrationFailureKind::StaleSourceEvidence,
            "Lingma source changed during reopening",
        ),
        CaptureError::Sqlite(rusqlite::Error::QueryReturnedNoRows) => hydration_failure(
            HydrationFailureKind::MissingRecord,
            "Lingma source record is missing",
        ),
        CaptureError::UnsupportedSchema(_) | CaptureError::UnsupportedSchemaVersion(_) => {
            hydration_failure(
                HydrationFailureKind::UnsupportedParserRevision,
                "Lingma SQLite schema is unsupported",
            )
        }
        CaptureError::InvalidPayload(_)
        | CaptureError::Json(_)
        | CaptureError::Sqlite(
            rusqlite::Error::FromSqlConversionFailure(..)
            | rusqlite::Error::IntegralValueOutOfRange(..)
            | rusqlite::Error::InvalidColumnType(..),
        ) => hydration_failure(
            HydrationFailureKind::StaleRecordEvidence,
            "Lingma SQLite row is malformed for the certified parser",
        ),
        _ => hydration_failure(
            HydrationFailureKind::TemporarilyUnavailable,
            "Lingma source could not be reopened",
        ),
    }
}

fn map_parser_hydration(error: CaptureError) -> HydrationFailure {
    match error {
        CaptureError::InvalidPayload(_)
        | CaptureError::UnsupportedSchema(_)
        | CaptureError::UnsupportedSchemaVersion(_) => hydration_failure(
            HydrationFailureKind::UnsupportedParserRevision,
            "Lingma SQLite schema is unsupported",
        ),
        error => map_capture_hydration(error),
    }
}

fn map_sqlite_hydration(error: SqliteSourceAccessError) -> HydrationFailure {
    match error {
        SqliteSourceAccessError::SourceChanged
        | SqliteSourceAccessError::ConnectionIdentityMismatch => hydration_failure(
            HydrationFailureKind::StaleSourceEvidence,
            "Lingma SQLite source changed during reopening",
        ),
        SqliteSourceAccessError::Io { source, .. }
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            hydration_failure(
                HydrationFailureKind::ConfirmedDeleted,
                "Lingma database leaf is absent beneath the admitted source root",
            )
        }
        error => hydration_failure(
            HydrationFailureKind::TemporarilyUnavailable,
            error.to_string(),
        ),
    }
}

fn map_lingma_source_hydration(error: LingmaSourceBackedErrorV0) -> HydrationFailure {
    match error {
        LingmaSourceBackedErrorV0::Capture(error) => map_capture_hydration(error),
        LingmaSourceBackedErrorV0::SqliteSource(error) => map_sqlite_hydration(error),
        LingmaSourceBackedErrorV0::Projection(error) => invalid_locator(error),
        LingmaSourceBackedErrorV0::Resolver(error) => invalid_locator(error),
        error => hydration_failure(
            HydrationFailureKind::TemporarilyUnavailable,
            error.to_string(),
        ),
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LingmaSourceBackedResolverV0 {
    data_root: PathBuf,
    pub(super) sources: BTreeMap<[u8; 32], (SourceKey, PathBuf)>,
}

impl LingmaSourceBackedResolverV0 {
    pub(crate) fn new(
        data_root: impl Into<PathBuf>,
        inventory: &LingmaSourceInventoryV0,
    ) -> LingmaSourceBackedResultV0<Self> {
        let mut sources = BTreeMap::new();
        for database in &inventory.databases {
            let source = database.source_key()?;
            sources.insert(source.identity().digest(), (source, database.path.clone()));
        }
        Ok(Self {
            data_root: data_root.into(),
            sources,
        })
    }

    #[cfg(test)]
    pub(crate) fn hydrate_record(
        &self,
        record: &LingmaSourceBackedRecordV0,
    ) -> Result<HydratedProviderRecord, HydrationFailure> {
        let request =
            EventHydrationRequest::new(record.document.event_id, record.document.locator.clone())
                .map_err(invalid_locator)?;
        self.hydrate_event(&request)
    }

    pub(crate) fn hydrate_requests(
        &self,
        requests: &[EventHydrationRequest],
    ) -> Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        if requests.is_empty() {
            return Ok(Vec::new());
        }
        let coordinates = requests
            .iter()
            .map(|request| decode_lingma_locator(request.locator()))
            .collect::<Result<Vec<_>, _>>()?;
        let source_key = requests[0].locator().source();
        if requests
            .iter()
            .any(|request| !request.locator().source().exact_descriptor_eq(source_key))
        {
            return Err(hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "Lingma hydration batch spans multiple source descriptors",
            ));
        }
        let (source, path) = self
            .sources
            .get(&source_key.identity().digest())
            .ok_or_else(|| {
                hydration_failure(
                    HydrationFailureKind::ConfirmedDeleted,
                    "Lingma source is absent from the complete admitted inventory",
                )
            })?;
        if !source.exact_descriptor_eq(source_key) {
            return Err(hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "Lingma source descriptor does not match the admitted source identity",
            ));
        }

        let root_authority = LingmaRootAuthorizedSource::retain(&self.data_root, path)
            .map_err(map_lingma_source_hydration)?;
        let sqlite_snapshot = root_authority
            .open_snapshot()
            .map_err(map_lingma_source_hydration)?;
        let hydration = (|| {
            let connection = sqlite_snapshot.connection().map_err(map_sqlite_hydration)?;
            let encoding = detect_schema(connection).map_err(map_parser_hydration)?;
            let identity_evidence = load_identity_evidence(connection, &coordinates)?;
            let rowids = coordinates
                .iter()
                .map(|coordinate| coordinate.rowid)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            let mut values_by_row = BTreeMap::new();
            visit_raw_rows(connection, &rowids, |raw| {
                let row = super::super::decode_raw_row(raw, encoding).map_err(|rowid| {
                    CaptureError::InvalidPayload(format!(
                        "Lingma SQLite row {rowid} is malformed for the certified parser"
                    ))
                })?;
                values_by_row.insert(row.rowid, native_values(&row));
                Ok(())
            })
            .map_err(map_capture_hydration)?;
            let mut hydrated = Vec::with_capacity(requests.len());
            for (request, coordinate) in requests.iter().zip(coordinates) {
                let values = values_by_row.get(&coordinate.rowid).ok_or_else(|| {
                    hydration_failure(
                        HydrationFailureKind::MissingRecord,
                        "Lingma chat_record row is missing",
                    )
                })?;
                if lingma_logical_record_sha256(values) != coordinate.row_digest {
                    return Err(hydration_failure(
                        HydrationFailureKind::StaleRecordEvidence,
                        "Lingma logical row version no longer matches",
                    ));
                }
                let row = row_from_native_values(values).map_err(map_capture_hydration)?;
                let current_record_scope =
                    TypedKey::bytes(coordinate.row_digest.to_vec()).map_err(invalid_locator)?;
                let native_item_key = coordinate.native_identity.validate_and_build(
                    &row,
                    &current_record_scope,
                    &identity_evidence,
                )?;
                let session_key = NativeSessionKey::native_id(
                    NATIVE_SESSION_NAMESPACE,
                    TypedKey::utf8(row.session_id.clone()).map_err(invalid_locator)?,
                )
                .map_err(invalid_locator)?;
                let session_id = derive_session_id(SessionIdentityInput {
                    source,
                    logical_session_kind: LOGICAL_SESSION_KIND,
                    native_session_key: &session_key,
                })
                .map_err(invalid_locator)?;
                let body_kind = coordinate.body_kind.as_str();
                let subrecord = SubrecordSelector::native_id(
                    NATIVE_SUBRECORD_NAMESPACE,
                    TypedKey::utf8(body_kind).map_err(invalid_locator)?,
                )
                .map_err(invalid_locator)?;
                let expected_event_id = derive_event_id(EventIdentityInput {
                    source,
                    session_id,
                    logical_item_kind: LOGICAL_EVENT_KIND,
                    native_item_key: &native_item_key,
                    subrecord_selector: Some(&subrecord),
                })
                .map_err(invalid_locator)?;
                if expected_event_id != request.event_id() {
                    return Err(hydration_failure(
                        HydrationFailureKind::InvalidLocator,
                        "Lingma native key does not derive the requested event identity",
                    ));
                }
                let text = coordinate.body_kind.logical_text(&row)?;
                verify_record_digest(request.locator(), text.as_bytes())?;
                hydrated.push(HydratedProviderRecord {
                    event_id: request.event_id(),
                    provider_bytes: text.into_bytes(),
                });
            }
            Ok(hydrated)
        })();
        let finished = sqlite_snapshot.finish().map_err(map_sqlite_hydration);
        let root_current = root_authority
            .source_root
            .revalidate()
            .map_err(map_capture_hydration);
        finished?;
        root_current?;
        hydration
    }

    pub(crate) fn hydrate_batch_request(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        let result = BatchHydrationResult::new(self.hydrate_requests(request.events())?).map_err(
            |error| {
                hydration_failure(
                    HydrationFailureKind::InvalidLocator,
                    format!("invalid Lingma batch hydration result: {error}"),
                )
            },
        )?;
        result.validate_for_request(request)?;
        Ok(result)
    }
}

impl ContentSourceResolver for LingmaSourceBackedResolverV0 {
    fn hydrate_event(
        &self,
        request: &EventHydrationRequest,
    ) -> Result<HydratedProviderRecord, HydrationFailure> {
        self.hydrate_requests(std::slice::from_ref(request))?
            .into_iter()
            .next()
            .ok_or_else(|| {
                hydration_failure(
                    HydrationFailureKind::MissingRecord,
                    "Lingma hydration returned no record",
                )
            })
    }

    fn hydrate_batch(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        self.hydrate_batch_request(request)
    }

    fn hydrate_session(
        &self,
        request: &SessionHydrationRequest,
    ) -> Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        self.hydrate_requests(request.events())
    }
}
