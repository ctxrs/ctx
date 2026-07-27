use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn prepare_cursor(
    conn: &Connection,
    database_path: &Path,
    path_identity: &str,
    source_revision: String,
    schema_fingerprint: String,
    sqlite_user_version: i64,
    proposed_source_identity: String,
    decoded: Option<DecodedCursor>,
) -> Result<PreparedCursor> {
    let Some(decoded) = decoded else {
        return Ok(PreparedCursor {
            cursor: ShelleyNativeCursor::fresh(
                database_path.to_path_buf(),
                path_identity.to_owned(),
                0,
                proposed_source_identity,
                source_revision,
                schema_fingerprint,
                sqlite_user_version,
                0,
            ),
            released_source_identity: None,
            retirement: None,
            needs_observation: true,
        });
    };
    let DecodedCursor::Native(mut prior) = decoded else {
        return Ok(PreparedCursor {
            cursor: ShelleyNativeCursor::fresh(
                database_path.to_path_buf(),
                path_identity.to_owned(),
                0,
                proposed_source_identity,
                source_revision,
                schema_fingerprint,
                sqlite_user_version,
                0,
            ),
            released_source_identity: None,
            retirement: None,
            needs_observation: true,
        });
    };
    prior.validate(database_path, path_identity)?;

    if prior.version == RELEASED_SHELLEY_NATIVE_CURSOR_VERSION {
        let released_source_identity = prior.canonical_source_identity.clone();
        let retirement = (!prior.route_retired).then(|| ShelleyRouteAuthority {
            locator_identity: prior.locator_identity.clone(),
            canonical_source_identity: prior.canonical_source_identity.clone(),
            source_revision: prior.source_revision.clone(),
        });
        return Ok(PreparedCursor {
            cursor: ShelleyNativeCursor::fresh(
                database_path.to_path_buf(),
                path_identity.to_owned(),
                prior
                    .route_epoch
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Shelley NativePath route epoch exhausted",
                    ))?,
                proposed_source_identity,
                source_revision,
                schema_fingerprint,
                sqlite_user_version,
                prior
                    .generation
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Shelley NativePath generation exhausted",
                    ))?,
            ),
            released_source_identity: Some(released_source_identity),
            retirement,
            needs_observation: true,
        });
    }

    if prior.route_retired {
        return Ok(PreparedCursor {
            cursor: ShelleyNativeCursor::fresh(
                database_path.to_path_buf(),
                path_identity.to_owned(),
                prior
                    .route_epoch
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Shelley NativePath route epoch exhausted",
                    ))?,
                proposed_source_identity,
                source_revision,
                schema_fingerprint,
                sqlite_user_version,
                prior
                    .generation
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Shelley NativePath generation exhausted",
                    ))?,
            ),
            released_source_identity: None,
            retirement: None,
            needs_observation: true,
        });
    }

    let schema_matches = prior.schema_fingerprint == schema_fingerprint
        && prior.sqlite_user_version == sqlite_user_version;
    let prefix_matches = schema_matches
        && verify_prefixes(
            conn,
            &prior.conversations,
            &prior.messages,
            &shelley_conversation_select_expressions(&shelley_conversation_columns(conn)?, "c"),
            &shelley_message_select_expressions(&shelley_message_columns(conn)?, "m"),
        )?;
    if !prefix_matches {
        let retirement = ShelleyRouteAuthority {
            locator_identity: prior.locator_identity.clone(),
            canonical_source_identity: prior.canonical_source_identity.clone(),
            source_revision: prior.source_revision.clone(),
        };
        return Ok(PreparedCursor {
            cursor: ShelleyNativeCursor::fresh(
                database_path.to_path_buf(),
                path_identity.to_owned(),
                prior
                    .route_epoch
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Shelley NativePath route epoch exhausted",
                    ))?,
                proposed_source_identity,
                source_revision,
                schema_fingerprint,
                sqlite_user_version,
                prior
                    .generation
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Shelley NativePath generation exhausted",
                    ))?,
            ),
            released_source_identity: None,
            retirement: Some(retirement),
            needs_observation: true,
        });
    }

    let source_changed = prior.source_revision != source_revision;
    prior.source_revision = source_revision;
    prior.schema_fingerprint = schema_fingerprint;
    prior.sqlite_user_version = sqlite_user_version;
    prior.route_retired = false;
    if source_changed {
        prior.phase = ShelleyPhase::Conversations;
        prior.terminal = false;
    }
    Ok(PreparedCursor {
        cursor: prior,
        released_source_identity: None,
        retirement: None,
        needs_observation: source_changed,
    })
}

impl ShelleyScanner<'_> {
    pub(super) fn next_page(&mut self) -> Result<Option<ShelleyCorePage>> {
        loop {
            match self.cursor.phase {
                ShelleyPhase::Conversations => {
                    let mut next = self.cursor.clone();
                    let mut rows = Vec::new();
                    let mut retained_bytes = SHELLEY_PAGE_FIXED_OVERHEAD;
                    while rows.len() < SHELLEY_PAGE_MAX_UNITS {
                        let Some((unit, row_digest)) = next_conversation_unit(
                            self.conn,
                            &self.conversation_select,
                            next.conversations.after_rowid,
                            None,
                        )?
                        else {
                            next.phase = ShelleyPhase::Messages;
                            break;
                        };
                        let bytes = unit.retained_bytes();
                        if !rows.is_empty()
                            && retained_bytes.saturating_add(bytes) > SHELLEY_PAGE_MAX_BYTES
                        {
                            break;
                        }
                        next.conversations.advance(unit.rowid(), row_digest)?;
                        retained_bytes = retained_bytes.saturating_add(bytes);
                        rows.push(unit);
                    }
                    if !rows.is_empty() {
                        let logical_units = rows.len();
                        self.cursor = next.clone();
                        self.needs_observation = false;
                        return Ok(Some(ShelleyCorePage {
                            next_cursor: next,
                            released_source_identity: self.released_source_identity.clone(),
                            rows: ShelleyCorePageRows::Conversations(rows),
                            logical_units,
                            retained_bytes,
                        }));
                    }
                    self.cursor = next;
                }
                ShelleyPhase::Messages => {
                    let mut next = self.cursor.clone();
                    let mut rows = Vec::new();
                    let mut retained_bytes = SHELLEY_PAGE_FIXED_OVERHEAD;
                    while rows.len() < SHELLEY_PAGE_MAX_UNITS {
                        let Some((unit, row_digest)) = next_message_unit(
                            self.conn,
                            &self.message_select,
                            &self.conversation_select,
                            self.has_message_sequence_id,
                            next.messages.after_rowid,
                            None,
                        )?
                        else {
                            next.phase = ShelleyPhase::Complete;
                            next.terminal = true;
                            self.needs_observation = true;
                            break;
                        };
                        let bytes = unit.retained_bytes();
                        if !rows.is_empty()
                            && retained_bytes.saturating_add(bytes) > SHELLEY_PAGE_MAX_BYTES
                        {
                            break;
                        }
                        next.messages.advance(unit.rowid(), row_digest)?;
                        retained_bytes = retained_bytes.saturating_add(bytes);
                        rows.push(unit);
                    }
                    if !rows.is_empty() {
                        let logical_units = rows.len();
                        self.cursor = next.clone();
                        self.needs_observation = false;
                        return Ok(Some(ShelleyCorePage {
                            next_cursor: next,
                            released_source_identity: self.released_source_identity.clone(),
                            rows: ShelleyCorePageRows::Messages(rows),
                            logical_units,
                            retained_bytes,
                        }));
                    }
                    self.cursor = next;
                }
                ShelleyPhase::Complete => {
                    if !self.needs_observation {
                        return Ok(None);
                    }
                    if !self.snapshot.revalidate(self.path)? {
                        return Err(CaptureError::SourceChangedDuringCapture);
                    }
                    self.needs_observation = false;
                    return Ok(Some(ShelleyCorePage {
                        next_cursor: self.cursor.clone(),
                        released_source_identity: self.released_source_identity.clone(),
                        rows: ShelleyCorePageRows::Observation,
                        logical_units: 1,
                        retained_bytes: SHELLEY_PAGE_FIXED_OVERHEAD,
                    }));
                }
            }
        }
    }
}

fn verify_prefixes(
    conn: &Connection,
    conversations: &ShelleyPrefix,
    messages: &ShelleyPrefix,
    conversation_select: &[String],
    message_select: &[String],
) -> Result<bool> {
    Ok(
        verify_conversation_prefix(conn, conversation_select, conversations)?
            && verify_message_prefix(conn, message_select, conversation_select, messages)?,
    )
}

fn verify_conversation_prefix(
    conn: &Connection,
    select: &[String],
    expected: &ShelleyPrefix,
) -> Result<bool> {
    let mut observed = ShelleyPrefix::initial(b'c');
    while let Some((unit, digest)) =
        next_conversation_unit(conn, select, observed.after_rowid, expected.after_rowid)?
    {
        observed.advance(unit.rowid(), digest)?;
    }
    Ok(&observed == expected)
}

pub(super) fn verify_message_prefix(
    conn: &Connection,
    message_select: &[String],
    conversation_select: &[String],
    expected: &ShelleyPrefix,
) -> Result<bool> {
    let has_sequence_id = shelley_message_columns(conn)?.contains("sequence_id");
    let mut observed = ShelleyPrefix::initial(b'm');
    while let Some((unit, digest)) = next_message_unit(
        conn,
        message_select,
        conversation_select,
        has_sequence_id,
        observed.after_rowid,
        expected.after_rowid,
    )? {
        observed.advance(unit.rowid(), digest)?;
    }
    Ok(&observed == expected)
}

fn next_conversation_unit(
    conn: &Connection,
    select: &[String],
    after: Option<i64>,
    through: Option<i64>,
) -> Result<Option<(ShelleyUnit<ShelleyConversationRow>, [u8; 32])>> {
    let Some((rowid, retained_bytes)) =
        next_candidate(conn, "conversations", "c", select, after, through)?
    else {
        return Ok(None);
    };
    if retained_bytes > SHELLEY_ROW_MAX_BYTES {
        let reason =
            format!("Shelley conversation row {rowid} exceeds the NativePath row byte limit");
        return Ok(Some((
            ShelleyUnit::Rejected {
                rowid,
                retained_bytes: SHELLEY_PAGE_FIXED_OVERHEAD.min(SHELLEY_ROW_MAX_BYTES),
                reason: reason.clone(),
            },
            rejected_row_digest(b'c', rowid, retained_bytes, &reason),
        )));
    }
    let values = query_row_values(conn, "conversations", "c", select, rowid)?;
    let row_digest = values_row_digest(b'c', rowid, &values, None);
    let unit = match decode_shelley_conversation(&values) {
        Ok(conversation) => ShelleyUnit::Accepted {
            rowid,
            retained_bytes: retained_bytes.saturating_add(512),
            value: conversation,
        },
        Err(error) => ShelleyUnit::Rejected {
            rowid,
            retained_bytes: retained_bytes.saturating_add(256),
            reason: error.to_string(),
        },
    };
    Ok(Some((unit, row_digest)))
}

pub(super) fn next_message_unit(
    conn: &Connection,
    message_select: &[String],
    conversation_select: &[String],
    has_sequence_id: bool,
    after: Option<i64>,
    through: Option<i64>,
) -> Result<Option<(ShelleyUnit<ShelleyMessage>, [u8; 32])>> {
    let Some((rowid, retained_bytes)) =
        next_candidate(conn, "messages", "m", message_select, after, through)?
    else {
        return Ok(None);
    };
    if retained_bytes > SHELLEY_ROW_MAX_BYTES {
        let reason = format!("Shelley message row {rowid} exceeds the NativePath row byte limit");
        return Ok(Some((
            ShelleyUnit::Rejected {
                rowid,
                retained_bytes: SHELLEY_PAGE_FIXED_OVERHEAD.min(SHELLEY_ROW_MAX_BYTES),
                reason: reason.clone(),
            },
            rejected_row_digest(b'm', rowid, retained_bytes, &reason),
        )));
    }
    let values = query_row_values(conn, "messages", "m", message_select, rowid)?;
    let message = match decode_shelley_message(&values) {
        Ok(message) => message,
        Err(error) => {
            let digest = values_row_digest(b'm', rowid, &values, None);
            return Ok(Some((
                ShelleyUnit::Rejected {
                    rowid,
                    retained_bytes: retained_bytes.saturating_add(256),
                    reason: error.to_string(),
                },
                digest,
            )));
        }
    };
    let parent = load_conversation_for_message(conn, conversation_select, &message)?;
    let (conversation, parent_values, parent_bytes) = match parent {
        ParentConversation::Accepted {
            conversation,
            values,
            retained_bytes,
        } => (conversation, values, retained_bytes),
        ParentConversation::Rejected { reason, digest } => {
            let row_digest = values_row_digest(b'm', rowid, &values, Some(&digest));
            return Ok(Some((
                ShelleyUnit::Rejected {
                    rowid,
                    retained_bytes: retained_bytes.saturating_add(256),
                    reason,
                },
                row_digest,
            )));
        }
    };
    let parent_bearing: bool = conn.query_row(
        "select not exists (
             select 1 from messages previous
             where typeof(previous.conversation_id) = 'text'
               and previous.conversation_id = ?1
               and previous.rowid < ?2
         )",
        rusqlite::params![message.conversation_id, rowid],
        |row| row.get(0),
    )?;
    let parent_digest = values_row_digest(b'p', conversation.rowid, &parent_values, None);
    let row_digest = values_row_digest(b'm', rowid, &values, Some(&parent_digest));
    let provider_event_index = shelley_stable_event_index(conn, &message, has_sequence_id)?;
    Ok(Some((
        ShelleyUnit::Accepted {
            rowid,
            retained_bytes: retained_bytes
                .saturating_add(parent_bytes)
                .saturating_add(1_024),
            value: ShelleyMessage {
                message,
                conversation,
                parent_bearing,
                provider_event_index,
            },
        },
        row_digest,
    )))
}

// This is constructed for every message; boxing the accepted row would add hot-path allocation.
#[allow(clippy::large_enum_variant)]
enum ParentConversation {
    Accepted {
        conversation: ShelleyConversationRow,
        values: Vec<NativeSqliteValue>,
        retained_bytes: usize,
    },
    Rejected {
        reason: String,
        digest: [u8; 32],
    },
}

fn load_conversation_for_message(
    conn: &Connection,
    select: &[String],
    message: &ShelleyMessageRow,
) -> Result<ParentConversation> {
    let lengths = shelley_retained_length_expr(select);
    let sql = format!(
        "select c.rowid, {lengths}
         from conversations c
         where typeof(c.conversation_id) = 'text' and c.conversation_id = ?1
         order by c.rowid limit 2"
    );
    let candidates = with_shelley_length_preflight(conn, || {
        let mut statement = conn.prepare(&sql)?;
        let rows = statement
            .query_map([message.conversation_id.as_str()], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    })?;
    let [(rowid, retained)] = candidates.as_slice() else {
        let reason = if candidates.is_empty() {
            format!(
                "Shelley message {} references missing conversation {}",
                message.message_id, message.conversation_id
            )
        } else {
            format!(
                "Shelley message {} references duplicate conversation {}",
                message.message_id, message.conversation_id
            )
        };
        return Ok(ParentConversation::Rejected {
            digest: rejected_row_digest(b'p', 0, candidates.len(), &reason),
            reason,
        });
    };
    let retained_bytes = usize::try_from(*retained).map_err(|_| {
        CaptureError::InvalidPayload(
            "Shelley conversation retained byte count must be nonnegative".to_owned(),
        )
    })?;
    if retained_bytes > SHELLEY_ROW_MAX_BYTES {
        let reason = format!(
            "Shelley message {} parent conversation exceeds the NativePath row byte limit",
            message.message_id
        );
        return Ok(ParentConversation::Rejected {
            digest: rejected_row_digest(b'p', *rowid, retained_bytes, &reason),
            reason,
        });
    }
    let values = query_row_values(conn, "conversations", "c", select, *rowid)?;
    match decode_shelley_conversation(&values) {
        Ok(conversation) => Ok(ParentConversation::Accepted {
            conversation,
            values,
            retained_bytes,
        }),
        Err(error) => Ok(ParentConversation::Rejected {
            digest: values_row_digest(b'p', *rowid, &values, None),
            reason: error.to_string(),
        }),
    }
}

fn next_candidate(
    conn: &Connection,
    table: &str,
    alias: &str,
    select: &[String],
    after: Option<i64>,
    through: Option<i64>,
) -> Result<Option<(i64, usize)>> {
    let lengths = shelley_retained_length_expr(select);
    let lower = after.map_or_else(String::new, |_| format!("and {alias}.rowid > ?1"));
    let upper_parameter = if after.is_some() { "?2" } else { "?1" };
    let upper = through.map_or_else(String::new, |_| {
        format!("and {alias}.rowid <= {upper_parameter}")
    });
    let sql = format!(
        "select {alias}.rowid, {lengths}
         from {table} {alias}
         where 1 = 1 {lower} {upper}
         order by {alias}.rowid limit 1"
    );
    let candidate: Option<(i64, i64)> =
        with_shelley_length_preflight(conn, || match (after, through) {
            (Some(after), Some(through)) => conn
                .query_row(&sql, rusqlite::params![after, through], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })
                .optional(),
            (Some(after), None) => conn
                .query_row(&sql, [after], |row| Ok((row.get(0)?, row.get(1)?)))
                .optional(),
            (None, Some(through)) => conn
                .query_row(&sql, [through], |row| Ok((row.get(0)?, row.get(1)?)))
                .optional(),
            (None, None) => conn
                .query_row(&sql, [], |row| Ok((row.get(0)?, row.get(1)?)))
                .optional(),
        })?;
    candidate
        .map(|(rowid, retained)| {
            let retained = usize::try_from(retained).map_err(|_| {
                CaptureError::InvalidPayload(format!(
                    "Shelley {table} retained byte count must be nonnegative"
                ))
            })?;
            Ok((rowid, retained.saturating_add(select.len() * 16)))
        })
        .transpose()
}

fn query_row_values(
    conn: &Connection,
    table: &str,
    alias: &str,
    select: &[String],
    rowid: i64,
) -> Result<Vec<NativeSqliteValue>> {
    let sql = format!(
        "select {} from {table} {alias} where {alias}.rowid = ?1",
        select.join(", ")
    );
    conn.query_row(&sql, [rowid], |row| {
        (0..select.len())
            .map(|index| row.get_ref(index).map(native_value))
            .collect::<rusqlite::Result<Vec<_>>>()
    })
    .map_err(CaptureError::from)
}

fn native_value(value: ValueRef<'_>) -> NativeSqliteValue {
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

fn values_row_digest(
    kind: u8,
    rowid: i64,
    values: &[NativeSqliteValue],
    parent: Option<&[u8; 32]>,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(SHELLEY_PREFIX_DOMAIN);
    digest.update([kind]);
    digest.update(rowid.to_le_bytes());
    digest.update((values.len() as u64).to_le_bytes());
    for value in values {
        match value {
            NativeSqliteValue::Null => digest.update([0]),
            NativeSqliteValue::Integer(value) => {
                digest.update([1]);
                digest.update(value.to_le_bytes());
            }
            NativeSqliteValue::RealBits(value) => {
                digest.update([2]);
                digest.update(value.to_le_bytes());
            }
            NativeSqliteValue::Text(value) => {
                digest.update([3]);
                hash_bytes(&mut digest, value.as_bytes());
            }
            NativeSqliteValue::Blob(value) => {
                digest.update([4]);
                hash_bytes(&mut digest, value);
            }
        }
    }
    if let Some(parent) = parent {
        digest.update([1]);
        digest.update(parent);
    } else {
        digest.update([0]);
    }
    digest.finalize().into()
}

fn rejected_row_digest(kind: u8, rowid: i64, retained_bytes: usize, reason: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(SHELLEY_PREFIX_DOMAIN);
    digest.update([kind]);
    digest.update(rowid.to_le_bytes());
    digest.update((retained_bytes as u64).to_le_bytes());
    hash_bytes(&mut digest, reason.as_bytes());
    digest.finalize().into()
}

pub(super) fn hash_bytes(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
}
