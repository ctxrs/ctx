use rusqlite::Connection;

use crate::provider::sqlite::SqliteLengthPreflightGuard;
use crate::{CaptureError, OutputOutcome, Result};

use super::position::GooseNativeRowKeyset;
use super::schema::{GooseNativeSchema, GooseSessionRow};

pub(super) fn goose_retained_length_expr(expressions: &[String]) -> String {
    expressions
        .iter()
        .map(|expression| format!("coalesce(octet_length({expression}), 0)"))
        .collect::<Vec<_>>()
        .join(" + ")
}

pub(super) const GOOSE_NATIVE_DEFAULT_PAGE_ROWS: usize = 64;
pub(super) const GOOSE_NATIVE_MAX_PAGE_ROWS: usize = 64;
pub(super) const GOOSE_NATIVE_DEFAULT_PAGE_BYTES: u64 = 8 * 1024 * 1024;
pub(super) const GOOSE_NATIVE_MAX_PAGE_BYTES: u64 = 8 * 1024 * 1024;
pub(super) const GOOSE_NATIVE_MIN_PAGE_BYTES: u64 = 4 * 1024;
pub(super) const GOOSE_NATIVE_MAX_RETAINED_CONTENT_BYTES: u64 = 8 * 1024 * 1024;
pub(super) const GOOSE_NATIVE_MAX_PRO_OUTPUT_BYTES: u64 = 8 * 1024 * 1024;
const GOOSE_NATIVE_PAGE_ENVELOPE_BYTES: u64 = 2 * 1024;
const GOOSE_NATIVE_PAGE_UNIT_OVERHEAD_BYTES: u64 = 1024;
const GOOSE_NATIVE_MAX_MESSAGE_ID_BYTES: u64 = 1_024;
const GOOSE_NATIVE_IDENTITY_TABLE: &str = "goose_native_message_identity_counts";
const GOOSE_NATIVE_MESSAGE_METADATA_TABLE: &str = "goose_native_message_metadata";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct GooseNativePageLimits {
    pub(super) rows: usize,
    pub(super) retained_bytes: u64,
}

impl GooseNativePageLimits {
    pub(super) fn new(rows: usize, retained_bytes: u64) -> Result<Self> {
        if rows == 0 || rows > GOOSE_NATIVE_MAX_PAGE_ROWS {
            return Err(CaptureError::InvalidPayload(
                format!(
                    "Goose NativePath page row limit must be between 1 and {GOOSE_NATIVE_MAX_PAGE_ROWS}"
                ),
            ));
        }
        if !(GOOSE_NATIVE_MIN_PAGE_BYTES..=GOOSE_NATIVE_MAX_PAGE_BYTES).contains(&retained_bytes) {
            return Err(CaptureError::InvalidPayload(format!(
                "Goose NativePath page byte limit must be between {GOOSE_NATIVE_MIN_PAGE_BYTES} and the frozen {GOOSE_NATIVE_MAX_PAGE_BYTES}-byte bound"
            )));
        }
        let row_reserve = u64::try_from(rows)
            .unwrap_or(u64::MAX)
            .saturating_mul(GOOSE_NATIVE_PAGE_UNIT_OVERHEAD_BYTES);
        if retained_bytes <= GOOSE_NATIVE_PAGE_ENVELOPE_BYTES.saturating_add(row_reserve) {
            return Err(CaptureError::InvalidPayload(
                "Goose NativePath page byte limit cannot contain the requested row envelope"
                    .to_owned(),
            ));
        }
        Ok(Self {
            rows,
            retained_bytes,
        })
    }
}

impl Default for GooseNativePageLimits {
    fn default() -> Self {
        Self {
            rows: GOOSE_NATIVE_DEFAULT_PAGE_ROWS,
            retained_bytes: GOOSE_NATIVE_DEFAULT_PAGE_BYTES,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct GooseScannedSession {
    pub(super) sqlite_rowid: i64,
    pub(super) bounded_native_identity: Option<String>,
    pub(super) observed_bytes: u64,
    pub(super) row: Option<GooseSessionRow>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GooseMessageCellDisposition {
    Retained,
    OutputSuccess,
    OutputFailure,
    OutputTimeout,
    OutputUnknown,
    MalformedJson,
    UnsupportedJsonRoot,
    NonObjectBlock,
    UnknownBlockType,
    OversizedRetainedContent,
    MissingSession,
    UnsupportedStorageClass,
    DuplicateBlockType,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GooseRetainedContentClass {
    Message,
    ToolCall,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct GooseScannedMessage {
    pub(super) sqlite_rowid: i64,
    pub(super) native_order: i64,
    pub(super) native_identity: String,
    pub(super) provider_message_identity: String,
    pub(super) identity_degraded: bool,
    pub(super) session_identity: String,
    pub(super) role: String,
    pub(super) disposition: GooseMessageCellDisposition,
    pub(super) output_outcome: Option<OutputOutcome>,
    pub(super) retained_class: Option<GooseRetainedContentClass>,
    pub(super) content_json: Option<String>,
    pub(super) content_bytes: u64,
    pub(super) created_timestamp: Option<i64>,
    pub(super) timestamp: Option<String>,
    pub(super) tokens_json: Option<String>,
    pub(super) metadata_json: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct GooseRetainedMessage {
    pub(super) sqlite_rowid: i64,
    pub(super) native_order: i64,
    pub(super) native_identity: String,
    pub(super) provider_message_identity: String,
    pub(super) identity_degraded: bool,
    pub(super) session_identity: String,
    pub(super) role: String,
    pub(super) retained_class: GooseRetainedContentClass,
    pub(super) content_json: String,
    pub(super) content_bytes: u64,
    pub(super) created_timestamp: Option<i64>,
    pub(super) timestamp: Option<String>,
    pub(super) tokens_json: Option<String>,
    pub(super) metadata_json: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct GooseScannedOutput {
    pub(super) sqlite_rowid: i64,
    pub(super) native_order: i64,
    pub(super) source_record_ordinal: u64,
    pub(super) native_identity: String,
    pub(super) provider_message_identity: String,
    pub(super) identity_degraded: bool,
    pub(super) session_identity: String,
    pub(super) outcome: OutputOutcome,
    pub(super) content_json: Option<String>,
    pub(super) content_bytes: u64,
    pub(super) created_timestamp: Option<i64>,
    pub(super) timestamp: Option<String>,
}

impl GooseScannedMessage {
    pub(super) fn into_retained(self) -> Result<GooseRetainedMessage> {
        if self.disposition != GooseMessageCellDisposition::Retained {
            return Err(CaptureError::SystemInvariant(
                "Goose attempted to normalize a non-retained message",
            ));
        }
        let content_json = self.content_json.ok_or(CaptureError::SystemInvariant(
            "Goose retained message was not hydrated",
        ))?;
        let retained_class = self.retained_class.ok_or(CaptureError::SystemInvariant(
            "Goose retained message omitted its SQLite visitor class",
        ))?;
        Ok(GooseRetainedMessage {
            sqlite_rowid: self.sqlite_rowid,
            native_order: self.native_order,
            native_identity: self.native_identity,
            provider_message_identity: self.provider_message_identity,
            identity_degraded: self.identity_degraded,
            session_identity: self.session_identity,
            role: self.role,
            retained_class,
            content_json,
            content_bytes: self.content_bytes,
            created_timestamp: self.created_timestamp,
            timestamp: self.timestamp,
            tokens_json: self.tokens_json,
            metadata_json: self.metadata_json,
        })
    }
}

pub(super) fn goose_prepare_native_identity_index(
    conn: &Connection,
    schema: &GooseNativeSchema,
) -> Result<()> {
    conn.execute_batch(&format!(
        "drop table if exists temp.{GOOSE_NATIVE_IDENTITY_TABLE};
         drop table if exists temp.{GOOSE_NATIVE_MESSAGE_METADATA_TABLE};
         create temp table {GOOSE_NATIVE_IDENTITY_TABLE} (
             message_id text primary key,
             uses integer not null
         ) without rowid;
         create temp table {GOOSE_NATIVE_MESSAGE_METADATA_TABLE} (
             sqlite_rowid integer primary key,
             native_message_id text,
             message_id_uses integer not null,
             message_ordinal integer not null
         );"
    ))?;
    let message_id = schema.message_id_expression("m");
    let normalized = format!(
        "case when {message_id} is not null \
              and octet_length(cast({message_id} as text)) <= {GOOSE_NATIVE_MAX_MESSAGE_ID_BYTES} \
         then nullif(trim(cast({message_id} as text)), '') else null end"
    );
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
    Ok(())
}

pub(super) fn goose_fetch_native_session_page(
    conn: &Connection,
    schema: &GooseNativeSchema,
    keyset: GooseNativeRowKeyset,
    limits: GooseNativePageLimits,
) -> Result<Vec<GooseScannedSession>> {
    let row_limit = i64::try_from(limits.rows)
        .map_err(|_| CaptureError::InvalidPayload("Goose page row limit exceeds i64".to_owned()))?;
    let expressions = schema.session_hydration_expressions("s");
    let retained_bytes = goose_retained_length_expr(&expressions);
    let guarded_select = expressions
        .iter()
        .map(|expression| {
            format!("case when selected.representable = 1 then {expression} else null end")
        })
        .collect::<Vec<_>>()
        .join(", ");
    let operator = keyset.sql_operator();
    let page_budget = goose_projection_page_budget(limits, false)?;
    let max_unit = i64::try_from(page_budget)
        .map_err(|_| CaptureError::SystemInvariant("Goose session page limit exceeds i64"))?;
    let page_budget = i64::try_from(page_budget)
        .map_err(|_| CaptureError::SystemInvariant("Goose session page limit exceeds i64"))?;
    let mut statement = conn.prepare(&format!(
        "with candidates as (
             select
                 s.rowid as sqlite_rowid,
                 {retained_bytes} as retained_bytes,
                 case
                     when octet_length(cast(s.id as text)) <= 16384
                     then cast(s.id as text)
                     else null
                 end as bounded_native_identity
             from sessions s
             where s.rowid {operator} ?1
             order by s.rowid
             limit ?2
         ),
         classified as (
             select *,
                 case when retained_bytes <= ?3 then 1 else 0 end as representable
             from candidates
         ),
         measured as (
             select *,
                 sum(
                     case when representable = 1
                          then retained_bytes + {GOOSE_NATIVE_PAGE_UNIT_OVERHEAD_BYTES}
                          else {GOOSE_NATIVE_PAGE_UNIT_OVERHEAD_BYTES}
                     end
                 ) over (order by sqlite_rowid rows unbounded preceding) as running_bytes
             from classified
         ),
         selected as (
             select *
             from measured
             where running_bytes <= ?4
         )
         select
             selected.sqlite_rowid,
             selected.bounded_native_identity,
             selected.retained_bytes,
             selected.representable,
             {guarded_select}
         from selected
         join sessions s on s.rowid = selected.sqlite_rowid
         order by selected.sqlite_rowid"
    ))?;
    let _length_guard = SqliteLengthPreflightGuard::new(conn);
    let rows = statement.query_map(
        rusqlite::params![keyset.bound(), row_limit, max_unit, page_budget],
        |row| {
            let raw_observed_bytes: i64 = row.get(2)?;
            let observed_bytes = u64::try_from(raw_observed_bytes)
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(2, raw_observed_bytes))?;
            let representable = row.get::<_, i64>(3)? != 0;
            Ok(GooseScannedSession {
                sqlite_rowid: row.get(0)?,
                bounded_native_identity: row.get(1)?,
                observed_bytes,
                row: if representable {
                    Some(GooseSessionRow {
                        id: row.get(4)?,
                        name: row.get(5)?,
                        description: row.get(6)?,
                        user_set_name: row.get::<_, i64>(7)? != 0,
                        session_type: row.get(8)?,
                        working_dir: row.get(9)?,
                        created_at: row.get(10)?,
                        updated_at: row.get(11)?,
                        extension_data: row.get(12)?,
                        total_tokens: row.get(13)?,
                        input_tokens: row.get(14)?,
                        output_tokens: row.get(15)?,
                        accumulated_total_tokens: row.get(16)?,
                        accumulated_input_tokens: row.get(17)?,
                        accumulated_output_tokens: row.get(18)?,
                        accumulated_cost: row.get(19)?,
                        provider_name: row.get(20)?,
                        model_config_json: row.get(21)?,
                        goose_mode: row.get(22)?,
                        archived_at: row.get(23)?,
                        project_id: row.get(24)?,
                    })
                } else {
                    None
                },
            })
        },
    )?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)
}

pub(super) fn goose_fetch_native_message_page(
    conn: &Connection,
    schema: &GooseNativeSchema,
    keyset: GooseNativeRowKeyset,
    limits: GooseNativePageLimits,
) -> Result<Vec<GooseScannedMessage>> {
    let row_limit = i64::try_from(limits.rows)
        .map_err(|_| CaptureError::InvalidPayload("Goose page row limit exceeds i64".to_owned()))?;
    let projection_budget = goose_projection_page_budget(limits, true)?;
    let max_content = i64::try_from(GOOSE_NATIVE_MAX_RETAINED_CONTENT_BYTES.min(projection_budget))
        .map_err(|_| CaptureError::SystemInvariant("Goose retained content limit exceeds i64"))?;
    let page_bytes = i64::try_from(projection_budget).map_err(|_| {
        CaptureError::InvalidPayload("Goose page byte limit exceeds i64".to_owned())
    })?;
    let message_id = schema.message_id_expression("m");
    let created_timestamp = schema.message_created_timestamp_expression("m");
    let timestamp = schema.message_timestamp_expression("m");
    let tokens = schema.message_tokens_expression("m");
    let metadata = schema.message_metadata_expression("m");
    let normalized_message_id = format!(
        "case when {message_id} is not null \
              and octet_length(cast({message_id} as text)) <= {GOOSE_NATIVE_MAX_MESSAGE_ID_BYTES} \
         then nullif(trim(cast({message_id} as text)), '') else null end"
    );
    let keyset_operator = keyset.sql_operator();
    let content_disposition = goose_native_content_visitor_sql();
    let output_outcome = goose_native_output_outcome_sql();
    let sql = format!(
        "with candidates as (
             select
                 m.rowid as sqlite_rowid,
                 cast(m.id as integer) as native_order,
                 {normalized_message_id} as native_message_id,
                 coalesce(ids.uses, 0) as message_id_uses,
                 cast(m.session_id as text) as session_identity,
                 s.rowid as parent_rowid,
                 cast(m.role as text) as role,
                 m.content_json as content_json,
                 typeof(m.content_json) as content_storage_class,
                 coalesce(octet_length(m.content_json), 0) as content_bytes,
                 coalesce(octet_length({tokens}), 0)
                     + coalesce(octet_length({metadata}), 0) as auxiliary_bytes,
                 {created_timestamp} as created_timestamp,
                 {timestamp} as native_timestamp,
                 {tokens} as tokens_json,
                 {metadata} as metadata_json
             from messages m
             left join sessions s
               on cast(s.id as text) = cast(m.session_id as text)
              and trim(cast(s.id as text)) != ''
             left join temp.{GOOSE_NATIVE_IDENTITY_TABLE} ids
               on ids.message_id = {normalized_message_id}
             where m.rowid {keyset_operator} ?1
             order by m.rowid
             limit ?2
         ),
         structural as (
             select *,
                 {content_disposition} as disposition
             from candidates
         ),
         classified as (
             select *,
                 case when disposition = 1 then {output_outcome} else disposition end
                     as classified_disposition
             from structural
         ),
         measured as (
             select *,
                 sum(
                     case
                         when classified_disposition in (0, 9)
                              then content_bytes + auxiliary_bytes
                         when classified_disposition in (11, 12)
                              and content_bytes + auxiliary_bytes <= ?3
                              then content_bytes + auxiliary_bytes
                         else 512 + auxiliary_bytes
                     end
                 )
                     over (order by sqlite_rowid rows unbounded preceding) as running_bytes
             from classified
         )
         select
             sqlite_rowid,
             native_order,
             native_message_id,
             message_id_uses,
             session_identity,
             role,
             classified_disposition,
             case
                 when classified_disposition in (0, 9) then content_json
                 when classified_disposition in (11, 12)
                      and content_bytes + auxiliary_bytes <= ?3 then content_json
                 else null
             end,
             content_bytes,
             created_timestamp,
             native_timestamp,
             case when classified_disposition in (0, 9, 11, 12)
                  then cast(tokens_json as text) else null end,
             case when classified_disposition in (0, 9, 11, 12)
                  then cast(metadata_json as text) else null end
         from measured
         where running_bytes <= ?4
         order by sqlite_rowid"
    );

    let _length_guard = SqliteLengthPreflightGuard::new(conn);
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(
        rusqlite::params![keyset.bound(), row_limit, max_content, page_bytes],
        |row| {
            let native_order: i64 = row.get(1)?;
            let native_message_id: Option<String> = row.get(2)?;
            let message_id_uses: i64 = row.get(3)?;
            let identity_degraded = native_message_id.is_none() || message_id_uses != 1;
            let provider_message_identity = if identity_degraded {
                format!("row-{native_order}")
            } else {
                native_message_id
                    .clone()
                    .unwrap_or_else(|| format!("row-{native_order}"))
            };
            let native_identity = match (identity_degraded, native_message_id) {
                (false, Some(native_message_id)) => {
                    goose_tagged_native_message_identity(&native_message_id)
                }
                (true, _) => goose_tagged_fallback_message_identity(native_order),
                (false, None) => {
                    return Err(rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Null,
                        std::io::Error::other(
                            "unique Goose native message identity unexpectedly missing",
                        )
                        .into(),
                    ));
                }
            };
            let (disposition, retained_class, output_outcome) = match row.get::<_, i64>(6)? {
                0 => (
                    GooseMessageCellDisposition::Retained,
                    Some(GooseRetainedContentClass::Message),
                    None,
                ),
                2 => (GooseMessageCellDisposition::MalformedJson, None, None),
                3 => (GooseMessageCellDisposition::UnsupportedJsonRoot, None, None),
                4 => (GooseMessageCellDisposition::NonObjectBlock, None, None),
                5 => (GooseMessageCellDisposition::UnknownBlockType, None, None),
                6 => (
                    GooseMessageCellDisposition::OversizedRetainedContent,
                    None,
                    None,
                ),
                7 => (GooseMessageCellDisposition::MissingSession, None, None),
                8 => (
                    GooseMessageCellDisposition::UnsupportedStorageClass,
                    None,
                    None,
                ),
                9 => (
                    GooseMessageCellDisposition::Retained,
                    Some(GooseRetainedContentClass::ToolCall),
                    None,
                ),
                10 => (GooseMessageCellDisposition::DuplicateBlockType, None, None),
                11 => (
                    GooseMessageCellDisposition::OutputFailure,
                    None,
                    Some(OutputOutcome::Failure),
                ),
                12 => (
                    GooseMessageCellDisposition::OutputTimeout,
                    None,
                    Some(OutputOutcome::Timeout),
                ),
                13 => (
                    GooseMessageCellDisposition::OutputUnknown,
                    None,
                    Some(OutputOutcome::Unknown),
                ),
                14 => (
                    GooseMessageCellDisposition::OutputSuccess,
                    None,
                    Some(OutputOutcome::Success),
                ),
                value => {
                    return Err(rusqlite::Error::FromSqlConversionFailure(
                        6,
                        rusqlite::types::Type::Integer,
                        format!("unknown Goose NativePath disposition {value}").into(),
                    ));
                }
            };
            let raw_content_bytes: i64 = row.get(8)?;
            let content_bytes = u64::try_from(raw_content_bytes)
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(8, raw_content_bytes))?;
            Ok(GooseScannedMessage {
                sqlite_rowid: row.get(0)?,
                native_order,
                native_identity,
                provider_message_identity,
                identity_degraded,
                session_identity: row.get(4)?,
                role: row.get(5)?,
                disposition,
                output_outcome,
                retained_class,
                content_json: row.get(7)?,
                content_bytes,
                created_timestamp: row.get(9)?,
                timestamp: row.get(10)?,
                tokens_json: row.get(11)?,
                metadata_json: row.get(12)?,
            })
        },
    )?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)
}

pub(super) fn goose_fetch_native_output_page(
    conn: &Connection,
    schema: &GooseNativeSchema,
    keyset: GooseNativeRowKeyset,
    limits: GooseNativePageLimits,
) -> Result<Vec<GooseScannedOutput>> {
    let row_limit = i64::try_from(limits.rows)
        .map_err(|_| CaptureError::InvalidPayload("Goose page row limit exceeds i64".to_owned()))?;
    let projection_budget = goose_projection_page_budget(limits, false)?;
    let max_content = i64::try_from(GOOSE_NATIVE_MAX_PRO_OUTPUT_BYTES.min(projection_budget))
        .map_err(|_| CaptureError::SystemInvariant("Goose Pro output limit exceeds i64"))?;
    let page_bytes = i64::try_from(projection_budget).map_err(|_| {
        CaptureError::InvalidPayload("Goose page byte limit exceeds i64".to_owned())
    })?;
    let created_timestamp = schema.message_created_timestamp_expression("m");
    let timestamp = schema.message_timestamp_expression("m");
    let tokens = schema.message_tokens_expression("m");
    let metadata = schema.message_metadata_expression("m");
    let keyset_operator = keyset.sql_operator();
    let content_disposition = goose_native_content_visitor_sql();
    let output_outcome = goose_native_output_outcome_sql();
    let sql = format!(
        "with candidates as (
             select
                 m.rowid as sqlite_rowid,
                 cast(m.id as integer) as native_order,
                 native.native_message_id,
                 native.message_id_uses,
                 native.message_ordinal,
                 cast(m.session_id as text) as session_identity,
                 s.rowid as parent_rowid,
                 cast(m.role as text) as role,
                 m.content_json as content_json,
                 typeof(m.content_json) as content_storage_class,
                 coalesce(octet_length(m.content_json), 0) as content_bytes,
                 coalesce(octet_length({tokens}), 0)
                     + coalesce(octet_length({metadata}), 0)
                     + coalesce(octet_length(cast(m.session_id as text)), 0)
                     + coalesce(octet_length(cast(m.role as text)), 0) as auxiliary_bytes,
                 {created_timestamp} as created_timestamp,
                 {timestamp} as native_timestamp
             from messages m
             join temp.{GOOSE_NATIVE_MESSAGE_METADATA_TABLE} native
               on native.sqlite_rowid = m.rowid
             left join sessions s
               on cast(s.id as text) = cast(m.session_id as text)
              and trim(cast(s.id as text)) != ''
             where m.rowid {keyset_operator} ?1
         ),
         structural as (
             select *, {content_disposition} as disposition
             from candidates
         ),
         output_candidates as (
             select *,
                 {output_outcome} as output_outcome,
                 case when content_bytes + auxiliary_bytes <= ?3 then 1 else 0 end
                     as representable
             from structural
             where disposition = 1
             order by sqlite_rowid
             limit ?2
         ),
         measured as (
             select *,
                 sum(
                     case when representable = 1
                          then content_bytes + auxiliary_bytes + 1024
                          else auxiliary_bytes + 1024
                     end
                 ) over (order by sqlite_rowid rows unbounded preceding) as running_bytes
             from output_candidates
         )
         select
             sqlite_rowid,
             native_order,
             native_message_id,
             message_id_uses,
             message_ordinal + (select count(*) from sessions),
             session_identity,
             output_outcome,
             case when representable = 1 then content_json else null end,
             content_bytes,
             created_timestamp,
             native_timestamp
         from measured
         where running_bytes <= ?4
         order by sqlite_rowid"
    );

    let _length_guard = SqliteLengthPreflightGuard::new(conn);
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(
        rusqlite::params![keyset.bound(), row_limit, max_content, page_bytes],
        |row| {
            let native_order: i64 = row.get(1)?;
            let native_message_id: Option<String> = row.get(2)?;
            let message_id_uses: i64 = row.get(3)?;
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
            } else if let Some(native_message_id) = native_message_id {
                goose_tagged_native_message_identity(&native_message_id)
            } else {
                return Err(rusqlite::Error::FromSqlConversionFailure(
                    2,
                    rusqlite::types::Type::Null,
                    std::io::Error::other(
                        "unique Goose native message identity unexpectedly missing",
                    )
                    .into(),
                ));
            };
            let raw_ordinal: i64 = row.get(4)?;
            let source_record_ordinal = u64::try_from(raw_ordinal)
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(4, raw_ordinal))?;
            let outcome = match row.get::<_, i64>(6)? {
                11 => OutputOutcome::Failure,
                12 => OutputOutcome::Timeout,
                13 => OutputOutcome::Unknown,
                14 => OutputOutcome::Success,
                value => {
                    return Err(rusqlite::Error::FromSqlConversionFailure(
                        6,
                        rusqlite::types::Type::Integer,
                        format!("unknown Goose NativePath output outcome {value}").into(),
                    ));
                }
            };
            let raw_content_bytes: i64 = row.get(8)?;
            let content_bytes = u64::try_from(raw_content_bytes)
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(8, raw_content_bytes))?;
            Ok(GooseScannedOutput {
                sqlite_rowid: row.get(0)?,
                native_order,
                source_record_ordinal,
                native_identity,
                provider_message_identity,
                identity_degraded,
                session_identity: row.get(5)?,
                outcome,
                content_json: row.get(7)?,
                content_bytes,
                created_timestamp: row.get(9)?,
                timestamp: row.get(10)?,
            })
        },
    )?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)
}

fn goose_projection_page_budget(
    limits: GooseNativePageLimits,
    duplicated_core_projection: bool,
) -> Result<u64> {
    let units = u64::try_from(limits.rows)
        .map_err(|_| CaptureError::InvalidPayload("Goose page row limit exceeds u64".to_owned()))?;
    let reserved = GOOSE_NATIVE_PAGE_ENVELOPE_BYTES
        .saturating_add(units.saturating_mul(GOOSE_NATIVE_PAGE_UNIT_OVERHEAD_BYTES));
    let available = limits.retained_bytes.checked_sub(reserved).ok_or_else(|| {
        CaptureError::InvalidPayload(
            "Goose page byte limit cannot contain its bounded envelope".to_owned(),
        )
    })?;
    Ok(if duplicated_core_projection {
        available / 2
    } else {
        available
    })
}

pub(super) fn goose_has_native_session_after(conn: &Connection, rowid: i64) -> Result<bool> {
    conn.query_row(
        "select exists(select 1 from sessions where rowid > ?1)",
        [rowid],
        |row| row.get::<_, i64>(0),
    )
    .map(|value| value != 0)
    .map_err(CaptureError::from)
}

pub(super) fn goose_has_native_message_after(conn: &Connection, rowid: i64) -> Result<bool> {
    conn.query_row(
        "select exists(select 1 from messages where rowid > ?1)",
        [rowid],
        |row| row.get::<_, i64>(0),
    )
    .map(|value| value != 0)
    .map_err(CaptureError::from)
}

pub(super) fn goose_has_any_native_message(conn: &Connection) -> Result<bool> {
    conn.query_row("select exists(select 1 from messages)", [], |row| {
        row.get::<_, i64>(0)
    })
    .map(|value| value != 0)
    .map_err(CaptureError::from)
}

pub(super) fn goose_has_native_output_after(conn: &Connection, rowid: i64) -> Result<bool> {
    let content_disposition = goose_native_content_visitor_sql();
    let tokens = "NULL";
    let metadata = "NULL";
    conn.query_row(
        &format!(
            "select exists(
                 select 1
                 from (
                     select
                         m.content_json as content_json,
                         typeof(m.content_json) as content_storage_class,
                         coalesce(octet_length(m.content_json), 0) as content_bytes,
                         coalesce(octet_length({tokens}), 0)
                             + coalesce(octet_length({metadata}), 0) as auxiliary_bytes,
                         s.rowid as parent_rowid
                     from messages m
                     left join sessions s
                       on cast(s.id as text) = cast(m.session_id as text)
                      and trim(cast(s.id as text)) != ''
                     where m.rowid > ?1
                 )
                 where {content_disposition} = 1
                 limit 1
             )"
        ),
        rusqlite::params![rowid, 0_i64, i64::MAX],
        |row| row.get::<_, i64>(0),
    )
    .map(|value| value != 0)
    .map_err(CaptureError::from)
}

pub(super) fn goose_tagged_native_message_identity(message_id: &str) -> String {
    format!(
        "goose-message-identity-v1:message-id:{}:{message_id}",
        message_id.len()
    )
}

fn goose_tagged_fallback_message_identity(native_order: i64) -> String {
    let ordered = format!("{:016x}", (native_order as u64) ^ (1_u64 << 63));
    format!(
        "goose-message-identity-v1:messages-id:{}:{ordered}",
        ordered.len()
    )
}

fn goose_native_content_visitor_sql() -> &'static str {
    // This provider-owned JSON1 visitor is the sole authority for whether a
    // content cell may cross the SQLite/Rust boundary and which retained class
    // normalization receives. Direct object members are visited rather than
    // json_extract'ed so duplicate `type` keys cannot acquire different
    // SQLite/serde semantics. Any direct toolResponse value dominates the
    // entire row; other duplicate type keys fail closed as a local rejection.
    "case
         when parent_rowid is null then 7
         when content_storage_class != 'text' then 8
         when json_valid(content_json) = 0 then 2
         when json_type(content_json) != 'array' then 3
         when exists (
             select 1
             from json_each(content_json) item
             where item.type != 'object'
         ) then 4
         when exists (
             select 1
             from json_each(content_json) item,
                  json_each(item.value) member
             where member.key = 'type'
               and member.type = 'text'
               and member.atom = 'toolResponse'
         ) then 1
         when exists (
             select 1
             from json_each(content_json) item
             where (
                 select count(*)
                 from json_each(item.value) member
                 where member.key = 'type'
             ) > 1
         ) then 10
         when exists (
             select 1
             from json_each(content_json) item
             where (
                 select count(*)
                 from json_each(item.value) member
                 where member.key = 'type'
             ) = 0
                or exists (
                    select 1
                    from json_each(item.value) member
                    where member.key = 'type'
                      and (
                          member.type != 'text'
                          or member.atom not in (
                              'text',
                              'thinking',
                              'redactedThinking',
                              'toolRequest',
                              'frontendToolRequest',
                              'toolConfirmationRequest',
                              'systemNotification',
                              'actionRequired'
                          )
                      )
                )
         ) then 5
         when content_bytes + auxiliary_bytes > ?3 then 6
         when exists (
             select 1
             from json_each(content_json) item,
                  json_each(item.value) member
             where member.key = 'type'
               and member.type = 'text'
               and member.atom in ('toolRequest', 'frontendToolRequest')
         ) then 9
         else 0
     end"
}

fn goose_native_output_outcome_sql() -> &'static str {
    // Outcome classification inspects only structural control fields. Result
    // body values never cross the SQLite/Rust boundary for success or unknown.
    "case
         when exists (
             select 1
             from json_each(content_json) item,
                  json_tree(item.value) node
             where node.key in ('timed_out', 'timedOut', 'timeout')
               and node.type in ('true', 'integer')
               and cast(node.atom as integer) != 0
         ) or exists (
             select 1
             from json_each(content_json) item,
                  json_tree(item.value) node
             where node.key in ('status', 'state', 'outcome')
               and node.type = 'text'
               and lower(trim(cast(node.atom as text)))
                   in ('timeout', 'timed_out', 'timedout')
         ) then 12
         when exists (
             select 1
             from json_each(content_json) item,
                  json_tree(item.value) node
             where (
                    node.key = 'success'
                    and node.type in ('false', 'integer')
                    and cast(node.atom as integer) = 0
                 ) or (
                    node.key in ('isError', 'is_error')
                    and node.type in ('true', 'integer')
                    and cast(node.atom as integer) != 0
                 ) or (
                    node.key in ('exit_code', 'exitCode')
                    and node.type in ('integer', 'real')
                    and cast(node.atom as integer) != 0
                 ) or (
                    node.key in ('status', 'state', 'outcome')
                    and node.type = 'text'
                    and lower(trim(cast(node.atom as text)))
                        in ('failed', 'failure', 'error', 'errored', 'cancelled', 'canceled')
                 ) or (
                    node.key = 'error'
                    and node.type not in ('null', 'false')
                    and (
                        node.type in ('array', 'object')
                        or trim(cast(node.atom as text)) not in ('', '0')
                    )
                 )
         ) then 11
         when exists (
             select 1
             from json_each(content_json) item,
                  json_tree(item.value) node
             where (
                    node.key = 'success'
                    and node.type in ('true', 'integer')
                    and cast(node.atom as integer) != 0
                 ) or (
                    node.key in ('exit_code', 'exitCode')
                    and node.type in ('integer', 'real')
                    and cast(node.atom as integer) = 0
                 ) or (
                    node.key in ('status', 'state', 'outcome')
                    and node.type = 'text'
                    and lower(trim(cast(node.atom as text)))
                        in ('success', 'succeeded', 'complete', 'completed', 'ok', 'passed')
                 )
         ) then 14
         else 13
     end"
}
