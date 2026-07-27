use super::*;

pub(super) fn stage_sessions(
    source: &Connection,
    schema: &OpenCodeNativeSchema,
    metadata_byte_limit: usize,
    index: &mut Connection,
) -> Result<u64> {
    let metadata_byte_limit = i64_limit(metadata_byte_limit)?;
    let preflight = session_metadata_preflight_sql(&schema.session_columns);
    let oversized: i64 = source.query_row(&preflight, [metadata_byte_limit], |row| row.get(0))?;
    if oversized != 0 {
        return Err(CaptureError::InvalidPayload(
            "OpenCode session metadata exceeds NativePath page byte limit".to_owned(),
        ));
    }
    let parent = optional_session_text(&schema.session_columns, "parent_id");
    let title = optional_session_text(&schema.session_columns, "title");
    let directory = optional_session_text(&schema.session_columns, "directory");
    let model = optional_session_text(&schema.session_columns, "model");
    let agent = optional_session_text(&schema.session_columns, "agent");
    let sql = format!(
        "select cast(id as text), {parent}, {title}, {directory}, {model}, {agent},
                cast(time_created as integer), cast(time_updated as integer)
         from session"
    );
    let mut source_statement = source.prepare(&sql)?;
    let mut source_rows = source_statement.query([])?;
    let transaction = index.transaction()?;
    let mut insert = transaction.prepare(
        "insert into raw_sessions
         (native_identity, parent_identity, title, directory, model_identity, agent_identity,
          time_created, time_updated, content_digest, metadata_bytes)
         values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    )?;
    let mut count = 0_u64;
    while let Some(row) = source_rows.next()? {
        let identity: String = row.get(0)?;
        let parent: String = row.get(1)?;
        let title: String = row.get(2)?;
        let directory: String = row.get(3)?;
        let model: String = row.get(4)?;
        let agent: String = row.get(5)?;
        let time_created: i64 = row.get(6)?;
        let time_updated: i64 = row.get(7)?;
        let digest = session_digest(
            [&identity, &parent, &title, &directory, &model, &agent],
            time_created,
            time_updated,
        );
        let metadata_bytes =
            session_metadata_bytes(&identity, &parent, &title, &directory, &model, &agent)?;
        insert.execute(params![
            identity,
            parent,
            title,
            directory,
            model,
            agent,
            time_created,
            time_updated,
            digest,
            metadata_bytes,
        ])?;
        count = count.saturating_add(1);
    }
    drop(insert);
    transaction.commit()?;
    Ok(count)
}

pub(super) fn stage_events(
    source: &Connection,
    schema: &OpenCodeNativeSchema,
    retained_page_bytes: usize,
    profile: OpenCodeNativeProfile,
    dialect: &OpenCodeSqliteDialect,
    index: &mut Connection,
) -> Result<u64> {
    let retained_page_bytes =
        retained_page_bytes.min(super::super::OPENCODE_CORE_EVENT_PROJECTION_PAGE_BYTES);
    let sql = event_source_sql(schema, profile);
    let max_json_bytes = i64::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).map_err(|_| {
        CaptureError::SystemInvariant("OpenCode provider SQLite value limit exceeds i64")
    })?;
    let mut source_statement = source.prepare(&sql)?;
    let mut source_rows = source_statement.query([max_json_bytes])?;
    let transaction = index.transaction()?;
    let mut insert = transaction.prepare(
        "insert into raw_events
         (native_identity, message_identity, session_identity, source_rowid,
          order_tag, order_a, order_b, time_created, time_updated,
          content_bytes, projection, content_digest, order_digest, retained_bytes)
         values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
    )?;
    let mut insert_output = transaction.prepare(
        "insert into raw_outputs
         (native_identity, subrecord_index, kind, call_id, tool_name, command,
          working_directory, outcome, exit_code, duration_ms, content, unit_bytes)
         values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
    )?;
    let mut insert_output_rejection = transaction.prepare(
        "insert into raw_output_rejections
         (native_identity, subrecord_index, reason, unit_bytes)
         values (?1, ?2, ?3, ?4)",
    )?;
    let mut count = 0_u64;
    while let Some(row) = source_rows.next()? {
        let native_identity: String = row.get(0)?;
        let message_identity: String = row.get(1)?;
        let session_identity: String = row.get(2)?;
        let order_tag: i64 = row.get(3)?;
        let order_a: i64 = row.get(4)?;
        let order_b: i64 = row.get(5)?;
        let time_created: i64 = row.get(6)?;
        let time_updated: i64 = row.get(7)?;
        let content_bytes =
            sqlite_nonnegative_u64(row.get::<_, i64>(8)?).map_err(CaptureError::from)?;
        let mut projection: Vec<u8> = row.get(9)?;
        let has_explicit_event_time: i64 = row.get(10)?;
        let source_rowid: i64 = row.get(11)?;
        let native_order = decode_order(
            order_tag,
            &session_identity,
            &message_identity,
            &native_identity,
            order_a,
            order_b,
        )?;
        let mut decoded = decode_projection(&projection)?;
        if has_explicit_event_time == 0 {
            if let Err(error) = provider_required_timestamp_millis(
                time_created,
                dialect.session_message_time_created_field,
            ) {
                let reason = error.to_string();
                projection = encode_rejection_reason(reason.clone());
                decoded = OpenCodeJsonProjection::RejectedWithReason(
                    super::super::model::OpenCodeNativeRejectionKind::InvalidTimestamp,
                    reason,
                );
            }
        }
        let retained = match decoded {
            OpenCodeJsonProjection::Retained(retained) => Some(retained),
            OpenCodeJsonProjection::Output(output) => {
                if profile == OpenCodeNativeProfile::CoreAndPro {
                    if let Some(reason) = output.pro_rejection {
                        let unit_bytes = rejection_unit_bytes(&native_identity, &reason)?;
                        insert_output_rejection.execute(params![
                            &native_identity,
                            i64::from(u32::MAX),
                            reason,
                            i64_from_u64(unit_bytes, "OpenCode Pro rejection bytes")?,
                        ])?;
                    }
                    for draft in output.outputs {
                        let unit_bytes = output_unit_bytes(&native_identity, &draft)?;
                        if unit_bytes
                            > u64::try_from(OPENCODE_NATIVE_PAGE_MAX_BYTES).map_err(|_| {
                                CaptureError::SystemInvariant(
                                    "OpenCode NativePath page byte limit exceeds u64",
                                )
                            })?
                        {
                            let reason = format!(
                                "OpenCode output subrecord {} requires {unit_bytes} encoded bytes",
                                draft.subrecord_index
                            );
                            insert_output_rejection.execute(params![
                                &native_identity,
                                i64::from(draft.subrecord_index),
                                reason,
                                i64_from_u64(
                                    rejection_unit_bytes(&native_identity, &reason)?,
                                    "OpenCode Pro rejection bytes",
                                )?,
                            ])?;
                            continue;
                        }
                        insert_output.execute(params![
                            &native_identity,
                            i64::from(draft.subrecord_index),
                            i64::from(draft.kind),
                            draft.call_id,
                            draft.tool_name,
                            draft.command,
                            draft.working_directory,
                            i64::from(draft.outcome),
                            draft.exit_code,
                            draft
                                .duration_ms
                                .map(|value| i64_from_u64(value, "OpenCode output duration",))
                                .transpose()?,
                            draft.content.as_bytes(),
                            i64_from_u64(unit_bytes, "OpenCode Pro output bytes")?,
                        ])?;
                    }
                }
                output.diagnostic
            }
            OpenCodeJsonProjection::ExcludedOutput
            | OpenCodeJsonProjection::Rejected(_)
            | OpenCodeJsonProjection::RejectedWithReason(_, _) => None,
        };
        if let Some(retained) = retained.as_ref() {
            projection = encode_retained_projection(retained)?;
        } else if matches!(
            decode_projection(&projection)?,
            OpenCodeJsonProjection::Output(_) | OpenCodeJsonProjection::ExcludedOutput
        ) {
            projection = excluded_output_projection();
        }
        let retained_bytes = retained
            .as_ref()
            .map(|_| u64::try_from(projection.len()))
            .transpose()
            .map_err(|_| CaptureError::SystemInvariant("OpenCode projection bytes exceed u64"))?
            .unwrap_or(0);
        let retained_bytes_limit = u64::try_from(retained_page_bytes).map_err(|_| {
            CaptureError::SystemInvariant("OpenCode retained page bytes exceed u64")
        })?;
        let (content_digest, retained_bytes) = if let Some(retained) = retained.as_ref() {
            if retained_bytes > retained_bytes_limit {
                projection = OVERSIZED_PROJECTION.to_vec();
                (None, 0_i64)
            } else {
                let normalized_time = retained
                    .body
                    .pointer("/time/created")
                    .and_then(Value::as_i64)
                    .unwrap_or(time_created);
                (
                    Some(event_digest(
                        schema.family,
                        &native_identity,
                        &native_order,
                        normalized_time,
                        time_updated,
                        retained,
                    )?),
                    i64_from_u64(retained_bytes, "OpenCode retained content bytes")?,
                )
            }
        } else {
            (None, 0_i64)
        };
        let order_digest = native_order_digest(&native_order);
        insert.execute(params![
            native_identity,
            message_identity,
            session_identity,
            source_rowid,
            order_tag,
            order_a,
            order_b,
            time_created,
            time_updated,
            i64::try_from(content_bytes).map_err(|_| {
                CaptureError::InvalidPayload(
                    "OpenCode content bytes exceed SQLite integer".to_owned(),
                )
            })?,
            projection,
            content_digest,
            order_digest,
            retained_bytes,
        ])?;
        count = count.saturating_add(1);
    }
    drop(insert);
    drop(insert_output);
    drop(insert_output_rejection);
    transaction.commit()?;
    Ok(count)
}
