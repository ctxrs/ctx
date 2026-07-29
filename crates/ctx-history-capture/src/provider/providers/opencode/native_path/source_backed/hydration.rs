use std::{collections::BTreeMap, path::PathBuf};

use ctx_history_core::{
    ContentSourceResolver, EventHydrationRequest, HydratedProviderRecord, HydrationFailure,
    HydrationFailureKind, LocatorRevisionPolicy, NativeRecordCoordinate, SessionHydrationRequest,
    SourceKey, SourceRecordLocator, TypedKey,
};
use rusqlite::{params, Connection};

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

/// Exact-row resolver bound to one already-discovered provider database.
#[derive(Debug)]
pub(crate) struct OpenCodeSourceBackedExactResolver {
    registration: OpenCodeSourceBackedRegistration,
    path: PathBuf,
}

impl OpenCodeSourceBackedExactResolver {
    pub(super) fn new(
        registration: OpenCodeSourceBackedRegistration,
        path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            registration,
            path: path.into(),
        }
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

    fn hydrate_session(
        &self,
        request: &SessionHydrationRequest,
    ) -> std::result::Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        let locators = request
            .events()
            .iter()
            .map(EventHydrationRequest::locator)
            .collect::<Vec<_>>();
        let provider_bytes = self.hydrate_locators(&locators)?;
        Ok(request
            .events()
            .iter()
            .zip(provider_bytes)
            .map(|(event, provider_bytes)| HydratedProviderRecord {
                event_id: event.event_id(),
                provider_bytes,
            })
            .collect())
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
            locators
                .iter()
                .map(|locator| {
                    hydrate_exact_row(connection, self.registration.dialect, &schema, locator)
                })
                .collect()
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

fn hydrate_exact_row(
    connection: &Connection,
    dialect: &OpenCodeSqliteDialect,
    schema: &OpenCodeNativeSchema,
    locator: &SourceRecordLocator,
) -> std::result::Result<Vec<u8>, HydrationFailure> {
    let NativeRecordCoordinate::ProviderSqlite {
        logical_relation,
        primary_key,
        row_version,
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

    let mut sql = source_backed_event_sql(schema);
    let alias = if schema.family == OpenCodeNativeSchemaFamily::MessagePart {
        "p"
    } else {
        "x"
    };
    sql.push_str(&format!(" where {alias}.id = ?2 limit 2"));
    let max_json_bytes = i64::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).map_err(|_| {
        hydration_failure(
            HydrationFailureKind::UnsupportedParserRevision,
            "provider SQLite value limit is unrepresentable",
        )
    })?;
    let mut statement = connection
        .prepare(&sql)
        .map_err(temporary_hydration_failure)?;
    let mut rows = statement
        .query(params![max_json_bytes, native_identity])
        .map_err(temporary_hydration_failure)?;
    let row = rows
        .next()
        .map_err(temporary_hydration_failure)?
        .ok_or_else(|| {
            hydration_failure(
                HydrationFailureKind::MissingRecord,
                "provider SQLite row no longer exists",
            )
        })?;
    let event = decode_source_event_row(row, schema, dialect).map_err(|error| {
        hydration_failure(HydrationFailureKind::StaleRecordEvidence, error.to_string())
    })?;
    if &event.native_identity != native_identity {
        return Err(hydration_failure(
            HydrationFailureKind::StaleRecordEvidence,
            "provider SQLite native row key no longer matches",
        ));
    }
    let record_digest = source_event_row_digest(&event);
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
