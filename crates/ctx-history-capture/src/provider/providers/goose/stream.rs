use rusqlite::Connection;

use crate::native_source::NativeSqliteValue;
use crate::provider::sqlite::SqliteLengthPreflightGuard;
use crate::{CaptureError, OutputOutcome, Result};

use super::position::GooseNativeRowKeyset;
use super::schema::{GooseNativeSchema, GooseSessionRow};

mod identity;

use identity::{goose_native_message_identity, goose_normalized_message_id_sql};
pub(super) use identity::{goose_native_message_identity_at, goose_prepare_native_identity_index};

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
const GOOSE_NATIVE_ACCEPTED_SESSION_TABLE: &str = "goose_native_accepted_sessions";

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
    pub(super) storage_class_supported: bool,
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
    pub(super) logical_row_digest: Option<[u8; 32]>,
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
    pub(super) logical_row_digest: [u8; 32],
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct GooseNativeMessageIdentity {
    pub(super) native_identity: String,
    pub(super) provider_message_identity: String,
    pub(super) identity_degraded: bool,
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
        let logical_row_digest = self
            .logical_row_digest
            .ok_or(CaptureError::SystemInvariant(
                "Goose retained message omitted its logical-row digest",
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
            logical_row_digest,
        })
    }
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
    let storage_class_supported = schema.session_storage_class_predicate("s");
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
                 case when {storage_class_supported} then 1 else 0 end
                     as storage_class_supported,
                 case
                     when typeof(s.id) = 'text'
                          and octet_length(s.id) <= 16384
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
                 case
                     when storage_class_supported = 1 and retained_bytes <= ?3 then 1
                     else 0
                 end as representable
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
             selected.storage_class_supported,
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
            let storage_class_supported = row.get::<_, i64>(3)? != 0;
            let representable = row.get::<_, i64>(4)? != 0;
            Ok(GooseScannedSession {
                sqlite_rowid: row.get(0)?,
                bounded_native_identity: row.get(1)?,
                observed_bytes,
                storage_class_supported,
                row: if representable {
                    Some(GooseSessionRow {
                        id: row.get(5)?,
                        name: row.get(6)?,
                        description: row.get(7)?,
                        user_set_name: row.get::<_, i64>(8)? != 0,
                        session_type: row.get(9)?,
                        working_dir: row.get(10)?,
                        created_at: row.get(11)?,
                        updated_at: row.get(12)?,
                        extension_data: row.get(13)?,
                        total_tokens: row.get(14)?,
                        input_tokens: row.get(15)?,
                        output_tokens: row.get(16)?,
                        accumulated_total_tokens: row.get(17)?,
                        accumulated_input_tokens: row.get(18)?,
                        accumulated_output_tokens: row.get(19)?,
                        accumulated_cost: row.get(20)?,
                        provider_name: row.get(21)?,
                        model_config_json: row.get(22)?,
                        goose_mode: row.get(23)?,
                        archived_at: row.get(24)?,
                        project_id: row.get(25)?,
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
    let normalized_message_id = goose_normalized_message_id_sql(&message_id);
    let storage_class_supported = schema.message_storage_class_predicate("m");
    let keyset_operator = keyset.sql_operator();
    let content_disposition = goose_native_content_visitor_sql();
    let output_outcome = goose_native_output_outcome_sql();
    let sql = format!(
        "with candidates as (
             select
                 m.rowid as sqlite_rowid,
                 cast(m.id as integer) as native_order,
                 {normalized_message_id} as native_message_id,
                 case
                     when typeof({message_id}) in ('null', 'text')
                     then cast({message_id} as text)
                 end as source_message_id,
                 coalesce(ids.uses, 0) as message_id_uses,
                 coalesce(
                     case when typeof(m.session_id) = 'text' then m.session_id end,
                     ''
                 ) as session_identity,
                 accepted.sqlite_rowid as parent_rowid,
                 coalesce(
                     case when typeof(m.role) = 'text' then m.role end,
                     ''
                 ) as role,
                 m.content_json as content_json,
                 typeof(m.content_json) as content_storage_class,
                 coalesce(octet_length(m.content_json), 0) as content_bytes,
                 coalesce(octet_length({tokens}), 0)
                     + coalesce(octet_length({metadata}), 0) as auxiliary_bytes,
                 case
                     when typeof({created_timestamp}) in ('null', 'integer')
                     then {created_timestamp}
                 end as created_timestamp,
                 case
                     when typeof({timestamp}) in ('null', 'text')
                     then {timestamp}
                 end as native_timestamp,
                 case
                     when typeof({tokens}) in ('null', 'integer', 'real')
                     then {tokens}
                 end as tokens_json,
                 case
                     when typeof({metadata}) in ('null', 'text')
                     then {metadata}
                 end as metadata_json,
                 case when {storage_class_supported} then 1 else 0 end
                     as storage_class_supported
             from messages m
             left join temp.{GOOSE_NATIVE_ACCEPTED_SESSION_TABLE} accepted
               on accepted.session_identity = m.session_id
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
             ,
             parent_rowid,
             source_message_id
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
            let identity =
                goose_native_message_identity(native_message_id, message_id_uses, native_order);
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
            let sqlite_rowid: i64 = row.get(0)?;
            let session_identity: String = row.get(4)?;
            let role: String = row.get(5)?;
            let content_json: Option<String> = row.get(7)?;
            let created_timestamp: Option<i64> = row.get(9)?;
            let timestamp: Option<String> = row.get(10)?;
            let tokens_json: Option<String> = row.get(11)?;
            let metadata_json: Option<String> = row.get(12)?;
            let parent_rowid: Option<i64> = row.get(13)?;
            let source_message_id: Option<String> = row.get(14)?;
            let logical_row_digest = content_json
                .as_ref()
                .map(|content_json| {
                    super::content::goose_logical_row_digest(&[
                        parent_rowid.map_or(NativeSqliteValue::Null, NativeSqliteValue::Integer),
                        NativeSqliteValue::Integer(sqlite_rowid),
                        NativeSqliteValue::Integer(native_order),
                        source_message_id
                            .clone()
                            .map_or(NativeSqliteValue::Null, NativeSqliteValue::Text),
                        NativeSqliteValue::Text(session_identity.clone()),
                        NativeSqliteValue::Text(role.clone()),
                        NativeSqliteValue::Text(content_json.clone()),
                        created_timestamp
                            .map_or(NativeSqliteValue::Null, NativeSqliteValue::Integer),
                        timestamp
                            .clone()
                            .map_or(NativeSqliteValue::Null, NativeSqliteValue::Text),
                        tokens_json
                            .clone()
                            .map_or(NativeSqliteValue::Null, NativeSqliteValue::Text),
                        metadata_json
                            .clone()
                            .map_or(NativeSqliteValue::Null, NativeSqliteValue::Text),
                    ])
                })
                .transpose()
                .map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        7,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?;
            Ok(GooseScannedMessage {
                sqlite_rowid,
                native_order,
                native_identity: identity.native_identity,
                provider_message_identity: identity.provider_message_identity,
                identity_degraded: identity.identity_degraded,
                session_identity,
                role,
                disposition,
                output_outcome,
                retained_class,
                content_json,
                content_bytes,
                created_timestamp,
                timestamp,
                tokens_json,
                metadata_json,
                logical_row_digest,
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
    let storage_class_supported = schema.message_storage_class_predicate("m");
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
                 coalesce(
                     case when typeof(m.session_id) = 'text' then m.session_id end,
                     ''
                 ) as session_identity,
                 accepted.sqlite_rowid as parent_rowid,
                 coalesce(
                     case when typeof(m.role) = 'text' then m.role end,
                     ''
                 ) as role,
                 m.content_json as content_json,
                 typeof(m.content_json) as content_storage_class,
                 coalesce(octet_length(m.content_json), 0) as content_bytes,
                 coalesce(octet_length({tokens}), 0)
                     + coalesce(octet_length({metadata}), 0)
                     + coalesce(
                         octet_length(
                             case when typeof(m.session_id) = 'text' then m.session_id end
                         ),
                         0
                     )
                     + coalesce(
                         octet_length(case when typeof(m.role) = 'text' then m.role end),
                         0
                     ) as auxiliary_bytes,
                 case
                     when typeof({created_timestamp}) in ('null', 'integer')
                     then {created_timestamp}
                 end as created_timestamp,
                 case
                     when typeof({timestamp}) in ('null', 'text')
                     then {timestamp}
                 end as native_timestamp,
                 case when {storage_class_supported} then 1 else 0 end
                     as storage_class_supported
             from messages m
             join temp.{GOOSE_NATIVE_MESSAGE_METADATA_TABLE} native
               on native.sqlite_rowid = m.rowid
             left join temp.{GOOSE_NATIVE_ACCEPTED_SESSION_TABLE} accepted
               on accepted.session_identity = m.session_id
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
            let identity =
                goose_native_message_identity(native_message_id, message_id_uses, native_order);
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
                native_identity: identity.native_identity,
                provider_message_identity: identity.provider_message_identity,
                identity_degraded: identity.identity_degraded,
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

pub(super) fn goose_has_native_output_after(
    conn: &Connection,
    schema: &GooseNativeSchema,
    rowid: i64,
) -> Result<bool> {
    let content_disposition = goose_native_content_visitor_sql();
    let storage_class_supported = schema.message_storage_class_predicate("m");
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
                         accepted.sqlite_rowid as parent_rowid,
                         case when {storage_class_supported} then 1 else 0 end
                             as storage_class_supported
                     from messages m
                     left join temp.{GOOSE_NATIVE_ACCEPTED_SESSION_TABLE} accepted
                       on accepted.session_identity = m.session_id
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
         when storage_class_supported = 0 then 8
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
