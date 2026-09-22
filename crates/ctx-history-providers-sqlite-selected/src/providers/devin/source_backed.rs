//! Devin logical projection into Core records.
//!
//! One pass over the pinned snapshot produces both the published records and
//! the logical fingerprint, so a refresh that observes an unchanged database
//! cannot disagree with the import that preceded it.
//!
//! Lineage follows the Codex and Claude precedent. A session's primary
//! transcript claims nothing about its parentage. A subagent thread Devin
//! foreground-linked or recorded with a durable head becomes its own delegated
//! session, keyed by the agent id, with the primary session as both its parent
//! and its root. Nothing claims an event copy: Devin records no proof that
//! would support one.

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use thiserror::Error;

use ctx_history_capture_model::{
    normalization::provider_timestamp_seconds, raw_object_keys_are_unique,
};
use ctx_history_capture_runtime::{
    SourceBackedRecordRejectionClass, SourceBackedRecordRejectionDrafts,
};
use ctx_history_core::{
    admit_optional_provider_call_id, derive_event_id, derive_native_session_id, ActivityInvocation,
    ActivityJsonCapture, ActivityResult, ActivityTextCapture, AgentScope, CaptureProvider,
    CertifiedSource, CoreActivity, CoreRecord, CoreRecordError, EventIdentityInput, EventType,
    LiteralFactKind, NativeItemKey, NativeSessionKey, PositionStability, ProjectionContractError,
    ProviderDeclaredFact, ProviderNativeSessionRelationship, ScannedSourceCounts, SourceAnchor,
    SourceAnchorScope, SourceKey, StableEntityId, SubrecordSelector, TypedKey,
    CORE_ACTIVITY_REVISION,
};
use ctx_history_source_sqlite::{
    sqlite_logical_record_digest_bytes, NativeSqliteValue, SqliteLogicalSnapshot,
};

use crate::{
    fingerprint::{hash_text, CountOverflowError, RelationFingerprint},
    provider_sources::SqliteSourceAccessError,
    CaptureError, DEVIN_CLI_SESSIONS_SQLITE_SOURCE_FORMAT,
};

use super::{
    chain::{plan_session, reject_oversized_session, DevinLineageKey, DevinSessionPlan},
    normalization::{enrich_tool_output, normalize_node, DevinNativeEvent, DevinNodeDisposition},
    schema::DevinNativeSchema,
    stream::{
        hydrate_nodes, read_session_facts_with_row_shape_rejections, read_session_page,
        read_subagent_heads_with_row_shape_rejections, DevinMalformedNode, DevinNodeRow,
        DevinSessionRow,
    },
    tool_state::read_tool_state,
};

mod diagnostics;
mod evidence;

use diagnostics::{
    diagnostic_id, malformed_parent_traversed, record_ambiguous_json_rejection,
    record_devin_rejection, record_malformed_head_rejection, record_malformed_node_rejection,
    record_malformed_session_metadata_rejection, record_malformed_tool_state_rejections,
    record_repeated_devin_rejection, session_rejection_detail,
};
use evidence::{malformed_session_evidence, node_evidence, session_evidence};

pub(super) const DEVIN_SOURCE_BACKED_PARSER_REVISION: &str = "devin-cli-sessions-sqlite-v3";
const DEVIN_SOURCE_ANCHOR_NAMESPACE: &str = "devin_cli.sessions_database";
const DEVIN_SOURCE_ANCHOR_KEY: &str = "devin_cli_sessions_sqlite";
const DEVIN_SOURCE_SCHEMA_VARIANT: &str = "devin-cli-sessions-sqlite-v1";
const DEVIN_LOGICAL_SESSION_KIND: &str = "devin.session";
const DEVIN_LOGICAL_EVENT_KIND: &str = "devin.message_node";
const DEVIN_SESSION_NAMESPACE: &str = "devin.session";
const DEVIN_EVENT_NAMESPACE: &str = "devin.message_node";
const DEVIN_SUBRECORD_KIND: &str = "devin.node_event_slot";
const DEVIN_LOGICAL_DATABASE_DOMAIN: &[u8] = b"ctx.devin.logical-database.v1\0";
const DEVIN_LOGICAL_SESSION_RELATION: u8 = 0;
const DEVIN_LOGICAL_NODE_RELATION: u8 = 1;
const DEVIN_LOGICAL_TOOL_STATE_RELATION: u8 = 2;

pub(super) type DevinResult<T> = std::result::Result<T, DevinSourceBackedError>;

#[derive(Debug, Error)]
pub(super) enum DevinSourceBackedError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    SqliteSource(#[from] SqliteSourceAccessError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    CoreRecord(#[from] CoreRecordError),
    #[error(transparent)]
    Route(#[from] ctx_history_capture_runtime::SourceBackedRouteError),
    #[error("unsupported Devin history format: {0}")]
    UnsupportedFormat(&'static str),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("Devin source-backed count overflow")]
    CountOverflow,
}

impl From<ctx_history_source_io::SourceIoError> for DevinSourceBackedError {
    fn from(error: ctx_history_source_io::SourceIoError) -> Self {
        Self::Capture(error.into())
    }
}

impl From<ctx_history_source_sqlite::SqliteIoError> for DevinSourceBackedError {
    fn from(error: ctx_history_source_sqlite::SqliteIoError) -> Self {
        Self::Capture(error.into())
    }
}

pub(super) const DEVIN_SOURCE_PATH_REASONS: crate::sqlite_common::SqliteSourcePathReasons =
    crate::sqlite_common::SqliteSourcePathReasons {
        missing_parent: "Devin SQLite source must have a parent directory",
        missing_leaf: "Devin SQLite source must have a database leaf name",
        not_regular_file: "Devin SQLite source must be a regular non-symlink file",
    };

/// Classifies a scan failure into the route taxonomy the coordinator acts on.
pub(super) fn devin_route_error(
    error: DevinSourceBackedError,
) -> ctx_history_capture_runtime::SourceBackedRouteError {
    use ctx_history_capture_runtime::{SourceBackedRouteError, SourceBackedRouteErrorKind};
    let error = match error {
        DevinSourceBackedError::Route(error) => return error,
        error => error,
    };
    let kind = match &error {
        DevinSourceBackedError::Capture(CaptureError::SourceChangedDuringCapture) => {
            SourceBackedRouteErrorKind::SourceChanged
        }
        DevinSourceBackedError::SqliteSource(_) => {
            let DevinSourceBackedError::SqliteSource(source) = error else {
                unreachable!("matched on the SqliteSource variant")
            };
            return crate::provider::source_backed::sqlite_source_route_error(source);
        }
        DevinSourceBackedError::Capture(CaptureError::SystemInvariant(_))
        | DevinSourceBackedError::CountOverflow
        | DevinSourceBackedError::Projection(_)
        | DevinSourceBackedError::CoreRecord(_) => SourceBackedRouteErrorKind::Internal,
        DevinSourceBackedError::UnsupportedFormat(_) => SourceBackedRouteErrorKind::InvalidSource,
        _ => SourceBackedRouteErrorKind::InvalidSource,
    };
    SourceBackedRouteError::new(kind, error.to_string())
}

impl CountOverflowError for DevinSourceBackedError {
    fn count_overflow() -> Self {
        Self::CountOverflow
    }
}

fn checked_add(left: u64, right: u64) -> DevinResult<u64> {
    crate::fingerprint::checked_add(left, right)
}

/// The exact-source key for a Devin database.
///
/// Devin keeps one database per installation with no platform-root variants,
/// so the anchor is a constant and the route is distinguished only by scope.
pub(super) fn devin_source_key_scoped(source_scope: SourceAnchorScope) -> DevinResult<SourceKey> {
    let anchor = SourceAnchor::provider_native(
        DEVIN_SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(DEVIN_SOURCE_ANCHOR_KEY)?,
    )?;
    Ok(SourceKey::derive_scoped(
        CaptureProvider::Devin.as_str(),
        DEVIN_CLI_SESSIONS_SQLITE_SOURCE_FORMAT,
        DEVIN_SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
        source_scope,
    )?)
}

/// One imported transcript's identity and lineage claims.
#[derive(Clone, Debug)]
struct DevinLineageProjection {
    session_id: StableEntityId,
    provider_session_id: String,
    agent_scope: AgentScope,
    parent_session_id: Option<StableEntityId>,
    root_session_id: Option<StableEntityId>,
    relationship: Option<ProviderNativeSessionRelationship>,
    working_directory: Option<String>,
}

fn primary_projection(
    source: &SourceKey,
    session: &DevinSessionRow,
) -> DevinResult<DevinLineageProjection> {
    let session_id = derive_native_session_id(
        source,
        DEVIN_LOGICAL_SESSION_KIND,
        DEVIN_SESSION_NAMESPACE,
        TypedKey::utf8(session.id.clone())?,
    )?;
    Ok(DevinLineageProjection {
        session_id,
        provider_session_id: session.id.clone(),
        agent_scope: AgentScope::Primary,
        parent_session_id: None,
        root_session_id: None,
        relationship: None,
        working_directory: non_empty(&session.working_directory),
    })
}

/// An exactly linked subagent thread, projected as a delegated child session.
///
/// A foreground link names the immediate parent transcript. A durable head
/// names only the containing root session, so it alone does not claim a parent.
fn subagent_projection(
    source: &SourceKey,
    session: &DevinSessionRow,
    primary: &DevinLineageProjection,
    agent_id: &str,
    parent_key: Option<&DevinLineageKey>,
) -> DevinResult<DevinLineageProjection> {
    let parent_session_id = match parent_key {
        Some(DevinLineageKey::Primary) => Some(primary.session_id),
        Some(DevinLineageKey::Subagent(parent_id)) => {
            Some(subagent_session_id(source, session, parent_id)?)
        }
        None => None,
    };
    Ok(DevinLineageProjection {
        session_id: subagent_session_id(source, session, agent_id)?,
        provider_session_id: format!("{}/subagents/{agent_id}", session.id),
        agent_scope: AgentScope::Subagent,
        parent_session_id,
        root_session_id: Some(primary.session_id),
        relationship: Some(ProviderNativeSessionRelationship::Delegated),
        working_directory: primary.working_directory.clone(),
    })
}

fn subagent_session_id(
    source: &SourceKey,
    session: &DevinSessionRow,
    agent_id: &str,
) -> DevinResult<StableEntityId> {
    let key = NativeSessionKey::composite(
        DEVIN_SESSION_NAMESPACE,
        vec![
            TypedKey::utf8(session.id.clone())?,
            TypedKey::utf8("subagents")?,
            TypedKey::utf8(agent_id.to_owned())?,
        ],
    )?;
    Ok(ctx_history_core::derive_session_id(
        ctx_history_core::SessionIdentityInput {
            source,
            logical_session_kind: DEVIN_LOGICAL_SESSION_KIND,
            native_session_key: &key,
        },
    )?)
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct DevinScanCounts {
    pub(super) sessions: u64,
    pub(super) complete_records: u64,
    pub(super) rejected_records: u64,
    pub(super) ignored_nodes: u64,
    pub(super) rejected_lineages: u64,
    pub(super) rejected_splices: u64,
    pub(super) rejected_sessions: u64,
    pub(super) certified_bytes: u64,
}

pub(super) struct DevinSourceBackedScan {
    pub(super) counts: DevinScanCounts,
    pub(super) logical_fingerprint: [u8; 32],
    pub(super) record_rejections: SourceBackedRecordRejectionDrafts,
    schema_evidence: String,
}

impl DevinSourceBackedScan {
    /// The scan's counts in Core's classification terms.
    ///
    /// `complete_records` is the total the scan classified, which Core
    /// requires to equal retained plus rejected plus ignored; the records
    /// actually published are `retained_records`.
    pub(super) fn scanned_counts(&self) -> DevinResult<ScannedSourceCounts> {
        let retained = self.counts.complete_records;
        let rejected = checked_add(
            checked_add(
                checked_add(self.counts.rejected_records, self.counts.rejected_sessions)?,
                self.counts.rejected_lineages,
            )?,
            self.counts.rejected_splices,
        )?;
        let classified = checked_add(checked_add(retained, rejected)?, self.counts.ignored_nodes)?;
        Ok(ScannedSourceCounts {
            complete_records: classified,
            retained_records: retained,
            rejected_records: rejected,
            ignored_records: self.counts.ignored_nodes,
            // One indexed document per retained record, matching the Core
            // records the adapter forwards.
            indexed_documents: retained,
            certified_bytes: self.counts.certified_bytes,
        })
    }

    /// Binds this scan's logical evidence to one Core certification identity.
    pub(super) fn certify(&self, source: SourceKey) -> DevinResult<CertifiedSource> {
        Ok(SqliteLogicalSnapshot::new(
            DEVIN_SOURCE_BACKED_PARSER_REVISION,
            self.schema_evidence.as_bytes(),
            self.logical_fingerprint,
            self.scanned_counts()?,
        )
        .certify(source)?)
    }
}

/// Projects every session in the snapshot, emitting Core records as they are
/// produced and folding the same evidence into the logical fingerprint.
pub(super) fn scan_devin_snapshot(
    conn: &Connection,
    schema: &DevinNativeSchema,
    source: &SourceKey,
    source_selector: &str,
    emit: &mut dyn FnMut(CoreRecord) -> DevinResult<()>,
) -> DevinResult<DevinSourceBackedScan> {
    let mut fingerprint =
        RelationFingerprint::new(DEVIN_LOGICAL_DATABASE_DOMAIN, &schema.capability_digest);
    let mut counts = DevinScanCounts::default();
    let mut cursor = None;
    let mut event_sequence = 0_u64;
    let mut record_rejections = SourceBackedRecordRejectionDrafts::default();

    loop {
        let page = read_session_page(conn, schema, cursor)?;
        if page.is_empty() {
            break;
        }
        for session in &page {
            counts.sessions = checked_add(counts.sessions, 1)?;
            if session.malformed_id {
                counts.rejected_sessions = checked_add(counts.rejected_sessions, 1)?;
                fingerprint.record::<DevinSourceBackedError>(
                    DEVIN_LOGICAL_SESSION_RELATION,
                    malformed_session_evidence(session),
                )?;
                record_devin_rejection(
                    &mut record_rejections,
                    source,
                    source_selector,
                    Some(session.sqlite_rowid),
                    SourceBackedRecordRejectionClass::MalformedRecord,
                    format!(
                        "Devin sessions row {} was rejected: id has SQLite storage class {} and {} bytes",
                        session.sqlite_rowid, session.id_storage_class, session.id_bytes
                    ),
                );
                continue;
            }
            if session.malformed_main_chain_id {
                counts.rejected_sessions = checked_add(counts.rejected_sessions, 1)?;
                fingerprint.record::<DevinSourceBackedError>(
                    DEVIN_LOGICAL_SESSION_RELATION,
                    malformed_session_evidence(session),
                )?;
                record_devin_rejection(
                    &mut record_rejections,
                    source,
                    source_selector,
                    None,
                    SourceBackedRecordRejectionClass::MalformedRecord,
                    format!(
                        "Devin session {} was rejected: main_chain_id has a non-integer SQLite scalar",
                        diagnostic_id(&session.id)
                    ),
                );
                continue;
            }
            if record_malformed_session_metadata_rejection(
                &mut record_rejections,
                source,
                source_selector,
                session,
            ) {
                counts.rejected_records = checked_add(counts.rejected_records, 1)?;
            }
            let session_facts = read_session_facts_with_row_shape_rejections(conn, &session.id)?;
            let heads = read_subagent_heads_with_row_shape_rejections(conn, &session.id)?;
            let plan = if session_facts.overflowed {
                reject_oversized_session(session_facts.total_rows)
            } else {
                plan_session(&session_facts.facts, session.main_chain_id, &heads.heads)
            };
            fingerprint.record::<DevinSourceBackedError>(
                DEVIN_LOGICAL_SESSION_RELATION,
                session_evidence(
                    session,
                    &plan,
                    &session_facts.facts,
                    &heads.heads,
                    session_facts.total_rows,
                    session_facts.overflowed,
                    &session_facts.malformed_nodes,
                    &heads.malformed_heads,
                    &session_facts.ambiguous_json_nodes,
                ),
            )?;
            for malformed in &session_facts.malformed_nodes {
                counts.rejected_records = checked_add(counts.rejected_records, 1)?;
                record_malformed_node_rejection(
                    &mut record_rejections,
                    source,
                    source_selector,
                    session,
                    malformed,
                );
            }
            if let Some(reason) = plan.rejection.as_ref() {
                counts.rejected_sessions = checked_add(counts.rejected_sessions, 1)?;
                let detail = malformed_parent_traversed(
                    &session_facts.facts,
                    session.main_chain_id,
                    &session_facts.malformed_parent_nodes,
                )
                .map(|node_id| {
                    format!(
                        "chain traversed node {node_id}, whose parent_node_id has a non-integer SQLite scalar"
                    )
                })
                .unwrap_or_else(|| session_rejection_detail(reason).to_owned());
                record_devin_rejection(
                    &mut record_rejections,
                    source,
                    source_selector,
                    session.main_chain_id,
                    SourceBackedRecordRejectionClass::MalformedRecord,
                    format!(
                        "Devin session {} was rejected: {}",
                        diagnostic_id(&session.id),
                        detail
                    ),
                );
            }
            let planned_nodes = plan
                .lineages
                .iter()
                .flat_map(|lineage| lineage.nodes.iter().map(|node| node.node_id))
                .collect::<std::collections::BTreeSet<_>>();
            let malformed_node_ids = session_facts
                .malformed_nodes
                .iter()
                .filter_map(|node| node.node_id)
                .collect::<std::collections::BTreeSet<_>>();
            let ambiguous_unplanned = session_facts
                .ambiguous_json_nodes
                .iter()
                .filter(|node| {
                    !planned_nodes.contains(&node.node_id)
                        && !malformed_node_ids.contains(&node.node_id)
                })
                .collect::<Vec<_>>();
            let ambiguous_reclassified =
                u64::try_from(ambiguous_unplanned.len()).unwrap_or(u64::MAX);
            let malformed_unplanned = session_facts
                .malformed_nodes
                .iter()
                .filter(|node| {
                    session_facts.overflowed
                        || (!node.malformed_node_id
                            && !node.malformed_parent_node_id
                            && node
                                .node_id
                                .is_some_and(|node_id| !planned_nodes.contains(&node_id)))
                })
                .count();
            let reclassified_ignored = checked_add(
                ambiguous_reclassified,
                u64::try_from(malformed_unplanned).unwrap_or(u64::MAX),
            )?;
            let session_ignored = plan
                .counts
                .ignored_nodes
                .checked_sub(reclassified_ignored)
                .ok_or(DevinSourceBackedError::CountOverflow)?;
            counts.ignored_nodes = checked_add(counts.ignored_nodes, session_ignored)?;
            counts.rejected_records = checked_add(counts.rejected_records, ambiguous_reclassified)?;
            for ambiguous in ambiguous_unplanned {
                record_ambiguous_json_rejection(
                    &mut record_rejections,
                    source,
                    source_selector,
                    session,
                    ambiguous,
                );
            }
            counts.rejected_lineages =
                checked_add(counts.rejected_lineages, plan.counts.rejected_lineages)?;
            counts.rejected_lineages = checked_add(
                counts.rejected_lineages,
                u64::try_from(heads.malformed_heads.len()).unwrap_or(u64::MAX),
            )?;
            for malformed in &heads.malformed_heads {
                record_malformed_head_rejection(
                    &mut record_rejections,
                    source,
                    source_selector,
                    session,
                    malformed,
                );
            }
            counts.rejected_splices =
                checked_add(counts.rejected_splices, plan.counts.rejected_splices)?;
            record_repeated_devin_rejection(
                &mut record_rejections,
                source,
                source_selector,
                session.main_chain_id,
                plan.counts.rejected_lineages,
                SourceBackedRecordRejectionClass::UnsupportedRecord,
                format!(
                    "Devin session {} contains an invalid, ambiguous, or overlapping subagent lineage",
                    diagnostic_id(&session.id)
                ),
            );
            record_repeated_devin_rejection(
                &mut record_rejections,
                source,
                source_selector,
                session.main_chain_id,
                plan.counts.rejected_splices,
                SourceBackedRecordRejectionClass::MalformedRecord,
                format!(
                    "Devin session {} contains a compaction splice whose referenced node is absent",
                    diagnostic_id(&session.id)
                ),
            );

            project_session(
                conn,
                source,
                session,
                &plan,
                &session_facts.malformed_nodes,
                &mut fingerprint,
                &mut counts,
                source_selector,
                &mut record_rejections,
                &mut event_sequence,
                emit,
            )?;
        }
        cursor = page.last().map(|session| session.sqlite_rowid);
    }

    Ok(DevinSourceBackedScan {
        counts,
        logical_fingerprint: fingerprint.finish(),
        record_rejections,
        schema_evidence: schema.capability_digest.clone(),
    })
}

#[allow(clippy::too_many_arguments)]
fn project_session(
    conn: &Connection,
    source: &SourceKey,
    session: &DevinSessionRow,
    plan: &DevinSessionPlan,
    malformed_nodes: &[DevinMalformedNode],
    fingerprint: &mut RelationFingerprint,
    counts: &mut DevinScanCounts,
    source_selector: &str,
    record_rejections: &mut SourceBackedRecordRejectionDrafts,
    event_sequence: &mut u64,
    emit: &mut dyn FnMut(CoreRecord) -> DevinResult<()>,
) -> DevinResult<()> {
    let Some(primary_plan) = plan.lineages.first() else {
        return Ok(());
    };
    debug_assert_eq!(primary_plan.key, DevinLineageKey::Primary);
    let primary = primary_projection(source, session)?;
    let unhydratable_nodes = malformed_nodes
        .iter()
        .filter(|node| !node.malformed_node_id && !node.malformed_parent_node_id)
        .filter_map(|node| node.node_id)
        .collect::<std::collections::BTreeSet<_>>();

    for lineage in &plan.lineages {
        let projection = match &lineage.key {
            DevinLineageKey::Primary => primary.clone(),
            DevinLineageKey::Subagent(agent_id) => subagent_projection(
                source,
                session,
                &primary,
                agent_id,
                lineage.parent_key.as_ref(),
            )?,
        };
        let node_ids = lineage
            .nodes
            .iter()
            .filter(|node| !unhydratable_nodes.contains(&node.node_id))
            .map(|node| node.node_id)
            .collect::<Vec<_>>();
        let splice_kinds = lineage
            .nodes
            .iter()
            .map(|node| (node.node_id, (node.chain_ord, node.splice_kind)))
            .collect::<std::collections::BTreeMap<_, _>>();
        // Within one lineage a repeated message_id whose payload is
        // byte-identical is a copy Devin rewrote, not a second turn.
        let mut seen_messages = std::collections::BTreeSet::<[u8; 32]>::new();
        let mut emit_error = None;

        hydrate_nodes::<DevinSourceBackedError>(conn, &session.id, &node_ids, &mut |node| {
            let (chain_ord, splice_kind) = splice_kinds[&node.node_id];
            let digest = node_row_digest(&node);
            fingerprint.record::<DevinSourceBackedError>(
                DEVIN_LOGICAL_NODE_RELATION,
                node_evidence(&node, lineage.lineage_ord, chain_ord, splice_kind, digest),
            )?;

            let metadata_has_duplicate_keys = node.metadata.as_deref().is_some_and(|metadata| {
                serde_json::from_str::<serde_json::Value>(metadata).is_ok()
                    && !raw_object_keys_are_unique(metadata.as_bytes())
            });
            if metadata_has_duplicate_keys {
                counts.rejected_records = checked_add(counts.rejected_records, 1)?;
                record_devin_rejection(
                    record_rejections,
                    source,
                    source_selector,
                    Some(node.node_id),
                    SourceBackedRecordRejectionClass::MalformedRecord,
                    format!(
                        "Devin session {} node {} has ambiguous duplicate metadata JSON keys",
                        diagnostic_id(&session.id),
                        node.node_id
                    ),
                );
                return Ok(());
            }
            let summarized_from = node
                .metadata
                .as_deref()
                .filter(|metadata| raw_object_keys_are_unique(metadata.as_bytes()))
                .and_then(|metadata| serde_json::from_str::<serde_json::Value>(metadata).ok())
                .and_then(|metadata| {
                    metadata
                        .get("summarized_from")
                        .and_then(serde_json::Value::as_i64)
                });
            let normalized = normalize_node(&node.chat_message, summarized_from)?;
            match normalized.disposition {
                Some(DevinNodeDisposition::Unsupported) => {
                    counts.rejected_records = checked_add(counts.rejected_records, 1)?;
                    record_devin_rejection(
                        record_rejections,
                        source,
                        source_selector,
                        Some(node.node_id),
                        SourceBackedRecordRejectionClass::UnsupportedRecord,
                        format!(
                            "Devin session {} node {} has an unsupported or malformed message payload",
                            diagnostic_id(&session.id),
                            node.node_id
                        ),
                    );
                    return Ok(());
                }
                Some(DevinNodeDisposition::Empty) => {
                    counts.ignored_nodes = checked_add(counts.ignored_nodes, 1)?;
                    return Ok(());
                }
                None => {}
            }
            if let Some(message_id) = normalized.message_id.as_deref() {
                if !seen_messages.insert(message_pair_digest(message_id, &node.chat_message)) {
                    counts.ignored_nodes = checked_add(counts.ignored_nodes, 1)?;
                    return Ok(());
                }
            }

            for (subrecord_index, mut event) in normalized.events.into_iter().enumerate() {
                if event.event_type == EventType::ToolOutput {
                    if let Some(call_id) = event.provider_call_id.clone() {
                        let state = read_tool_state(conn, &session.id, &call_id)?;
                        counts.rejected_records = checked_add(
                            counts.rejected_records,
                            u64::try_from(state.malformed_fields().count()).unwrap_or(u64::MAX),
                        )?;
                        record_malformed_tool_state_rejections(
                            record_rejections,
                            source,
                            source_selector,
                            session,
                            node.node_id,
                            &call_id,
                            &state,
                        );
                        enrich_tool_output(
                            &mut event,
                            state.tool_call.as_ref(),
                            state.tool_call_update.as_ref(),
                        );
                        if let Some(evidence) = state.evidence {
                            fingerprint.record::<DevinSourceBackedError>(
                                DEVIN_LOGICAL_TOOL_STATE_RELATION,
                                evidence,
                            )?;
                        }
                    }
                }
                let record = match devin_core_record(
                    source,
                    &projection,
                    &node,
                    &normalized.created_at_rfc3339,
                    &event,
                    subrecord_index as u32,
                    *event_sequence,
                ) {
                    Ok(record) => record,
                    Err(error) => {
                        counts.rejected_records = checked_add(counts.rejected_records, 1)?;
                        record_devin_rejection(
                            record_rejections,
                            source,
                            source_selector,
                            Some(node.node_id),
                            SourceBackedRecordRejectionClass::UnsupportedRecord,
                            format!(
                                "Devin session {} node {} could not satisfy the Core projection contract: {error}",
                                diagnostic_id(&session.id),
                                node.node_id
                            ),
                        );
                        continue;
                    }
                };
                *event_sequence = checked_add(*event_sequence, 1)?;
                counts.complete_records = checked_add(counts.complete_records, 1)?;
                counts.certified_bytes =
                    checked_add(counts.certified_bytes, node.chat_message.len() as u64)?;
                if let Err(error) = emit(record) {
                    emit_error = Some(error);
                    return Ok(());
                }
            }
            Ok(())
        })?;

        if let Some(error) = emit_error {
            return Err(error);
        }
    }
    Ok(())
}

/// The logical row digest for one node, over the columns that carry content.
///
/// `row_id` is deliberately excluded: it is an insertion artifact, and a
/// database that was vacuumed or recopied must still fingerprint identically.
fn node_row_digest(node: &DevinNodeRow) -> [u8; 32] {
    sqlite_logical_record_digest_bytes(&[
        NativeSqliteValue::Text(node.chat_message.clone()),
        NativeSqliteValue::Integer(node.created_at),
        node.metadata
            .clone()
            .map_or(NativeSqliteValue::Null, NativeSqliteValue::Text),
    ])
}

/// Content identity used only to collapse Devin's rewritten compaction copy.
/// Row timestamps and metadata remain in the logical fingerprint, but they do
/// not make the same exact native message a second searchable turn.
pub(super) fn message_pair_digest(message_id: &str, chat_message: &str) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    hash_text(&mut digest, message_id);
    hash_text(&mut digest, chat_message);
    digest.finalize().into()
}

#[allow(clippy::too_many_arguments)]
fn devin_core_record(
    source: &SourceKey,
    projection: &DevinLineageProjection,
    node: &DevinNodeRow,
    created_at_rfc3339: &Option<String>,
    event: &DevinNativeEvent,
    subrecord_index: u32,
    event_sequence: u64,
) -> DevinResult<CoreRecord> {
    let native_item_key = NativeItemKey::composite(
        DEVIN_EVENT_NAMESPACE,
        vec![
            TypedKey::utf8(projection.provider_session_id.clone())?,
            TypedKey::I64(node.node_id),
        ],
    )?;
    // One node fans out into several events, and the slot each event occupies
    // is fixed by the node's own payload: reasoning, then content, then each
    // tool call in the order the node lists them. Re-reading the same payload
    // therefore yields the same slot, which is what StableSlot asserts.
    let subrecord_selector = SubrecordSelector::certified_position(
        DEVIN_SUBRECORD_KIND,
        TypedKey::U64(u64::from(subrecord_index)),
        PositionStability::StableSlot,
    )?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id: projection.session_id,
        logical_item_kind: DEVIN_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: Some(&subrecord_selector),
    })?;

    // Devin writes integer seconds; the RFC3339 copy in the payload is the
    // fallback when a row's own column is unusable.
    let occurred_at_unix_ms = Some(
        provider_timestamp_seconds(Some(node.created_at as f64), DateTime::<Utc>::UNIX_EPOCH)
            .timestamp_millis(),
    )
    .filter(|millis| *millis != 0)
    .or_else(|| {
        created_at_rfc3339
            .as_deref()
            .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
            .map(|parsed| parsed.timestamp_millis())
    });

    let mut record = CoreRecord::new_selected(
        event_id,
        projection.session_id,
        source.clone(),
        event_sequence,
        event.event_type.as_str(),
        DEVIN_SOURCE_BACKED_PARSER_REVISION,
        event.text.clone(),
    )?;
    record.agent_scope = Some(projection.agent_scope);
    record.parent_session_id = projection.parent_session_id;
    record.root_session_id = projection.root_session_id;
    record.session_relationship = projection.relationship;
    record.provider_session_id = Some(projection.provider_session_id.clone());
    record.occurred_at_unix_ms = occurred_at_unix_ms;
    record.role = Some(event.role.as_str().to_owned());

    let mut facts = Vec::new();
    if let Some(cwd) = projection.working_directory.as_deref() {
        push_fact(&mut facts, LiteralFactKind::SessionCwd, cwd);
    }
    if let Some(workdir) = event.workdir.as_deref() {
        push_fact(&mut facts, LiteralFactKind::ToolWorkdir, workdir);
    }
    if let Some(command) = event.command.as_deref() {
        push_fact(&mut facts, LiteralFactKind::Command, command);
    }
    for path in &event.file_paths {
        push_fact(&mut facts, LiteralFactKind::File, path);
    }

    let (invocation, result) = devin_activity(event, occurred_at_unix_ms);
    let provider_call_id = admit_optional_provider_call_id(event.provider_call_id.clone());
    if invocation.is_some() || result.is_some() || !facts.is_empty() || provider_call_id.is_some() {
        record.content.activity = Some(CoreActivity {
            revision: CORE_ACTIVITY_REVISION,
            provider_call_id,
            invocation,
            result,
            facts,
        });
    }
    record.content.structured_content = event.structured_content.clone();
    record
        .content
        .omit_structured_content_if_aggregate_exceeds_limit()?;
    record.validate_contract()?;
    Ok(record)
}

fn push_fact(facts: &mut Vec<ProviderDeclaredFact>, kind: LiteralFactKind, value: &str) {
    let Some(value) = non_empty(value) else {
        return;
    };
    if let Some(fact) = ctx_history_core::admit_provider_declared_fact(kind, value, facts.len()) {
        facts.push(fact);
    }
}

/// The invocation and result channels for a tool event.
///
/// No `protocol` is claimed: Devin's MCP dispatch does carry a server alias in
/// its arguments, but ctx has no published writer contract for that field, so
/// the capability matrix records the route as not qualified and this projection
/// stays silent rather than asserting one.
fn devin_activity(
    event: &DevinNativeEvent,
    occurred_at_unix_ms: Option<i64>,
) -> (Option<ActivityInvocation>, Option<ActivityResult>) {
    match event.event_type {
        EventType::ToolCall => {
            let tool = event.tool_name.clone().unwrap_or_default();
            if tool.is_empty() {
                return (None, None);
            }
            let arguments = event
                .structured_content
                .clone()
                .map_or(ActivityJsonCapture::Absent, |value| {
                    ActivityJsonCapture::Present { value }
                });
            (
                Some(ActivityInvocation {
                    protocol: None,
                    server: None,
                    tool,
                    arguments,
                    started_at_unix_ms: occurred_at_unix_ms,
                }),
                None,
            )
        }
        EventType::ToolOutput => (
            None,
            Some(ActivityResult {
                status: event.status.clone(),
                completed_at_unix_ms: event
                    .completed_at
                    .as_deref()
                    .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
                    .map(|parsed| parsed.timestamp_millis())
                    .or(occurred_at_unix_ms),
                duration_ns: event
                    .duration_ms
                    .and_then(|millis| u64::try_from(millis).ok())
                    .and_then(|millis| millis.checked_mul(1_000_000)),
                text: ActivityTextCapture::NormalizedBody,
                structured_content: ActivityJsonCapture::Absent,
            }),
        ),
        _ => (None, None),
    }
}

/// The digest published for a database whose logical tree is absent.
pub(super) fn devin_missing_tree_fingerprint(source: &SourceKey) -> [u8; 32] {
    crate::fingerprint::missing_tree_fingerprint(b"ctx.devin.missing-logical-tree.v1\0", source)
}
