//! Thin source-backed projection adapter for Claude Code project transcripts.
//!
//! Discovery, parsing, checkpoint validation, and exact record reopening stay
//! provider-owned. The shared coordinator remains responsible for lifecycle
//! admission, staging, deletion, and publication.

use std::{
    collections::HashSet,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CertifiedSource, CertifiedSourceInventory,
    EventIdentityInput, EventType, LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate,
    NativeSessionKey, PositionStability, ProjectionContractError, ScannedSourceCounts,
    SessionIdentityInput, SourceAnchor, SourceFrontier, SourceInventoryObservation, SourceKey,
    SourceObservation, SourceRecordLocator, SourceResolverContractError, StableEntityId, TypedKey,
};
use ctx_history_index::LexicalDocument;
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    checkpoint::ChangeSignal,
    source::{open_discovered_file, revalidate_open_file, ClaudeDiscovery, ClaudeSessionKey},
    ClaudeEventKind, ClaudeNativeOwnedPage, ClaudeNativePathError, ClaudeNativeProfile,
    ClaudeNativeScanner, ClaudeRetainedRow, ClaudeSessionMetadata, DiscoveredClaudeSession,
    ParseCheckpoint, SessionLayout,
};
use crate::{
    provider::normalization::provider_policy_event_text, CLAUDE_PROJECTS_SOURCE_FORMAT,
    MAX_PROVIDER_JSONL_LINE_BYTES,
};

const SOURCE_ANCHOR_NAMESPACE: &str = "claude.session-leaf";
const SESSION_KEY_NAMESPACE: &str = "claude.session";
const NATIVE_EVENT_KEY_NAMESPACE: &str = "claude.event";
const EVENT_POSITION_KIND: &str = "claude.jsonl.event-position";
const LOGICAL_SESSION_KIND: &str = "claude-session";
const LOGICAL_EVENT_KIND: &str = "claude-event";
const SOURCE_SCHEMA_VARIANT: &str = "claude-nativepath-jsonl-v5";
const SOURCE_REVISION_KIND: &str = "claude-ordinary-file-observation-v1";
const INVENTORY_AUTHORITY_NAMESPACE: &str = "claude.projects-root";
const INVENTORY_REVISION_KIND: &str = "claude-projects-inventory-v1";
const INVENTORY_DISCOVERY_REVISION: &str = "claude-projects-discovery-v1";
const FRONTIER_KIND: &str = "claude-nativepath-checkpoint-v5";
const PARSER_REVISION: &str = "claude-nativepath-source-backed-v1";
const MAX_HYDRATED_RECORD_BYTES: u64 = MAX_PROVIDER_JSONL_LINE_BYTES as u64 + 1;

#[derive(Debug, Error)]
pub(crate) enum ClaudeSourceBackedError {
    #[error(transparent)]
    Native(#[from] ClaudeNativePathError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    Resolver(#[from] SourceResolverContractError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("Claude source-backed discovery requires an authoritative projects directory")]
    NonAuthoritativeRoot,
    #[error("Claude authoritative inventory contains duplicate native leaf identity {0:?}")]
    DuplicateLeaf(String),
    #[error("Claude source-backed checkpoint is missing or malformed")]
    InvalidCheckpoint,
    #[error("Claude source-backed scan cannot publish a live copy or ambiguous lineage")]
    AmbiguousLifecycle,
    #[error("Claude source-backed scan counts do not reconcile")]
    CountMismatch,
    #[error("Claude source-backed event sequence overflow")]
    EventSequenceOverflow,
    #[error("locator is not a Claude project JSONL record")]
    InvalidLocator,
    #[error("Claude locator source is absent from the authoritative projects inventory")]
    LocatorSourceMissing,
    #[error("Claude locator byte range is invalid or too large")]
    LocatorRangeInvalid,
    #[error("Claude locator record is absent")]
    LocatorRecordMissing,
    #[error("Claude locator record evidence is stale")]
    LocatorRecordChanged,
    #[error("Claude locator record has no exact canonical logical display content")]
    ExactDisplayUnavailable,
}

pub(crate) type ClaudeSourceBackedResult<T> = Result<T, ClaudeSourceBackedError>;

#[derive(Debug, Clone)]
pub(crate) struct ClaudeSourceBackedLeaf {
    source: DiscoveredClaudeSession,
    source_key: SourceKey,
    session_id: StableEntityId,
    parent_session_id: Option<StableEntityId>,
    root_session_id: StableEntityId,
    native_session_key: TypedKey,
    agent_type: &'static str,
    is_primary: bool,
}

impl ClaudeSourceBackedLeaf {
    pub(crate) fn source_key(&self) -> &SourceKey {
        &self.source_key
    }

    pub(crate) fn session_id(&self) -> StableEntityId {
        self.session_id
    }

    pub(crate) fn provider_session_id(&self) -> String {
        self.source.key.provider_session_id()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ClaudeSourceBackedInventory {
    discovery: ClaudeDiscovery,
    opening: SourceInventoryObservation,
    leaves: Vec<ClaudeSourceBackedLeaf>,
}

impl ClaudeSourceBackedInventory {
    pub(crate) fn leaves(&self) -> &[ClaudeSourceBackedLeaf] {
        &self.leaves
    }

    /// Repeats the bounded provider traversal and certifies only an unchanged,
    /// complete inventory. The shared coordinator may derive deletions from
    /// this proof; an explicit file import never reaches this API.
    pub(crate) fn certify(&self) -> ClaudeSourceBackedResult<CertifiedSourceInventory> {
        let closing_discovery = self.discovery.rediscover()?;
        if !closing_discovery.has_directory_authority() || !closing_discovery.inventory.complete {
            return Err(ClaudeSourceBackedError::NonAuthoritativeRoot);
        }
        let (closing, closing_leaves) = bind_discovery(&closing_discovery)?;
        Ok(CertifiedSourceInventory::certify(
            self.opening.clone(),
            closing,
            INVENTORY_DISCOVERY_REVISION,
            closing_leaves
                .into_iter()
                .map(|leaf| leaf.source_key)
                .collect(),
        )?)
    }
}

pub(crate) fn discover_claude_source_backed(
    projects_root: &Path,
) -> ClaudeSourceBackedResult<ClaudeSourceBackedInventory> {
    let discovery = authoritative_discovery(projects_root)?;
    let (opening, leaves) = bind_discovery(&discovery)?;
    Ok(ClaudeSourceBackedInventory {
        discovery,
        opening,
        leaves,
    })
}

fn authoritative_discovery(root: &Path) -> ClaudeSourceBackedResult<ClaudeDiscovery> {
    let discovery = super::discover_projects(root)?;
    if !discovery.has_directory_authority() || !discovery.inventory.complete {
        return Err(ClaudeSourceBackedError::NonAuthoritativeRoot);
    }
    Ok(discovery)
}

fn bind_discovery(
    discovery: &ClaudeDiscovery,
) -> ClaudeSourceBackedResult<(SourceInventoryObservation, Vec<ClaudeSourceBackedLeaf>)> {
    let mut source_ids = HashSet::with_capacity(discovery.sessions.len());
    let mut leaves = Vec::with_capacity(discovery.sessions.len());
    for source in &discovery.sessions {
        let (native_session_key, source_key, session_id) = claude_session_identity(&source.key)?;
        if !source_ids.insert(source_key.identity().digest()) {
            return Err(ClaudeSourceBackedError::DuplicateLeaf(
                source.key.provider_session_id(),
            ));
        }
        let root_key = ClaudeSessionKey {
            root_session_id: source.key.root_session_id.clone(),
            workflow_run_id: None,
            agent_id: None,
        };
        let root_session_id = if source.layout == SessionLayout::Primary {
            session_id
        } else {
            claude_session_identity(&root_key)?.2
        };
        let parent_session_id = source.key.agent_id.as_ref().map(|_| root_session_id);
        let (agent_type, is_primary) = match source.layout {
            SessionLayout::Primary => ("primary", true),
            SessionLayout::Subagent => ("subagent", false),
            SessionLayout::WorkflowSubagent => ("workflow_subagent", false),
        };
        leaves.push(ClaudeSourceBackedLeaf {
            source: source.clone(),
            source_key,
            session_id,
            parent_session_id,
            root_session_id,
            native_session_key,
            agent_type,
            is_primary,
        });
    }
    let observation = inventory_observation(discovery)?;
    Ok((observation, leaves))
}

fn inventory_observation(
    discovery: &ClaudeDiscovery,
) -> ClaudeSourceBackedResult<SourceInventoryObservation> {
    let mut revision = Vec::with_capacity(41);
    revision.extend_from_slice(
        &u64::try_from(discovery.inventory.route_count)
            .map_err(|_| ClaudeSourceBackedError::CountMismatch)?
            .to_be_bytes(),
    );
    revision.extend_from_slice(&discovery.inventory.routes_sha256);
    revision.push(u8::from(discovery.inventory.complete));
    Ok(SourceInventoryObservation::new(
        CaptureProvider::Claude.as_str(),
        INVENTORY_AUTHORITY_NAMESPACE,
        TypedKey::bytes(
            discovery
                .inventory
                .canonical_root
                .as_os_str()
                .as_encoded_bytes()
                .to_vec(),
        )?,
        INVENTORY_REVISION_KIND,
        revision,
    )?)
}

fn claude_session_typed_key(key: &ClaudeSessionKey) -> ClaudeSourceBackedResult<TypedKey> {
    Ok(TypedKey::composite(vec![
        TypedKey::utf8(&key.root_session_id)?,
        key.workflow_run_id
            .as_deref()
            .map(TypedKey::utf8)
            .transpose()?
            .unwrap_or(TypedKey::Null),
        key.agent_id
            .as_deref()
            .map(TypedKey::utf8)
            .transpose()?
            .unwrap_or(TypedKey::Null),
    ])?)
}

fn claude_source_key(native_session_key: &TypedKey) -> ClaudeSourceBackedResult<SourceKey> {
    let anchor =
        SourceAnchor::provider_native(SOURCE_ANCHOR_NAMESPACE, native_session_key.clone())?;
    Ok(SourceKey::derive(
        CaptureProvider::Claude.as_str(),
        CLAUDE_PROJECTS_SOURCE_FORMAT,
        SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )?)
}

fn claude_session_identity(
    key: &ClaudeSessionKey,
) -> ClaudeSourceBackedResult<(TypedKey, SourceKey, StableEntityId)> {
    let native_session_key = claude_session_typed_key(key)?;
    let source_key = claude_source_key(&native_session_key)?;
    let session_key =
        NativeSessionKey::native_id(SESSION_KEY_NAMESPACE, native_session_key.clone())?;
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source_key,
        logical_session_kind: LOGICAL_SESSION_KIND,
        native_session_key: &session_key,
    })?;
    Ok((native_session_key, source_key, session_id))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClaudeSourceBackedDisposition {
    Full,
    Append,
    Unchanged,
}

#[derive(Debug)]
pub(crate) struct ClaudeSourceBackedPage {
    pub(crate) documents: Vec<LexicalDocument>,
    pub(crate) next_frontier: SourceFrontier,
    pub(crate) terminal: bool,
}

#[derive(Debug)]
pub(crate) struct ClaudeSourceBackedScan {
    pub(crate) disposition: ClaudeSourceBackedDisposition,
    pub(crate) source: CertifiedSource,
}

pub(crate) struct ClaudeSourceBackedScanner {
    leaf: ClaudeSourceBackedLeaf,
    inner: ClaudeNativeScanner,
    base_counts: Option<ScannedSourceCounts>,
    staged_documents: u64,
    retained_physical_records: u64,
    last_retained_ordinal: Option<u64>,
}

impl ClaudeSourceBackedScanner {
    pub(crate) fn new(
        leaf: ClaudeSourceBackedLeaf,
        previous: Option<&CertifiedSource>,
    ) -> ClaudeSourceBackedResult<Self> {
        let previous_checkpoint = previous
            .map(|certificate| decode_checkpoint(&leaf, certificate))
            .transpose()?;
        let inner = ClaudeNativeScanner::new(
            leaf.source.clone(),
            previous_checkpoint.as_ref(),
            ClaudeNativeProfile::CoreOnly,
        )?;
        Ok(Self {
            leaf,
            inner,
            base_counts: previous.map(CertifiedSource::counts),
            staged_documents: 0,
            retained_physical_records: 0,
            last_retained_ordinal: None,
        })
    }

    pub(crate) fn next_page(&mut self) -> ClaudeSourceBackedResult<Option<ClaudeSourceBackedPage>> {
        let Some(page) = self.inner.next_page()? else {
            return Ok(None);
        };
        let ClaudeNativeOwnedPage::Core(page) = page else {
            return Err(ClaudeSourceBackedError::InvalidCheckpoint);
        };
        let checkpoint = self
            .inner
            .checkpoint_at(&page.next_safe_frontier, page.terminal);
        let next_frontier = source_frontier(&checkpoint)?;
        let session = page.session.clone();
        let mut documents = Vec::with_capacity(page.rows.len());
        for row in page.rows {
            if self.last_retained_ordinal != Some(row.identity.source_record_ordinal) {
                self.retained_physical_records = self
                    .retained_physical_records
                    .checked_add(1)
                    .ok_or(ClaudeSourceBackedError::CountMismatch)?;
                self.last_retained_ordinal = Some(row.identity.source_record_ordinal);
            }
            documents.push(lexical_document(&self.leaf, &session, row)?);
            self.staged_documents = self
                .staged_documents
                .checked_add(1)
                .ok_or(ClaudeSourceBackedError::CountMismatch)?;
        }
        Ok(Some(ClaudeSourceBackedPage {
            documents,
            next_frontier,
            terminal: page.terminal,
        }))
    }

    pub(crate) fn finish(self) -> ClaudeSourceBackedResult<ClaudeSourceBackedScan> {
        let opening = source_observation(&self.leaf)?;
        let output = self.inner.finish()?;
        let disposition = match output.change {
            ChangeSignal::Fresh
            | ChangeSignal::Rewrite
            | ChangeSignal::Truncation
            | ChangeSignal::Replacement
            | ChangeSignal::Reparse => ClaudeSourceBackedDisposition::Full,
            ChangeSignal::Append | ChangeSignal::Relocation => {
                ClaudeSourceBackedDisposition::Append
            }
            ChangeSignal::Unchanged => ClaudeSourceBackedDisposition::Unchanged,
            ChangeSignal::LiveCopy | ChangeSignal::ConflictingLiveCopy => {
                return Err(ClaudeSourceBackedError::AmbiguousLifecycle);
            }
        };
        let delta = scan_counts(
            output.stats.complete_records,
            self.retained_physical_records,
            self.staged_documents,
            output.rejections.total,
            output.checkpoint.complete_offset,
        )?;
        let counts = match disposition {
            ClaudeSourceBackedDisposition::Full => delta,
            ClaudeSourceBackedDisposition::Append => {
                add_counts(self.base_counts.unwrap_or_default(), delta)?
            }
            ClaudeSourceBackedDisposition::Unchanged => {
                if self.staged_documents != 0 {
                    return Err(ClaudeSourceBackedError::CountMismatch);
                }
                self.base_counts
                    .ok_or(ClaudeSourceBackedError::InvalidCheckpoint)?
            }
        };
        let frontier = source_frontier(&output.checkpoint)?;
        let closing = source_observation(&self.leaf)?;
        let source = CertifiedSource::certify_with_frontier(
            opening,
            closing,
            PARSER_REVISION,
            *frontier.certified_prefix_digest(),
            counts,
            Some(frontier),
        )?;
        Ok(ClaudeSourceBackedScan {
            disposition,
            source,
        })
    }
}

fn decode_checkpoint(
    leaf: &ClaudeSourceBackedLeaf,
    certificate: &CertifiedSource,
) -> ClaudeSourceBackedResult<ParseCheckpoint> {
    leaf.source_key
        .validate_exact_descriptor(certificate.observation().source())?;
    if certificate.parser_revision() != PARSER_REVISION {
        return Err(ClaudeSourceBackedError::InvalidCheckpoint);
    }
    let frontier = certificate
        .frontier()
        .ok_or(ClaudeSourceBackedError::InvalidCheckpoint)?;
    if frontier.checkpoint_kind() != FRONTIER_KIND {
        return Err(ClaudeSourceBackedError::InvalidCheckpoint);
    }
    let TypedKey::Bytes(bytes) = frontier.checkpoint() else {
        return Err(ClaudeSourceBackedError::InvalidCheckpoint);
    };
    let checkpoint: ParseCheckpoint = serde_json::from_slice(bytes)?;
    if checkpoint.session_key != leaf.source.key {
        return Err(ClaudeSourceBackedError::InvalidCheckpoint);
    }
    Ok(checkpoint)
}

fn source_frontier(checkpoint: &ParseCheckpoint) -> ClaudeSourceBackedResult<SourceFrontier> {
    Ok(SourceFrontier::new(
        FRONTIER_KIND,
        TypedKey::bytes(serde_json::to_vec(checkpoint)?)?,
        checkpoint.complete_offset,
        checkpoint.complete_record_chain_sha256,
    )?)
}

fn source_observation(
    leaf: &ClaudeSourceBackedLeaf,
) -> ClaudeSourceBackedResult<SourceObservation> {
    Ok(SourceObservation::new(
        leaf.source_key.clone(),
        SOURCE_REVISION_KIND,
        serde_json::to_vec(&leaf.source.fingerprint)?,
    )?)
}

fn scan_counts(
    physical_complete: u64,
    retained_physical: u64,
    documents: u64,
    rejected: u64,
    certified_bytes: u64,
) -> ClaudeSourceBackedResult<ScannedSourceCounts> {
    let classified_physical = retained_physical
        .checked_add(rejected)
        .ok_or(ClaudeSourceBackedError::CountMismatch)?;
    let ignored = physical_complete
        .checked_sub(classified_physical)
        .ok_or(ClaudeSourceBackedError::CountMismatch)?;
    let logical_complete = documents
        .checked_add(rejected)
        .and_then(|count| count.checked_add(ignored))
        .ok_or(ClaudeSourceBackedError::CountMismatch)?;
    Ok(ScannedSourceCounts {
        complete_records: logical_complete,
        retained_records: documents,
        rejected_records: rejected,
        ignored_records: ignored,
        indexed_documents: documents,
        certified_bytes,
    })
}

fn add_counts(
    base: ScannedSourceCounts,
    delta: ScannedSourceCounts,
) -> ClaudeSourceBackedResult<ScannedSourceCounts> {
    Ok(ScannedSourceCounts {
        complete_records: checked_add(base.complete_records, delta.complete_records)?,
        retained_records: checked_add(base.retained_records, delta.retained_records)?,
        rejected_records: checked_add(base.rejected_records, delta.rejected_records)?,
        ignored_records: checked_add(base.ignored_records, delta.ignored_records)?,
        indexed_documents: checked_add(base.indexed_documents, delta.indexed_documents)?,
        certified_bytes: delta.certified_bytes,
    })
}

fn checked_add(left: u64, right: u64) -> ClaudeSourceBackedResult<u64> {
    left.checked_add(right)
        .ok_or(ClaudeSourceBackedError::CountMismatch)
}

fn lexical_document(
    leaf: &ClaudeSourceBackedLeaf,
    session: &ClaudeSessionMetadata,
    row: ClaudeRetainedRow,
) -> ClaudeSourceBackedResult<LexicalDocument> {
    let native_item_key = native_item_key(&row)?;
    let event_id = derive_event_id(EventIdentityInput {
        source: &leaf.source_key,
        session_id: leaf.session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let byte_length = row
        .locator
        .byte_end_exclusive
        .checked_sub(row.locator.byte_start)
        .ok_or(ClaudeSourceBackedError::LocatorRangeInvalid)?;
    let locator = SourceRecordLocator::new(
        leaf.source_key.clone(),
        NativeRecordCoordinate::Jsonl {
            byte_offset: row.locator.byte_start,
            byte_length,
            physical_ordinal: row.identity.source_record_ordinal,
            native_session_key: Some(leaf.native_session_key.clone()),
            native_event_key: Some(native_event_typed_key(&row)?),
        },
        LocatorRevisionPolicy::StableRecordEvidence,
        None,
        row.locator.record_sha256,
    )?;
    let event_sequence = row
        .identity
        .source_record_ordinal
        .checked_mul(1_u64 << 16)
        .and_then(|value| value.checked_add(row.identity.source_subrecord_index))
        .ok_or(ClaudeSourceBackedError::EventSequenceOverflow)?;
    let touched_files = row
        .tool_call
        .as_ref()
        .map(|call| {
            call.file_touches
                .iter()
                .map(|touch| touch.path.clone())
                .collect()
        })
        .unwrap_or_default();
    Ok(LexicalDocument {
        event_id,
        session_id: leaf.session_id,
        parent_session_id: leaf.parent_session_id,
        root_session_id: leaf.root_session_id,
        source: leaf.source_key.clone(),
        locator,
        provider_session_id: Some(leaf.source.key.provider_session_id()),
        branch: session.git_branch.clone(),
        source_path: Some(leaf.source.canonical_path.to_string_lossy().into_owned()),
        agent_type: leaf.agent_type.to_owned(),
        is_primary: leaf.is_primary,
        event_sequence,
        occurred_at_unix_ms: row
            .occurred_at
            .as_deref()
            .and_then(|value| value.parse::<DateTime<Utc>>().ok())
            .map(|value| value.timestamp_millis()),
        event_type: event_kind(row.kind).to_owned(),
        role: row.role.clone(),
        body: lexical_body(&row),
        workspace: leaf.source.project_dir.to_str().map(str::to_owned),
        cwd: session.cwd.clone(),
        touched_files,
    })
}

fn native_item_key(row: &ClaudeRetainedRow) -> ClaudeSourceBackedResult<NativeItemKey> {
    if let Some(native_record_id) = row.native_record_id.as_deref() {
        return Ok(NativeItemKey::composite(
            NATIVE_EVENT_KEY_NAMESPACE,
            vec![
                TypedKey::utf8(native_record_id)?,
                TypedKey::U64(row.identity.source_subrecord_index),
            ],
        )?);
    }
    Ok(NativeItemKey::certified_position(
        EVENT_POSITION_KIND,
        native_event_typed_key(row)?,
        PositionStability::AppendStable,
    )?)
}

fn native_event_typed_key(row: &ClaudeRetainedRow) -> ClaudeSourceBackedResult<TypedKey> {
    Ok(TypedKey::composite(vec![
        row.native_record_id
            .as_deref()
            .map(TypedKey::utf8)
            .transpose()?
            .unwrap_or(TypedKey::Null),
        TypedKey::U64(row.identity.source_record_ordinal),
        TypedKey::U64(row.identity.source_subrecord_index),
    ])?)
}

fn lexical_body(row: &ClaudeRetainedRow) -> String {
    let text = row
        .body
        .clone()
        .or_else(|| {
            row.tool_call.as_ref().map(|call| {
                let mut parts = vec!["tool call".to_owned()];
                parts.extend(call.tool_name.clone());
                parts.extend(call.call_id.clone());
                parts.extend(call.file_touches.iter().map(|touch| touch.path.clone()));
                parts.join(" ")
            })
        })
        .or_else(|| {
            row.sparse_output.as_ref().map(|output| {
                format!(
                    "tool output {}{}{}",
                    match output.outcome {
                        super::rows::ClaudeOutputOutcome::Failure => "failure",
                        super::rows::ClaudeOutputOutcome::Timeout => "timeout",
                    },
                    output
                        .call_id
                        .as_deref()
                        .map(|id| format!(" {id}"))
                        .unwrap_or_default(),
                    output
                        .exit_code
                        .map(|code| format!(" exit {code}"))
                        .unwrap_or_default()
                )
            })
        })
        .unwrap_or_else(|| event_kind(row.kind).to_owned());
    if text.trim().is_empty() {
        event_kind(row.kind).to_owned()
    } else {
        text
    }
}

fn event_kind(kind: ClaudeEventKind) -> &'static str {
    match kind {
        ClaudeEventKind::Message => "message",
        ClaudeEventKind::Summary => "summary",
        ClaudeEventKind::Notice => "notice",
        ClaudeEventKind::ToolCall => "tool_call",
        ClaudeEventKind::ToolOutput => "tool_output",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaudeHydratedRecord {
    pub(crate) provider_bytes: Vec<u8>,
    pub(crate) decoded_display_text: Option<String>,
}

/// Reopens one exact JSONL range through the provider's authoritative tree and
/// reuses the released Claude complete-message decoder for display text.
pub(crate) fn hydrate_claude_source_record(
    projects_root: &Path,
    locator: &SourceRecordLocator,
) -> ClaudeSourceBackedResult<ClaudeHydratedRecord> {
    locator.validate_contract()?;
    let inventory = discover_claude_source_backed(projects_root)?;
    let leaf = inventory
        .leaves
        .iter()
        .find(|leaf| leaf.source_key.exact_descriptor_eq(locator.source()))
        .ok_or(ClaudeSourceBackedError::LocatorSourceMissing)?;
    let NativeRecordCoordinate::Jsonl {
        byte_offset,
        byte_length,
        physical_ordinal,
        native_session_key,
        ..
    } = locator.coordinate()
    else {
        return Err(ClaudeSourceBackedError::InvalidLocator);
    };
    if locator.source().provider() != CaptureProvider::Claude.as_str()
        || locator.source().source_format() != CLAUDE_PROJECTS_SOURCE_FORMAT
        || locator.source().schema_variant() != SOURCE_SCHEMA_VARIANT
        || locator.source().provider_identity_version() != 1
        || native_session_key.as_ref() != Some(&leaf.native_session_key)
        || *byte_length == 0
        || *byte_length > MAX_HYDRATED_RECORD_BYTES
    {
        return Err(ClaudeSourceBackedError::InvalidLocator);
    }
    let byte_end = byte_offset
        .checked_add(*byte_length)
        .ok_or(ClaudeSourceBackedError::LocatorRangeInvalid)?;
    let mut file = open_discovered_file(&leaf.source)?;
    if file.metadata()?.len() < byte_end {
        return Err(ClaudeSourceBackedError::LocatorRecordMissing);
    }
    if *byte_offset > 0 {
        file.seek(SeekFrom::Start(byte_offset - 1))?;
        let mut prior = [0_u8; 1];
        file.read_exact(&mut prior)?;
        if prior[0] != b'\n' {
            return Err(ClaudeSourceBackedError::LocatorRecordChanged);
        }
    }
    file.seek(SeekFrom::Start(*byte_offset))?;
    let length =
        usize::try_from(*byte_length).map_err(|_| ClaudeSourceBackedError::LocatorRangeInvalid)?;
    let mut provider_bytes = vec![0_u8; length];
    file.read_exact(&mut provider_bytes)?;
    if provider_bytes.last() != Some(&b'\n')
        || provider_bytes[..provider_bytes.len().saturating_sub(1)].contains(&b'\n')
    {
        return Err(ClaudeSourceBackedError::LocatorRecordChanged);
    }
    let payload = json_record_bytes(&provider_bytes);
    let observed_digest: [u8; 32] = Sha256::digest(payload).into();
    if &observed_digest != locator.record_digest() {
        return Err(ClaudeSourceBackedError::LocatorRecordChanged);
    }
    let value: Value = serde_json::from_slice(payload)?;
    let line_number = usize::try_from(*physical_ordinal)
        .ok()
        .and_then(|ordinal| ordinal.checked_add(1))
        .ok_or(ClaudeSourceBackedError::LocatorRangeInvalid)?;
    let (text, _) =
        super::super::complete_content::claude_complete_content_message_record(&value, line_number)
            .ok_or(ClaudeSourceBackedError::ExactDisplayUnavailable)?;
    let retained = provider_policy_event_text(EventType::Message, &text, &Value::Null);
    let decoded_display_text = if retained.text.trim().is_empty() {
        "message".to_owned()
    } else {
        retained.text
    };
    revalidate_open_file(&leaf.source, &file, &leaf.source.fingerprint)?;
    Ok(ClaudeHydratedRecord {
        provider_bytes: decoded_display_text.as_bytes().to_vec(),
        decoded_display_text: Some(decoded_display_text),
    })
}

fn json_record_bytes(bytes: &[u8]) -> &[u8] {
    let without_newline = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    without_newline
        .strip_suffix(b"\r")
        .unwrap_or(without_newline)
}

#[cfg(test)]
mod tests;
