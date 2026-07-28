use super::event_projection::attach_crush_complete_content_locator;
use super::*;

pub(super) fn read_core_page(
    source: &CrushNativeSource,
    context: &ProviderAdapterContext,
    current: &CrushNativeCursor,
) -> Result<CrushNativePage> {
    if current.terminal {
        return Err(CaptureError::SystemInvariant(
            "Crush NativePath attempted to read beyond its terminal frontier",
        ));
    }
    if !source.snapshot.revalidate(&source.canonical_path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let mut frontier = current.frontier.clone();
    let row = loop {
        let candidate = next_candidate(&source.connection, &source.schema, &frontier)?;
        let Some(candidate) = candidate else {
            let Some(next_phase) = frontier.phase.next() else {
                break None;
            };
            frontier.phase = next_phase;
            frontier.after_rowid = None;
            continue;
        };
        let rowid = candidate.rowid;
        let ordinal = frontier.next_ordinal;
        frontier.after_rowid = Some(rowid);
        frontier.next_ordinal =
            frontier
                .next_ordinal
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Crush NativePath ordinal exhausted",
                ))?;
        if candidate.observed_bytes > CRUSH_NATIVE_MAX_ROW_BYTES {
            break Some(CrushNativeRow::Rejection {
                line: provider_line_from_index(ordinal.saturating_add(1)),
                reason: format!(
                    "Crush {} row {rowid} exceeds the NativePath retained-row bound",
                    frontier.phase.label()
                ),
                retained_bytes: CRUSH_NATIVE_PAGE_OVERHEAD_BYTES,
            });
        }
        break Some(
            match hydrate_row(source, frontier.phase, rowid, candidate.observed_bytes)
                .and_then(|row| prepare_native_row(source, context, row))
            {
                Ok(row) => row,
                Err(error) if row_decode_error_is_local(&error) => CrushNativeRow::Rejection {
                    line: provider_line_from_index(ordinal.saturating_add(1)),
                    reason: format!(
                        "Crush {} row {rowid} could not be decoded: {error}",
                        frontier.phase.label()
                    ),
                    retained_bytes: CRUSH_NATIVE_PAGE_OVERHEAD_BYTES,
                },
                Err(error) => return Err(error),
            },
        );
    };
    if !source.snapshot.revalidate(&source.canonical_path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let mut next = current.clone();
    next.frontier = frontier;
    next.terminal = row.is_none();
    if let Some(row) = row.as_ref() {
        if matches!(
            row,
            CrushNativeRow::Message { projection, .. } if projection.event.is_some()
        ) {
            next.retained_events = next.retained_events.saturating_add(1);
        }
        for failure in row.rejections() {
            next.record_rejection(failure)?;
        }
    }
    Ok(CrushNativePage {
        expected: current.clone(),
        next,
        row,
    })
}

pub(super) fn row_decode_error_is_local(error: &CaptureError) -> bool {
    match error {
        CaptureError::InvalidPayload(_) | CaptureError::Json(_) => true,
        CaptureError::Sqlite(error) => matches!(
            error,
            rusqlite::Error::FromSqlConversionFailure(..)
                | rusqlite::Error::IntegralValueOutOfRange(..)
                | rusqlite::Error::Utf8Error(..)
                | rusqlite::Error::InvalidColumnType(..)
        ),
        _ => false,
    }
}

pub(super) struct CrushCandidate {
    pub(super) rowid: i64,
    pub(super) observed_bytes: u64,
}

pub(super) fn next_candidate(
    conn: &Connection,
    schema: &CrushNativeSchema,
    frontier: &CrushNativeFrontier,
) -> Result<Option<CrushCandidate>> {
    let (rowid, retained, from) = match frontier.phase {
        CrushNativePhase::Sessions => (
            "s.rowid".to_owned(),
            retained_length_expr(
                &schema.session_columns,
                "s",
                &[
                    "id",
                    "parent_session_id",
                    "title",
                    "created_at",
                    "updated_at",
                    "prompt_tokens",
                    "completion_tokens",
                    "cost",
                    "summary_message_id",
                ],
            ),
            "sessions s".to_owned(),
        ),
        CrushNativePhase::Messages => {
            let local = retained_length_expr(
                &schema.message_columns,
                "m",
                &[
                    "id",
                    "session_id",
                    "role",
                    "parts",
                    "created_at",
                    "updated_at",
                    "provider",
                    "model",
                    "is_summary_message",
                ],
            );
            let parent = retained_length_expr(
                &schema.session_columns,
                "s",
                &["parent_session_id", "created_at", "updated_at"],
            );
            (
                "m.rowid".to_owned(),
                format!("{local} + {parent}"),
                message_session_join().to_owned(),
            )
        }
        CrushNativePhase::Files => {
            let Some(columns) = schema
                .file_columns
                .as_ref()
                .filter(|columns| columns.contains("session_id"))
            else {
                return Ok(None);
            };
            (
                "f.rowid".to_owned(),
                retained_length_expr(
                    columns,
                    "f",
                    &["session_id", "path", "version", "created_at", "updated_at"],
                ),
                "files f".to_owned(),
            )
        }
        CrushNativePhase::ReadFiles => {
            let Some(columns) = schema.read_file_columns.as_ref() else {
                return Ok(None);
            };
            (
                "r.rowid".to_owned(),
                retained_length_expr(columns, "r", &["session_id", "path", "read_at"]),
                "read_files r".to_owned(),
            )
        }
    };
    let after = if frontier.after_rowid.is_some() {
        format!(" where {rowid} > ?1")
    } else {
        String::new()
    };
    let sql = format!("select {rowid}, {retained} from {from}{after} order by {rowid} limit 1");
    let _guard = SqliteLengthPreflightGuard::new(conn);
    let read = |row: &rusqlite::Row<'_>| {
        let rowid = row.get::<_, i64>(0)?;
        let retained = row.get::<_, i64>(1)?;
        Ok((rowid, retained))
    };
    let candidate = match frontier.after_rowid {
        Some(rowid) => conn.query_row(&sql, [rowid], read).optional()?,
        None => conn.query_row(&sql, [], read).optional()?,
    };
    let Some((rowid, retained)) = candidate else {
        return Ok(None);
    };
    if rowid <= 0 || retained < 0 {
        return Err(CaptureError::InvalidPayload(format!(
            "Crush {} keyset metadata is invalid",
            frontier.phase.label()
        )));
    }
    let retained = u64::try_from(retained).map_err(|_| {
        CaptureError::InvalidPayload("Crush retained byte count is invalid".to_owned())
    })?;
    let observed_bytes = CRUSH_SQLITE_VALUE_OVERHEAD_BYTES
        .checked_add(retained)
        .ok_or(CaptureError::SystemInvariant(
            "Crush retained byte count overflowed",
        ))?;
    Ok(Some(CrushCandidate {
        rowid,
        observed_bytes,
    }))
}

pub(super) fn hydrate_row(
    source: &CrushNativeSource,
    phase: CrushNativePhase,
    rowid: i64,
    observed_bytes: u64,
) -> Result<CrushHydratedRow> {
    hydrate_row_from_connection(
        &source.connection,
        &source.schema,
        phase,
        rowid,
        observed_bytes,
    )
}

pub(super) fn hydrate_row_from_connection(
    connection: &Connection,
    schema: &CrushNativeSchema,
    phase: CrushNativePhase,
    rowid: i64,
    observed_bytes: u64,
) -> Result<CrushHydratedRow> {
    let retained_bytes = usize::try_from(observed_bytes)
        .unwrap_or(usize::MAX)
        .saturating_add(CRUSH_NATIVE_PAGE_OVERHEAD_BYTES);
    match phase {
        CrushNativePhase::Sessions => {
            let projection = session_projection(&schema.session_columns, "s");
            let values = connection.query_row(
                &format!("select s.rowid, {projection} from sessions s where s.rowid = ?1"),
                [rowid],
                |row| raw_sqlite_values(row, 10),
            )?;
            Ok(CrushHydratedRow::Session {
                row: decode_session(&values)?,
                retained_bytes,
            })
        }
        CrushNativePhase::Messages => {
            let parent_created_at = optional_session_column(&schema.session_columns, "created_at");
            let parent_updated_at = optional_session_column(&schema.session_columns, "updated_at");
            let projection = message_projection(&schema.message_columns, "m");
            let values = connection.query_row(
                &format!(
                    "select s.rowid, {parent_created_at}, \
                     {parent_updated_at}, {projection} \
                     from {} \
                     where m.rowid = ?1",
                    message_session_join()
                ),
                [rowid],
                |row| raw_sqlite_values(row, 13),
            )?;
            let child = decode_message_child(&values)?;
            let session = message_parent_session(connection, &schema.session_columns, &child)?;
            Ok(CrushHydratedRow::Message {
                row: child.message,
                session,
                digest_values: values,
                retained_bytes,
            })
        }
        CrushNativePhase::Files => {
            let columns = schema
                .file_columns
                .as_ref()
                .ok_or(CaptureError::SystemInvariant(
                    "Crush file phase has no schema",
                ))?;
            let projection = file_projection(columns, "f");
            let values = connection.query_row(
                &format!("select {projection} from files f where f.rowid = ?1"),
                [rowid],
                |row| raw_sqlite_values(row, 6),
            )?;
            Ok(CrushHydratedRow::File {
                row: decode_file(&values)?,
                retained_bytes,
            })
        }
        CrushNativePhase::ReadFiles => {
            let columns =
                schema
                    .read_file_columns
                    .as_ref()
                    .ok_or(CaptureError::SystemInvariant(
                        "Crush read-file phase has no schema",
                    ))?;
            let projection = read_file_projection(columns, "r");
            let values = connection.query_row(
                &format!("select {projection} from read_files r where r.rowid = ?1"),
                [rowid],
                |row| raw_sqlite_values(row, 4),
            )?;
            Ok(CrushHydratedRow::ReadFile {
                row: decode_read_file(&values)?,
                retained_bytes,
            })
        }
    }
}

fn prepare_native_row(
    source: &CrushNativeSource,
    context: &ProviderAdapterContext,
    row: CrushHydratedRow,
) -> Result<CrushNativeRow> {
    match row {
        CrushHydratedRow::Session {
            row,
            retained_bytes,
        } => Ok(CrushNativeRow::Session {
            row,
            retained_bytes,
        }),
        CrushHydratedRow::Message {
            row,
            session,
            digest_values,
            retained_bytes,
        } => match project_message(&row, session.as_ref(), context)? {
            CrushRecordProjection::Rejection {
                line_number,
                reason,
            } => Ok(CrushNativeRow::Rejection {
                line: line_number,
                reason,
                retained_bytes,
            }),
            CrushRecordProjection::Message(mut projection) => {
                if projection.output.is_none() {
                    let event = projection
                        .event
                        .as_mut()
                        .ok_or(CaptureError::SystemInvariant(
                            "Crush non-output projection has no event",
                        ))?;
                    let complete_text = projection.complete_text.as_deref().ok_or(
                        CaptureError::SystemInvariant(
                            "Crush non-output projection has no complete text",
                        ),
                    )?;
                    attach_crush_complete_content_locator(
                        event,
                        &projection.native_record_id,
                        row.rowid,
                        &digest_values,
                        complete_text,
                    )?;
                }
                let mut touches = Vec::new();
                let outcome = visit_provider_file_touch_drafts_with_limit(
                    &projection.raw_parts,
                    event_type_supports_structured_file_touches(projection.event_type),
                    CRUSH_NATIVE_MAX_EVENT_TOUCHES,
                    |(touch_ordinal, touch)| {
                        let provider_touch_index =
                            if projection.provider_event_index > MAX_PACKED_PROVIDER_EVENT_INDEX {
                                touch_ordinal
                            } else {
                                (projection.provider_event_index << 16) | touch_ordinal
                            };
                        touches.push(CrushFileTouchDraft {
                            provider_session_id: projection.provider_session_id.clone(),
                            provider_touch_index,
                            provider_event_index: Some(projection.provider_event_index),
                            path: touch.path,
                            change_kind: touch.change_kind,
                            old_path: touch.old_path,
                            line_count_delta: None,
                            confidence: touch.confidence,
                            occurred_at: projection.occurred_at,
                            metadata: touch.metadata,
                        });
                        Ok::<(), CaptureError>(())
                    },
                )?;
                let rejections = outcome.limit_exceeded().then(|| ProviderImportFailure {
                    line: projection.line_number,
                    error: PROVIDER_FILE_TOUCH_LIMIT_REJECTION.to_owned(),
                });
                Ok(CrushNativeRow::Message {
                    projection,
                    touches,
                    rejections: rejections.into_iter().collect(),
                    retained_bytes,
                })
            }
        },
        CrushHydratedRow::File {
            row,
            retained_bytes,
        } => {
            let line = provider_line_from_index(
                0x0100_0000_0000_u64.saturating_add(row.rowid.max(0) as u64),
            );
            let Some(touch) = file_touch(row, context.imported_at) else {
                return Ok(CrushNativeRow::Rejection {
                    line,
                    reason: "Crush file row has no provider session id".to_owned(),
                    retained_bytes,
                });
            };
            if !provider_session_exists(&source.connection, &touch.provider_session_id)? {
                return Ok(CrushNativeRow::Rejection {
                    line,
                    reason: format!(
                        "Crush file row references missing session {}",
                        touch.provider_session_id
                    ),
                    retained_bytes,
                });
            }
            Ok(CrushNativeRow::File {
                touch,
                retained_bytes,
            })
        }
        CrushHydratedRow::ReadFile {
            row,
            retained_bytes,
        } => {
            let line = provider_line_from_index(
                0x0200_0000_0000_u64.saturating_add(row.rowid.max(0) as u64),
            );
            let touch = read_file_touch(row, context.imported_at);
            if !provider_session_exists(&source.connection, &touch.provider_session_id)? {
                return Ok(CrushNativeRow::Rejection {
                    line,
                    reason: format!(
                        "Crush read-file row references missing session {}",
                        touch.provider_session_id
                    ),
                    retained_bytes,
                });
            }
            Ok(CrushNativeRow::ReadFile {
                touch,
                retained_bytes,
            })
        }
    }
}

fn provider_session_exists(conn: &Connection, provider_session_id: &str) -> Result<bool> {
    conn.query_row(
        "select exists(
            select 1 from sessions
            where typeof(id) = 'text'
              and id collate binary = ?1 collate binary
         )",
        [provider_session_id],
        |row| row.get::<_, bool>(0),
    )
    .map_err(CaptureError::from)
}

fn message_parent_session(
    conn: &Connection,
    columns: &BTreeSet<String>,
    child: &super::super::projection::CrushChildMessageRow,
) -> Result<Option<CrushSessionRow>> {
    let Some(parent_rowid) = child.parent_rowid else {
        return Ok(None);
    };
    let parent_session_id = if columns.contains("parent_session_id") {
        let values = conn.query_row(
            "select parent_session_id from sessions where rowid = ?1",
            [parent_rowid],
            |row| raw_sqlite_values(row, 1),
        )?;
        optional_text(&values, 0)?
    } else {
        None
    };
    Ok(Some(CrushSessionRow {
        id: child.message.session_id.clone(),
        parent_session_id,
        title: None,
        created_at: child.parent_created_at,
        updated_at: child.parent_updated_at,
        prompt_tokens: None,
        completion_tokens: None,
        cost: None,
        summary_message_id: None,
    }))
}

fn raw_sqlite_values(
    row: &rusqlite::Row<'_>,
    count: usize,
) -> rusqlite::Result<Vec<NativeSqliteValue>> {
    (0..count)
        .map(|index| row.get_ref(index).map(raw_sqlite_value))
        .collect()
}

fn raw_sqlite_value(value: ValueRef<'_>) -> NativeSqliteValue {
    match value {
        ValueRef::Null => NativeSqliteValue::Null,
        ValueRef::Integer(value) => NativeSqliteValue::Integer(value),
        ValueRef::Real(value) => NativeSqliteValue::from_real(value),
        ValueRef::Text(value) => std::str::from_utf8(value).map_or_else(
            |_| NativeSqliteValue::Blob(value.to_vec()),
            |value| NativeSqliteValue::Text(value.to_owned()),
        ),
        ValueRef::Blob(value) => NativeSqliteValue::Blob(value.to_vec()),
    }
}
