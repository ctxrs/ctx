use super::*;

pub(super) fn validate_frontier(
    conn: &Connection,
    sql: &AstrBotSql,
    frontier: &AstrBotFrontier,
) -> Result<bool> {
    if frontier.version != FRONTIER_VERSION {
        return Ok(false);
    }
    let (conversation_hash, conversation_order) = recompute_prefix(
        conn,
        &sql.conversation_candidate_initial,
        &sql.conversation_candidate_after,
        frontier.conversation_after_rowid,
        |candidate| {
            if candidate.observed_bytes()?
                > u64::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).unwrap_or(u64::MAX)
            {
                Ok(candidate_hash(
                    b"astrbot-conversation-oversize-v1\0",
                    candidate,
                ))
            } else {
                let row = hydrate_conversation(
                    conn,
                    &sql.conversation_hydration,
                    candidate.physical_rowid,
                )?;
                serialized_hash(b"astrbot-conversation-row-v1\0", &row)
            }
        },
    )?;
    if conversation_hash != frontier.conversation_prefix_sha256
        || conversation_order != frontier.last_conversation_order
    {
        return Ok(false);
    }
    if let Some(in_row) = &frontier.conversation_in_row {
        let row = hydrate_conversation(conn, &sql.conversation_hydration, in_row.physical_rowid)?;
        if serialized_hash(b"astrbot-conversation-row-v1\0", &row)? != in_row.row_sha256
            || usize::try_from(in_row.next_item_index).unwrap_or(usize::MAX)
                >= conversation_items(&row.content).0.len().max(1)
        {
            return Ok(false);
        }
    }
    let Some(platform_initial) = sql.platform_message_candidate_initial.as_deref() else {
        return Ok(frontier.platform_after_rowid.is_none()
            && frontier.platform_prefix_sha256 == [0; 32]
            && frontier.last_platform_order.is_none());
    };
    let platform_after =
        sql.platform_message_candidate_after
            .as_deref()
            .ok_or(CaptureError::SystemInvariant(
                "AstrBot platform-message keyset SQL is incomplete",
            ))?;
    let (platform_hash, platform_order) = recompute_prefix(
        conn,
        platform_initial,
        platform_after,
        frontier.platform_after_rowid,
        |candidate| {
            if candidate.observed_bytes()?
                > u64::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).unwrap_or(u64::MAX)
            {
                Ok(candidate_hash(b"astrbot-platform-oversize-v1\0", candidate))
            } else {
                let hydration = sql.platform_message_hydration.as_deref().ok_or(
                    CaptureError::SystemInvariant(
                        "AstrBot platform-message hydration SQL is missing",
                    ),
                )?;
                let row = hydrate_platform_message(conn, hydration, candidate.physical_rowid)?;
                serialized_hash(b"astrbot-platform-row-v1\0", &row)
            }
        },
    )?;
    Ok(platform_hash == frontier.platform_prefix_sha256
        && platform_order == frontier.last_platform_order)
}

pub(super) fn recompute_prefix(
    conn: &Connection,
    initial_sql: &str,
    after_sql: &str,
    through_rowid: Option<i64>,
    mut row_hash: impl FnMut(RowCandidate) -> Result<[u8; 32]>,
) -> Result<([u8; 32], Option<LegacyOrderKey>)> {
    let Some(through_rowid) = through_rowid else {
        return Ok(([0; 32], None));
    };
    let mut after = None;
    let mut digest = [0; 32];
    loop {
        let Some(candidate) = fetch_candidate(conn, initial_sql, after_sql, after)? else {
            return Ok(([u8::MAX; 32], None));
        };
        if candidate.physical_rowid > through_rowid {
            return Ok(([u8::MAX; 32], None));
        }
        digest = chain_hash(digest, row_hash(candidate)?);
        after = Some(candidate.physical_rowid);
        if candidate.physical_rowid == through_rowid {
            return Ok((digest, Some(candidate.legacy_order)));
        }
    }
}

pub(super) fn decode_prior_cursor(stored: Option<SyncCursor>) -> Result<PriorCursor> {
    let Some(stored) = stored else {
        return Ok(PriorCursor::None);
    };
    if let Ok(committed) = decode_native_path_committed_cursor(&stored.cursor) {
        return Ok(PriorCursor::Native {
            encoded: stored.cursor,
            cursor: AstrBotStoreCursor::decode(committed.provider_cursor())?,
        });
    }
    if let Some(released) = CertifiedProviderCursor::decode_if_certified(&stored.cursor)? {
        return Ok(PriorCursor::Released {
            encoded: stored.cursor,
            rejected_records: released.rejected_records(),
        });
    }
    if stored.cursor.trim().is_empty() || !stored.cursor.trim_start().starts_with('{') {
        return Ok(PriorCursor::Released {
            encoded: stored.cursor,
            rejected_records: 0,
        });
    }
    Err(CaptureError::InvalidPayload(
        "AstrBot cursor is neither NativePath nor a released migration cursor".to_owned(),
    ))
}
