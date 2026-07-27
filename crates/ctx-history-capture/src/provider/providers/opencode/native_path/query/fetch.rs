use super::*;

pub(in super::super) fn fetch_session_page(
    conn: &Connection,
    keyset: SessionKeyset,
    limit: usize,
    metadata_byte_limit: usize,
) -> Result<Vec<ScannedSession>> {
    let maximum_prefix = keyset
        .metadata_prefix_bytes
        .checked_add(u64::try_from(metadata_byte_limit).map_err(|_| {
            CaptureError::SystemInvariant("OpenCode session metadata limit exceeds u64")
        })?)
        .ok_or(CaptureError::SystemInvariant(
            "OpenCode indexed session metadata prefix overflowed",
        ))?;
    let maximum_prefix = i64::try_from(maximum_prefix).map_err(|_| {
        CaptureError::InvalidPayload(
            "OpenCode indexed session metadata prefix exceeds SQLite integer".to_owned(),
        )
    })?;
    let mut statement = conn.prepare(
        "select scan_ordinal, metadata_prefix_bytes,
                native_identity, parent_identity, root_identity, title, directory,
                model_identity, agent_identity,
                time_created, time_updated, content_digest
         from ordered_sessions
         where scan_ordinal > ?1
           and metadata_prefix_bytes <= ?2
         order by scan_ordinal
         limit ?3",
    )?;
    let rows = statement.query_map(
        params![keyset.scan_ordinal, maximum_prefix, i64_limit(limit)?],
        |row| {
            let parent_identity: String = row.get(3)?;
            let root_identity: String = row.get(4)?;
            let title: String = row.get(5)?;
            let directory: String = row.get(6)?;
            let model_identity: String = row.get(7)?;
            let agent_identity: String = row.get(8)?;
            Ok(ScannedSession {
                scan_ordinal: row.get(0)?,
                metadata_prefix_bytes: sqlite_nonnegative_u64(row.get::<_, i64>(1)?)?,
                row: OpenCodeNativeSession {
                    native_identity: row.get(2)?,
                    parent_identity: nonempty(parent_identity),
                    root_identity,
                    title: nonempty(title),
                    directory: nonempty(directory),
                    model_identity: nonempty(model_identity),
                    agent_identity: nonempty(agent_identity),
                    time_created: row.get(9)?,
                    time_updated: row.get(10)?,
                    content_digest: row.get(11)?,
                },
            })
        },
    )?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)
}

pub(in super::super) fn fetch_event_metadata_page(
    conn: &Connection,
    keyset: EventKeyset,
    row_limit: usize,
    retained_byte_limit: usize,
    family: OpenCodeNativeSchemaFamily,
) -> Result<Vec<RecordMetadata>> {
    let maximum_ordinal = keyset
        .scan_ordinal
        .checked_add(i64_limit(row_limit)?)
        .ok_or(CaptureError::SystemInvariant(
            "OpenCode indexed event page ordinal overflowed",
        ))?;
    let maximum_prefix = keyset
        .retained_prefix_bytes
        .checked_add(u64::try_from(retained_byte_limit).map_err(|_| {
            CaptureError::SystemInvariant("OpenCode retained byte limit exceeds u64")
        })?)
        .ok_or(CaptureError::SystemInvariant(
            "OpenCode indexed retained byte prefix overflowed",
        ))?;
    let maximum_prefix = i64::try_from(maximum_prefix).map_err(|_| {
        CaptureError::InvalidPayload(
            "OpenCode indexed retained byte prefix exceeds SQLite integer".to_owned(),
        )
    })?;
    let mut statement = conn.prepare(
        "select scan_ordinal, retained_prefix_bytes, native_identity,
                message_identity, session_identity, source_rowid,
                native_ordinal, order_tag, order_a, order_b,
                time_created, time_updated, content_bytes, projection, content_digest
         from ordered_events
         where scan_ordinal > ?1
           and scan_ordinal <= ?2
           and retained_prefix_bytes <= ?3
         order by scan_ordinal",
    )?;
    let mut rows = statement.query(params![
        keyset.scan_ordinal,
        maximum_ordinal,
        maximum_prefix
    ])?;
    let mut records = Vec::new();
    while let Some(row) = rows.next()? {
        let source_rowid: i64 = row.get(5)?;
        let order_tag: i64 = row.get(7)?;
        let session_identity: String = row.get(4)?;
        let message_identity: String = row.get(3)?;
        let native_identity: String = row.get(2)?;
        let order_a: i64 = row.get(8)?;
        let order_b: i64 = row.get(9)?;
        let native_order = decode_order(
            order_tag,
            &session_identity,
            &message_identity,
            &native_identity,
            order_a,
            order_b,
        )?;
        let retained_prefix_bytes =
            sqlite_nonnegative_u64(row.get::<_, i64>(1)?).map_err(CaptureError::from)?;
        let _source_content_bytes =
            sqlite_nonnegative_u64(row.get::<_, i64>(12)?).map_err(CaptureError::from)?;
        let projection_bytes: Vec<u8> = row.get(13)?;
        let content_bytes = u64::try_from(projection_bytes.len()).map_err(|_| {
            CaptureError::SystemInvariant("OpenCode retained projection bytes exceed u64")
        })?;
        records.push(RecordMetadata {
            scan_ordinal: row.get(0)?,
            stable_native_ordinal: stable_native_event_index(
                &session_identity,
                &native_record_identity(family, &message_identity, &native_identity),
            ),
            legacy_native_ordinal: sqlite_nonnegative_u64(row.get::<_, i64>(6)?)?,
            source_record_ordinal: ordered_source_rowid(source_rowid),
            retained_prefix_bytes,
            native_identity,
            message_identity,
            source_session_identity: session_identity,
            native_order,
            time_created: row.get(10)?,
            time_updated: row.get(11)?,
            content_bytes,
            content_digest: row.get(14)?,
            projection: decode_projection(&projection_bytes)?,
            locator: native_locator(native_shape_from_family(family), source_rowid)?,
        });
    }
    Ok(records)
}

pub(in super::super) fn fetch_pro_metadata_page(
    conn: &Connection,
    keyset: ProKeyset,
    row_limit: usize,
    byte_limit: usize,
    family: OpenCodeNativeSchemaFamily,
) -> Result<Vec<ProRecordMetadata>> {
    let maximum_ordinal = keyset
        .pro_ordinal
        .checked_add(i64_limit(row_limit)?)
        .ok_or(CaptureError::SystemInvariant(
            "OpenCode Pro page ordinal overflowed",
        ))?;
    let maximum_prefix = keyset
        .output_prefix_bytes
        .checked_add(u64::try_from(byte_limit).map_err(|_| {
            CaptureError::SystemInvariant("OpenCode Pro page byte limit exceeds u64")
        })?)
        .ok_or(CaptureError::SystemInvariant(
            "OpenCode Pro output prefix overflowed",
        ))?;
    let mut statement = conn.prepare(
        "select p.pro_ordinal, p.output_prefix_bytes, p.source_event_ordinal,
                p.subrecord_index, p.native_identity, e.message_identity,
                e.session_identity, s.parent_identity, s.root_identity,
                s.directory,
                s.agent_identity, e.time_created, p.kind, p.call_id, p.tool_name, p.command,
                p.working_directory, p.outcome, p.exit_code, p.duration_ms,
                p.content, p.rejection, e.source_rowid, e.native_ordinal,
                p.unit_bytes
         from ordered_pro_units p
         join ordered_events e on e.scan_ordinal = p.source_event_ordinal
         join ordered_sessions s on s.native_identity = e.session_identity
         where p.pro_ordinal > ?1
           and p.pro_ordinal <= ?2
           and p.output_prefix_bytes <= ?3
         order by p.pro_ordinal",
    )?;
    let rows = statement.query_map(
        params![
            keyset.pro_ordinal,
            maximum_ordinal,
            i64_from_u64(maximum_prefix, "OpenCode Pro prefix bytes")?,
        ],
        |row| {
            let parent: String = row.get(7)?;
            let directory: String = row.get(9)?;
            let agent_identity: String = row.get(10)?;
            let rejection: Option<String> = row.get(21)?;
            let draft = if rejection.is_some() {
                None
            } else {
                Some(OpenCodeOutputDraft {
                    subrecord_index: u32::try_from(row.get::<_, i64>(3)?).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            3,
                            rusqlite::types::Type::Integer,
                            Box::new(error),
                        )
                    })?,
                    kind: u8::try_from(row.get::<_, i64>(12)?).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            12,
                            rusqlite::types::Type::Integer,
                            Box::new(error),
                        )
                    })?,
                    call_id: row.get(13)?,
                    tool_name: row.get(14)?,
                    command: row.get(15)?,
                    working_directory: row.get(16)?,
                    outcome: u8::try_from(row.get::<_, i64>(17)?).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            17,
                            rusqlite::types::Type::Integer,
                            Box::new(error),
                        )
                    })?,
                    exit_code: row.get(18)?,
                    duration_ms: row
                        .get::<_, Option<i64>>(19)?
                        .map(u64::try_from)
                        .transpose()
                        .map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                19,
                                rusqlite::types::Type::Integer,
                                Box::new(error),
                            )
                        })?,
                    content: String::from_utf8(row.get::<_, Vec<u8>>(20)?).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            20,
                            rusqlite::types::Type::Blob,
                            Box::new(error),
                        )
                    })?,
                })
            };
            let source_rowid: i64 = row.get(22)?;
            let native_identity: String = row.get(4)?;
            let message_identity: String = row.get(5)?;
            let source_native_identity = if family == OpenCodeNativeSchemaFamily::MessagePart {
                format!("{message_identity}:{native_identity}")
            } else {
                native_identity.clone()
            };
            let session_identity: String = row.get(6)?;
            Ok(ProRecordMetadata {
                pro_ordinal: row.get(0)?,
                output_prefix_bytes: sqlite_nonnegative_u64(row.get::<_, i64>(1)?)?,
                source_event_ordinal: u64::try_from(row.get::<_, i64>(2)?).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Integer,
                        Box::new(error),
                    )
                })?,
                native_record_ordinal: stable_native_event_index(
                    &session_identity,
                    &source_native_identity,
                ),
                source_record_ordinal: ordered_source_rowid(source_rowid),
                subrecord_index: u32::try_from(row.get::<_, i64>(3)?).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        3,
                        rusqlite::types::Type::Integer,
                        Box::new(error),
                    )
                })?,
                native_identity,
                source_native_identity,
                message_identity,
                session_identity,
                parent_session_identity: nonempty(parent),
                root_session_identity: row.get(8)?,
                session_directory: nonempty(directory),
                agent_identity: nonempty(agent_identity),
                time_created: row.get(11)?,
                draft,
                rejection,
                locator: native_locator(native_shape_from_family(family), source_rowid).map_err(
                    |error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            22,
                            rusqlite::types::Type::Integer,
                            Box::new(error),
                        )
                    },
                )?,
                unit_bytes: sqlite_nonnegative_u64(row.get::<_, i64>(24)?)?,
            })
        },
    )?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)
}

pub(in super::super) fn has_pro_metadata_after(
    conn: &Connection,
    pro_ordinal: i64,
) -> Result<bool> {
    Ok(conn.query_row(
        "select exists(
             select 1 from ordered_pro_units where pro_ordinal > ?1 limit 1
         )",
        [pro_ordinal],
        |row| row.get::<_, i64>(0),
    )? != 0)
}

pub(in super::super) fn pro_keyset_for_frontier(
    conn: &Connection,
    frontier: super::super::model::OpenCodeNativeProFrontier,
) -> Result<ProKeyset> {
    if frontier.source_event_ordinal == 0 && frontier.subrecord_index == 0 && !frontier.terminal {
        return Ok(ProKeyset::default());
    }
    conn.query_row(
        "select pro_ordinal, output_prefix_bytes
         from ordered_pro_units
         where source_event_ordinal = ?1 and subrecord_index = ?2",
        params![
            i64_from_u64(
                frontier.source_event_ordinal,
                "OpenCode Pro frontier event ordinal",
            )?,
            i64::from(frontier.subrecord_index),
        ],
        |row| {
            Ok(ProKeyset {
                pro_ordinal: row.get(0)?,
                output_prefix_bytes: sqlite_nonnegative_u64(row.get::<_, i64>(1)?)?,
            })
        },
    )
    .map_err(|error| {
        CaptureError::InvalidPayload(format!(
            "OpenCode Pro replay frontier is not present in this exact generation: {error}"
        ))
    })
}
