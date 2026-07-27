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
    schema::{OpenCodeCapturedShape, OPENCODE_SQLITE_DIALECT},
};

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
    pub(super) native_ordinal: u64,
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
        prior_prefix_evidence: Option<&OpenCodeNativeOrderedPrefixEvidence>,
    ) -> Result<Self> {
        let projection_metrics = register_projection_function(source, profile)?;
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
                    (select count(*) from raw_sessions)
                        + row_number() over (order by source_rowid) - 1,
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

struct OrderedPrefixBuild {
    evidence: OpenCodeNativeOrderedPrefixEvidence,
    comparison: Option<OpenCodeNativeRestartPrefixComparison>,
    session_rows_read: u64,
    event_rows_read: u64,
    pro_rows_read: u64,
}

struct SequencePrefixBuild {
    evidence: OpenCodeNativeSequencePrefixEvidence,
    prior_prefix_matches: bool,
    rows_read: u64,
}

struct SequencePrefixAccumulator<'prior> {
    hasher: Sha256,
    empty_max_key_digest: String,
    count: u64,
    max_key_digest: String,
    prior: Option<&'prior OpenCodeNativeSequencePrefixEvidence>,
    observed_prior_prefix: Option<OpenCodeNativeSequencePrefixEvidence>,
}

impl<'prior> SequencePrefixAccumulator<'prior> {
    fn new(
        domain: &'static [u8],
        prior: Option<&'prior OpenCodeNativeSequencePrefixEvidence>,
    ) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(domain);
        let mut empty_max = Sha256::new();
        empty_max.update(domain);
        empty_max.update(b"empty-max-key");
        let empty_max_key_digest = super::schema::hex_digest(empty_max.finalize().into());
        let observed_prior_prefix = prior.filter(|evidence| evidence.count == 0).map(|_| {
            OpenCodeNativeSequencePrefixEvidence {
                count: 0,
                max_key_digest: empty_max_key_digest.clone(),
                rolling_digest: super::schema::hex_digest(hasher.clone().finalize().into()),
            }
        });
        Self {
            hasher,
            max_key_digest: empty_max_key_digest.clone(),
            empty_max_key_digest,
            count: 0,
            prior,
            observed_prior_prefix,
        }
    }

    fn observe(&mut self, key_digest: [u8; 32], unit_digest: [u8; 32]) -> Result<()> {
        self.count = self
            .count
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "OpenCode prefix evidence count overflowed",
            ))?;
        self.hasher.update(key_digest);
        self.hasher.update(unit_digest);
        self.max_key_digest = super::schema::hex_digest(key_digest);
        if self
            .prior
            .is_some_and(|evidence| evidence.count == self.count)
        {
            self.observed_prior_prefix = Some(OpenCodeNativeSequencePrefixEvidence {
                count: self.count,
                max_key_digest: self.max_key_digest.clone(),
                rolling_digest: super::schema::hex_digest(self.hasher.clone().finalize().into()),
            });
        }
        Ok(())
    }

    fn finish(self, rows_read: u64) -> SequencePrefixBuild {
        let evidence = OpenCodeNativeSequencePrefixEvidence {
            count: self.count,
            max_key_digest: if self.count == 0 {
                self.empty_max_key_digest
            } else {
                self.max_key_digest
            },
            rolling_digest: super::schema::hex_digest(self.hasher.finalize().into()),
        };
        let prior_prefix_matches = self.prior.is_none_or(|prior| {
            self.observed_prior_prefix
                .as_ref()
                .is_some_and(|observed| observed == prior)
        });
        SequencePrefixBuild {
            evidence,
            prior_prefix_matches,
            rows_read,
        }
    }
}

fn compute_ordered_prefix_evidence(
    connection: &Connection,
    prior: Option<&OpenCodeNativeOrderedPrefixEvidence>,
    profile: OpenCodeNativeProfile,
) -> Result<OrderedPrefixBuild> {
    let sessions = compute_session_prefix(connection, prior.map(|value| &value.sessions))?;
    let core_events = compute_core_event_prefix(connection, prior.map(|value| &value.core_events))?;
    let pro_units = match profile {
        OpenCodeNativeProfile::CoreOnly => SequencePrefixAccumulator::new(
            b"ctx-opencode-prefix-pro-units-v1\0",
            prior.map(|value| &value.pro_units),
        )
        .finish(0),
        OpenCodeNativeProfile::CoreAndPro => {
            compute_pro_prefix(connection, prior.map(|value| &value.pro_units))?
        }
    };
    let comparison = prior.map(|prior| OpenCodeNativeRestartPrefixComparison {
        prior_evidence_fingerprint: prior.fingerprint(),
        sessions_prefix_matches: sessions.prior_prefix_matches,
        core_events_prefix_matches: core_events.prior_prefix_matches,
        pro_units_prefix_matches: pro_units.prior_prefix_matches,
    });
    Ok(OrderedPrefixBuild {
        evidence: OpenCodeNativeOrderedPrefixEvidence {
            sessions: sessions.evidence,
            core_events: core_events.evidence,
            pro_units: pro_units.evidence,
        },
        comparison,
        session_rows_read: sessions.rows_read,
        event_rows_read: core_events.rows_read,
        pro_rows_read: pro_units.rows_read,
    })
}

fn compute_session_prefix(
    connection: &Connection,
    prior: Option<&OpenCodeNativeSequencePrefixEvidence>,
) -> Result<SequencePrefixBuild> {
    let mut accumulator =
        SequencePrefixAccumulator::new(b"ctx-opencode-prefix-sessions-v1\0", prior);
    let mut statement = connection.prepare(
        "select native_identity, time_created, content_digest
         from ordered_sessions
         order by scan_ordinal",
    )?;
    let mut rows = statement.query([])?;
    let mut rows_read = 0_u64;
    while let Some(row) = rows.next()? {
        rows_read = rows_read.saturating_add(1);
        let native_identity: String = row.get(0)?;
        let time_created: i64 = row.get(1)?;
        let content_digest: String = row.get(2)?;
        let mut key = Sha256::new();
        key.update(b"session-key");
        key.update(time_created.to_le_bytes());
        hash_str(&mut key, &native_identity);
        let mut unit = Sha256::new();
        unit.update(b"session-unit");
        hash_str(&mut unit, &native_identity);
        hash_str(&mut unit, &content_digest);
        accumulator.observe(key.finalize().into(), unit.finalize().into())?;
    }
    Ok(accumulator.finish(rows_read))
}

fn compute_core_event_prefix(
    connection: &Connection,
    prior: Option<&OpenCodeNativeSequencePrefixEvidence>,
) -> Result<SequencePrefixBuild> {
    let mut accumulator =
        SequencePrefixAccumulator::new(b"ctx-opencode-prefix-core-events-v1\0", prior);
    let mut statement = connection.prepare(
        "select native_identity, session_identity, order_tag, order_a, order_b,
                message_identity, projection
         from ordered_events
         order by scan_ordinal",
    )?;
    let mut rows = statement.query([])?;
    let mut rows_read = 0_u64;
    while let Some(row) = rows.next()? {
        rows_read = rows_read.saturating_add(1);
        let native_identity: String = row.get(0)?;
        let session_identity: String = row.get(1)?;
        let order_tag: i64 = row.get(2)?;
        let order_a: i64 = row.get(3)?;
        let order_b: i64 = row.get(4)?;
        let message_identity: String = row.get(5)?;
        let projection: Vec<u8> = row.get(6)?;
        let mut key = Sha256::new();
        key.update(b"core-event-key");
        hash_str(&mut key, &session_identity);
        key.update(order_tag.to_le_bytes());
        key.update(order_a.to_le_bytes());
        hash_str(&mut key, &message_identity);
        key.update(order_b.to_le_bytes());
        hash_str(&mut key, &native_identity);
        let mut unit = Sha256::new();
        unit.update(b"core-event-unit");
        hash_str(&mut unit, &native_identity);
        unit.update((projection.len() as u64).to_le_bytes());
        unit.update(projection);
        accumulator.observe(key.finalize().into(), unit.finalize().into())?;
    }
    Ok(accumulator.finish(rows_read))
}

fn compute_pro_prefix(
    connection: &Connection,
    prior: Option<&OpenCodeNativeSequencePrefixEvidence>,
) -> Result<SequencePrefixBuild> {
    let mut accumulator =
        SequencePrefixAccumulator::new(b"ctx-opencode-prefix-pro-units-v1\0", prior);
    let mut statement = connection.prepare(
        "select native_identity, subrecord_index, kind, call_id, tool_name, command,
                working_directory, outcome, exit_code, duration_ms, content, rejection
         from ordered_pro_units
         order by pro_ordinal",
    )?;
    let mut rows = statement.query([])?;
    let mut rows_read = 0_u64;
    while let Some(row) = rows.next()? {
        rows_read = rows_read.saturating_add(1);
        let native_identity: String = row.get(0)?;
        let subrecord_index: i64 = row.get(1)?;
        let mut key = Sha256::new();
        key.update(b"pro-unit-key");
        hash_str(&mut key, &native_identity);
        key.update(subrecord_index.to_le_bytes());
        let mut unit = Sha256::new();
        unit.update(b"pro-unit");
        hash_optional_i64(&mut unit, row.get(2)?);
        hash_optional_string(&mut unit, row.get(3)?);
        hash_optional_string(&mut unit, row.get(4)?);
        hash_optional_string(&mut unit, row.get(5)?);
        hash_optional_string(&mut unit, row.get(6)?);
        hash_optional_i64(&mut unit, row.get(7)?);
        hash_optional_i64(&mut unit, row.get(8)?);
        hash_optional_i64(&mut unit, row.get(9)?);
        hash_optional_bytes(&mut unit, row.get(10)?);
        hash_optional_string(&mut unit, row.get(11)?);
        accumulator.observe(key.finalize().into(), unit.finalize().into())?;
    }
    Ok(accumulator.finish(rows_read))
}

fn hash_optional_i64(hasher: &mut Sha256, value: Option<i64>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hasher.update(value.to_le_bytes());
        }
        None => hasher.update([0]),
    }
}

fn hash_optional_string(hasher: &mut Sha256, value: Option<String>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hash_str(hasher, &value);
        }
        None => hasher.update([0]),
    }
}

fn hash_optional_bytes(hasher: &mut Sha256, value: Option<Vec<u8>>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hasher.update((value.len() as u64).to_le_bytes());
            hasher.update(value);
        }
        None => hasher.update([0]),
    }
}

pub(super) fn fetch_session_page(
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

pub(super) fn fetch_event_metadata_page(
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
            native_ordinal: sqlite_nonnegative_u64(row.get::<_, i64>(6)?)?,
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

pub(super) fn fetch_pro_metadata_page(
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
                native_record_ordinal: u64::try_from(row.get::<_, i64>(23)?).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        23,
                        rusqlite::types::Type::Integer,
                        Box::new(error),
                    )
                })?,
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
                session_identity: row.get(6)?,
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

pub(super) fn has_pro_metadata_after(conn: &Connection, pro_ordinal: i64) -> Result<bool> {
    Ok(conn.query_row(
        "select exists(
             select 1 from ordered_pro_units where pro_ordinal > ?1 limit 1
         )",
        [pro_ordinal],
        |row| row.get::<_, i64>(0),
    )? != 0)
}

pub(super) fn pro_keyset_for_frontier(
    conn: &Connection,
    frontier: super::model::OpenCodeNativeProFrontier,
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

fn stage_sessions(
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

fn stage_events(
    source: &Connection,
    schema: &OpenCodeNativeSchema,
    retained_page_bytes: usize,
    profile: OpenCodeNativeProfile,
    index: &mut Connection,
) -> Result<u64> {
    let retained_page_bytes =
        retained_page_bytes.min(super::OPENCODE_CORE_EVENT_PROJECTION_PAGE_BYTES);
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
                OPENCODE_SQLITE_DIALECT.session_message_time_created_field,
            ) {
                let reason = error.to_string();
                projection = encode_rejection_reason(reason.clone());
                decoded = OpenCodeJsonProjection::RejectedWithReason(
                    super::model::OpenCodeNativeRejectionKind::InvalidTimestamp,
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

fn event_source_sql(schema: &OpenCodeNativeSchema, profile: OpenCodeNativeProfile) -> String {
    match schema.family {
        OpenCodeNativeSchemaFamily::SessionMessageSeq => {
            row_event_source_sql(schema, "session_message", true, profile)
        }
        OpenCodeNativeSchemaFamily::SessionMessageSynthesizedSeq => {
            row_event_source_sql(schema, "session_message", false, profile)
        }
        OpenCodeNativeSchemaFamily::SessionEntry => {
            row_event_source_sql(schema, "session_entry", false, profile)
        }
        OpenCodeNativeSchemaFamily::LegacyMessage => {
            row_event_source_sql(schema, "message", false, profile)
        }
        OpenCodeNativeSchemaFamily::MessagePart => part_event_source_sql(schema, profile),
    }
}

fn row_event_source_sql(
    schema: &OpenCodeNativeSchema,
    table: &str,
    explicit_sequence: bool,
    profile: OpenCodeNativeProfile,
) -> String {
    let type_column = type_expression(schema.event_has_type, "x");
    let projection = projection_sql("x.data", &type_column, None, schema.family, "?1", profile);
    let order_a = if explicit_sequence {
        "cast(x.seq as integer)"
    } else {
        "cast(x.time_created as integer)"
    };
    let order_tag = if explicit_sequence { 1 } else { 2 };
    format!(
        "select cast(x.id as text), cast(x.id as text), cast(x.session_id as text),
                {order_tag}, {order_a}, 0,
                case
                    when typeof(x.data) = 'text' and json_valid(x.data)
                         and json_type(x.data, '$.time.created') = 'integer'
                    then cast(json_extract(x.data, '$.time.created') as integer)
                    else cast(x.time_created as integer)
                end,
                cast(x.time_updated as integer),
                case when typeof(x.data) in ('text', 'blob')
                     then octet_length(x.data) else 0 end,
                case when s.id is null
                     then X'{missing_session}'
                     else {projection}
                end,
                case
                    when typeof(x.data) = 'text' and json_valid(x.data)
                         and json_type(x.data, '$.time.created') is not null
                    then 1 else 0
                end,
                x.rowid
         from {table} x
         left join session s on s.id = x.session_id",
        missing_session = hex_bytes(MISSING_SESSION_PROJECTION),
    )
}

fn part_event_source_sql(schema: &OpenCodeNativeSchema, profile: OpenCodeNativeProfile) -> String {
    let type_column = type_expression(schema.event_has_type, "p");
    let projection = projection_sql(
        "p.data",
        &type_column,
        Some("m.data"),
        schema.family,
        "?1",
        profile,
    );
    format!(
        "select cast(p.id as text), cast(p.message_id as text),
                cast(p.session_id as text), 3,
                coalesce(cast(m.time_created as integer), cast(p.time_created as integer)),
                cast(p.time_created as integer),
                cast(p.time_created as integer),
                cast(p.time_updated as integer),
                case when typeof(p.data) in ('text', 'blob')
                     then octet_length(p.data) else 0 end,
                case
                    when m.id is null then X'{missing_message}'
                    when s.id is null then X'{missing_session}'
                    when cast(m.session_id as text) <> cast(p.session_id as text)
                        then X'{relationship_mismatch}'
                    else {projection}
                end,
                0,
                p.rowid
         from part p
         left join message m on m.id = p.message_id
         left join session s on s.id = p.session_id",
        missing_message = hex_bytes(MISSING_MESSAGE_PROJECTION),
        missing_session = hex_bytes(MISSING_SESSION_PROJECTION),
        relationship_mismatch = hex_bytes(RELATIONSHIP_MISMATCH_PROJECTION),
    )
}

fn type_expression(has_type: bool, alias: &str) -> String {
    if has_type {
        format!(
            "case when typeof({alias}.type) = 'text'
                  then lower(substr(trim({alias}.type), 1, {JSON_HINT_BYTES}))
                  else '' end"
        )
    } else {
        "'message'".to_owned()
    }
}

fn decode_order(
    order_tag: i64,
    session_identity: &str,
    message_identity: &str,
    native_identity: &str,
    order_a: i64,
    order_b: i64,
) -> Result<OpenCodeNativeOrder> {
    match order_tag {
        1 => Ok(OpenCodeNativeOrder::ExplicitSequence {
            session_id: session_identity.to_owned(),
            sequence: order_a,
            message_id: message_identity.to_owned(),
        }),
        2 => Ok(OpenCodeNativeOrder::SynthesizedSequence {
            session_id: session_identity.to_owned(),
            time_created: order_a,
            message_id: message_identity.to_owned(),
        }),
        3 => Ok(OpenCodeNativeOrder::MessagePart {
            session_id: session_identity.to_owned(),
            message_time_created: order_a,
            message_id: message_identity.to_owned(),
            part_time_created: order_b,
            part_id: native_identity.to_owned(),
        }),
        _ => Err(CaptureError::SystemInvariant(
            "OpenCode snapshot index contains an unknown order tag",
        )),
    }
}

fn event_digest(
    family: OpenCodeNativeSchemaFamily,
    native_identity: &str,
    native_order: &OpenCodeNativeOrder,
    time_created: i64,
    time_updated: i64,
    retained: &OpenCodeRetainedJson,
) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-opencode-nativepath-retained-event-v2\0");
    hash_str(&mut hasher, family.label());
    hash_str(&mut hasher, native_identity);
    hash_order(&mut hasher, native_order);
    hash_str(&mut hasher, &retained.effective_type);
    hash_str(&mut hasher, &retained.role);
    hasher.update(time_created.to_le_bytes());
    hasher.update(time_updated.to_le_bytes());
    let canonical = serde_json::to_vec(&retained.body).map_err(|error| {
        CaptureError::InvalidPayload(format!(
            "OpenCode retained projection cannot be hashed: {error}"
        ))
    })?;
    hasher.update((canonical.len() as u64).to_le_bytes());
    hasher.update(canonical);
    Ok(super::schema::hex_digest(hasher.finalize().into()))
}

fn native_order_digest(order: &OpenCodeNativeOrder) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-opencode-nativepath-order-v1\0");
    hash_order(&mut hasher, order);
    super::schema::hex_digest(hasher.finalize().into())
}

fn hash_order(hasher: &mut Sha256, order: &OpenCodeNativeOrder) {
    match order {
        OpenCodeNativeOrder::ExplicitSequence {
            session_id,
            sequence,
            message_id,
        } => {
            hasher.update([1]);
            hash_str(hasher, session_id);
            hasher.update(sequence.to_le_bytes());
            hash_str(hasher, message_id);
        }
        OpenCodeNativeOrder::SynthesizedSequence {
            session_id,
            time_created,
            message_id,
        } => {
            hasher.update([2]);
            hash_str(hasher, session_id);
            hasher.update(time_created.to_le_bytes());
            hash_str(hasher, message_id);
        }
        OpenCodeNativeOrder::MessagePart {
            session_id,
            message_time_created,
            message_id,
            part_time_created,
            part_id,
        } => {
            hasher.update([3]);
            hash_str(hasher, session_id);
            hasher.update(message_time_created.to_le_bytes());
            hash_str(hasher, message_id);
            hasher.update(part_time_created.to_le_bytes());
            hash_str(hasher, part_id);
        }
    }
}

fn hash_str(hasher: &mut Sha256, value: &str) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

fn session_digest(values: [&str; 6], time_created: i64, time_updated: i64) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-opencode-nativepath-session-v1\0");
    for value in values {
        hash_str(&mut hasher, value);
    }
    hasher.update(time_created.to_le_bytes());
    hasher.update(time_updated.to_le_bytes());
    super::schema::hex_digest(hasher.finalize().into())
}

fn optional_session_text(columns: &std::collections::BTreeSet<String>, column: &str) -> String {
    if columns.contains(column) {
        format!("case when typeof({column}) = 'text' then cast({column} as text) else '' end")
    } else {
        "''".to_owned()
    }
}

fn session_metadata_preflight_sql(columns: &std::collections::BTreeSet<String>) -> String {
    let optional_lengths = ["parent_id", "title", "directory", "model", "agent"]
        .into_iter()
        .filter(|column| columns.contains(*column))
        .map(|column| {
            format!("case when typeof({column}) = 'text' then octet_length({column}) else 0 end")
        })
        .collect::<Vec<_>>();
    let total = std::iter::once("octet_length(id)".to_owned())
        .chain(optional_lengths)
        .collect::<Vec<_>>()
        .join(" + ");
    format!(
        "select exists(
             select 1 from session
             where typeof(id) <> 'text'
                or octet_length(id) > ?1
                or ({total}) + {SESSION_INDEX_FIXED_BYTES} > ?1
             limit 1
         )"
    )
}

fn session_metadata_bytes(
    identity: &str,
    parent: &str,
    title: &str,
    directory: &str,
    model: &str,
    agent: &str,
) -> Result<i64> {
    let bytes = identity
        .len()
        .checked_add(parent.len())
        .and_then(|bytes| bytes.checked_add(title.len()))
        .and_then(|bytes| bytes.checked_add(directory.len()))
        .and_then(|bytes| bytes.checked_add(model.len()))
        .and_then(|bytes| bytes.checked_add(agent.len()))
        .and_then(|bytes| bytes.checked_add(SESSION_INDEX_FIXED_BYTES as usize))
        .ok_or(CaptureError::SystemInvariant(
            "OpenCode session metadata byte count overflowed",
        ))?;
    i64::try_from(bytes).map_err(|_| {
        CaptureError::InvalidPayload(
            "OpenCode session metadata bytes exceed SQLite integer".to_owned(),
        )
    })
}

fn nonempty(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}

fn table_count(conn: &Connection, table: &str) -> Result<u64> {
    let sql = format!("select count(*) from {table}");
    let count: i64 = conn.query_row(&sql, [], |row| row.get(0))?;
    u64::try_from(count).map_err(|_| {
        CaptureError::InvalidPayload(format!(
            "OpenCode snapshot index table {table} has a negative row count"
        ))
    })
}

fn i64_limit(limit: usize) -> Result<i64> {
    i64::try_from(limit)
        .map_err(|_| CaptureError::SystemInvariant("OpenCode page limit exceeds i64"))
}

fn i64_from_u64(value: u64, label: &'static str) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| CaptureError::InvalidPayload(format!("{label} exceed SQLite integer")))
}

fn native_shape_from_family(family: OpenCodeNativeSchemaFamily) -> OpenCodeCapturedShape {
    match family {
        OpenCodeNativeSchemaFamily::SessionMessageSeq
        | OpenCodeNativeSchemaFamily::SessionMessageSynthesizedSeq => {
            OpenCodeCapturedShape::SessionMessage
        }
        OpenCodeNativeSchemaFamily::SessionEntry => OpenCodeCapturedShape::SessionEntry,
        OpenCodeNativeSchemaFamily::LegacyMessage => OpenCodeCapturedShape::Message,
        OpenCodeNativeSchemaFamily::MessagePart => OpenCodeCapturedShape::MessagePart,
    }
}

fn native_locator(shape: OpenCodeCapturedShape, rowid: i64) -> Result<OpenCodeNativeLocator> {
    let locator = opencode_message_locator(shape, rowid)?;
    Ok(OpenCodeNativeLocator {
        version: 1,
        kind: locator.kind().to_owned(),
        payload: locator.value().to_vec(),
    })
}

fn output_unit_bytes(native_identity: &str, output: &OpenCodeOutputDraft) -> Result<u64> {
    let variable = [
        native_identity.len(),
        output.call_id.as_ref().map_or(0, String::len),
        output.tool_name.as_ref().map_or(0, String::len),
        output.command.as_ref().map_or(0, String::len),
        output.working_directory.as_ref().map_or(0, String::len),
        output.content.len(),
    ]
    .into_iter()
    .try_fold(0_usize, usize::checked_add)
    .ok_or(CaptureError::SystemInvariant(
        "OpenCode output byte accounting overflowed",
    ))?;
    // Includes frontier, locator, source/session/message associations, option/length prefixes,
    // fixed scalar fields, and the maximum validated native identity relationship envelope.
    let bytes = variable
        .checked_add(48 * 1024)
        .ok_or(CaptureError::SystemInvariant(
            "OpenCode output byte accounting overflowed",
        ))?;
    u64::try_from(bytes)
        .map_err(|_| CaptureError::SystemInvariant("OpenCode output bytes exceed u64"))
}

fn rejection_unit_bytes(native_identity: &str, reason: &str) -> Result<u64> {
    let bytes = native_identity
        .len()
        .checked_add(reason.len())
        .and_then(|bytes| bytes.checked_add(48 * 1024))
        .ok_or(CaptureError::SystemInvariant(
            "OpenCode rejection byte accounting overflowed",
        ))?;
    u64::try_from(bytes)
        .map_err(|_| CaptureError::SystemInvariant("OpenCode rejection bytes exceed u64"))
}

fn sqlite_nonnegative_u64(value: i64) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
