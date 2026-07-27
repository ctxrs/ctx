use super::*;

pub(crate) fn goose_prepare_native_identity_index(
    conn: &Connection,
    schema: &GooseNativeSchema,
    limits: GooseNativePageLimits,
) -> Result<()> {
    conn.execute_batch(&format!(
        "drop table if exists temp.{GOOSE_NATIVE_IDENTITY_TABLE};
         drop table if exists temp.{GOOSE_NATIVE_MESSAGE_METADATA_TABLE};
         drop table if exists temp.{GOOSE_NATIVE_ACCEPTED_SESSION_TABLE};
         create temp table {GOOSE_NATIVE_IDENTITY_TABLE} (
             message_id text primary key,
             uses integer not null
         ) without rowid;
         create temp table {GOOSE_NATIVE_MESSAGE_METADATA_TABLE} (
             sqlite_rowid integer primary key,
             native_message_id text,
             message_id_uses integer not null,
             message_ordinal integer not null
         );
         create temp table {GOOSE_NATIVE_ACCEPTED_SESSION_TABLE} (
             sqlite_rowid integer primary key,
             session_identity text not null unique
         );"
    ))?;
    let message_id = schema.message_id_expression("m");
    let normalized = goose_normalized_message_id_sql(&message_id);
    conn.execute(
        &format!(
            "insert into temp.{GOOSE_NATIVE_IDENTITY_TABLE} (message_id, uses)
             select {normalized}, count(*)
             from messages m
             where {normalized} is not null
             group by {normalized}"
        ),
        [],
    )?;
    conn.execute(
        &format!(
            "insert into temp.{GOOSE_NATIVE_MESSAGE_METADATA_TABLE} (
                 sqlite_rowid, native_message_id, message_id_uses, message_ordinal
             )
             select
                 m.rowid,
                 {normalized},
                 coalesce(ids.uses, 0),
                 row_number() over (order by m.rowid) - 1
             from messages m
             left join temp.{GOOSE_NATIVE_IDENTITY_TABLE} ids
               on ids.message_id = {normalized}
             order by m.rowid"
        ),
        [],
    )?;
    let session_expressions = schema.session_hydration_expressions("s");
    let retained_bytes = goose_retained_length_expr(&session_expressions);
    let max_session_bytes = i64::try_from(goose_projection_page_budget(limits, false)?)
        .map_err(|_| CaptureError::SystemInvariant("Goose session page limit exceeds i64"))?;
    conn.execute(
        &format!(
            "insert into temp.{GOOSE_NATIVE_ACCEPTED_SESSION_TABLE} (
                 sqlite_rowid, session_identity
             )
             select s.rowid, s.id
             from sessions s
             where {}
               and trim(s.id) != ''
               and {retained_bytes} <= ?1",
            schema.session_storage_class_predicate("s")
        ),
        [max_session_bytes],
    )?;
    Ok(())
}

pub(crate) fn goose_native_message_identity_at(
    conn: &Connection,
    schema: &GooseNativeSchema,
    rowid: i64,
    native_order: i64,
) -> Result<GooseNativeMessageIdentity> {
    let current_message_id = goose_normalized_message_id_sql(&schema.message_id_expression("m"));
    let candidate_message_id =
        goose_normalized_message_id_sql(&schema.message_id_expression("candidate"));
    let sql = format!(
        "with current as (
             select {current_message_id} as native_message_id
             from messages m
             where m.rowid = ?1
         )
         select
             native_message_id,
             case
                 when native_message_id is null then 0
                 else (
                     select count(*)
                     from messages candidate
                     where {candidate_message_id} = current.native_message_id
                 )
             end
         from current"
    );
    let (native_message_id, message_id_uses) = conn.query_row(&sql, [rowid], |row| {
        Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?))
    })?;
    Ok(goose_native_message_identity(
        native_message_id,
        message_id_uses,
        native_order,
    ))
}

pub(super) fn goose_normalized_message_id_sql(message_id: &str) -> String {
    format!(
        "case when typeof({message_id}) in ('null', 'text')
                   and {message_id} is not null
                   and octet_length(cast({message_id} as text)) <= {GOOSE_NATIVE_MAX_MESSAGE_ID_BYTES}
              then nullif(trim(cast({message_id} as text)), '')
              else null
         end"
    )
}

pub(super) fn goose_native_message_identity(
    native_message_id: Option<String>,
    message_id_uses: i64,
    native_order: i64,
) -> GooseNativeMessageIdentity {
    let identity_degraded = native_message_id.is_none() || message_id_uses != 1;
    let provider_message_identity = if identity_degraded {
        format!("row-{native_order}")
    } else {
        native_message_id
            .clone()
            .unwrap_or_else(|| format!("row-{native_order}"))
    };
    let native_identity = if identity_degraded {
        goose_tagged_fallback_message_identity(native_order)
    } else {
        native_message_id
            .as_deref()
            .map(goose_tagged_native_message_identity)
            .unwrap_or_else(|| goose_tagged_fallback_message_identity(native_order))
    };
    GooseNativeMessageIdentity {
        native_identity,
        provider_message_identity,
        identity_degraded,
    }
}
