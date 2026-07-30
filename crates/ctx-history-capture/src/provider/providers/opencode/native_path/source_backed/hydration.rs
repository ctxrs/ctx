use std::{collections::BTreeMap, path::PathBuf};

#[cfg(test)]
use std::cell::Cell;

use ctx_history_core::{
    BatchHydrationRequest, BatchHydrationResult, ContentSourceResolver, EventHydrationRequest,
    HydratedProviderRecord, HydrationFailure, HydrationFailureKind, LocatorRevisionPolicy,
    NativeRecordCoordinate, SessionHydrationRequest, SourceKey, SourceRecordLocator, TypedKey,
};
use rusqlite::{types::Value, Connection};

use super::{
    open_root_authorized_snapshot,
    projection::{
        decode_source_event_row, retained_projection, source_backed_retained_event_kind,
        source_backed_retained_searchable_text,
    },
    source_event_row_digest, source_key, OpenCodeSourceBackedError,
    OpenCodeSourceBackedRegistration, OpenCodeSourceBackedResult,
};
use crate::{
    provider::providers::opencode::{
        native_path::{
            json::register_projection_function,
            model::OpenCodeNativeSchemaFamily,
            query::{source_backed_event_digest, source_backed_event_sql},
            schema::OpenCodeNativeSchema,
        },
        OpenCodeSqliteDialect,
    },
    CaptureError, MAX_PROVIDER_SQLITE_VALUE_BYTES,
};

pub(super) const HYDRATION_NATIVE_KEY_BATCH: usize = 256;

/// Exact-row resolver bound to one already-discovered provider database.
#[derive(Debug)]
pub(crate) struct OpenCodeSourceBackedExactResolver {
    registration: OpenCodeSourceBackedRegistration,
    path: PathBuf,
    #[cfg(test)]
    snapshot_opens: Cell<u64>,
    #[cfg(test)]
    native_key_batches: Cell<u64>,
}

impl OpenCodeSourceBackedExactResolver {
    pub(super) fn new(
        registration: OpenCodeSourceBackedRegistration,
        path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            registration,
            path: path.into(),
            #[cfg(test)]
            snapshot_opens: Cell::new(0),
            #[cfg(test)]
            native_key_batches: Cell::new(0),
        }
    }

    #[cfg(test)]
    pub(super) fn hydration_counters(&self) -> (u64, u64) {
        (self.snapshot_opens.get(), self.native_key_batches.get())
    }
}

impl ContentSourceResolver for OpenCodeSourceBackedExactResolver {
    fn hydrate_event(
        &self,
        request: &EventHydrationRequest,
    ) -> std::result::Result<HydratedProviderRecord, HydrationFailure> {
        let provider_bytes = self.hydrate_locator(request.locator())?;
        Ok(HydratedProviderRecord {
            event_id: request.event_id(),
            provider_bytes,
        })
    }

    fn hydrate_batch(
        &self,
        request: &BatchHydrationRequest,
    ) -> std::result::Result<BatchHydrationResult, HydrationFailure> {
        let locators = request
            .events()
            .iter()
            .map(EventHydrationRequest::locator)
            .collect::<Vec<_>>();
        let provider_bytes = self.hydrate_locators(&locators)?;
        let records = request
            .events()
            .iter()
            .zip(provider_bytes)
            .map(|(event, provider_bytes)| HydratedProviderRecord {
                event_id: event.event_id(),
                provider_bytes,
            })
            .collect();
        let result = BatchHydrationResult::new(records).map_err(|error| {
            hydration_failure(
                HydrationFailureKind::InvalidLocator,
                format!("invalid OpenCode-family batch hydration result: {error}"),
            )
        })?;
        result.validate_for_request(request)?;
        Ok(result)
    }

    fn hydrate_session(
        &self,
        request: &SessionHydrationRequest,
    ) -> std::result::Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        self.hydrate_batch(request.batch())
            .map(BatchHydrationResult::into_records)
    }
}

impl OpenCodeSourceBackedExactResolver {
    fn hydrate_locator(
        &self,
        locator: &SourceRecordLocator,
    ) -> std::result::Result<Vec<u8>, HydrationFailure> {
        self.hydrate_locators(&[locator])?
            .into_iter()
            .next()
            .ok_or_else(|| {
                hydration_failure(
                    HydrationFailureKind::InvalidLocator,
                    "OpenCode-family hydration request contained no locator",
                )
            })
    }

    fn hydrate_locators(
        &self,
        locators: &[&SourceRecordLocator],
    ) -> std::result::Result<Vec<Vec<u8>>, HydrationFailure> {
        if locators.is_empty() {
            return Ok(Vec::new());
        }
        for locator in locators {
            self.validate_locator(locator)?;
        }
        let family = locator_schema_family(self.registration.dialect, locators[0].source())
            .ok_or_else(|| {
                hydration_failure(
                    HydrationFailureKind::InvalidLocator,
                    "locator has an unsupported OpenCode-family schema descriptor",
                )
            })?;
        let (source_root, sqlite_snapshot) =
            open_root_authorized_snapshot(&self.path).map_err(temporary_hydration_failure)?;
        #[cfg(test)]
        self.snapshot_opens
            .set(self.snapshot_opens.get().saturating_add(1));
        let resolved = (|| {
            let connection = sqlite_snapshot
                .connection()
                .map_err(temporary_hydration_failure)?;
            register_projection_function(connection, self.registration.dialect)
                .map_err(temporary_hydration_failure)?;
            let schema = probe_hydration_schema(connection, family).map_err(|error| {
                hydration_failure(
                    HydrationFailureKind::UnsupportedParserRevision,
                    error.to_string(),
                )
            })?;
            let current_source =
                source_key(self.registration.dialect, schema.family).map_err(|error| {
                    hydration_failure(HydrationFailureKind::InvalidLocator, error.to_string())
                })?;
            if locators
                .iter()
                .any(|locator| !current_source.exact_descriptor_eq(locator.source()))
            {
                return Err(hydration_failure(
                    HydrationFailureKind::StaleSourceEvidence,
                    "provider SQLite schema family no longer matches the certified source",
                ));
            }
            let (provider_bytes, _native_key_batches) =
                hydrate_exact_rows(connection, self.registration.dialect, &schema, locators)?;
            #[cfg(test)]
            self.native_key_batches.set(
                self.native_key_batches
                    .get()
                    .saturating_add(_native_key_batches),
            );
            Ok(provider_bytes)
        })();
        let snapshot_finish = sqlite_snapshot.finish();
        let root_finish = source_root.revalidate();
        snapshot_finish.map_err(temporary_hydration_failure)?;
        root_finish.map_err(temporary_hydration_failure)?;
        resolved
    }

    fn validate_locator(
        &self,
        locator: &SourceRecordLocator,
    ) -> std::result::Result<(), HydrationFailure> {
        locator.validate_contract().map_err(|error| {
            hydration_failure(HydrationFailureKind::InvalidLocator, error.to_string())
        })?;
        if locator.source().provider() != self.registration.provider().as_str()
            || locator.source().source_format() != self.registration.source_format()
            || locator.revision_policy() != LocatorRevisionPolicy::StableRecordEvidence
            || locator.certified_source_revision_digest().is_some()
            || locator_schema_family(self.registration.dialect, locator.source()).is_none()
        {
            return Err(hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "locator does not belong to this OpenCode-family registration",
            ));
        }
        Ok(())
    }
}

#[derive(Debug)]
struct HydrationColumnCapability {
    declared_type: String,
    primary_key_ordinal: i64,
}

/// Selects and structurally validates the current schema family without
/// scanning provider rows. Exact hydration validates only the addressed row;
/// the generation scan remains responsible for corpus-wide admission.
fn probe_hydration_schema(
    connection: &Connection,
    family: OpenCodeNativeSchemaFamily,
) -> OpenCodeSourceBackedResult<OpenCodeNativeSchema> {
    let session_columns = hydration_table_capabilities(connection, "session")?;
    validate_hydration_identity_column(&session_columns, "session", "id")?;
    validate_hydration_column(&session_columns, "session", "time_created", "INTEGER")?;
    validate_hydration_column(&session_columns, "session", "time_updated", "INTEGER")?;
    let event_columns = hydration_table_capabilities(connection, family.event_table())?;
    let event_has_type = event_columns.contains_key("type");
    validate_hydration_identity_column(&event_columns, family.event_table(), "id")?;
    validate_hydration_column(&event_columns, family.event_table(), "session_id", "TEXT")?;
    validate_hydration_column(
        &event_columns,
        family.event_table(),
        "time_created",
        "INTEGER",
    )?;
    validate_hydration_column(
        &event_columns,
        family.event_table(),
        "time_updated",
        "INTEGER",
    )?;
    validate_hydration_column(&event_columns, family.event_table(), "data", "TEXT")?;
    if family == OpenCodeNativeSchemaFamily::SessionMessageSeq {
        validate_hydration_column(&event_columns, "session_message", "seq", "INTEGER")?;
    }
    if family == OpenCodeNativeSchemaFamily::MessagePart {
        validate_hydration_column(&event_columns, "part", "message_id", "TEXT")?;
        let message_columns = hydration_table_capabilities(connection, "message")?;
        validate_hydration_identity_column(&message_columns, "message", "id")?;
        validate_hydration_column(&message_columns, "message", "session_id", "TEXT")?;
        validate_hydration_column(&message_columns, "message", "time_created", "INTEGER")?;
        validate_hydration_column(&message_columns, "message", "time_updated", "INTEGER")?;
        validate_hydration_column(&message_columns, "message", "data", "TEXT")?;
    }

    let user_version = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let schema_version = connection.pragma_query_value(None, "schema_version", |row| row.get(0))?;
    Ok(OpenCodeNativeSchema {
        family,
        capability_digest: String::new(),
        user_version,
        schema_version,
        session_columns: session_columns.keys().cloned().collect(),
        event_has_type,
    })
}

fn locator_schema_family(
    dialect: &OpenCodeSqliteDialect,
    source: &SourceKey,
) -> Option<OpenCodeNativeSchemaFamily> {
    [
        OpenCodeNativeSchemaFamily::SessionMessageSeq,
        OpenCodeNativeSchemaFamily::SessionMessageSynthesizedSeq,
        OpenCodeNativeSchemaFamily::SessionEntry,
        OpenCodeNativeSchemaFamily::LegacyMessage,
        OpenCodeNativeSchemaFamily::MessagePart,
    ]
    .into_iter()
    .find(|family| {
        source_key(dialect, *family).is_ok_and(|candidate| candidate.exact_descriptor_eq(source))
    })
}

fn hydration_table_capabilities(
    connection: &Connection,
    table: &str,
) -> OpenCodeSourceBackedResult<BTreeMap<String, HydrationColumnCapability>> {
    let sql = format!("pragma table_info(\"{}\")", table.replace('"', "\"\""));
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(1)?,
            HydrationColumnCapability {
                declared_type: row.get::<_, String>(2)?.trim().to_ascii_uppercase(),
                primary_key_ordinal: row.get(5)?,
            },
        ))
    })?;
    rows.collect::<std::result::Result<BTreeMap<_, _>, _>>()
        .map_err(OpenCodeSourceBackedError::from)
}

fn validate_hydration_identity_column(
    columns: &BTreeMap<String, HydrationColumnCapability>,
    table: &str,
    column: &str,
) -> OpenCodeSourceBackedResult<()> {
    let capability = validate_hydration_column(columns, table, column, "TEXT")?;
    if capability.primary_key_ordinal != 1 {
        return Err(CaptureError::InvalidPayload(format!(
            "OpenCode NativePath {table}.{column} is no longer the primary identity"
        ))
        .into());
    }
    Ok(())
}

fn validate_hydration_column<'a>(
    columns: &'a BTreeMap<String, HydrationColumnCapability>,
    table: &str,
    column: &str,
    declared_type: &str,
) -> OpenCodeSourceBackedResult<&'a HydrationColumnCapability> {
    let capability = columns.get(column).ok_or_else(|| {
        CaptureError::InvalidPayload(format!(
            "OpenCode NativePath {table} is missing required column {column}"
        ))
    })?;
    if capability.declared_type != declared_type {
        return Err(CaptureError::InvalidPayload(format!(
            "OpenCode NativePath {table}.{column} changed type from {declared_type}"
        ))
        .into());
    }
    Ok(capability)
}

fn hydrate_exact_rows(
    connection: &Connection,
    dialect: &OpenCodeSqliteDialect,
    schema: &OpenCodeNativeSchema,
    locators: &[&SourceRecordLocator],
) -> std::result::Result<(Vec<Vec<u8>>, u64), HydrationFailure> {
    let mut positions_by_key = BTreeMap::<&str, Vec<usize>>::new();
    for (position, locator) in locators.iter().enumerate() {
        positions_by_key
            .entry(provider_sqlite_native_key(locator, schema)?)
            .or_default()
            .push(position);
    }
    let native_keys = positions_by_key.keys().copied().collect::<Vec<_>>();
    let mut hydrated = (0..locators.len())
        .map(|_| None)
        .collect::<Vec<Option<Vec<u8>>>>();
    let max_json_bytes = i64::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).map_err(|_| {
        hydration_failure(
            HydrationFailureKind::UnsupportedParserRevision,
            "provider SQLite value limit is unrepresentable",
        )
    })?;
    let alias = if schema.family == OpenCodeNativeSchemaFamily::MessagePart {
        "p"
    } else {
        "x"
    };
    let mut batch_count = 0_u64;

    for native_key_batch in native_keys.chunks(HYDRATION_NATIVE_KEY_BATCH) {
        let mut sql = source_backed_event_sql(schema);
        let placeholders = (2..native_key_batch.len() + 2)
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        sql.push_str(&format!(
            " where {alias}.id in ({placeholders}) order by {alias}.id"
        ));
        let mut parameters = Vec::with_capacity(native_key_batch.len() + 1);
        parameters.push(Value::Integer(max_json_bytes));
        parameters.extend(
            native_key_batch
                .iter()
                .map(|native_key| Value::Text((*native_key).to_owned())),
        );
        let mut statement = connection
            .prepare(&sql)
            .map_err(temporary_hydration_failure)?;
        let mut rows = statement
            .query(rusqlite::params_from_iter(parameters.iter()))
            .map_err(temporary_hydration_failure)?;
        while let Some(row) = rows.next().map_err(temporary_hydration_failure)? {
            let event = decode_source_event_row(row, schema, dialect).map_err(|error| {
                hydration_failure(HydrationFailureKind::StaleRecordEvidence, error.to_string())
            })?;
            let positions = positions_by_key
                .get(event.native_identity.as_str())
                .ok_or_else(|| {
                    hydration_failure(
                        HydrationFailureKind::StaleRecordEvidence,
                        "provider SQLite native-key batch returned an unrequested row",
                    )
                })?;
            for position in positions {
                let output = hydrated.get_mut(*position).ok_or_else(|| {
                    hydration_failure(
                        HydrationFailureKind::InvalidLocator,
                        "provider SQLite batch position is out of range",
                    )
                })?;
                if output.is_some() {
                    return Err(hydration_failure(
                        HydrationFailureKind::StaleRecordEvidence,
                        "provider SQLite native-key batch returned a duplicate row",
                    ));
                }
                *output = Some(validate_exact_event(&event, schema, locators[*position])?);
            }
        }
        batch_count = batch_count.saturating_add(1);
    }

    let hydrated = hydrated
        .into_iter()
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| {
            hydration_failure(
                HydrationFailureKind::MissingRecord,
                "one or more provider SQLite rows no longer exist",
            )
        })?;
    Ok((hydrated, batch_count))
}

fn provider_sqlite_native_key<'a>(
    locator: &'a SourceRecordLocator,
    schema: &OpenCodeNativeSchema,
) -> std::result::Result<&'a str, HydrationFailure> {
    let NativeRecordCoordinate::ProviderSqlite {
        logical_relation,
        primary_key,
        ..
    } = locator.coordinate()
    else {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "locator is not a provider SQLite coordinate",
        ));
    };
    let TypedKey::Utf8(native_identity) = primary_key else {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "OpenCode-family primary key is not typed UTF-8",
        ));
    };
    if logical_relation != schema.family.event_table() {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "OpenCode-family logical relation does not match the selected schema",
        ));
    }
    Ok(native_identity)
}

fn validate_exact_event(
    event: &super::SourceEventRow,
    schema: &OpenCodeNativeSchema,
    locator: &SourceRecordLocator,
) -> std::result::Result<Vec<u8>, HydrationFailure> {
    let NativeRecordCoordinate::ProviderSqlite {
        primary_key,
        row_version,
        ..
    } = locator.coordinate()
    else {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "locator is not a provider SQLite coordinate",
        ));
    };
    let TypedKey::Utf8(native_identity) = primary_key else {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "OpenCode-family primary key is not typed UTF-8",
        ));
    };
    if &event.native_identity != native_identity {
        return Err(hydration_failure(
            HydrationFailureKind::StaleRecordEvidence,
            "provider SQLite native row key no longer matches",
        ));
    }
    let record_digest = source_event_row_digest(event);
    if &record_digest != locator.record_digest() {
        return Err(hydration_failure(
            HydrationFailureKind::StaleRecordEvidence,
            "provider SQLite exact row digest no longer matches",
        ));
    }
    let retained = retained_projection(&event.projection).ok_or_else(|| {
        hydration_failure(
            HydrationFailureKind::StaleRecordEvidence,
            "provider SQLite row is no longer a retained lexical event",
        )
    })?;
    event.source_data.exact_text().ok_or_else(|| {
        hydration_failure(
            HydrationFailureKind::StaleRecordEvidence,
            "provider SQLite row is no longer stored as text",
        )
    })?;
    let normalized_time = retained
        .body
        .pointer("/time/created")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(event.time_created);
    let semantic_digest = source_backed_event_digest(
        schema.family,
        &event.native_identity,
        &event.native_order,
        normalized_time,
        event.time_updated,
        &retained,
    )
    .map_err(|error| {
        hydration_failure(HydrationFailureKind::StaleRecordEvidence, error.to_string())
    })?;
    let expected_version = TypedKey::composite(vec![
        TypedKey::I64(event.time_updated),
        TypedKey::utf8(semantic_digest).map_err(|error| {
            hydration_failure(HydrationFailureKind::InvalidLocator, error.to_string())
        })?,
    ])
    .map_err(|error| hydration_failure(HydrationFailureKind::InvalidLocator, error.to_string()))?;
    if row_version.as_ref() != Some(&expected_version) {
        return Err(hydration_failure(
            HydrationFailureKind::StaleRecordEvidence,
            "provider SQLite typed row version no longer matches",
        ));
    }
    let kind =
        source_backed_retained_event_kind(&retained.effective_type, &retained.role, &retained.body);
    let display_text =
        source_backed_retained_searchable_text(kind, &retained.effective_type, &retained.body);
    if display_text.is_empty() {
        Ok(b"OpenCode event".to_vec())
    } else {
        Ok(display_text.into_bytes())
    }
}

fn hydration_failure(kind: HydrationFailureKind, detail: impl Into<String>) -> HydrationFailure {
    HydrationFailure {
        kind,
        detail: detail.into(),
    }
}

fn temporary_hydration_failure(error: impl std::fmt::Display) -> HydrationFailure {
    hydration_failure(
        HydrationFailureKind::TemporarilyUnavailable,
        error.to_string(),
    )
}
