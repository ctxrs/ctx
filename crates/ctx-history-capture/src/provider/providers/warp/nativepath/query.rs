#[cfg(test)]
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashSet};

use chrono::{DateTime, NaiveDateTime, Utc};
use rusqlite::{types::ValueRef, Connection, OptionalExtension, Statement};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::super::schema::{warp_quote_identifier, WarpSqliteSchema};
use super::decode::{
    decode_warp_native_task, WarpDecodeCounters, WarpDecodedMessage, WarpDecodedMessagePayload,
    WarpOutputLocalFailureKind, WarpProOutputPayload,
};
use super::publication::{
    finish_core_hasher, hex_digest, new_core_hasher, WarpNativeCounters, WarpNativeDigestChain,
    WarpNativeEvent, WarpNativeEventDraft, WarpNativeFrontier, WarpNativeFrontierPhase,
    WarpNativeHierarchyEdge, WarpNativeMessageIdentity, WarpNativeOutputRejection,
    WarpNativeOutputRejectionKind, WarpNativePageAccumulator, WarpNativeProPageAccumulator,
    WarpNativeProfile, WarpNativeRejection, WarpNativeRejectionKind, WarpNativeSession,
    WarpNativeSink, WarpNativeUnit, WARP_NATIVE_PAGE_MAX_BYTES, WARP_SOURCE_DIGEST_DOMAIN,
};
use crate::provider::sqlite::SqliteLengthPreflightGuard;
use crate::{
    complete_content::CompleteContentBodyDigest, CaptureError, OutputAssociations,
    OutputNativeCoordinate, OutputObservationKind, OutputOutcome, OutputOutcomeMetadata,
    OutputSourceLocator, ProOutputObservation, Result, MAX_PROVIDER_SQLITE_VALUE_BYTES,
};

const WARP_SESSION_METADATA_MAX_BYTES: usize = 64 * 1024;
const WARP_ORDERING_KEY_MAX_BYTES: usize = 240 * 1024;
const WARP_CONTENT_LOCATOR_KIND: &str = "warp-task-message-v1";
const WARP_NATIVE_SQLITE_CONVERSATION_ROW_OVERHEAD_BYTES: u64 = 64 * 4;
const WARP_NATIVE_SQLITE_ROW_OVERHEAD_BYTES: u64 = 64 * 5;

#[cfg(test)]
thread_local! {
    static WARP_NATIVE_TASK_HYDRATION_ROWIDS: RefCell<Option<Vec<i64>>> =
        const { RefCell::new(None) };
}

#[derive(Debug)]
struct WarpHierarchyNode {
    parent_conversation_id: Option<String>,
    root_conversation_id: String,
    root_resolved: bool,
    parent_present: bool,
    title: String,
    modified_at: Option<DateTime<Utc>>,
    metadata: Value,
    rejections: Vec<WarpNativeRejection>,
}

#[derive(Debug)]
enum WarpConversationEmission {
    Session {
        conversation_id: String,
        rowid: i64,
        source_digest: [u8; 32],
    },
    Rejection {
        rejection: WarpNativeRejection,
        rowid: i64,
        source_digest: [u8; 32],
    },
}

#[derive(Debug)]
struct WarpTaskCellMetadata {
    storage_class: String,
    bytes: i64,
}

#[derive(Debug)]
struct WarpConversationCandidate {
    rowid: i64,
    conversation_id: WarpTaskCellMetadata,
    conversation_data: WarpTaskCellMetadata,
    last_modified_at: WarpTaskCellMetadata,
    hydrated_conversation_id: Option<String>,
    hydrated_conversation_data: Option<String>,
    hydrated_last_modified_at: Option<String>,
}

#[derive(Debug)]
struct WarpTaskCandidate {
    rowid: i64,
    conversation_id: WarpTaskCellMetadata,
    task_id: WarpTaskCellMetadata,
    task: WarpTaskCellMetadata,
    last_modified_at: WarpTaskCellMetadata,
}

pub(super) struct WarpNativeQueryResult {
    pub(super) counters: WarpNativeCounters,
    pub(super) source_integrity_digest: String,
    pub(super) core_generation_digest: String,
    pub(super) eof: WarpNativeEof,
    pub(super) pages_emitted: u64,
    pub(super) pro_output_pages_emitted: u64,
}

/// Opaque proof that the pinned immutable snapshot query reached exact EOF.
///
/// The scanner constructs this only after both authoritative table traversals
/// finish and every pending page is accepted. The outer transaction wrapper
/// releases it only after the read transaction also closes cleanly.
pub(super) struct WarpNativeEof {
    frontier: WarpNativeFrontier,
}

impl WarpNativeEof {
    pub(super) fn frontier(&self) -> &WarpNativeFrontier {
        &self.frontier
    }

    pub(super) fn into_frontier(self) -> WarpNativeFrontier {
        self.frontier
    }
}

pub(super) fn scan_warp_native_snapshot(
    conn: &Connection,
    schema: &WarpSqliteSchema,
    profile: WarpNativeProfile,
    resume_frontier: Option<WarpNativeFrontier>,
    sink: &mut dyn WarpNativeSink,
) -> Result<WarpNativeQueryResult> {
    conn.execute_batch("begin")?;
    let result = scan_warp_native_snapshot_inner(conn, schema, profile, resume_frontier, sink);
    let rollback = conn.execute_batch("rollback");
    match (result, rollback) {
        (Ok(result), Ok(())) => Ok(result),
        (Err(error), Ok(())) => Err(error),
        (_, Err(error)) => Err(CaptureError::from(error)),
    }
}

fn scan_warp_native_snapshot_inner(
    conn: &Connection,
    schema: &WarpSqliteSchema,
    profile: WarpNativeProfile,
    resume_frontier: Option<WarpNativeFrontier>,
    sink: &mut dyn WarpNativeSink,
) -> Result<WarpNativeQueryResult> {
    let resume_frontier = resume_frontier.unwrap_or_default();
    let mut builder = WarpNativePageEmitter::new(sink, profile, resume_frontier.clone());
    let mut counters = WarpNativeCounters::default();

    let hierarchy = if resume_frontier.phase == WarpNativeFrontierPhase::Tasks {
        load_task_hierarchy(
            conn,
            schema,
            &resume_frontier,
            BTreeMap::new(),
            &mut counters,
        )?
    } else {
        let (hierarchy, conversation_emissions) =
            scan_conversations(conn, &mut counters, &resume_frontier)?;
        counters.hierarchy_nodes_retained = u64::try_from(hierarchy.len()).unwrap_or(u64::MAX);
        counters.hierarchy_edges = hierarchy
            .values()
            .filter(|node| node.parent_conversation_id.is_some())
            .count()
            .try_into()
            .unwrap_or(u64::MAX);
        emit_sessions_and_hierarchy(
            &hierarchy,
            conversation_emissions,
            &mut builder,
            &mut counters,
            &resume_frontier,
        )?;
        if resume_frontier.phase == WarpNativeFrontierPhase::Conversations {
            load_task_hierarchy(conn, schema, &resume_frontier, hierarchy, &mut counters)?
        } else {
            hierarchy
        }
    };
    scan_tasks(
        conn,
        schema,
        &hierarchy,
        &mut builder,
        &mut counters,
        profile,
        &resume_frontier,
    )?;
    let (
        source_integrity_digest,
        core_generation_digest,
        final_frontier,
        pages_emitted,
        pro_output_pages_emitted,
    ) = builder.finish()?;
    Ok(WarpNativeQueryResult {
        counters,
        source_integrity_digest,
        core_generation_digest,
        eof: WarpNativeEof {
            frontier: final_frontier,
        },
        pages_emitted,
        pro_output_pages_emitted,
    })
}

struct WarpNativePageEmitter<'a> {
    sink: &'a mut dyn WarpNativeSink,
    page: WarpNativePageAccumulator,
    current_frontier: WarpNativeFrontier,
    pro_page: Option<WarpNativeProPageAccumulator>,
    pro_current_frontier: WarpNativeFrontier,
    source_hasher: WarpNativeDigestChain,
    core_hasher: WarpNativeDigestChain,
    pages_emitted: u64,
    pro_output_pages_emitted: u64,
}

impl<'a> WarpNativePageEmitter<'a> {
    fn new(
        sink: &'a mut dyn WarpNativeSink,
        profile: WarpNativeProfile,
        mut current_frontier: WarpNativeFrontier,
    ) -> Self {
        let source_hasher =
            WarpNativeDigestChain::new(WARP_SOURCE_DIGEST_DOMAIN, current_frontier.source_digest);
        let core_hasher = new_core_hasher(current_frontier.core_digest);
        current_frontier.source_digest = source_hasher.state();
        current_frontier.core_digest = core_hasher.state();
        Self {
            sink,
            page: WarpNativePageAccumulator::new(current_frontier.clone()),
            current_frontier: current_frontier.clone(),
            pro_page: profile
                .wants_transient_outputs()
                .then(|| WarpNativeProPageAccumulator::new(current_frontier.clone())),
            pro_current_frontier: current_frontier,
            source_hasher,
            core_hasher,
            pages_emitted: 0,
            pro_output_pages_emitted: 0,
        }
    }

    fn record_source(&mut self, label: &[u8], digest: [u8; 32]) -> Result<()> {
        self.source_hasher.push(label, digest)
    }

    fn frontier(&self) -> &WarpNativeFrontier {
        &self.current_frontier
    }

    fn push(
        &mut self,
        mut unit: WarpNativeUnit,
        next_frontier: WarpNativeFrontier,
        native_key: String,
        counters: &mut WarpNativeCounters,
    ) -> Result<()> {
        if unit.estimated_bytes() > WARP_NATIVE_PAGE_MAX_BYTES {
            counters.oversized_normalized_units =
                counters.oversized_normalized_units.saturating_add(1);
            unit = unit.into_oversized_rejection(
                WarpNativeRejectionKind::OversizedNormalizedUnit,
                native_key,
            )?;
        }
        let (core_unit, pro_unit) = unit.into_lanes();
        if !self.page.can_accept(&core_unit) {
            self.flush_core()?;
        }
        let mut next_frontier = next_frontier;
        next_frontier.source_digest = self.source_hasher.state();
        let next_frontier = self
            .page
            .push(core_unit, next_frontier, &mut self.core_hasher)?;
        self.current_frontier = next_frontier.clone();
        if self.page.is_full() {
            self.flush_core()?;
        }

        if let Some(page) = self.pro_page.as_ref() {
            if !page.can_accept(&pro_unit) {
                self.flush_pro()?;
            }
            let page = self.pro_page.as_mut().ok_or(CaptureError::SystemInvariant(
                "Warp NativePath Pro page disappeared during fanout",
            ))?;
            page.push(pro_unit, next_frontier.clone())?;
            self.pro_current_frontier = next_frontier;
            if page.is_full() {
                self.flush_pro()?;
            }
        }
        Ok(())
    }

    fn finish(mut self) -> Result<(String, String, WarpNativeFrontier, u64, u64)> {
        self.flush_core()?;
        self.flush_pro()?;
        Ok((
            hex_digest(self.source_hasher.state()),
            finish_core_hasher(&self.core_hasher),
            self.current_frontier,
            self.pages_emitted,
            self.pro_output_pages_emitted,
        ))
    }

    fn flush_core(&mut self) -> Result<()> {
        let next = WarpNativePageAccumulator::new(self.current_frontier.clone());
        let page = std::mem::replace(&mut self.page, next);
        let Some(page) = page.finish()? else {
            return Ok(());
        };
        self.sink.push_page(page)?;
        self.pages_emitted =
            self.pages_emitted
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Warp NativePath page count overflowed",
                ))?;
        Ok(())
    }

    fn flush_pro(&mut self) -> Result<()> {
        let Some(page) = self.pro_page.as_mut() else {
            return Ok(());
        };
        let next = WarpNativeProPageAccumulator::new(self.pro_current_frontier.clone());
        let page = std::mem::replace(page, next);
        let Some(page) = page.finish()? else {
            return Ok(());
        };
        let expected_receipt = page.receipt();
        let receipt = self.sink.push_pro_output_page(page);
        if receipt != expected_receipt {
            return Err(CaptureError::SystemInvariant(
                "Warp NativePath Pro page receipt did not match the emitted page",
            ));
        }
        self.pro_output_pages_emitted =
            self.pro_output_pages_emitted
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Warp NativePath Pro page count overflowed",
                ))?;
        Ok(())
    }
}

fn scan_conversations(
    conn: &Connection,
    counters: &mut WarpNativeCounters,
    resume: &WarpNativeFrontier,
) -> Result<(
    BTreeMap<String, WarpHierarchyNode>,
    Vec<WarpConversationEmission>,
)> {
    let _guard = SqliteLengthPreflightGuard::new(conn);
    let mut statement = prepare_conversation_candidates(conn)?;
    let limit = conversation_hydration_limit()?;
    let after_rowid = if resume.phase == WarpNativeFrontierPhase::Conversations {
        resume.last_conversation_rowid.ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Warp conversation resume frontier omitted its rowid".to_owned(),
            )
        })?
    } else {
        0
    };
    let mut rows = statement.query(rusqlite::params![limit, after_rowid])?;
    let mut hierarchy = BTreeMap::new();
    let mut emissions = Vec::new();
    while let Some(row) = rows.next()? {
        counters.conversation_rows = counters.conversation_rows.saturating_add(1);
        let candidate = conversation_candidate_from_row(row)?;
        if let Some(rejection) = reject_conversation_candidate(&candidate)? {
            emissions.push(WarpConversationEmission::Rejection {
                rejection,
                rowid: candidate.rowid,
                source_digest: rejected_conversation_candidate_digest(&candidate)?,
            });
            continue;
        }
        let (Some(conversation_id), Some(raw_data), Some(raw_modified)) = (
            candidate.hydrated_conversation_id,
            candidate.hydrated_conversation_data,
            candidate.hydrated_last_modified_at,
        ) else {
            return Err(CaptureError::SystemInvariant(
                "Warp conversation passed preflight without bounded hydrated values",
            ));
        };
        counters.conversation_rows_hydrated = counters.conversation_rows_hydrated.saturating_add(1);
        let evidence_digest = source_text_row_digest(
            b"conversation\0",
            [&conversation_id, &raw_data, &raw_modified],
        )?;
        counters.conversation_json_objects_parsed =
            counters.conversation_json_objects_parsed.saturating_add(1);
        let mut rejections = Vec::new();
        let conversation_data =
            parse_conversation_data(&raw_data, &conversation_id, &mut rejections);
        let modified_at =
            parse_optional_conversation_timestamp(&raw_modified, &conversation_id, &mut rejections);
        let parent_conversation_id = conversation_data
            .get("parent_conversation_id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .filter(|value| !value.is_empty());
        let title = conversation_data
            .get("agent_name")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(|value| truncate_chars(value, 240))
            .unwrap_or_else(|| format!("Warp {conversation_id}"));
        let metadata = bounded_session_metadata(&conversation_data)?;
        counters.peak_session_metadata_rows = counters.peak_session_metadata_rows.max(1);
        let node = WarpHierarchyNode {
            parent_conversation_id,
            root_conversation_id: conversation_id.clone(),
            root_resolved: false,
            parent_present: false,
            title,
            modified_at,
            metadata,
            rejections,
        };
        if hierarchy.insert(conversation_id.clone(), node).is_some() {
            return Err(CaptureError::InvalidPayload(format!(
                "Warp source contains duplicate conversation identity {conversation_id:?}"
            )));
        }
        emissions.push(WarpConversationEmission::Session {
            conversation_id,
            rowid: candidate.rowid,
            source_digest: evidence_digest,
        });
    }
    let pending = hierarchy
        .values()
        .filter_map(|node| node.parent_conversation_id.clone())
        .filter(|parent| !hierarchy.contains_key(parent))
        .collect::<Vec<_>>();
    load_hierarchy_closure(conn, pending, &mut hierarchy, counters)?;
    resolve_hierarchy(&mut hierarchy)?;
    Ok((hierarchy, emissions))
}

fn load_task_hierarchy(
    conn: &Connection,
    schema: &WarpSqliteSchema,
    resume: &WarpNativeFrontier,
    mut hierarchy: BTreeMap<String, WarpHierarchyNode>,
    counters: &mut WarpNativeCounters,
) -> Result<BTreeMap<String, WarpHierarchyNode>> {
    let task_resume = resume.phase == WarpNativeFrontierPhase::Tasks;
    let comparison = if !task_resume {
        "1 = 1"
    } else if resume.next_message_ordinal == 0 {
        "t.task_id collate binary > (
             select previous.task_id from agent_tasks previous
             where previous.rowid = ?1
         )"
    } else {
        "t.rowid = ?1 or t.task_id collate binary > (
             select previous.task_id from agent_tasks previous
             where previous.rowid = ?1
         )"
    };
    let index = warp_quote_identifier(&schema.task_keyset_index);
    let mut task_conversations = conn.prepare(&format!(
        "select distinct t.conversation_id
         from agent_tasks t indexed by {index}
         where typeof(t.conversation_id) = 'text'
           and ({comparison})
         order by t.conversation_id collate binary"
    ))?;
    let mut pending = Vec::new();
    {
        let _guard = SqliteLengthPreflightGuard::new(conn);
        let mut rows = if task_resume {
            let last_task_rowid = resume.last_task_rowid.ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "Warp task resume frontier omitted its rowid".to_owned(),
                )
            })?;
            task_conversations.query([last_task_rowid])?
        } else {
            task_conversations.query([])?
        };
        while let Some(row) = rows.next()? {
            pending.push(row.get::<_, String>(0)?);
        }
    }
    load_hierarchy_closure(conn, pending, &mut hierarchy, counters)?;
    resolve_hierarchy(&mut hierarchy)?;
    counters.hierarchy_nodes_retained = u64::try_from(hierarchy.len()).unwrap_or(u64::MAX);
    counters.hierarchy_edges = hierarchy
        .values()
        .filter(|node| node.parent_conversation_id.is_some())
        .count()
        .try_into()
        .unwrap_or(u64::MAX);
    Ok(hierarchy)
}

fn load_hierarchy_closure(
    conn: &Connection,
    mut pending: Vec<String>,
    hierarchy: &mut BTreeMap<String, WarpHierarchyNode>,
    counters: &mut WarpNativeCounters,
) -> Result<()> {
    let limit = conversation_hydration_limit()?;
    let mut conversation = conn.prepare(
        "select rowid, \
                typeof(conversation_id), coalesce(octet_length(conversation_id), 0), \
                typeof(conversation_data), coalesce(octet_length(conversation_data), 0), \
                typeof(last_modified_at), coalesce(octet_length(last_modified_at), 0), \
                case when typeof(conversation_id) = 'text' \
                           and typeof(conversation_data) = 'text' \
                           and typeof(last_modified_at) = 'text' \
                           and coalesce(octet_length(conversation_id), 0) \
                             + coalesce(octet_length(conversation_data), 0) \
                             + coalesce(octet_length(last_modified_at), 0) <= ?2 \
                     then conversation_id end, \
                case when typeof(conversation_id) = 'text' \
                           and typeof(conversation_data) = 'text' \
                           and typeof(last_modified_at) = 'text' \
                           and coalesce(octet_length(conversation_id), 0) \
                             + coalesce(octet_length(conversation_data), 0) \
                             + coalesce(octet_length(last_modified_at), 0) <= ?2 \
                     then conversation_data end, \
                case when typeof(conversation_id) = 'text' \
                           and typeof(conversation_data) = 'text' \
                           and typeof(last_modified_at) = 'text' \
                           and coalesce(octet_length(conversation_id), 0) \
                             + coalesce(octet_length(conversation_data), 0) \
                             + coalesce(octet_length(last_modified_at), 0) <= ?2 \
                     then last_modified_at end \
         from agent_conversations
         where conversation_id = ?1 collate binary
         limit 1",
    )?;
    let mut seen = hierarchy.keys().cloned().collect::<BTreeSet<_>>();
    while let Some(requested_id) = pending.pop() {
        if !seen.insert(requested_id.clone()) {
            continue;
        }
        let _guard = SqliteLengthPreflightGuard::new(conn);
        let candidate = conversation
            .query_row(
                rusqlite::params![requested_id, limit],
                conversation_candidate_from_row,
            )
            .optional()?;
        let Some(candidate) = candidate else {
            continue;
        };
        counters.conversation_rows = counters.conversation_rows.saturating_add(1);
        if reject_conversation_candidate(&candidate)?.is_some() {
            continue;
        }
        let (Some(conversation_id), Some(raw_data), Some(raw_modified)) = (
            candidate.hydrated_conversation_id,
            candidate.hydrated_conversation_data,
            candidate.hydrated_last_modified_at,
        ) else {
            return Err(CaptureError::SystemInvariant(
                "Warp resume conversation passed preflight without bounded values",
            ));
        };
        counters.conversation_rows_hydrated = counters.conversation_rows_hydrated.saturating_add(1);
        counters.conversation_json_objects_parsed =
            counters.conversation_json_objects_parsed.saturating_add(1);
        let mut rejections = Vec::new();
        let conversation_data =
            parse_conversation_data(&raw_data, &conversation_id, &mut rejections);
        let modified_at =
            parse_optional_conversation_timestamp(&raw_modified, &conversation_id, &mut rejections);
        let parent_conversation_id = conversation_data
            .get("parent_conversation_id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .filter(|value| !value.is_empty());
        if let Some(parent) = parent_conversation_id.as_ref() {
            pending.push(parent.clone());
        }
        let title = conversation_data
            .get("agent_name")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(|value| truncate_chars(value, 240))
            .unwrap_or_else(|| format!("Warp {conversation_id}"));
        let metadata = bounded_session_metadata(&conversation_data)?;
        counters.peak_session_metadata_rows = counters.peak_session_metadata_rows.max(1);
        hierarchy.insert(
            conversation_id.clone(),
            WarpHierarchyNode {
                parent_conversation_id,
                root_conversation_id: conversation_id,
                root_resolved: false,
                parent_present: false,
                title,
                modified_at,
                metadata,
                rejections,
            },
        );
    }
    Ok(())
}

fn prepare_conversation_candidates(conn: &Connection) -> Result<Statement<'_>> {
    conn.prepare(
        "select rowid, \
                typeof(conversation_id), coalesce(octet_length(conversation_id), 0), \
                typeof(conversation_data), coalesce(octet_length(conversation_data), 0), \
                typeof(last_modified_at), coalesce(octet_length(last_modified_at), 0), \
                case when typeof(conversation_id) = 'text' \
                           and typeof(conversation_data) = 'text' \
                           and typeof(last_modified_at) = 'text' \
                           and coalesce(octet_length(conversation_id), 0) \
                             + coalesce(octet_length(conversation_data), 0) \
                             + coalesce(octet_length(last_modified_at), 0) <= ?1 \
                     then conversation_id end, \
                case when typeof(conversation_id) = 'text' \
                           and typeof(conversation_data) = 'text' \
                           and typeof(last_modified_at) = 'text' \
                           and coalesce(octet_length(conversation_id), 0) \
                             + coalesce(octet_length(conversation_data), 0) \
                             + coalesce(octet_length(last_modified_at), 0) <= ?1 \
                     then conversation_data end, \
                case when typeof(conversation_id) = 'text' \
                           and typeof(conversation_data) = 'text' \
                           and typeof(last_modified_at) = 'text' \
                           and coalesce(octet_length(conversation_id), 0) \
                             + coalesce(octet_length(conversation_data), 0) \
                             + coalesce(octet_length(last_modified_at), 0) <= ?1 \
                     then last_modified_at end \
         from agent_conversations \
         where rowid > ?2 \
         order by rowid",
    )
    .map_err(CaptureError::from)
}

fn conversation_hydration_limit() -> Result<i64> {
    let maximum = u64::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).map_err(|_| {
        CaptureError::SystemInvariant("Warp NativePath SQLite byte limit exceeds u64")
    })?;
    let payload = maximum
        .checked_sub(WARP_NATIVE_SQLITE_CONVERSATION_ROW_OVERHEAD_BYTES)
        .ok_or(CaptureError::SystemInvariant(
            "Warp NativePath conversation row overhead exceeds its byte limit",
        ))?;
    i64::try_from(payload).map_err(|_| {
        CaptureError::SystemInvariant("Warp NativePath conversation byte limit exceeds i64")
    })
}

fn conversation_candidate_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<WarpConversationCandidate> {
    Ok(WarpConversationCandidate {
        rowid: row.get(0)?,
        conversation_id: WarpTaskCellMetadata {
            storage_class: row.get(1)?,
            bytes: row.get(2)?,
        },
        conversation_data: WarpTaskCellMetadata {
            storage_class: row.get(3)?,
            bytes: row.get(4)?,
        },
        last_modified_at: WarpTaskCellMetadata {
            storage_class: row.get(5)?,
            bytes: row.get(6)?,
        },
        hydrated_conversation_id: row.get(7)?,
        hydrated_conversation_data: row.get(8)?,
        hydrated_last_modified_at: row.get(9)?,
    })
}

fn emit_sessions_and_hierarchy(
    hierarchy: &BTreeMap<String, WarpHierarchyNode>,
    emissions: Vec<WarpConversationEmission>,
    builder: &mut WarpNativePageEmitter<'_>,
    counters: &mut WarpNativeCounters,
    resume: &WarpNativeFrontier,
) -> Result<()> {
    let mut completed_conversations = resume.completed_conversation_rows;
    let mut completed_edges = resume.completed_hierarchy_edges;
    for emission in emissions {
        let mut unit = WarpNativeUnit::progress();
        let (native_key, rowid, source_digest) = match emission {
            WarpConversationEmission::Session {
                conversation_id,
                rowid,
                source_digest,
            } => {
                let node = hierarchy.get(&conversation_id).ok_or_else(|| {
                    CaptureError::InvalidPayload(format!(
                        "Warp hierarchy index omitted conversation {conversation_id:?}"
                    ))
                })?;
                for rejection in &node.rejections {
                    unit.push_rejection(rejection.clone())?;
                }
                unit.push_session(WarpNativeSession {
                    conversation_id: conversation_id.clone(),
                    parent_conversation_id: node.parent_conversation_id.clone(),
                    root_conversation_id: node.root_conversation_id.clone(),
                    parent_present: node.parent_present,
                    title: node.title.clone(),
                    modified_at: node.modified_at,
                    metadata: node.metadata.clone(),
                })?;
                if let Some(parent) = node.parent_conversation_id.as_ref() {
                    unit.push_edge(WarpNativeHierarchyEdge {
                        child_conversation_id: conversation_id.clone(),
                        parent_conversation_id: parent.clone(),
                        parent_present: node.parent_present,
                    })?;
                    completed_edges = completed_edges.saturating_add(1);
                }
                counters.sessions_retained = counters.sessions_retained.saturating_add(1);
                (conversation_id, rowid, source_digest)
            }
            WarpConversationEmission::Rejection {
                rejection,
                rowid,
                source_digest,
            } => {
                let native_key = rejection.native_key.clone();
                unit.push_rejection(rejection)?;
                (native_key, rowid, source_digest)
            }
        };
        builder.record_source(b"conversation\0", source_digest)?;
        completed_conversations = completed_conversations.saturating_add(1);
        builder.push(
            unit,
            WarpNativeFrontier::after_conversation(completed_conversations, completed_edges, rowid),
            native_key,
            counters,
        )?;
    }
    Ok(())
}

fn reject_conversation_candidate(
    candidate: &WarpConversationCandidate,
) -> Result<Option<WarpNativeRejection>> {
    let native_key = format!("rowid:{}", candidate.rowid);
    for (field, metadata) in [
        ("conversation_id", &candidate.conversation_id),
        ("conversation_data", &candidate.conversation_data),
        ("last_modified_at", &candidate.last_modified_at),
    ] {
        if metadata.storage_class != "text" {
            return Ok(Some(WarpNativeRejection {
                kind: WarpNativeRejectionKind::ConversationRecord,
                native_key,
                reason: format!(
                    "Warp conversation {field} must use SQLite TEXT storage \
                     (observed {})",
                    metadata.storage_class
                ),
            }));
        }
    }
    let observed_bytes = [
        ("conversation_id", &candidate.conversation_id),
        ("conversation_data", &candidate.conversation_data),
        ("last_modified_at", &candidate.last_modified_at),
    ]
    .into_iter()
    .try_fold(
        WARP_NATIVE_SQLITE_CONVERSATION_ROW_OVERHEAD_BYTES,
        |total, (field, metadata)| {
            total
                .checked_add(metadata.observed_bytes(field)?)
                .ok_or(CaptureError::SystemInvariant(
                    "Warp NativePath conversation row byte count overflowed",
                ))
        },
    )?;
    let limit = u64::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).map_err(|_| {
        CaptureError::SystemInvariant("Warp NativePath SQLite byte limit exceeds u64")
    })?;
    if observed_bytes > limit {
        return Ok(Some(WarpNativeRejection {
            kind: WarpNativeRejectionKind::ConversationRecord,
            native_key,
            reason: format!(
                "Warp conversation row exceeds \
                 {MAX_PROVIDER_SQLITE_VALUE_BYTES}-byte hydration limit \
                 ({observed_bytes} bytes)"
            ),
        }));
    }
    if candidate
        .hydrated_conversation_id
        .as_deref()
        .is_none_or(str::is_empty)
    {
        return Ok(Some(WarpNativeRejection {
            kind: WarpNativeRejectionKind::ConversationRecord,
            native_key,
            reason: "Warp conversation_id is empty".to_owned(),
        }));
    }
    if candidate.hydrated_conversation_data.is_none()
        || candidate.hydrated_last_modified_at.is_none()
    {
        return Err(CaptureError::SystemInvariant(
            "Warp conversation metadata preflight omitted a bounded value",
        ));
    }
    Ok(None)
}

fn parse_conversation_data(
    raw_data: &str,
    conversation_id: &str,
    rejections: &mut Vec<WarpNativeRejection>,
) -> Value {
    match serde_json::from_str::<Value>(raw_data) {
        Ok(Value::Object(value)) => Value::Object(value),
        Ok(_) => {
            rejections.push(WarpNativeRejection {
                kind: WarpNativeRejectionKind::ConversationRecord,
                native_key: conversation_id.to_owned(),
                reason: "Warp conversation_data must be a JSON object".to_owned(),
            });
            Value::Object(serde_json::Map::new())
        }
        Err(error) => {
            rejections.push(WarpNativeRejection {
                kind: WarpNativeRejectionKind::ConversationRecord,
                native_key: conversation_id.to_owned(),
                reason: format!("invalid Warp conversation_data JSON: {error}"),
            });
            Value::Object(serde_json::Map::new())
        }
    }
}

fn parse_optional_conversation_timestamp(
    raw_modified: &str,
    conversation_id: &str,
    rejections: &mut Vec<WarpNativeRejection>,
) -> Option<DateTime<Utc>> {
    if raw_modified.is_empty() {
        return None;
    }
    match parse_warp_timestamp(raw_modified) {
        Ok(value) => Some(value),
        Err(error) => {
            rejections.push(WarpNativeRejection {
                kind: WarpNativeRejectionKind::ConversationRecord,
                native_key: conversation_id.to_owned(),
                reason: error.to_string(),
            });
            None
        }
    }
}

fn resolve_hierarchy(hierarchy: &mut BTreeMap<String, WarpHierarchyNode>) -> Result<()> {
    let conversation_ids = hierarchy.keys().cloned().collect::<Vec<_>>();
    for conversation_id in &conversation_ids {
        resolve_hierarchy_root(conversation_id, hierarchy)?;
    }
    for conversation_id in conversation_ids {
        let parent_present = hierarchy
            .get(&conversation_id)
            .and_then(|node| node.parent_conversation_id.as_ref())
            .is_some_and(|parent| hierarchy.contains_key(parent));
        let node = hierarchy
            .get_mut(&conversation_id)
            .ok_or(CaptureError::SystemInvariant(
                "Warp hierarchy node disappeared during resolution",
            ))?;
        node.parent_present = parent_present;
    }
    Ok(())
}

fn resolve_hierarchy_root(
    conversation_id: &str,
    hierarchy: &mut BTreeMap<String, WarpHierarchyNode>,
) -> Result<()> {
    let mut seen = BTreeSet::new();
    let mut path = Vec::new();
    let mut current = conversation_id.to_owned();
    let root_conversation_id = loop {
        if !seen.insert(current.clone()) {
            return Err(CaptureError::InvalidPayload(format!(
                "Warp conversation hierarchy contains a cycle at {current:?}"
            )));
        }
        let Some(node) = hierarchy.get(&current) else {
            break current;
        };
        if node.root_resolved {
            break node.root_conversation_id.clone();
        }
        path.push(current.clone());
        let Some(parent) = node.parent_conversation_id.as_deref() else {
            break current;
        };
        current = parent.to_owned();
    };
    for conversation_id in path {
        let node = hierarchy
            .get_mut(&conversation_id)
            .ok_or(CaptureError::SystemInvariant(
                "Warp hierarchy node disappeared during root caching",
            ))?;
        node.root_conversation_id.clone_from(&root_conversation_id);
        node.root_resolved = true;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn scan_tasks(
    conn: &Connection,
    schema: &WarpSqliteSchema,
    hierarchy: &BTreeMap<String, WarpHierarchyNode>,
    builder: &mut WarpNativePageEmitter<'_>,
    counters: &mut WarpNativeCounters,
    profile: WarpNativeProfile,
    resume: &WarpNativeFrontier,
) -> Result<()> {
    let index = warp_quote_identifier(&schema.task_keyset_index);
    let mut first_candidate = conn.prepare(&format!(
        "select t.rowid, \
                typeof(t.conversation_id), coalesce(octet_length(t.conversation_id), 0), \
                typeof(t.task_id), coalesce(octet_length(t.task_id), 0), \
                typeof(t.task), coalesce(octet_length(t.task), 0), \
                typeof(t.last_modified_at), coalesce(octet_length(t.last_modified_at), 0) \
         from agent_tasks t indexed by {index} \
         order by t.task_id collate binary limit 1"
    ))?;
    let mut next_candidate = conn.prepare(&format!(
        "select t.rowid, \
                typeof(t.conversation_id), coalesce(octet_length(t.conversation_id), 0), \
                typeof(t.task_id), coalesce(octet_length(t.task_id), 0), \
                typeof(t.task), coalesce(octet_length(t.task), 0), \
                typeof(t.last_modified_at), coalesce(octet_length(t.last_modified_at), 0) \
         from agent_tasks t indexed by {index} \
         where t.task_id collate binary > ( \
                   select previous.task_id from agent_tasks previous \
                   where previous.rowid = ?1 \
               ) \
         order by t.task_id collate binary limit 1"
    ))?;
    let mut resumed_candidate = conn.prepare(&format!(
        "select t.rowid, \
                typeof(t.conversation_id), coalesce(octet_length(t.conversation_id), 0), \
                typeof(t.task_id), coalesce(octet_length(t.task_id), 0), \
                typeof(t.task), coalesce(octet_length(t.task), 0), \
                typeof(t.last_modified_at), coalesce(octet_length(t.last_modified_at), 0) \
         from agent_tasks t indexed by {index} \
         where t.rowid = ?1"
    ))?;
    let mut hydration = conn.prepare(
        "select conversation_id, task_id, task, last_modified_at \
         from agent_tasks where rowid = ?1",
    )?;
    let mut after_rowid = (resume.phase == WarpNativeFrontierPhase::Tasks)
        .then_some(resume.last_task_rowid)
        .flatten();
    let mut resume_inside_task =
        resume.phase == WarpNativeFrontierPhase::Tasks && resume.next_message_ordinal != 0;
    let mut completed_tasks = resume.completed_task_rows;
    let completed_conversations = builder.frontier().completed_conversation_rows;
    let completed_edges = builder.frontier().completed_hierarchy_edges;
    loop {
        let candidate = if resume_inside_task {
            let rowid = after_rowid.ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "Warp in-task resume frontier omitted its task rowid".to_owned(),
                )
            })?;
            let _guard = SqliteLengthPreflightGuard::new(conn);
            resumed_candidate
                .query_row([rowid], task_candidate_from_row)
                .optional()?
        } else {
            next_task_candidate(conn, &mut first_candidate, &mut next_candidate, after_rowid)?
        };
        let Some(candidate) = candidate else {
            break;
        };
        let resumed_message_ordinal = resume_inside_task.then_some(resume.next_message_ordinal);
        resume_inside_task = false;
        after_rowid = Some(candidate.rowid);
        counters.task_rows = counters.task_rows.saturating_add(1);
        if let Some(rejection) = reject_task_candidate(&candidate)? {
            counters.oversized_task_rows = counters.oversized_task_rows.saturating_add(u64::from(
                rejection.kind == WarpNativeRejectionKind::OversizedTask,
            ));
            if resumed_message_ordinal.is_some() {
                return Err(CaptureError::InvalidPayload(
                    "Warp resume frontier points inside an invalid task row".to_owned(),
                ));
            }
            builder.record_source(b"task\0", rejected_task_candidate_digest(&candidate)?)?;
            let mut unit = WarpNativeUnit::progress();
            let native_key = rejection.native_key.clone();
            unit.push_rejection(rejection)?;
            completed_tasks = completed_tasks.saturating_add(1);
            builder.push(
                unit,
                WarpNativeFrontier::after_task(
                    completed_conversations,
                    completed_edges,
                    completed_tasks,
                    candidate.rowid,
                ),
                native_key,
                counters,
            )?;
            continue;
        }
        hydrate_task_candidate(
            &mut hydration,
            &candidate,
            hierarchy,
            builder,
            counters,
            profile,
            resumed_message_ordinal,
            completed_tasks,
            completed_conversations,
            completed_edges,
        )?;
        completed_tasks = completed_tasks.saturating_add(1);
    }
    Ok(())
}

fn next_task_candidate(
    conn: &Connection,
    first: &mut Statement<'_>,
    next: &mut Statement<'_>,
    after_rowid: Option<i64>,
) -> Result<Option<WarpTaskCandidate>> {
    let _guard = SqliteLengthPreflightGuard::new(conn);
    match after_rowid {
        Some(rowid) => next
            .query_row([rowid], task_candidate_from_row)
            .optional()
            .map_err(CaptureError::from),
        None => first
            .query_row([], task_candidate_from_row)
            .optional()
            .map_err(CaptureError::from),
    }
}

fn task_candidate_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WarpTaskCandidate> {
    Ok(WarpTaskCandidate {
        rowid: row.get(0)?,
        conversation_id: WarpTaskCellMetadata {
            storage_class: row.get(1)?,
            bytes: row.get(2)?,
        },
        task_id: WarpTaskCellMetadata {
            storage_class: row.get(3)?,
            bytes: row.get(4)?,
        },
        task: WarpTaskCellMetadata {
            storage_class: row.get(5)?,
            bytes: row.get(6)?,
        },
        last_modified_at: WarpTaskCellMetadata {
            storage_class: row.get(7)?,
            bytes: row.get(8)?,
        },
    })
}

fn reject_task_candidate(candidate: &WarpTaskCandidate) -> Result<Option<WarpNativeRejection>> {
    let native_key = format!("rowid:{}", candidate.rowid);
    for (field, metadata, required_storage) in [
        ("conversation_id", &candidate.conversation_id, "text"),
        ("task_id", &candidate.task_id, "text"),
        ("task", &candidate.task, "blob"),
        ("last_modified_at", &candidate.last_modified_at, "text"),
    ] {
        if metadata.storage_class != required_storage {
            return Ok(Some(WarpNativeRejection {
                kind: WarpNativeRejectionKind::TaskRecord,
                native_key,
                reason: format!(
                    "Warp task {field} must use SQLite {} storage (observed {})",
                    required_storage.to_ascii_uppercase(),
                    metadata.storage_class
                ),
            }));
        }
    }
    if candidate.task_id.bytes == 0 {
        return Ok(Some(WarpNativeRejection {
            kind: WarpNativeRejectionKind::TaskRecord,
            native_key,
            reason: "Warp task_id is empty".to_owned(),
        }));
    }
    if candidate.conversation_id.bytes == 0 {
        return Ok(Some(WarpNativeRejection {
            kind: WarpNativeRejectionKind::TaskRecord,
            native_key,
            reason: "Warp task conversation_id is empty".to_owned(),
        }));
    }
    let task_id_bytes = candidate.task_id.observed_bytes("task_id")?;
    if task_id_bytes > WARP_ORDERING_KEY_MAX_BYTES as u64 {
        return Ok(Some(WarpNativeRejection {
            kind: WarpNativeRejectionKind::TaskRecord,
            native_key,
            reason: format!(
                "Warp task_id exceeds {WARP_ORDERING_KEY_MAX_BYTES}-byte native ordering limit \
                 ({task_id_bytes} bytes)"
            ),
        }));
    }
    let observed_bytes = candidate.hydrated_bytes()?;
    let limit = u64::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).map_err(|_| {
        CaptureError::SystemInvariant("Warp NativePath SQLite byte limit exceeds u64")
    })?;
    if observed_bytes > limit {
        return Ok(Some(WarpNativeRejection {
            kind: WarpNativeRejectionKind::OversizedTask,
            native_key,
            reason: format!(
                "Warp task row exceeds {MAX_PROVIDER_SQLITE_VALUE_BYTES}-byte hydration limit \
                 ({observed_bytes} bytes)"
            ),
        }));
    }
    Ok(None)
}

impl WarpTaskCellMetadata {
    fn observed_bytes(&self, field: &str) -> Result<u64> {
        u64::try_from(self.bytes).map_err(|_| {
            CaptureError::InvalidPayload(format!(
                "Warp task {field} byte count must be nonnegative"
            ))
        })
    }
}

impl WarpTaskCandidate {
    fn hydrated_bytes(&self) -> Result<u64> {
        [
            ("conversation_id", &self.conversation_id),
            ("task_id", &self.task_id),
            ("task", &self.task),
            ("last_modified_at", &self.last_modified_at),
        ]
        .into_iter()
        .try_fold(
            WARP_NATIVE_SQLITE_ROW_OVERHEAD_BYTES,
            |total, (field, cell)| {
                total
                    .checked_add(cell.observed_bytes(field)?)
                    .ok_or(CaptureError::SystemInvariant(
                        "Warp NativePath task row byte count overflowed",
                    ))
            },
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn hydrate_task_candidate(
    hydration: &mut Statement<'_>,
    candidate: &WarpTaskCandidate,
    hierarchy: &BTreeMap<String, WarpHierarchyNode>,
    builder: &mut WarpNativePageEmitter<'_>,
    counters: &mut WarpNativeCounters,
    profile: WarpNativeProfile,
    resumed_message_ordinal: Option<u32>,
    completed_tasks: u64,
    completed_conversations: u64,
    completed_edges: u64,
) -> Result<()> {
    #[cfg(test)]
    trace_native_task_hydration(candidate.rowid);
    let mut rows = hydration.query([candidate.rowid])?;
    let row = rows.next()?.ok_or_else(|| {
        CaptureError::InvalidPayload(format!(
            "Warp task row {} disappeared during immutable scan",
            candidate.rowid
        ))
    })?;
    let conversation_value = row.get_ref(0)?;
    let task_id_value = row.get_ref(1)?;
    let task_value = row.get_ref(2)?;
    let modified_value = row.get_ref(3)?;

    // This digest is control-plane evidence only. Output result bytes never
    // enter retained event bodies, hashes, previews, or downstream records.
    let source_values = [
        conversation_value,
        task_id_value,
        task_value,
        modified_value,
    ];
    let evidence_digest = source_row_digest(b"task\0", &source_values)?;
    let complete_content_record_digest =
        complete_content_record_digest(candidate.rowid, &source_values)?;
    if resumed_message_ordinal.is_none() {
        builder.record_source(b"task\0", evidence_digest)?;
    }

    let task_id = required_text(task_id_value, "task_id")?.to_owned();
    let conversation_id = required_text(conversation_value, "conversation_id")?.to_owned();
    if !hierarchy.contains_key(&conversation_id) {
        if resumed_message_ordinal.is_some() {
            return Err(CaptureError::InvalidPayload(
                "Warp resume frontier points inside a task with no conversation".to_owned(),
            ));
        }
        let mut unit = WarpNativeUnit::progress();
        unit.push_rejection(WarpNativeRejection {
            kind: WarpNativeRejectionKind::MissingConversation,
            native_key: task_id.clone(),
            reason: format!("Warp task references missing conversation {conversation_id:?}"),
        })?;
        builder.push(
            unit,
            WarpNativeFrontier::after_task(
                completed_conversations,
                completed_edges,
                completed_tasks.saturating_add(1),
                candidate.rowid,
            ),
            task_id,
            counters,
        )?;
        return Ok(());
    }
    let ValueRef::Blob(task_blob) = task_value else {
        return Err(CaptureError::SystemInvariant(
            "Warp task storage changed after metadata preflight",
        ));
    };
    counters.protobuf_bytes_scanned = counters
        .protobuf_bytes_scanned
        .saturating_add(u64::try_from(task_blob.len()).unwrap_or(u64::MAX));
    let mut task_prefix_unit = WarpNativeUnit::progress();
    let task_modified =
        match required_text(modified_value, "last_modified_at").and_then(parse_warp_timestamp) {
            Ok(value) => Some(value),
            Err(error) => {
                task_prefix_unit.push_rejection(WarpNativeRejection {
                    kind: WarpNativeRejectionKind::TaskRecord,
                    native_key: task_id.clone(),
                    reason: error.to_string(),
                })?;
                None
            }
        };
    let decoded = match decode_warp_native_task(task_blob, profile) {
        Ok(decoded) => decoded,
        Err(error) => {
            if resumed_message_ordinal.is_some() {
                return Err(CaptureError::InvalidPayload(
                    "Warp resume frontier points inside an undecodable task".to_owned(),
                ));
            }
            counters.malformed_task_cells = counters.malformed_task_cells.saturating_add(1);
            task_prefix_unit.push_rejection(WarpNativeRejection {
                kind: WarpNativeRejectionKind::MalformedProtobuf,
                native_key: task_id.clone(),
                reason: format!("failed to decode Warp task protobuf: {error}"),
            })?;
            builder.push(
                task_prefix_unit,
                WarpNativeFrontier::after_task(
                    completed_conversations,
                    completed_edges,
                    completed_tasks.saturating_add(1),
                    candidate.rowid,
                ),
                task_id,
                counters,
            )?;
            return Ok(());
        }
    };
    merge_decode_counters(counters, decoded.counters);
    if let Some(rejection) = prevalidate_message_identities(&task_id, &decoded.messages, counters) {
        if resumed_message_ordinal.is_some() {
            return Err(CaptureError::InvalidPayload(
                "Warp resume frontier points inside a task with duplicate message identity"
                    .to_owned(),
            ));
        }
        counters.duplicate_message_identity_tasks =
            counters.duplicate_message_identity_tasks.saturating_add(1);
        task_prefix_unit.push_rejection(rejection)?;
        builder.push(
            task_prefix_unit,
            WarpNativeFrontier::after_task(
                completed_conversations,
                completed_edges,
                completed_tasks.saturating_add(1),
                candidate.rowid,
            ),
            task_id,
            counters,
        )?;
        return Ok(());
    }
    let message_count = decoded.messages.len();
    if message_count == 0 {
        if resumed_message_ordinal.is_some() {
            return Err(CaptureError::InvalidPayload(
                "Warp resume frontier points inside an empty task".to_owned(),
            ));
        }
        builder.push(
            task_prefix_unit,
            WarpNativeFrontier::after_task(
                completed_conversations,
                completed_edges,
                completed_tasks.saturating_add(1),
                candidate.rowid,
            ),
            task_id,
            counters,
        )?;
        return Ok(());
    }
    if let Some(resume_at) = resumed_message_ordinal {
        if !decoded
            .messages
            .iter()
            .any(|message| message.message_ordinal == resume_at)
        {
            return Err(CaptureError::InvalidPayload(
                "Warp resume frontier message ordinal is absent from its certified task".to_owned(),
            ));
        }
        task_prefix_unit = WarpNativeUnit::progress();
    }
    for (index, decoded_message) in decoded.messages.into_iter().enumerate() {
        let message_ordinal = decoded_message.message_ordinal;
        if resumed_message_ordinal.is_some_and(|resume_at| message_ordinal < resume_at) {
            continue;
        }
        let next_frontier = if index.saturating_add(1) == message_count {
            WarpNativeFrontier::after_task(
                completed_conversations,
                completed_edges,
                completed_tasks.saturating_add(1),
                candidate.rowid,
            )
        } else {
            WarpNativeFrontier::in_task(
                completed_conversations,
                completed_edges,
                completed_tasks,
                candidate.rowid,
                message_ordinal.saturating_add(1),
            )
        };
        let mut unit = if index == 0 {
            std::mem::replace(&mut task_prefix_unit, WarpNativeUnit::progress())
        } else {
            WarpNativeUnit::progress()
        };
        let WarpDecodedMessage {
            message_id,
            request_id,
            occurred_at,
            payload,
            ..
        } = decoded_message;
        match payload {
            WarpDecodedMessagePayload::Retained(message) => {
                let event = WarpNativeEvent::from_draft(WarpNativeEventDraft {
                    provider_event_index: builder.frontier().retained_events,
                    task_rowid: candidate.rowid,
                    conversation_id: conversation_id.clone(),
                    task_id: task_id.clone(),
                    message_id,
                    message_ordinal,
                    event_type: message.event_type,
                    role: message.role,
                    kind: message.kind,
                    request_id,
                    result_outcome: None,
                    call_id: None,
                    occurred_at: occurred_at.or(task_modified),
                    body: message.body,
                    source_record_digest: complete_content_record_digest.clone(),
                })?;
                record_retained_event_counters(counters, &event);
                if message.tool_call {
                    counters.tool_calls_retained = counters.tool_calls_retained.saturating_add(1);
                }
                unit.push_event(event)?;
            }
            WarpDecodedMessagePayload::Output(output) => {
                let outcome = output.outcome;
                let call_id = output.call_id;
                let tool_name = output.tool_name;
                if matches!(outcome, OutputOutcome::Failure | OutputOutcome::Timeout) {
                    let event = WarpNativeEvent::from_draft(WarpNativeEventDraft {
                        provider_event_index: builder.frontier().retained_events,
                        task_rowid: candidate.rowid,
                        conversation_id: conversation_id.clone(),
                        task_id: task_id.clone(),
                        message_id: message_id.clone(),
                        message_ordinal,
                        event_type: ctx_history_core::EventType::ToolOutput,
                        role: Some(ctx_history_core::EventRole::Tool),
                        kind: "tool_call_result",
                        request_id: request_id.clone(),
                        result_outcome: Some(outcome),
                        call_id: call_id.clone(),
                        occurred_at: occurred_at.or(task_modified),
                        body: format!("tool result: {tool_name}"),
                        source_record_digest: complete_content_record_digest.clone(),
                    })?;
                    record_retained_event_counters(counters, &event);
                    counters.result_events_created =
                        counters.result_events_created.saturating_add(1);
                    unit.push_event(event)?;
                }
                if let Some(pro_payload) = output.pro_payload {
                    match pro_payload {
                        WarpProOutputPayload::Content(content) => {
                            let observation = warp_output_observation(
                                candidate.rowid,
                                completed_tasks,
                                &conversation_id,
                                &task_id,
                                message_ordinal,
                                message_id,
                                request_id,
                                occurred_at.or(task_modified),
                                hierarchy,
                                call_id,
                                outcome,
                                content,
                            )?;
                            if unit.try_push_output(observation) {
                                counters.result_handoffs_created =
                                    counters.result_handoffs_created.saturating_add(1);
                            } else {
                                counters.oversized_output_records =
                                    counters.oversized_output_records.saturating_add(1);
                                unit.push_output_rejection(output_rejection(
                                    WarpNativeOutputRejectionKind::Oversized,
                                    &task_id,
                                    message_ordinal,
                                    format!(
                                        "Warp output observation exceeds the \
                                         {WARP_NATIVE_PAGE_MAX_BYTES}-byte safe-page limit"
                                    ),
                                ))?;
                            }
                        }
                        WarpProOutputPayload::Rejected { kind, reason } => {
                            unit.push_output_rejection(output_rejection(
                                match kind {
                                    WarpOutputLocalFailureKind::Malformed => {
                                        WarpNativeOutputRejectionKind::Malformed
                                    }
                                    WarpOutputLocalFailureKind::Oversized => {
                                        WarpNativeOutputRejectionKind::Oversized
                                    }
                                },
                                &task_id,
                                message_ordinal,
                                reason,
                            ))?;
                        }
                    }
                }
            }
            WarpDecodedMessagePayload::OutputLocalFailure { reason } => {
                unit.push_output_rejection(output_rejection(
                    WarpNativeOutputRejectionKind::Malformed,
                    &task_id,
                    message_ordinal,
                    reason,
                ))?;
            }
            WarpDecodedMessagePayload::Excluded => {}
        }
        builder.push(
            unit,
            next_frontier,
            format!("{task_id}:message:{message_ordinal}"),
            counters,
        )?;
    }
    Ok(())
}

fn prevalidate_message_identities(
    task_id: &str,
    messages: &[WarpDecodedMessage],
    counters: &mut WarpNativeCounters,
) -> Option<WarpNativeRejection> {
    let mut message_identities = HashSet::new();
    for message in messages {
        let message_identity = message.message_id.as_ref().map_or(
            WarpNativeMessageIdentity::MessageOrdinal(message.message_ordinal),
            |message_id| WarpNativeMessageIdentity::ProviderId(message_id.clone()),
        );
        if !message_identities.insert(message_identity) {
            return Some(WarpNativeRejection {
                kind: WarpNativeRejectionKind::DuplicateMessageIdentity,
                native_key: task_id.to_owned(),
                reason: format!(
                    "Warp task contains duplicate message identity at ordinal {}",
                    message.message_ordinal
                ),
            });
        }
        counters.peak_task_identity_entries = counters
            .peak_task_identity_entries
            .max(u64::try_from(message_identities.len()).unwrap_or(u64::MAX));
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn warp_output_observation(
    rowid: i64,
    task_ordinal: u64,
    conversation_id: &str,
    task_id: &str,
    message_ordinal: u32,
    message_id: Option<String>,
    request_id: Option<String>,
    occurred_at: Option<DateTime<Utc>>,
    hierarchy: &BTreeMap<String, WarpHierarchyNode>,
    call_id: Option<String>,
    outcome: OutputOutcome,
    content: Vec<u8>,
) -> Result<ProOutputObservation> {
    let mut locator = Vec::with_capacity(12);
    locator.extend_from_slice(&rowid.to_be_bytes());
    locator.extend_from_slice(&message_ordinal.to_be_bytes());
    let hierarchy = hierarchy
        .get(conversation_id)
        .ok_or(CaptureError::SystemInvariant(
            "Warp output conversation disappeared from hierarchy",
        ))?;
    let native_record_id = message_id.unwrap_or_else(|| format!("{task_id}:{message_ordinal}"));
    Ok(ProOutputObservation {
        kind: OutputObservationKind::Tool,
        coordinate: OutputNativeCoordinate {
            unit_key: format!("warp/nativepath/{conversation_id}/{task_id}/{message_ordinal:010}"),
            native_sequence: task_ordinal,
            native_record_id: Some(native_record_id),
            source_record_ordinal: Some(task_ordinal),
            source_record_subrecord_index: Some(message_ordinal),
            byte_start: None,
            byte_end_exclusive: None,
        },
        occurred_at_unix_ms: occurred_at.map(|value| value.timestamp_millis()),
        associations: OutputAssociations {
            direct_session_id: conversation_id.to_owned(),
            root_session_id: hierarchy.root_conversation_id.clone(),
            parent_session_id: hierarchy.parent_conversation_id.clone(),
            provider_session_id: Some(conversation_id.to_owned()),
            agent_id: Some("warp-agent".to_owned()),
            repository: None,
        },
        call_id: call_id.or(request_id),
        command: None,
        outcome: OutputOutcomeMetadata {
            outcome,
            exit_code: None,
            duration_ms: None,
        },
        locator: OutputSourceLocator {
            version: 1,
            kind: WARP_CONTENT_LOCATOR_KIND.to_owned(),
            payload: locator,
        },
        content,
    })
}

fn output_rejection(
    kind: WarpNativeOutputRejectionKind,
    task_id: &str,
    message_ordinal: u32,
    reason: String,
) -> WarpNativeOutputRejection {
    WarpNativeOutputRejection {
        kind,
        native_key: format!("{task_id}:message:{message_ordinal}"),
        reason,
    }
}

fn record_retained_event_counters(counters: &mut WarpNativeCounters, event: &WarpNativeEvent) {
    counters.retained_events = counters.retained_events.saturating_add(1);
    counters.retained_body_bytes = counters
        .retained_body_bytes
        .saturating_add(u64::try_from(event.body.len()).unwrap_or(u64::MAX));
    counters.retained_content_hashes = counters.retained_content_hashes.saturating_add(1);
    counters.retained_previews = counters.retained_previews.saturating_add(1);
}

fn hash_rejected_task_candidate(hasher: &mut Sha256, candidate: &WarpTaskCandidate) -> Result<()> {
    hasher.update(b"task-preflight-rejection\0");
    hasher.update(candidate.rowid.to_le_bytes());
    for cell in [
        &candidate.conversation_id,
        &candidate.task_id,
        &candidate.task,
        &candidate.last_modified_at,
    ] {
        hash_bytes(hasher, cell.storage_class.as_bytes())?;
        hasher.update(cell.bytes.to_le_bytes());
    }
    Ok(())
}

fn rejected_task_candidate_digest(candidate: &WarpTaskCandidate) -> Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-warp-task-preflight-evidence-v1\0");
    hash_rejected_task_candidate(&mut hasher, candidate)?;
    Ok(hasher.finalize().into())
}

fn hash_rejected_conversation_candidate(
    hasher: &mut Sha256,
    candidate: &WarpConversationCandidate,
) -> Result<()> {
    hasher.update(b"conversation-preflight-rejection\0");
    hasher.update(candidate.rowid.to_le_bytes());
    for cell in [
        &candidate.conversation_id,
        &candidate.conversation_data,
        &candidate.last_modified_at,
    ] {
        hash_bytes(hasher, cell.storage_class.as_bytes())?;
        hasher.update(cell.bytes.to_le_bytes());
    }
    Ok(())
}

fn rejected_conversation_candidate_digest(
    candidate: &WarpConversationCandidate,
) -> Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-warp-conversation-preflight-evidence-v1\0");
    hash_rejected_conversation_candidate(&mut hasher, candidate)?;
    Ok(hasher.finalize().into())
}

fn source_text_row_digest<const N: usize>(label: &[u8], values: [&str; N]) -> Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-warp-native-row-evidence-v1\0");
    hash_source_text_row(&mut hasher, label, values)?;
    Ok(hasher.finalize().into())
}

fn hash_source_text_row<const N: usize>(
    hasher: &mut Sha256,
    label: &[u8],
    values: [&str; N],
) -> Result<()> {
    hasher.update(label);
    for value in values {
        hasher.update([3]);
        hash_bytes(hasher, value.as_bytes())?;
    }
    Ok(())
}

#[cfg(test)]
fn trace_native_task_hydration(rowid: i64) {
    WARP_NATIVE_TASK_HYDRATION_ROWIDS.with(|trace| {
        if let Some(rowids) = trace.borrow_mut().as_mut() {
            rowids.push(rowid);
        }
    });
}

#[cfg(test)]
pub(super) fn start_native_task_hydration_trace() {
    WARP_NATIVE_TASK_HYDRATION_ROWIDS.with(|trace| *trace.borrow_mut() = Some(Vec::new()));
}

#[cfg(test)]
pub(super) fn take_native_task_hydration_trace() -> Vec<i64> {
    WARP_NATIVE_TASK_HYDRATION_ROWIDS
        .with(|trace| trace.borrow_mut().take())
        .unwrap_or_default()
}

fn merge_decode_counters(counters: &mut WarpNativeCounters, decoded: WarpDecodeCounters) {
    counters.unknown_fields = counters
        .unknown_fields
        .saturating_add(decoded.unknown_fields);
    counters.unknown_oneofs = counters
        .unknown_oneofs
        .saturating_add(decoded.unknown_oneofs);
    counters.native_result_records = counters
        .native_result_records
        .saturating_add(decoded.native_result_records);
    counters.native_result_envelope_bytes = counters
        .native_result_envelope_bytes
        .saturating_add(decoded.native_result_envelope_bytes);
    counters.native_result_body_bytes_observed = counters
        .native_result_body_bytes_observed
        .saturating_add(decoded.native_result_body_bytes_observed);
    counters.native_results_success = counters
        .native_results_success
        .saturating_add(decoded.native_results_success);
    counters.native_results_failure = counters
        .native_results_failure
        .saturating_add(decoded.native_results_failure);
    counters.native_results_timeout = counters
        .native_results_timeout
        .saturating_add(decoded.native_results_timeout);
    counters.native_results_unknown = counters
        .native_results_unknown
        .saturating_add(decoded.native_results_unknown);
    counters.malformed_output_records = counters
        .malformed_output_records
        .saturating_add(decoded.malformed_output_records);
    counters.oversized_output_records = counters
        .oversized_output_records
        .saturating_add(decoded.oversized_output_records);
    counters.result_body_bytes_decoded = counters
        .result_body_bytes_decoded
        .saturating_add(decoded.result_body_bytes_decoded);
    counters.result_body_strings_allocated = counters
        .result_body_strings_allocated
        .saturating_add(decoded.result_body_strings_allocated);
}

fn required_text<'a>(value: ValueRef<'a>, field: &str) -> Result<&'a str> {
    match value {
        ValueRef::Text(value) => std::str::from_utf8(value).map_err(|error| {
            CaptureError::InvalidPayload(format!("Warp {field} contains invalid UTF-8: {error}"))
        }),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Warp {field} must use SQLite TEXT storage"
        ))),
    }
}

fn parse_warp_timestamp(raw: &str) -> Result<DateTime<Utc>> {
    if let Ok(timestamp) = DateTime::parse_from_rfc3339(raw) {
        return Ok(timestamp.with_timezone(&Utc));
    }
    let naive = NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S%.f").map_err(|_| {
        CaptureError::InvalidPayload(format!(
            "Warp timestamp is not RFC3339 or SQLite UTC text: {raw:?}"
        ))
    })?;
    Ok(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc))
}

fn bounded_session_metadata(conversation_data: &Value) -> Result<Value> {
    let bounded_text = |field: &str| {
        conversation_data
            .get(field)
            .and_then(Value::as_str)
            .map(|value| truncate_chars(value, 1_024))
    };
    let mut metadata = json!({
        "agent_name": bounded_text("agent_name"),
        "run_id": bounded_text("run_id"),
        "parent_conversation_id": bounded_text("parent_conversation_id"),
        "server_conversation_token_present": conversation_data
            .get("server_conversation_token")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty()),
        "forked_from_server_conversation_token_present": conversation_data
            .get("forked_from_server_conversation_token")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty()),
        "conversation_usage_metadata": conversation_data
            .get("conversation_usage_metadata")
            .cloned()
            .unwrap_or(Value::Null),
    });
    if serde_json::to_vec(&metadata)?.len() > WARP_SESSION_METADATA_MAX_BYTES {
        metadata["conversation_usage_metadata"] =
            json!({"truncated": true, "reason": "bounded_nativepath_metadata"});
    }
    Ok(metadata)
}

fn truncate_chars(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        value.to_owned()
    } else {
        value.chars().take(limit).collect()
    }
}

fn hash_source_row(hasher: &mut Sha256, label: &[u8], values: &[ValueRef<'_>]) -> Result<()> {
    hasher.update(label);
    for value in values {
        match value {
            ValueRef::Null => hasher.update([0]),
            ValueRef::Integer(value) => {
                hasher.update([1]);
                hasher.update(value.to_le_bytes());
            }
            ValueRef::Real(value) => {
                hasher.update([2]);
                hasher.update(value.to_bits().to_le_bytes());
            }
            ValueRef::Text(value) => {
                hasher.update([3]);
                hash_bytes(hasher, value)?;
            }
            ValueRef::Blob(value) => {
                hasher.update([4]);
                hash_bytes(hasher, value)?;
            }
        }
    }
    Ok(())
}

fn source_row_digest(label: &[u8], values: &[ValueRef<'_>]) -> Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-warp-native-row-evidence-v1\0");
    hash_source_row(&mut hasher, label, values)?;
    Ok(hasher.finalize().into())
}

fn complete_content_record_digest(
    rowid: i64,
    values: &[ValueRef<'_>],
) -> Result<CompleteContentBodyDigest> {
    const DOMAIN: &[u8] = b"ctx-complete-content-sqlite-logical-row-v1\0";
    let mut digest = Sha256::new();
    digest.update(DOMAIN);
    digest.update(
        u64::try_from(values.len().saturating_add(1))
            .map_err(|_| CaptureError::SystemInvariant("Warp SQLite value count overflowed"))?
            .to_be_bytes(),
    );
    digest.update([1]);
    digest.update(rowid.to_be_bytes());
    for value in values {
        match value {
            ValueRef::Null => digest.update([0]),
            ValueRef::Integer(value) => {
                digest.update([1]);
                digest.update(value.to_be_bytes());
            }
            ValueRef::Real(value) => {
                digest.update([2]);
                digest.update(value.to_bits().to_be_bytes());
            }
            ValueRef::Text(value) => {
                digest.update([3]);
                digest.update(
                    u64::try_from(value.len())
                        .map_err(|_| {
                            CaptureError::SystemInvariant("Warp SQLite text length overflowed")
                        })?
                        .to_be_bytes(),
                );
                digest.update(value);
            }
            ValueRef::Blob(value) => {
                digest.update([4]);
                digest.update(
                    u64::try_from(value.len())
                        .map_err(|_| {
                            CaptureError::SystemInvariant("Warp SQLite blob length overflowed")
                        })?
                        .to_be_bytes(),
                );
                digest.update(value);
            }
        }
    }
    CompleteContentBodyDigest::parse(format!("{:x}", digest.finalize())).ok_or(
        CaptureError::SystemInvariant("Warp complete-content digest was not canonical SHA-256"),
    )
}

fn hash_bytes(hasher: &mut Sha256, value: &[u8]) -> Result<()> {
    let length = u64::try_from(value.len()).map_err(|_| {
        CaptureError::SystemInvariant("Warp source digest field length exceeds u64")
    })?;
    hasher.update(length.to_le_bytes());
    hasher.update(value);
    Ok(())
}
