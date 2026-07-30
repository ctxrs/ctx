use std::collections::BTreeMap;

use ctx_history_core::{
    derive_event_id, BatchHydrationRequest, BatchHydrationResult, ContentSourceResolver,
    EventHydrationRequest, EventIdentityInput, HydratedProviderRecord, HydrationFailure,
    HydrationFailureKind, LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate,
    SessionHydrationRequest, SourceKey, SourceRecordLocator, TypedKey,
};
use sha2::{Digest, Sha256};

use crate::{
    native_source::NativeSqliteValue,
    provider::normalization::{provider_json_text, provider_value_text},
    provider_sources::SqliteSourceAccessError,
    CaptureError, MAX_PROVIDER_SQLITE_VALUE_BYTES,
};

use super::super::super::{
    model::{checkpoint_id, item_is_output, item_text, provider_session_id, PlatformMessageLink},
    source::{fetch_candidate, hydrate_conversation, hydrate_platform_message, AstrBotSql},
};
use super::{
    discovery::{
        open_root_authorized_snapshot, AstrBotSourceBackedInventoryV0, AstrBotSourceBackedSourceV0,
    },
    identity::{conversation_native_item_key, logical_values_digest, stable_session_id},
    parsing::{conversation_items, platform_session_fact, serialized_hash},
    AstrBotSourceBackedErrorV0, AstrBotSourceBackedResultV0, CONVERSATION_MESSAGE_RELATION,
    CONVERSATION_OUTPUT_RELATION, LOGICAL_EVENT_KIND, PLATFORM_MESSAGE_RELATION,
};

#[derive(Debug, Clone)]
pub(crate) struct AstrBotSourceBackedResolverV0 {
    pub(super) sources: BTreeMap<[u8; 32], AstrBotSourceBackedSourceV0>,
}

impl AstrBotSourceBackedResolverV0 {
    pub(crate) fn from_inventory(
        inventory: &AstrBotSourceBackedInventoryV0,
    ) -> AstrBotSourceBackedResultV0<Self> {
        let sources = inventory
            .sources
            .iter()
            .cloned()
            .map(|source| (source.source_key.identity().digest(), source))
            .collect();
        Ok(Self { sources })
    }

    pub(crate) fn hydrate_requests(
        &self,
        requests: &[EventHydrationRequest],
    ) -> std::result::Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        if requests.is_empty() {
            return Ok(Vec::new());
        }
        let mut coordinates = Vec::with_capacity(requests.len());
        for request in requests {
            coordinates.push(decode_locator(request.locator())?);
        }
        let source_key = requests[0].locator().source();
        let source = self
            .sources
            .get(&source_key.identity().digest())
            .ok_or_else(|| {
                hydration_failure(
                    HydrationFailureKind::ConfirmedDeleted,
                    "AstrBot source is absent from the complete admitted inventory",
                )
            })?;
        if !source.source_key.exact_descriptor_eq(source_key) {
            return Err(hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "AstrBot source descriptor does not match the admitted source identity",
            ));
        }
        if requests
            .iter()
            .any(|request| !request.locator().source().exact_descriptor_eq(source_key))
        {
            return Err(hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "AstrBot hydration batch spans multiple source descriptors",
            ));
        }

        let (source_root, sqlite_snapshot) =
            open_root_authorized_snapshot(&source.path).map_err(map_source_hydration)?;
        let hydration = (|| {
            let conn = sqlite_snapshot.connection().map_err(map_sqlite_hydration)?;
            let sql = AstrBotSql::new(conn).map_err(map_parser_hydration)?;
            let checkpoint_links = coordinates
                .iter()
                .any(AstrBotCoordinate::is_platform)
                .then(|| load_checkpoint_links(conn, &sql))
                .transpose()?
                .unwrap_or_default();
            let mut hydrated = Vec::with_capacity(requests.len());
            let mut conversations = BTreeMap::new();
            for (request, coordinate) in requests.iter().zip(coordinates) {
                let provider_bytes = match coordinate {
                    AstrBotCoordinate::Conversation {
                        physical_rowid,
                        item_index,
                        row_digest,
                        content_kind,
                    } => {
                        if let std::collections::btree_map::Entry::Vacant(entry) =
                            conversations.entry(physical_rowid)
                        {
                            let row = hydrate_conversation(
                                conn,
                                &sql.conversation_hydration,
                                physical_rowid,
                            )
                            .map_err(map_capture_hydration)?;
                            let values = super::super::super::model::conversation_values(row);
                            entry.insert(values);
                        }
                        let values = conversations.get(&physical_rowid).ok_or_else(|| {
                            hydration_failure(
                                HydrationFailureKind::MissingRecord,
                                "AstrBot conversation row is missing",
                            )
                        })?;
                        if logical_values_digest(values) != row_digest {
                            return Err(hydration_failure(
                                HydrationFailureKind::StaleRecordEvidence,
                                "AstrBot conversation row version no longer matches",
                            ));
                        }
                        hydrate_conversation_coordinate(
                            &source.source_key,
                            request,
                            values,
                            physical_rowid,
                            item_index,
                            content_kind,
                        )?
                    }
                    AstrBotCoordinate::Platform {
                        physical_rowid,
                        logical_id,
                        row_digest,
                    } => hydrate_platform_coordinate(
                        &source.source_key,
                        request,
                        conn,
                        &sql,
                        physical_rowid,
                        logical_id,
                        row_digest,
                        &checkpoint_links,
                    )?,
                };
                hydrated.push(HydratedProviderRecord {
                    event_id: request.event_id(),
                    provider_bytes,
                });
            }
            Ok(hydrated)
        })();
        let finished = sqlite_snapshot.finish().map_err(map_sqlite_hydration);
        let root_current = source_root.revalidate().map_err(map_capture_hydration);
        finished?;
        root_current?;
        hydration
    }

    pub(crate) fn hydrate_batch_request(
        &self,
        request: &BatchHydrationRequest,
    ) -> std::result::Result<BatchHydrationResult, HydrationFailure> {
        let result = BatchHydrationResult::new(self.hydrate_requests(request.events())?).map_err(
            |error| {
                hydration_failure(
                    HydrationFailureKind::InvalidLocator,
                    format!("invalid AstrBot batch hydration result: {error}"),
                )
            },
        )?;
        result.validate_for_request(request)?;
        Ok(result)
    }
}

impl ContentSourceResolver for AstrBotSourceBackedResolverV0 {
    fn hydrate_event(
        &self,
        request: &EventHydrationRequest,
    ) -> std::result::Result<HydratedProviderRecord, HydrationFailure> {
        self.hydrate_requests(std::slice::from_ref(request))?
            .into_iter()
            .next()
            .ok_or_else(|| {
                hydration_failure(
                    HydrationFailureKind::MissingRecord,
                    "AstrBot hydration returned no record",
                )
            })
    }

    fn hydrate_batch(
        &self,
        request: &BatchHydrationRequest,
    ) -> std::result::Result<BatchHydrationResult, HydrationFailure> {
        self.hydrate_batch_request(request)
    }

    fn hydrate_session(
        &self,
        request: &SessionHydrationRequest,
    ) -> std::result::Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        self.hydrate_requests(request.events())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConversationContentKind {
    Message,
    Output,
}

#[derive(Debug, Clone, Copy)]
enum AstrBotCoordinate {
    Conversation {
        physical_rowid: i64,
        item_index: u32,
        row_digest: [u8; 32],
        content_kind: ConversationContentKind,
    },
    Platform {
        physical_rowid: i64,
        logical_id: i64,
        row_digest: [u8; 32],
    },
}

impl AstrBotCoordinate {
    const fn is_platform(&self) -> bool {
        matches!(self, Self::Platform { .. })
    }
}

fn decode_locator(
    locator: &SourceRecordLocator,
) -> std::result::Result<AstrBotCoordinate, HydrationFailure> {
    if locator.revision_policy() != LocatorRevisionPolicy::StableRecordEvidence
        || locator.certified_source_revision_digest().is_some()
    {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "AstrBot SQLite locator is not stable-record scoped",
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
            "AstrBot locator is not a provider SQLite coordinate",
        ));
    };
    let Some(TypedKey::Bytes(row_digest)) = row_version else {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "AstrBot SQLite locator has no typed row version",
        ));
    };
    let row_digest: [u8; 32] = row_digest.as_slice().try_into().map_err(|_| {
        hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "AstrBot SQLite row version has an invalid length",
        )
    })?;
    let TypedKey::Composite(parts) = primary_key else {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "AstrBot SQLite key is not composite",
        ));
    };
    if logical_relation == PLATFORM_MESSAGE_RELATION {
        let [TypedKey::I64(physical_rowid), TypedKey::I64(logical_id)] = parts.as_slice() else {
            return Err(hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "AstrBot platform-message key has an invalid shape",
            ));
        };
        return Ok(AstrBotCoordinate::Platform {
            physical_rowid: *physical_rowid,
            logical_id: *logical_id,
            row_digest,
        });
    }
    let content_kind = match logical_relation.as_str() {
        CONVERSATION_MESSAGE_RELATION => ConversationContentKind::Message,
        CONVERSATION_OUTPUT_RELATION => ConversationContentKind::Output,
        _ => {
            return Err(hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "AstrBot logical relation is unsupported",
            ));
        }
    };
    let [TypedKey::I64(physical_rowid), TypedKey::U64(item_index)] = parts.as_slice() else {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "AstrBot conversation key has an invalid shape",
        ));
    };
    Ok(AstrBotCoordinate::Conversation {
        physical_rowid: *physical_rowid,
        item_index: u32::try_from(*item_index).map_err(|_| {
            hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "AstrBot conversation item index exceeds u32",
            )
        })?,
        row_digest,
        content_kind,
    })
}

fn hydrate_conversation_coordinate(
    source: &SourceKey,
    request: &EventHydrationRequest,
    values: &[NativeSqliteValue],
    physical_rowid: i64,
    item_index: u32,
    content_kind: ConversationContentKind,
) -> std::result::Result<Vec<u8>, HydrationFailure> {
    let row =
        super::super::super::model::decode_conversation(values).map_err(map_capture_hydration)?;
    let (items, content_is_array) = conversation_items(&row.content);
    let item = items
        .get(usize::try_from(item_index).unwrap_or(usize::MAX))
        .ok_or_else(|| {
            hydration_failure(
                HydrationFailureKind::MissingRecord,
                "AstrBot conversation subrecord is missing",
            )
        })?;
    if checkpoint_id(item).is_some() {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "AstrBot locator addresses a non-indexed checkpoint",
        ));
    }
    let is_output = item_is_output(item);
    if matches!(
        (content_kind, is_output),
        (ConversationContentKind::Message, true) | (ConversationContentKind::Output, false)
    ) {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "AstrBot locator relation does not match the native subrecord kind",
        ));
    }
    let text = if content_is_array {
        item_text(item)
    } else {
        provider_value_text(item)
    }
    .filter(|text| !text.trim().is_empty())
    .ok_or_else(|| {
        hydration_failure(
            HydrationFailureKind::MissingRecord,
            "AstrBot conversation subrecord has no meaningful display text",
        )
    })?;
    let session_id =
        stable_session_id(source, &provider_session_id(&row)).map_err(invalid_locator)?;
    let revision_scope =
        TypedKey::bytes(logical_values_digest(values).to_vec()).map_err(invalid_locator)?;
    let native_item_key = conversation_native_item_key(
        physical_rowid,
        usize::try_from(item_index).unwrap_or(usize::MAX),
        Some(item),
        &revision_scope,
    )
    .map_err(invalid_locator)?;
    let expected_event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .map_err(invalid_locator)?;
    if expected_event_id != request.event_id() {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "AstrBot conversation native key does not derive the requested event identity",
        ));
    }
    verify_record_digest(request.locator(), text.as_bytes(), "conversation subrecord")?;
    Ok(text.into_bytes())
}

#[allow(clippy::too_many_arguments)]
fn hydrate_platform_coordinate(
    source: &SourceKey,
    request: &EventHydrationRequest,
    conn: &rusqlite::Connection,
    sql: &AstrBotSql,
    physical_rowid: i64,
    logical_id: i64,
    row_digest: [u8; 32],
    checkpoint_links: &BTreeMap<String, PlatformMessageLink>,
) -> std::result::Result<Vec<u8>, HydrationFailure> {
    let hydration = sql.platform_message_hydration.as_deref().ok_or_else(|| {
        hydration_failure(
            HydrationFailureKind::UnsupportedParserRevision,
            "AstrBot source has no supported platform-message relation",
        )
    })?;
    let row =
        hydrate_platform_message(conn, hydration, physical_rowid).map_err(map_capture_hydration)?;
    if row.id != logical_id {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "AstrBot platform-message logical key does not match the reopened row",
        ));
    }
    let observed_row_digest =
        serialized_hash(b"astrbot-platform-row-v1\0", &row).map_err(map_capture_hydration)?;
    if observed_row_digest != row_digest {
        return Err(hydration_failure(
            HydrationFailureKind::StaleRecordEvidence,
            "AstrBot platform-message row version no longer matches",
        ));
    }
    let text = row
        .content
        .as_deref()
        .map(provider_json_text)
        .as_ref()
        .and_then(provider_value_text)
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| {
            hydration_failure(
                HydrationFailureKind::MissingRecord,
                "AstrBot platform-message row has no meaningful display text",
            )
        })?;
    let link = row
        .llm_checkpoint_id
        .as_ref()
        .and_then(|checkpoint| checkpoint_links.get(checkpoint));
    let session = platform_session_fact(&row, link);
    let session_id =
        stable_session_id(source, &session.provider_session_id).map_err(invalid_locator)?;
    let native_item_key =
        NativeItemKey::native_id("astrbot.platform-message", TypedKey::I64(logical_id))
            .map_err(invalid_locator)?;
    let expected_event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .map_err(invalid_locator)?;
    if expected_event_id != request.event_id() {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "AstrBot platform-message native key does not derive the requested event identity",
        ));
    }
    verify_record_digest(
        request.locator(),
        text.as_bytes(),
        "platform-message display text",
    )?;
    Ok(text.into_bytes())
}

fn load_checkpoint_links(
    conn: &rusqlite::Connection,
    sql: &AstrBotSql,
) -> std::result::Result<BTreeMap<String, PlatformMessageLink>, HydrationFailure> {
    let mut links = BTreeMap::new();
    let mut after = None;
    while let Some(candidate) = fetch_candidate(
        conn,
        &sql.conversation_candidate_initial,
        &sql.conversation_candidate_after,
        after,
    )
    .map_err(map_capture_hydration)?
    {
        after = Some(candidate.physical_rowid);
        if candidate.observed_bytes().map_err(map_capture_hydration)?
            > u64::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).unwrap_or(u64::MAX)
        {
            continue;
        }
        let row = hydrate_conversation(conn, &sql.conversation_hydration, candidate.physical_rowid)
            .map_err(map_capture_hydration)?;
        let provider_session_id = provider_session_id(&row);
        for item in conversation_items(&row.content).0 {
            if let Some(checkpoint) = checkpoint_id(&item) {
                links.insert(
                    checkpoint,
                    PlatformMessageLink {
                        provider_session_id: provider_session_id.clone(),
                        parent_created_at: row.created_at,
                    },
                );
            }
        }
    }
    Ok(links)
}

fn verify_record_digest(
    locator: &SourceRecordLocator,
    provider_bytes: &[u8],
    label: &str,
) -> std::result::Result<(), HydrationFailure> {
    let digest: [u8; 32] = Sha256::digest(provider_bytes).into();
    if &digest == locator.record_digest() {
        Ok(())
    } else {
        Err(hydration_failure(
            HydrationFailureKind::StaleRecordEvidence,
            format!("AstrBot {label} digest no longer matches"),
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
            "AstrBot source changed during reopening",
        ),
        CaptureError::Sqlite(rusqlite::Error::QueryReturnedNoRows) => hydration_failure(
            HydrationFailureKind::MissingRecord,
            "AstrBot source record is missing",
        ),
        CaptureError::UnsupportedSchema(_) | CaptureError::UnsupportedSchemaVersion(_) => {
            hydration_failure(
                HydrationFailureKind::UnsupportedParserRevision,
                "AstrBot SQLite schema is unsupported",
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
            "AstrBot SQLite row is malformed for the certified parser",
        ),
        _ => hydration_failure(
            HydrationFailureKind::TemporarilyUnavailable,
            "AstrBot source could not be reopened",
        ),
    }
}

fn map_parser_hydration(error: CaptureError) -> HydrationFailure {
    match error {
        CaptureError::InvalidPayload(_)
        | CaptureError::UnsupportedSchema(_)
        | CaptureError::UnsupportedSchemaVersion(_) => hydration_failure(
            HydrationFailureKind::UnsupportedParserRevision,
            "AstrBot SQLite schema is unsupported",
        ),
        error => map_capture_hydration(error),
    }
}

fn map_sqlite_hydration(error: SqliteSourceAccessError) -> HydrationFailure {
    match error {
        SqliteSourceAccessError::SourceChanged
        | SqliteSourceAccessError::ConnectionIdentityMismatch => hydration_failure(
            HydrationFailureKind::StaleSourceEvidence,
            "AstrBot SQLite source changed during reopening",
        ),
        SqliteSourceAccessError::Io { source, .. }
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            hydration_failure(
                HydrationFailureKind::ConfirmedDeleted,
                "AstrBot database leaf is absent beneath the admitted source root",
            )
        }
        error => hydration_failure(
            HydrationFailureKind::TemporarilyUnavailable,
            error.to_string(),
        ),
    }
}

fn map_source_hydration(error: AstrBotSourceBackedErrorV0) -> HydrationFailure {
    match error {
        AstrBotSourceBackedErrorV0::Capture(error) => map_capture_hydration(error),
        AstrBotSourceBackedErrorV0::SqliteSource(error) => map_sqlite_hydration(error),
        AstrBotSourceBackedErrorV0::Projection(error) => invalid_locator(error),
        AstrBotSourceBackedErrorV0::Resolver(error) => invalid_locator(error),
        AstrBotSourceBackedErrorV0::Index(error) => invalid_locator(error),
        error => hydration_failure(
            HydrationFailureKind::TemporarilyUnavailable,
            error.to_string(),
        ),
    }
}
