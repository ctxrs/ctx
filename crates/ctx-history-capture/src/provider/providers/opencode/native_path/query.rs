use rusqlite::{params, Connection};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use crate::{CaptureError, Result, MAX_PROVIDER_SQLITE_VALUE_BYTES};

use super::{
    json::{
        decode_projection, encode_rejection_reason, encode_retained_projection,
        excluded_output_projection, projection_sql, register_projection_function,
        OpenCodeJsonProjection, OpenCodeOutputDraft, OpenCodeRetainedJson,
        MISSING_MESSAGE_PROJECTION, MISSING_SESSION_PROJECTION, OVERSIZED_PROJECTION,
        RELATIONSHIP_MISMATCH_PROJECTION,
    },
    model::{
        OpenCodeNativeLocator, OpenCodeNativeOrder, OpenCodeNativeOrderedPrefixEvidence,
        OpenCodeNativeProfile, OpenCodeNativeRestartPrefixComparison, OpenCodeNativeSchemaFamily,
        OpenCodeNativeSequencePrefixEvidence, OpenCodeNativeSession,
        OPENCODE_NATIVE_PAGE_MAX_BYTES,
    },
    schema::OpenCodeNativeSchema,
};
use crate::provider::normalization::provider_required_timestamp_millis;
use crate::provider::providers::opencode::{
    content_locator::opencode_message_locator,
    schema::{OpenCodeCapturedShape, OpenCodeSqliteDialect},
};

mod fetch;
mod prefix;
mod sql;
mod stage;

pub(super) use fetch::{
    fetch_event_metadata_page, fetch_pro_metadata_page, fetch_session_page, has_pro_metadata_after,
    pro_keyset_for_frontier,
};
use prefix::compute_ordered_prefix_evidence;
use sql::*;
use stage::{stage_events, stage_sessions};

const JSON_HINT_BYTES: usize = 256;
const SESSION_INDEX_FIXED_BYTES: u64 = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ScannedSession {
    pub(super) scan_ordinal: i64,
    pub(super) metadata_prefix_bytes: u64,
    pub(super) row: OpenCodeNativeSession,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct RecordMetadata {
    pub(super) scan_ordinal: i64,
    pub(super) stable_native_ordinal: u64,
    pub(super) legacy_native_ordinal: u64,
    pub(super) source_record_ordinal: u64,
    pub(super) retained_prefix_bytes: u64,
    pub(super) native_identity: String,
    pub(super) message_identity: String,
    pub(super) source_session_identity: String,
    pub(super) native_order: OpenCodeNativeOrder,
    pub(super) time_created: i64,
    pub(super) time_updated: i64,
    pub(super) content_bytes: u64,
    pub(super) content_digest: Option<String>,
    pub(super) projection: OpenCodeJsonProjection,
    pub(super) locator: OpenCodeNativeLocator,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ProRecordMetadata {
    pub(super) pro_ordinal: i64,
    pub(super) output_prefix_bytes: u64,
    pub(super) source_event_ordinal: u64,
    pub(super) native_record_ordinal: u64,
    pub(super) source_record_ordinal: u64,
    pub(super) subrecord_index: u32,
    pub(super) native_identity: String,
    pub(super) source_native_identity: String,
    pub(super) message_identity: String,
    pub(super) session_identity: String,
    pub(super) parent_session_identity: Option<String>,
    pub(super) root_session_identity: String,
    pub(super) session_directory: Option<String>,
    pub(super) agent_identity: Option<String>,
    pub(super) time_created: i64,
    pub(super) draft: Option<OpenCodeOutputDraft>,
    pub(super) rejection: Option<String>,
    pub(super) locator: OpenCodeNativeLocator,
    pub(super) unit_bytes: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct SessionKeyset {
    pub(super) scan_ordinal: i64,
    pub(super) metadata_prefix_bytes: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct EventKeyset {
    pub(super) scan_ordinal: i64,
    pub(super) retained_prefix_bytes: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct ProKeyset {
    pub(super) pro_ordinal: i64,
    pub(super) output_prefix_bytes: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct OpenCodeIndexBuildMetrics {
    pub(super) source_session_rows_scanned: u64,
    pub(super) source_event_rows_scanned: u64,
    pub(super) snapshot_session_rows_indexed: u64,
    pub(super) snapshot_event_rows_indexed: u64,
    pub(super) snapshot_ordering_passes: u64,
    pub(super) prefix_session_rows_read: u64,
    pub(super) prefix_event_rows_read: u64,
    pub(super) prefix_pro_rows_read: u64,
    pub(super) json_records_visited: u64,
    pub(super) json_bytes_visited: u64,
}

pub(super) struct OpenCodeScanIndex {
    connection: Connection,
    _directory: TempDir,
    build_metrics: OpenCodeIndexBuildMetrics,
    ordered_prefix_evidence: OpenCodeNativeOrderedPrefixEvidence,
    restart_prefix_comparison: Option<OpenCodeNativeRestartPrefixComparison>,
}

impl OpenCodeScanIndex {
    pub(super) fn build(
        source: &Connection,
        schema: &OpenCodeNativeSchema,
        retained_page_bytes: usize,
        profile: OpenCodeNativeProfile,
        dialect: &OpenCodeSqliteDialect,
        prior_prefix_evidence: Option<&OpenCodeNativeOrderedPrefixEvidence>,
    ) -> Result<Self> {
        let projection_metrics = register_projection_function(source, profile, dialect)?;
        let directory = tempfile::Builder::new()
            .prefix("ctx-opencode-nativepath-index-")
            .tempdir()?;
        let path = directory.path().join("generation.sqlite");
        let mut connection = Connection::open(&path)?;
        connection.execute_batch(
            "pragma journal_mode = off;
             pragma synchronous = off;
             pragma temp_store = file;
             pragma cache_size = -4096;
             pragma locking_mode = exclusive;
             create table raw_sessions (
                 native_identity text primary key,
                 parent_identity text not null,
                 title text not null,
                 directory text not null,
                 model_identity text not null,
                 agent_identity text not null,
                 time_created integer not null,
                 time_updated integer not null,
                 content_digest text not null,
                 metadata_bytes integer not null
             );
             create table raw_events (
                 native_identity text primary key,
                 message_identity text not null,
                 session_identity text not null,
                 source_rowid integer not null,
                 order_tag integer not null,
                 order_a integer not null,
                 order_b integer not null,
                 time_created integer not null,
                 time_updated integer not null,
                 content_bytes integer not null,
                 projection blob not null,
                 content_digest text,
                 order_digest text not null,
                 retained_bytes integer not null
             );
             create table raw_outputs (
                 native_identity text not null,
                 subrecord_index integer not null,
                 kind integer not null,
                 call_id text,
                 tool_name text,
                 command text,
                 working_directory text,
                 outcome integer not null,
                 exit_code integer,
                 duration_ms integer,
                 content blob not null,
                 unit_bytes integer not null,
                 primary key (native_identity, subrecord_index)
             );
             create table raw_output_rejections (
                 native_identity text not null,
                 subrecord_index integer not null,
                 reason text not null,
                 unit_bytes integer not null,
                 primary key (native_identity, subrecord_index)
             );",
        )?;
        let source_session_rows_scanned =
            stage_sessions(source, schema, retained_page_bytes, &mut connection)?;
        let source_event_rows_scanned = stage_events(
            source,
            schema,
            retained_page_bytes,
            profile,
            dialect,
            &mut connection,
        )?;
        connection.execute_batch(
            "create table ordered_sessions (
                 scan_ordinal integer primary key,
                 metadata_prefix_bytes integer not null,
                 native_identity text not null unique,
                 parent_identity text not null,
                 root_identity text not null,
                 title text not null,
                 directory text not null,
                 model_identity text not null,
                 agent_identity text not null,
                 time_created integer not null,
                 time_updated integer not null,
                 content_digest text not null
             );
             with recursive ancestry(
                 origin_identity, parent_identity, root_identity, depth, visited
             ) as (
                 select native_identity, parent_identity, native_identity, 0,
                        ',' || native_identity || ','
                 from raw_sessions
                 union all
                 select ancestry.origin_identity, parent.parent_identity,
                        parent.native_identity, ancestry.depth + 1,
                        ancestry.visited || parent.native_identity || ','
                 from ancestry
                 join raw_sessions parent
                   on parent.native_identity = ancestry.parent_identity
                 where ancestry.depth < 64
                   and instr(
                       ancestry.visited,
                       ',' || parent.native_identity || ','
                   ) = 0
             ),
             roots as (
                 select origin_identity, root_identity
                 from (
                     select origin_identity, root_identity,
                            row_number() over (
                                partition by origin_identity order by depth desc
                            ) as root_rank
                     from ancestry
                 )
                 where root_rank = 1
             )
             insert into ordered_sessions
             select row_number() over (
                        order by raw_sessions.time_created, raw_sessions.native_identity
                    ),
                    sum(metadata_bytes) over (
                        order by raw_sessions.time_created, raw_sessions.native_identity
                        rows unbounded preceding
                    ),
                    raw_sessions.native_identity, raw_sessions.parent_identity,
                    roots.root_identity, title, directory, model_identity, agent_identity,
                    time_created, time_updated, content_digest
             from raw_sessions
             join roots on roots.origin_identity = raw_sessions.native_identity
             order by raw_sessions.time_created, raw_sessions.native_identity;

             create table ordered_events (
                 scan_ordinal integer primary key,
                 retained_prefix_bytes integer not null,
                 native_identity text not null unique,
                 message_identity text not null,
                 session_identity text not null,
                 source_rowid integer not null,
                 native_ordinal integer not null,
                 order_tag integer not null,
                 order_a integer not null,
                 order_b integer not null,
                 time_created integer not null,
                 time_updated integer not null,
                 content_bytes integer not null,
                 projection blob not null,
                 content_digest text,
                 order_digest text not null
             );
             insert into ordered_events
             select row_number() over (
                        order by session_identity, order_a, message_identity,
                                 order_b, native_identity
                    ),
                    sum(retained_bytes) over (
                        order by session_identity, order_a, message_identity,
                                 order_b, native_identity
                        rows unbounded preceding
                    ),
                    native_identity, message_identity, session_identity, source_rowid,
                    row_number() over (order by source_rowid) - 1,
                    order_tag, order_a, order_b, time_created, time_updated,
                    content_bytes, projection, content_digest, order_digest
             from raw_events
             order by session_identity, order_a, message_identity,
                      order_b, native_identity;

             insert into raw_output_rejections
                 (native_identity, subrecord_index, reason, unit_bytes)
             select output.native_identity, output.subrecord_index,
                    printf(
                        'OpenCode output subrecord %d plus exact session association requires %d encoded bytes',
                        output.subrecord_index,
                        output.unit_bytes + session.metadata_bytes
                    ),
                    49152 + octet_length(output.native_identity)
                          + length(printf('%d', output.subrecord_index))
             from raw_outputs output
             join ordered_events event
               on event.native_identity = output.native_identity
             join raw_sessions session
               on session.native_identity = event.session_identity
             where output.unit_bytes + session.metadata_bytes > 8388608;
             delete from raw_outputs
             where exists (
                 select 1 from raw_output_rejections rejection
                 where rejection.native_identity = raw_outputs.native_identity
                   and rejection.subrecord_index = raw_outputs.subrecord_index
             );
             update raw_outputs
             set unit_bytes = unit_bytes + (
                 select session.metadata_bytes
                 from ordered_events event
                 join raw_sessions session
                   on session.native_identity = event.session_identity
                 where event.native_identity = raw_outputs.native_identity
             );

             create table ordered_pro_units (
                 pro_ordinal integer primary key,
                 output_prefix_bytes integer not null,
                 source_event_ordinal integer not null,
                 subrecord_index integer not null,
                 native_identity text not null,
                 kind integer,
                 call_id text,
                 tool_name text,
                 command text,
                 working_directory text,
                 outcome integer,
                 exit_code integer,
                 duration_ms integer,
                 content blob,
                 rejection text,
                 unit_bytes integer not null
             );
             insert into ordered_pro_units
             select row_number() over (
                        order by source_event_ordinal, subrecord_index
                    ),
                    sum(unit_bytes) over (
                        order by source_event_ordinal, subrecord_index
                        rows unbounded preceding
                    ),
                    source_event_ordinal, subrecord_index, native_identity,
                    kind, call_id, tool_name, command, working_directory,
                    outcome, exit_code, duration_ms, content, rejection, unit_bytes
             from (
                 select e.scan_ordinal as source_event_ordinal,
                        o.subrecord_index, o.native_identity, o.kind,
                        o.call_id, o.tool_name, o.command, o.working_directory,
                        o.outcome, o.exit_code, o.duration_ms, o.content,
                        null as rejection, o.unit_bytes
                 from raw_outputs o
                 join ordered_events e using (native_identity)
                 union all
                 select e.scan_ordinal as source_event_ordinal,
                        r.subrecord_index, r.native_identity,
                        null, null, null, null, null, null, null, null, null,
                        r.reason, r.unit_bytes
                 from raw_output_rejections r
                 join ordered_events e using (native_identity)
             )
             order by source_event_ordinal, subrecord_index;
             create index ordered_pro_units_source
                 on ordered_pro_units(source_event_ordinal, subrecord_index);
             create unique index ordered_sessions_native_identity
                 on ordered_sessions(native_identity);
             create unique index ordered_events_native_identity
                 on ordered_events(native_identity);
             create unique index ordered_pro_units_native_subrecord
                 on ordered_pro_units(native_identity, subrecord_index);
             drop table raw_sessions;
             drop table raw_events;
             drop table raw_outputs;
             drop table raw_output_rejections;",
        )?;
        let snapshot_session_rows_indexed = table_count(&connection, "ordered_sessions")?;
        let snapshot_event_rows_indexed = table_count(&connection, "ordered_events")?;
        if snapshot_session_rows_indexed != source_session_rows_scanned
            || snapshot_event_rows_indexed != source_event_rows_scanned
        {
            return Err(CaptureError::SystemInvariant(
                "OpenCode snapshot index did not preserve source row cardinality",
            ));
        }
        let prefix = compute_ordered_prefix_evidence(&connection, prior_prefix_evidence, profile)?;
        connection.pragma_update(None, "query_only", true)?;
        Ok(Self {
            connection,
            _directory: directory,
            build_metrics: OpenCodeIndexBuildMetrics {
                source_session_rows_scanned,
                source_event_rows_scanned,
                snapshot_session_rows_indexed,
                snapshot_event_rows_indexed,
                snapshot_ordering_passes: match profile {
                    OpenCodeNativeProfile::CoreOnly => 2,
                    OpenCodeNativeProfile::CoreAndPro => 3,
                },
                prefix_session_rows_read: prefix.session_rows_read,
                prefix_event_rows_read: prefix.event_rows_read,
                prefix_pro_rows_read: prefix.pro_rows_read,
                json_records_visited: projection_metrics.records(),
                json_bytes_visited: projection_metrics.bytes(),
            },
            ordered_prefix_evidence: prefix.evidence,
            restart_prefix_comparison: prefix.comparison,
        })
    }

    pub(super) fn connection(&self) -> &Connection {
        &self.connection
    }

    pub(super) fn build_metrics(&self) -> OpenCodeIndexBuildMetrics {
        self.build_metrics
    }

    pub(super) fn ordered_prefix_evidence(&self) -> &OpenCodeNativeOrderedPrefixEvidence {
        &self.ordered_prefix_evidence
    }

    pub(super) fn restart_prefix_comparison(
        &self,
    ) -> Option<&OpenCodeNativeRestartPrefixComparison> {
        self.restart_prefix_comparison.as_ref()
    }

    #[cfg(test)]
    pub(super) fn event_page_query_plan(&self) -> Result<Vec<String>> {
        let mut statement = self.connection.prepare(
            "explain query plan
             select scan_ordinal from ordered_events
             where scan_ordinal > ?1
               and scan_ordinal <= ?2
               and retained_prefix_bytes <= ?3
             order by scan_ordinal",
        )?;
        let rows = statement.query_map(params![0_i64, 512_i64, i64::MAX], |row| row.get(3))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(CaptureError::from)
    }
}
