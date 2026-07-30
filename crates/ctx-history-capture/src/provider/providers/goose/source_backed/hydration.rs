use std::collections::{BTreeMap, BTreeSet};

use ctx_history_core::{
    BatchHydrationRequest, BatchHydrationResult, ContentSourceResolver, EventHydrationRequest,
    HydratedProviderRecord, HydrationFailure, HydrationFailureKind, LocatorRevisionPolicy,
    NativeRecordCoordinate, SessionHydrationRequest, SourceKey, SourceRecordLocator, TypedKey,
};
use rusqlite::{params_from_iter, Connection};
use serde_json::Value;

use super::{
    goose_source_key, GooseSourceBackedErrorV0, GooseSourceBackedResultV0,
    GooseSourceBackedSelectionV0, RetainedGooseDirectory, GOOSE_LOGICAL_RELATION,
};
use crate::{
    native_source::NativeSqliteValue,
    provider::{
        providers::goose::{
            content::goose_message_record_digest,
            normalization::{
                goose_complete_content_text, goose_output_projection,
                normalize_goose_native_message, normalize_goose_native_output_diagnostic,
            },
            position::decode_goose_message_locator,
            schema::{self, GooseNativeSchema},
            stream::{
                goose_native_message_identity, goose_normalized_message_id_sql,
                GooseMessageCellDisposition, GooseRetainedContentClass, GooseRetainedMessage,
                GooseScannedMessage,
            },
        },
        source_backed::hydration_failure,
    },
    CaptureError, OutputOutcome,
};

const GOOSE_HYDRATION_KEY_BATCH: usize = 256;
const GOOSE_MAX_MESSAGE_ID_BYTES: usize = 1_024;

#[derive(Clone, Debug)]
pub(crate) struct GooseSourceBackedResolverV0 {
    pub(super) selection: GooseSourceBackedSelectionV0,
    pub(super) source: SourceKey,
}

impl GooseSourceBackedResolverV0 {
    pub(crate) fn new(selection: GooseSourceBackedSelectionV0) -> GooseSourceBackedResultV0<Self> {
        Ok(Self {
            selection,
            source: goose_source_key()?,
        })
    }

    pub(super) fn hydrate_batch(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        let coordinates = request
            .events()
            .iter()
            .map(|event| validate_goose_locator(&self.source, event.locator()))
            .collect::<Result<Vec<_>, _>>()?;
        let keys = coordinates
            .iter()
            .map(|coordinate| coordinate.native_id)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let mut bodies = vec![None; coordinates.len()];
        let mut stale = vec![false; coordinates.len()];
        let mut opened_route = false;

        for route in self.selection.routes() {
            if bodies.iter().all(Option::is_some) {
                break;
            }
            let retained = match RetainedGooseDirectory::open(route.selected_database()) {
                Ok(retained) => retained,
                Err(_) => continue,
            };
            let snapshot = match retained.open_snapshot() {
                Ok(Some(snapshot)) => snapshot,
                Ok(None) | Err(_) => continue,
            };
            let loaded = (|| {
                let connection = snapshot.connection().map_err(goose_unavailable)?;
                let schema = GooseNativeSchema::probe(connection).map_err(goose_unavailable)?;
                load_hydration_rows(connection, &schema, &keys).map_err(goose_unavailable)
            })();
            snapshot.finish().map_err(goose_unavailable)?;
            retained.revalidate().map_err(goose_unavailable)?;
            let rows = loaded?;
            opened_route = true;
            for (index, coordinate) in coordinates.iter().enumerate() {
                if bodies[index].is_some() {
                    continue;
                }
                let Some(row) = rows.get(&coordinate.native_id) else {
                    continue;
                };
                let digest = goose_message_record_digest(&row.values).map_err(goose_stale)?;
                if digest != coordinate.record_digest {
                    stale[index] = true;
                    continue;
                }
                match hydrated_goose_body(row, digest) {
                    Ok(body) => bodies[index] = Some(body),
                    Err(_) => stale[index] = true,
                }
            }
        }

        if !opened_route {
            return Err(goose_unavailable(
                "the selected and explicitly retained Goose routes are unavailable",
            ));
        }
        let records = request
            .events()
            .iter()
            .enumerate()
            .map(|(index, event)| {
                let provider_bytes = bodies[index].take().ok_or_else(|| {
                    if stale[index] {
                        goose_stale("Goose native message evidence changed")
                    } else {
                        hydration_failure(
                            HydrationFailureKind::MissingRecord,
                            "Goose native message is missing",
                        )
                    }
                })?;
                Ok(HydratedProviderRecord {
                    event_id: event.event_id(),
                    provider_bytes,
                })
            })
            .collect::<Result<Vec<_>, HydrationFailure>>()?;
        let result = BatchHydrationResult::new(records).map_err(goose_invalid)?;
        result.validate_for_request(request)?;
        Ok(result)
    }
}

impl ContentSourceResolver for GooseSourceBackedResolverV0 {
    fn hydrate_event(
        &self,
        request: &EventHydrationRequest,
    ) -> Result<HydratedProviderRecord, HydrationFailure> {
        let batch = BatchHydrationRequest::new(vec![request.clone()]).map_err(goose_invalid)?;
        self.hydrate_batch(&batch)?
            .into_records()
            .into_iter()
            .next()
            .ok_or_else(|| goose_invalid("Goose one-record hydration returned no record"))
    }

    fn hydrate_session(
        &self,
        request: &SessionHydrationRequest,
    ) -> Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        let batch = BatchHydrationRequest::new(request.events().to_vec()).map_err(goose_invalid)?;
        Ok(self.hydrate_batch(&batch)?.into_records())
    }
}

struct GooseHydrationCoordinate {
    native_id: i64,
    record_digest: [u8; 32],
}

fn validate_goose_locator(
    source: &SourceKey,
    locator: &SourceRecordLocator,
) -> Result<GooseHydrationCoordinate, HydrationFailure> {
    locator.validate_contract().map_err(goose_invalid)?;
    if !source.exact_descriptor_eq(locator.source())
        || locator.revision_policy() != LocatorRevisionPolicy::StableRecordEvidence
        || locator.certified_source_revision_digest().is_some()
    {
        return Err(goose_invalid(
            "locator does not identify the stable Goose source",
        ));
    }
    let NativeRecordCoordinate::ProviderSqlite {
        logical_relation,
        primary_key,
        row_version,
    } = locator.coordinate()
    else {
        return Err(goose_invalid("Goose locator is not a SQLite coordinate"));
    };
    let (TypedKey::Bytes(primary_key), Some(TypedKey::Bytes(row_version))) =
        (primary_key, row_version)
    else {
        return Err(goose_invalid(
            "Goose locator has invalid native-row evidence",
        ));
    };
    if logical_relation != GOOSE_LOGICAL_RELATION
        || row_version.as_slice() != locator.record_digest()
    {
        return Err(goose_invalid(
            "Goose locator native-row evidence is inconsistent",
        ));
    }
    let native_id = decode_goose_message_locator(primary_key)
        .ok_or_else(|| goose_invalid("Goose native message coordinate is invalid"))?;
    Ok(GooseHydrationCoordinate {
        native_id,
        record_digest: *locator.record_digest(),
    })
}

struct GooseHydrationRow {
    values: Vec<NativeSqliteValue>,
    message_id_uses: i64,
}

fn load_hydration_rows(
    connection: &Connection,
    schema: &GooseNativeSchema,
    keys: &[i64],
) -> GooseSourceBackedResultV0<BTreeMap<i64, GooseHydrationRow>> {
    let columns = schema::goose_message_columns(connection)?;
    let expressions = schema::goose_message_expressions(&columns, "m");
    let select = expressions.hydration.join(", ");
    let current_id = goose_normalized_message_id_sql(&schema.message_id_expression("m"));
    let candidate_id = goose_normalized_message_id_sql(&schema.message_id_expression("candidate"));
    let mut loaded = BTreeMap::new();
    for batch in keys.chunks(GOOSE_HYDRATION_KEY_BATCH) {
        let placeholders = std::iter::repeat_n("?", batch.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "select s.rowid, {select},
                    case when {current_id} is null then 0 else (
                        select count(*) from messages candidate
                        where {candidate_id} = {current_id}
                    ) end
             from messages m
             left join sessions s on s.id = m.session_id
             where m.id in ({placeholders})
             order by m.id"
        );
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(batch.iter()), |row| {
            let mut values = vec![row
                .get::<_, Option<i64>>(0)?
                .map_or(NativeSqliteValue::Null, NativeSqliteValue::Integer)];
            values.extend(schema::goose_message_values_at(row, 1)?);
            let native_id = match values.get(2) {
                Some(NativeSqliteValue::Integer(native_id)) => *native_id,
                _ => {
                    return Err(rusqlite::Error::InvalidColumnType(
                        2,
                        "messages.id".to_owned(),
                        rusqlite::types::Type::Integer,
                    ));
                }
            };
            Ok((
                native_id,
                GooseHydrationRow {
                    values,
                    message_id_uses: row.get(11)?,
                },
            ))
        })?;
        for row in rows {
            let (native_id, row) = row?;
            if loaded.insert(native_id, row).is_some() {
                return Err(GooseSourceBackedErrorV0::InvalidLocator);
            }
        }
    }
    Ok(loaded)
}

fn hydrated_goose_body(
    row: &GooseHydrationRow,
    digest: [u8; 32],
) -> GooseSourceBackedResultV0<Vec<u8>> {
    let (_, message) = schema::decode_goose_message_record(&row.values)?;
    let native_message_id = message
        .message_id
        .as_deref()
        .filter(|value| value.len() <= GOOSE_MAX_MESSAGE_ID_BYTES)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let identity =
        goose_native_message_identity(native_message_id, row.message_id_uses, message.id);
    let content: Value = serde_json::from_str(&message.content_json).map_err(|error| {
        CaptureError::InvalidPayload(format!("Goose native message is malformed: {error}"))
    })?;
    let blocks = content.as_array().ok_or_else(|| {
        CaptureError::InvalidPayload("Goose native message is no longer an array".to_owned())
    })?;
    let direct_types = blocks
        .iter()
        .filter_map(Value::as_object)
        .filter_map(|block| block.get("type"))
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    if direct_types.contains(&"toolResponse") {
        let outcome = goose_output_projection(&content).outcome.outcome;
        if !matches!(outcome, OutputOutcome::Failure | OutputOutcome::Timeout) {
            return Err(GooseSourceBackedErrorV0::InvalidLocator);
        }
        let scanned = GooseScannedMessage {
            sqlite_rowid: message.id,
            native_order: message.id,
            native_identity: identity.native_identity,
            provider_message_identity: identity.provider_message_identity,
            identity_degraded: identity.identity_degraded,
            session_identity: message.session_id,
            role: message.role,
            disposition: if outcome == OutputOutcome::Timeout {
                GooseMessageCellDisposition::OutputTimeout
            } else {
                GooseMessageCellDisposition::OutputFailure
            },
            output_outcome: Some(outcome),
            retained_class: None,
            content_bytes: message.content_json.len() as u64,
            content_json: Some(message.content_json),
            created_timestamp: message.created_timestamp,
            timestamp: message.timestamp,
            tokens_json: message.tokens,
            metadata_json: message.metadata_json,
            logical_row_digest: Some(digest),
        };
        return Ok(normalize_goose_native_output_diagnostic(&scanned)?
            .searchable_text
            .into_bytes());
    }
    let retained_class = if direct_types
        .iter()
        .any(|kind| matches!(*kind, "toolRequest" | "frontendToolRequest"))
    {
        GooseRetainedContentClass::ToolCall
    } else {
        GooseRetainedContentClass::Message
    };
    let event = normalize_goose_native_message(GooseRetainedMessage {
        sqlite_rowid: message.id,
        native_order: message.id,
        native_identity: identity.native_identity,
        provider_message_identity: identity.provider_message_identity,
        identity_degraded: identity.identity_degraded,
        session_identity: message.session_id,
        role: message.role,
        retained_class,
        content_bytes: message.content_json.len() as u64,
        content_json: message.content_json,
        created_timestamp: message.created_timestamp,
        timestamp: message.timestamp,
        tokens_json: message.tokens,
        metadata_json: message.metadata_json,
        logical_row_digest: digest,
    })?;
    Ok(goose_complete_content_text(&event.content)
        .unwrap_or(event.searchable_text)
        .into_bytes())
}

fn goose_invalid(detail: impl std::fmt::Display) -> HydrationFailure {
    hydration_failure(HydrationFailureKind::InvalidLocator, detail)
}

fn goose_stale(detail: impl std::fmt::Display) -> HydrationFailure {
    hydration_failure(HydrationFailureKind::StaleRecordEvidence, detail)
}

fn goose_unavailable(detail: impl std::fmt::Display) -> HydrationFailure {
    hydration_failure(HydrationFailureKind::TemporarilyUnavailable, detail)
}
