use std::{
    fs::File,
    io::{self, BufRead, BufReader, Seek, SeekFrom, Write},
};

use chrono::{DateTime, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    OutputAssociations, OutputNativeCoordinate, OutputObservationKind, OutputOutcome,
    OutputSourceLocator, ProOutputObservation, MAX_PROVIDER_JSONL_LINE_BYTES,
};

use super::{
    checkpoint::{
        lane_observation_binding, ChangeSignal, ClaudeNativeFrontier, ParseCheckpoint,
        CLAUDE_NATIVEPATH_PARSER_REVISION, CLAUDE_NATIVEPATH_POLICY_REVISION,
    },
    record::{parse_native_record, ClaudeRecordMode, ParsedClaudeRecord},
    rows::{
        ClaudePhysicalLocator, ClaudeRetainedRow, ClaudeRowPage, ClaudeSessionMetadata, ParseStats,
        RecordRejection, RejectionKind, RejectionSummary, CLAUDE_MAX_PAGE_BYTES,
        CLAUDE_MAX_PAGE_ROWS,
    },
    source::{
        open_discovered_file, revalidate_open_file, ClaudeFileFingerprint, ClaudeNativePathError,
        ClaudePhysicalFileId, ClaudeSourceLifecycle, DiscoveredClaudeSession,
    },
};

const CLAUDE_RECORD_CHAIN_DOMAIN: &[u8] = b"ctx-claude-nativepath-record-chain-v1\0";
const CLAUDE_BOUNDARY_PROOF_DOMAIN: &[u8] = b"ctx-claude-nativepath-boundary-proof-v1\0";
const CLAUDE_IDENTITY_HASH_DOMAIN: &[u8] = b"ctx-claude-nativepath-native-identity-v1\0";
const CLAUDE_CORE_PAGE_IDENTITY_DOMAIN: &[u8] = b"ctx/claude-nativepath/core-page/v1\0";
const CLAUDE_PRO_PAGE_IDENTITY_DOMAIN: &[u8] = b"ctx/claude-nativepath/pro-page/v1\0";
const CLAUDE_BOUNDARY_PROOF_BYTES: usize = 64 * 1024;
const CLAUDE_PAGE_ENCODING_ALLOWANCE: usize = 4 * 1024;
const CLAUDE_PRO_OUTPUT_ENCODING_ALLOWANCE: usize = 8 * 1024;
const CLAUDE_OUTPUT_LOCATOR_KIND: &str = "jsonl-source-item-byte-range-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClaudeNativeProfile {
    CoreOnly,
    CoreAndPro,
    ProReplayOnly,
}

impl ClaudeNativeProfile {
    fn includes_core(self) -> bool {
        matches!(self, Self::CoreOnly | Self::CoreAndPro)
    }

    fn includes_pro(self) -> bool {
        matches!(self, Self::CoreAndPro | Self::ProReplayOnly)
    }

    fn record_mode(self) -> ClaudeRecordMode {
        match self {
            Self::CoreOnly => ClaudeRecordMode::CoreOnly,
            Self::CoreAndPro => ClaudeRecordMode::CoreAndPro,
            Self::ProReplayOnly => ClaudeRecordMode::ProReplayOnly,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IncompleteTail {
    pub(crate) byte_start: u64,
    pub(crate) observed_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClaudePageCertificate {
    pub(crate) canonical_route: std::path::PathBuf,
    pub(crate) observation_sha256: [u8; 32],
    pub(crate) physical_file_id: Option<ClaudePhysicalFileId>,
    pub(crate) certified_prefix_end: u64,
    pub(crate) certified_prefix_chain_sha256: [u8; 32],
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ClaudeNativePageIdentity([u8; 32]);

#[derive(Debug)]
pub(crate) struct ClaudeNativePage {
    pub(crate) identity: ClaudeNativePageIdentity,
    pub(crate) session: ClaudeSessionMetadata,
    pub(crate) expected_frontier: ClaudeNativeFrontier,
    pub(crate) next_safe_frontier: ClaudeNativeFrontier,
    pub(crate) rows: Vec<ClaudeRetainedRow>,
    pub(crate) rejections: Vec<RecordRejection>,
    pub(crate) rejected_records: u64,
    pub(crate) logical_units: usize,
    pub(crate) serialized_bytes: usize,
    pub(crate) terminal: bool,
    pub(crate) certificate: ClaudePageCertificate,
}

impl ClaudeNativePage {
    pub(crate) fn receipt(&self) -> ClaudeNativePageReceipt {
        ClaudeNativePageReceipt {
            identity: self.identity,
            expected_frontier: self.expected_frontier.clone(),
            committed_frontier: self.next_safe_frontier.clone(),
            accepted_rows: self.rows.len(),
            accepted_physical_records: self.logical_units,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaudeNativePageReceipt {
    pub(crate) identity: ClaudeNativePageIdentity,
    pub(crate) expected_frontier: ClaudeNativeFrontier,
    pub(crate) committed_frontier: ClaudeNativeFrontier,
    pub(crate) accepted_rows: usize,
    pub(crate) accepted_physical_records: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ClaudeNativeProOutputPageIdentity([u8; 32]);

#[derive(Debug)]
pub(crate) struct ClaudeNativeProOutputPage {
    pub(crate) identity: ClaudeNativeProOutputPageIdentity,
    pub(crate) expected_frontier: ClaudeNativeFrontier,
    pub(crate) next_safe_frontier: ClaudeNativeFrontier,
    pub(crate) outputs: Vec<ProOutputObservation>,
    pub(crate) rejections: Vec<RecordRejection>,
    pub(crate) rejected_outputs: u64,
    pub(crate) logical_units: usize,
    pub(crate) serialized_bytes: usize,
    pub(crate) terminal: bool,
    pub(crate) certificate: ClaudePageCertificate,
}

impl ClaudeNativeProOutputPage {
    pub(crate) fn receipt(&self) -> ClaudeNativeProOutputPageReceipt {
        ClaudeNativeProOutputPageReceipt {
            identity: self.identity,
            expected_frontier: self.expected_frontier.clone(),
            committed_frontier: self.next_safe_frontier.clone(),
            accepted_outputs: self.outputs.len(),
            accepted_physical_records: self.logical_units,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaudeNativeProOutputPageReceipt {
    pub(crate) identity: ClaudeNativeProOutputPageIdentity,
    pub(crate) expected_frontier: ClaudeNativeFrontier,
    pub(crate) committed_frontier: ClaudeNativeFrontier,
    pub(crate) accepted_outputs: usize,
    pub(crate) accepted_physical_records: usize,
}

#[derive(Debug)]
pub(crate) enum ClaudeNativeOwnedPage {
    Core(Box<ClaudeNativePage>),
    Pro(Box<ClaudeNativeProOutputPage>),
}

#[derive(Debug)]
pub(crate) struct ParseOutput {
    pub(crate) change: ChangeSignal,
    pub(crate) lifecycle: ClaudeSourceLifecycle,
    pub(crate) rejections: RejectionSummary,
    pub(crate) pro_rejections: RejectionSummary,
    pub(crate) session: ClaudeSessionMetadata,
    pub(crate) checkpoint: ParseCheckpoint,
    pub(crate) incomplete_tail: Option<IncompleteTail>,
    pub(crate) stats: ParseStats,
    pub(crate) source_certified: bool,
}

#[derive(Debug)]
struct CorePageBuilder {
    expected_frontier: ClaudeNativeFrontier,
    next_safe_frontier: ClaudeNativeFrontier,
    rows: Vec<ClaudeRetainedRow>,
    rejections: Vec<RecordRejection>,
    rejected_records: u64,
    logical_units: usize,
    encoded_row_bytes: usize,
    encoded_rejection_bytes: usize,
}

impl CorePageBuilder {
    fn new(frontier: ClaudeNativeFrontier) -> Self {
        Self {
            expected_frontier: frontier.clone(),
            next_safe_frontier: frontier,
            rows: Vec::new(),
            rejections: Vec::new(),
            rejected_records: 0,
            logical_units: 0,
            encoded_row_bytes: 0,
            encoded_rejection_bytes: 0,
        }
    }
}

#[derive(Debug)]
struct ProPageBuilder {
    expected_frontier: ClaudeNativeFrontier,
    next_safe_frontier: ClaudeNativeFrontier,
    outputs: Vec<ProOutputObservation>,
    rejections: Vec<RecordRejection>,
    rejected_outputs: u64,
    logical_units: usize,
    encoded_output_bytes: usize,
    encoded_rejection_bytes: usize,
}

impl ProPageBuilder {
    fn new(frontier: ClaudeNativeFrontier) -> Self {
        Self {
            expected_frontier: frontier.clone(),
            next_safe_frontier: frontier,
            outputs: Vec::new(),
            rejections: Vec::new(),
            rejected_outputs: 0,
            logical_units: 0,
            encoded_output_bytes: 0,
            encoded_rejection_bytes: 0,
        }
    }
}

struct PendingRecord {
    before: ClaudeNativeFrontier,
    after: ClaudeNativeFrontier,
    parsed: ParsedClaudeRecord,
    locator: ClaudePhysicalLocator,
    core_rows_encoded_bytes: usize,
    intrinsic_core_rejection: Option<RecordRejection>,
}

pub(crate) struct ClaudeNativeScanner {
    source: DiscoveredClaudeSession,
    before: ClaudeFileFingerprint,
    reader: BufReader<File>,
    profile: ClaudeNativeProfile,
    change: ChangeSignal,
    lifecycle: ClaudeSourceLifecycle,
    previous: Option<ParseCheckpoint>,
    offset: u64,
    raw_ordinal: u64,
    record_chain: [u8; 32],
    boundary_window: BoundaryWindow,
    native_identity_chain: [u8; 32],
    native_identity_records: u64,
    last_complete_terminated: bool,
    session: ClaudeSessionMetadata,
    rejections: RejectionSummary,
    pro_rejections: RejectionSummary,
    incomplete_tail: Option<IncompleteTail>,
    stats: ParseStats,
    core_page: Option<CorePageBuilder>,
    pro_page: Option<ProPageBuilder>,
    ready_core: Option<ClaudeNativePage>,
    ready_pro: Option<ClaudeNativeProOutputPage>,
    pending: Option<PendingRecord>,
    exhausted: bool,
    replay: bool,
    emitted_core: bool,
    emitted_pro: bool,
}

impl ClaudeNativeScanner {
    pub(crate) fn new(
        source: DiscoveredClaudeSession,
        previous: Option<&ParseCheckpoint>,
        profile: ClaudeNativeProfile,
    ) -> Result<Self, ClaudeNativePathError> {
        let mut file = open_discovered_file(&source)?;
        let before = source.fingerprint.clone();
        let mut stats = ParseStats::default();
        let plan = plan_read(&source, previous, profile, &mut file, &mut stats)?;
        if plan.parse {
            file.seek(SeekFrom::Start(plan.frontier.complete_offset))
                .map_err(|error| ClaudeNativePathError::Io {
                    path: source.path.clone(),
                    source: error,
                })?;
        }
        let initial = plan.frontier.clone();
        let replay = !plan.parse;
        let replay_terminal = previous.is_none_or(|checkpoint| match profile {
            ClaudeNativeProfile::CoreOnly => checkpoint.terminal,
            ClaudeNativeProfile::CoreAndPro => checkpoint.terminal && checkpoint.pro_terminal,
            ClaudeNativeProfile::ProReplayOnly => checkpoint.pro_terminal,
        });
        let replay_incomplete = (replay && !replay_terminal).then(|| IncompleteTail {
            byte_start: initial.complete_offset,
            observed_bytes: before.len.saturating_sub(initial.complete_offset),
        });
        Ok(Self {
            session: ClaudeSessionMetadata::new(source.key.clone()),
            source,
            before,
            reader: BufReader::new(file),
            profile,
            change: plan.change,
            lifecycle: lifecycle_from_change(plan.change),
            previous: previous.cloned(),
            offset: initial.complete_offset,
            raw_ordinal: initial.next_raw_ordinal,
            record_chain: initial.complete_record_chain_sha256,
            boundary_window: plan.boundary_window,
            native_identity_chain: initial.native_identity_chain_sha256,
            native_identity_records: initial.native_identity_records,
            last_complete_terminated: initial.appendable_boundary,
            rejections: RejectionSummary::default(),
            pro_rejections: RejectionSummary::default(),
            incomplete_tail: replay_incomplete,
            stats,
            core_page: profile
                .includes_core()
                .then(|| CorePageBuilder::new(initial.clone())),
            pro_page: profile.includes_pro().then(|| ProPageBuilder::new(initial)),
            ready_core: None,
            ready_pro: None,
            pending: None,
            exhausted: replay,
            replay,
            emitted_core: false,
            emitted_pro: false,
        })
    }

    /// Restarts one lane from a failed page receipt without reparsing its
    /// certified prefix. The caller supplies the already accepted
    /// provider-private session metadata; it is deliberately absent from the
    /// content-free frontier.
    pub(crate) fn resume_page(
        source: DiscoveredClaudeSession,
        frontier: ClaudeNativeFrontier,
        session: ClaudeSessionMetadata,
        profile: ClaudeNativeProfile,
    ) -> Result<Self, ClaudeNativePathError> {
        if profile == ClaudeNativeProfile::CoreAndPro {
            return Err(ClaudeNativePathError::InvalidCheckpoint {
                reason: "independent page restart must select exactly one Claude lane".to_owned(),
            });
        }
        if !frontier.appendable_boundary || frontier.complete_offset > source.fingerprint.len {
            return Err(ClaudeNativePathError::InvalidCheckpoint {
                reason: "Claude page restart frontier is outside the observed source".to_owned(),
            });
        }
        let mut file = open_discovered_file(&source)?;
        let before = source.fingerprint.clone();
        let mut stats = ParseStats::default();
        let Some(boundary_window) =
            verify_committed_prefix(&mut file, &frontier, &source.path, &mut stats)?
        else {
            return Err(ClaudeNativePathError::InvalidCheckpoint {
                reason: "Claude page restart committed prefix does not match".to_owned(),
            });
        };
        file.seek(SeekFrom::Start(frontier.complete_offset))
            .map_err(|error| ClaudeNativePathError::Io {
                path: source.path.clone(),
                source: error,
            })?;
        Ok(Self {
            source,
            before,
            reader: BufReader::new(file),
            profile,
            change: ChangeSignal::Append,
            lifecycle: ClaudeSourceLifecycle::Append,
            previous: None,
            offset: frontier.complete_offset,
            raw_ordinal: frontier.next_raw_ordinal,
            record_chain: frontier.complete_record_chain_sha256,
            boundary_window,
            native_identity_chain: frontier.native_identity_chain_sha256,
            native_identity_records: frontier.native_identity_records,
            last_complete_terminated: frontier.appendable_boundary,
            session,
            rejections: RejectionSummary::default(),
            pro_rejections: RejectionSummary::default(),
            incomplete_tail: None,
            stats,
            core_page: profile
                .includes_core()
                .then(|| CorePageBuilder::new(frontier.clone())),
            pro_page: profile
                .includes_pro()
                .then(|| ProPageBuilder::new(frontier)),
            ready_core: None,
            ready_pro: None,
            pending: None,
            exhausted: false,
            replay: false,
            emitted_core: false,
            emitted_pro: false,
        })
    }

    pub(crate) fn next_page(
        &mut self,
    ) -> Result<Option<ClaudeNativeOwnedPage>, ClaudeNativePathError> {
        if let Some(page) = self.take_ready()? {
            return Ok(Some(page));
        }
        if self.exhausted {
            return Ok(None);
        }

        loop {
            if self.pending.is_some() {
                if self.stage_pending()? {
                    return self.take_ready();
                }
                if let Some(page) = self.take_ready()? {
                    return Ok(Some(page));
                }
            }

            if self.lane_at_physical_bound() {
                self.flush_full_lanes()?;
                if let Some(page) = self.take_ready()? {
                    return Ok(Some(page));
                }
            }

            let before = self.frontier();
            let Some(raw_line) = read_raw_line(&mut self.reader, &self.source.path)? else {
                self.exhausted = true;
                self.queue_end_pages(true)?;
                return self.take_ready();
            };
            observe_parse_io(&mut self.stats, raw_line.observed_bytes)?;
            let byte_start = self.offset;
            let byte_end_exclusive = byte_start
                .checked_add(raw_line.observed_bytes)
                .ok_or(ClaudeNativePathError::PositionOverflow)?;
            let locator = ClaudePhysicalLocator {
                path: self.source.canonical_path.clone(),
                byte_start,
                byte_end_exclusive,
                line_number: self
                    .raw_ordinal
                    .checked_add(1)
                    .ok_or(ClaudeNativePathError::PositionOverflow)?,
            };

            if !raw_line.terminated {
                self.incomplete_tail = Some(IncompleteTail {
                    byte_start,
                    observed_bytes: raw_line.observed_bytes,
                });
                self.exhausted = true;
                self.queue_end_pages(false)?;
                return self.take_ready();
            }

            let mut intrinsic_core_rejection = None;
            let parsed = if raw_line.oversized {
                self.stats.malformed_records = self.stats.malformed_records.saturating_add(1);
                intrinsic_core_rejection = Some(RecordRejection {
                    kind: RejectionKind::OversizeRecord,
                    source_record_ordinal: self.raw_ordinal,
                    locator: locator.clone(),
                    diagnostic: format!(
                        "Claude JSONL record exceeds the {MAX_PROVIDER_JSONL_LINE_BYTES} byte limit"
                    ),
                });
                None
            } else {
                let json = json_record_bytes(&raw_line.buffer);
                if json.iter().all(u8::is_ascii_whitespace) {
                    self.stats.ignored_records = self.stats.ignored_records.saturating_add(1);
                    None
                } else {
                    self.stats.semantic_record_parses =
                        self.stats.semantic_record_parses.saturating_add(1);
                    match parse_native_record(
                        json,
                        self.raw_ordinal,
                        &locator,
                        self.profile.record_mode(),
                    ) {
                        Ok(parsed) => {
                            if !parsed.result.is_result() {
                                self.stats.retention_pass_records =
                                    self.stats.retention_pass_records.saturating_add(1);
                            }
                            Some(parsed)
                        }
                        Err(error) => {
                            self.stats.malformed_records =
                                self.stats.malformed_records.saturating_add(1);
                            intrinsic_core_rejection = Some(RecordRejection {
                                kind: RejectionKind::MalformedJson,
                                source_record_ordinal: self.raw_ordinal,
                                locator: locator.clone(),
                                diagnostic: format!("malformed Claude JSONL record: {error}"),
                            });
                            None
                        }
                    }
                }
            };

            let parsed = parsed.and_then(|parsed| {
                if parsed
                    .session_id
                    .as_deref()
                    .filter(|session_id| !session_id.trim().is_empty())
                    .is_some_and(|session_id| session_id != self.source.key.root_session_id)
                {
                    intrinsic_core_rejection = Some(RecordRejection {
                        kind: RejectionKind::SessionIdentityMismatch,
                        source_record_ordinal: self.raw_ordinal,
                        locator: locator.clone(),
                        diagnostic:
                            "Claude record sessionId does not match its project/session path"
                                .to_owned(),
                    });
                    self.stats.malformed_records = self.stats.malformed_records.saturating_add(1);
                    None
                } else {
                    Some(parsed)
                }
            });

            let identity_kind = parsed.as_ref().map_or(b"ignored".as_slice(), |parsed| {
                if parsed.result.is_result() {
                    b"result".as_slice()
                } else {
                    b"retained".as_slice()
                }
            });
            let native_record_id = parsed
                .as_ref()
                .and_then(|parsed| parsed.native_record_id.as_deref());
            self.commit_record(
                &raw_line,
                byte_end_exclusive,
                identity_kind,
                native_record_id,
            )?;
            self.stats.complete_records = self.stats.complete_records.saturating_add(1);
            let after = self.frontier();
            let parsed = parsed.unwrap_or_else(empty_parsed_record);
            self.observe_parsed_record(&parsed, raw_line.observed_bytes);
            let core_rows_encoded_bytes = parsed.rows.iter().try_fold(0_usize, |total, row| {
                total
                    .checked_add(row.exact_encoded_bytes()?)
                    .ok_or(ClaudeNativePathError::PositionOverflow)
            })?;
            self.pending = Some(PendingRecord {
                before,
                after,
                parsed,
                locator,
                core_rows_encoded_bytes,
                intrinsic_core_rejection,
            });
        }
    }

    pub(crate) fn checkpoint_at(
        &self,
        frontier: &ClaudeNativeFrontier,
        terminal: bool,
    ) -> ParseCheckpoint {
        self.build_checkpoint(frontier, terminal)
    }

    pub(crate) fn finish(mut self) -> Result<ParseOutput, ClaudeNativePathError> {
        if !self.exhausted
            || self.pending.is_some()
            || self.ready_core.is_some()
            || self.ready_pro.is_some()
            || self
                .core_page
                .as_ref()
                .is_some_and(|page| page.logical_units != 0)
            || self
                .pro_page
                .as_ref()
                .is_some_and(|page| page.logical_units != 0)
        {
            return Err(ClaudeNativePathError::InvalidCheckpoint {
                reason: "Claude NativePath scan must drain all owned pages before certification"
                    .to_owned(),
            });
        }
        revalidate_open_file(&self.source, self.reader.get_ref(), &self.before)?;
        let frontier = self.frontier();
        let terminal = self.incomplete_tail.is_none();
        let checkpoint = self.build_checkpoint(&frontier, terminal);
        self.change = refine_change_signal(self.change, self.previous.as_ref(), &checkpoint);
        self.lifecycle = lifecycle_from_change(self.change);
        if self.replay {
            self.stats.metadata_only_noop = true;
        }
        debug_assert_eq!(self.stats.result_hashes_created, 0);
        debug_assert_eq!(self.stats.result_previews_created, 0);
        debug_assert_eq!(self.stats.result_touches_created, 0);
        debug_assert_eq!(self.stats.result_fts_rows_created, 0);
        Ok(ParseOutput {
            change: self.change,
            lifecycle: self.lifecycle,
            rejections: self.rejections,
            pro_rejections: self.pro_rejections,
            session: self.session,
            checkpoint,
            incomplete_tail: self.incomplete_tail,
            stats: self.stats,
            source_certified: true,
        })
    }

    fn stage_pending(&mut self) -> Result<bool, ClaudeNativePathError> {
        let pending =
            self.pending
                .as_ref()
                .ok_or_else(|| ClaudeNativePathError::InvalidCheckpoint {
                    reason: "Claude pending record disappeared".to_owned(),
                })?;
        let mut prospective_session = self.session.clone();
        prospective_session.observe(
            pending.parsed.timestamp.as_deref(),
            pending.parsed.cwd.as_deref(),
            pending.parsed.version.as_deref(),
            pending.parsed.git_branch.as_deref(),
        );

        let core_record_rejection = self.core_record_rejection(pending, &prospective_session)?;
        let pro_record_rejection = self.pro_record_rejection(pending)?;
        let candidate_core_row_bytes = if core_record_rejection.is_some() {
            0
        } else {
            pending.core_rows_encoded_bytes
        };
        let core_needs_flush = self.core_page.as_ref().is_some_and(|page| {
            page.logical_units != 0
                && core_candidate_bytes(
                    page,
                    &prospective_session,
                    candidate_core_row_bytes,
                    core_record_rejection.as_ref(),
                    &pending.after,
                    false,
                    &self.source,
                )
                .is_none_or(|bytes| bytes > CLAUDE_MAX_PAGE_BYTES)
        });
        let pro_needs_flush = self.pro_page.as_ref().is_some_and(|page| {
            page.logical_units >= CLAUDE_MAX_PAGE_ROWS
                || (page.logical_units != 0
                    && pro_record_rejection.is_none()
                    && !pending.parsed.outputs.is_empty()
                    && (page
                        .outputs
                        .len()
                        .checked_add(pending.parsed.outputs.len())
                        .is_none_or(|units| units > CLAUDE_MAX_PAGE_ROWS)
                        || pro_candidate_bytes(page, &pending.parsed, &self.source)
                            .is_none_or(|bytes| bytes > CLAUDE_MAX_PAGE_BYTES)))
        });
        if core_needs_flush || pro_needs_flush {
            if self.profile == ClaudeNativeProfile::CoreAndPro {
                self.flush_core_page(false)?;
                self.flush_pro_page(false)?;
            } else {
                if core_needs_flush {
                    self.flush_core_page(false)?;
                }
                if pro_needs_flush {
                    self.flush_pro_page(false)?;
                }
            }
            return Ok(true);
        }

        let mut pending =
            self.pending
                .take()
                .ok_or_else(|| ClaudeNativePathError::InvalidCheckpoint {
                    reason: "Claude pending record disappeared before staging".to_owned(),
                })?;
        self.session = prospective_session;
        if let Some(rejection) = core_record_rejection.as_ref() {
            self.rejections.record(rejection.clone())?;
        }
        if let Some(page) = self.core_page.as_mut() {
            let mut rows = std::mem::take(&mut pending.parsed.rows);
            if let Some(rejection) = core_record_rejection {
                rows.clear();
                page.rejected_records = page.rejected_records.saturating_add(1);
                if page.rejections.len() < super::rows::CLAUDE_MAX_REJECTION_SAMPLES {
                    page.encoded_rejection_bytes = page
                        .encoded_rejection_bytes
                        .saturating_add(exact_json_encoded_bytes(&rejection).unwrap_or(usize::MAX));
                    page.rejections.push(rejection);
                }
            }
            for row in &rows {
                self.stats.observe_row(row);
            }
            page.encoded_row_bytes = page
                .encoded_row_bytes
                .saturating_add(candidate_core_row_bytes);
            page.rows.extend(rows);
            page.logical_units = page.logical_units.saturating_add(1);
            page.next_safe_frontier = pending.after.clone();
        }

        if let Some(page) = self.pro_page.as_mut() {
            if let Some(rejection) = pro_record_rejection {
                page.rejected_outputs = page.rejected_outputs.saturating_add(
                    u64::try_from(pending.parsed.outputs.len()).unwrap_or(u64::MAX),
                );
                if page.rejections.len() < super::rows::CLAUDE_MAX_REJECTION_SAMPLES {
                    page.encoded_rejection_bytes = page
                        .encoded_rejection_bytes
                        .saturating_add(exact_json_encoded_bytes(&rejection).unwrap_or(usize::MAX));
                    page.rejections.push(rejection.clone());
                }
                self.pro_rejections.record(rejection)?;
            } else {
                let outputs =
                    build_output_observations(&self.source, &pending.locator, pending.parsed);
                page.encoded_output_bytes = page
                    .encoded_output_bytes
                    .saturating_add(outputs.iter().map(output_wire_bytes).sum::<usize>());
                page.outputs.extend(outputs);
            }
            page.logical_units = page.logical_units.saturating_add(1);
            page.next_safe_frontier = pending.after;
        }
        Ok(false)
    }

    fn core_record_rejection(
        &self,
        pending: &PendingRecord,
        session: &ClaudeSessionMetadata,
    ) -> Result<Option<RecordRejection>, ClaudeNativePathError> {
        if let Some(rejection) = pending.intrinsic_core_rejection.as_ref() {
            return Ok(Some(rejection.clone()));
        }
        if !self.profile.includes_core() {
            return Ok(None);
        }
        if pending.parsed.rows.len() > CLAUDE_MAX_PAGE_ROWS {
            return Ok(Some(RecordRejection {
                kind: RejectionKind::OversizeRetainedRecord,
                source_record_ordinal: pending.before.next_raw_ordinal,
                locator: pending.locator.clone(),
                diagnostic: "Claude record projects more than 64 Core rows".to_owned(),
            }));
        }
        let empty = CorePageBuilder::new(pending.before.clone());
        let bytes = core_candidate_bytes(
            &empty,
            session,
            pending.core_rows_encoded_bytes,
            None,
            &pending.after,
            false,
            &self.source,
        )
        .ok_or(ClaudeNativePathError::PositionOverflow)?;
        Ok((bytes > CLAUDE_MAX_PAGE_BYTES).then(|| RecordRejection {
            kind: RejectionKind::OversizeRetainedRecord,
            source_record_ordinal: pending.before.next_raw_ordinal,
            locator: pending.locator.clone(),
            diagnostic: "Claude record projection exceeds the 8 MiB Core page bound".to_owned(),
        }))
    }

    fn pro_record_rejection(
        &self,
        pending: &PendingRecord,
    ) -> Result<Option<RecordRejection>, ClaudeNativePathError> {
        if !self.profile.includes_pro() || pending.parsed.outputs.is_empty() {
            return Ok(None);
        }
        if pending.parsed.outputs.len() > CLAUDE_MAX_PAGE_ROWS {
            return Ok(Some(RecordRejection {
                kind: RejectionKind::TooManyResultSubrecords,
                source_record_ordinal: pending.before.next_raw_ordinal,
                locator: pending.locator.clone(),
                diagnostic: "Claude record projects more than 64 native result subrecords"
                    .to_owned(),
            }));
        }
        let empty = ProPageBuilder::new(pending.before.clone());
        let bytes = pro_candidate_bytes(&empty, &pending.parsed, &self.source)
            .ok_or(ClaudeNativePathError::PositionOverflow)?;
        Ok((bytes > CLAUDE_MAX_PAGE_BYTES).then(|| RecordRejection {
            kind: RejectionKind::OversizeProOutput,
            source_record_ordinal: pending.before.next_raw_ordinal,
            locator: pending.locator.clone(),
            diagnostic: "Claude native result projection exceeds the 8 MiB Pro page bound"
                .to_owned(),
        }))
    }

    fn lane_at_physical_bound(&self) -> bool {
        self.core_page
            .as_ref()
            .is_some_and(|page| page.logical_units >= CLAUDE_MAX_PAGE_ROWS)
            || self
                .pro_page
                .as_ref()
                .is_some_and(|page| page.logical_units >= CLAUDE_MAX_PAGE_ROWS)
    }

    fn flush_full_lanes(&mut self) -> Result<(), ClaudeNativePathError> {
        if self.profile == ClaudeNativeProfile::CoreAndPro {
            self.flush_core_page(false)?;
            self.flush_pro_page(false)?;
            return Ok(());
        }
        if self
            .core_page
            .as_ref()
            .is_some_and(|page| page.logical_units >= CLAUDE_MAX_PAGE_ROWS)
        {
            self.flush_core_page(false)?;
        }
        if self
            .pro_page
            .as_ref()
            .is_some_and(|page| page.logical_units >= CLAUDE_MAX_PAGE_ROWS)
        {
            self.flush_pro_page(false)?;
        }
        Ok(())
    }

    fn queue_end_pages(&mut self, terminal: bool) -> Result<(), ClaudeNativePathError> {
        if self.profile.includes_core()
            && (self
                .core_page
                .as_ref()
                .is_some_and(|page| page.logical_units != 0)
                || !self.emitted_core)
        {
            self.flush_core_page(terminal)?;
        }
        if self.profile.includes_pro()
            && (self
                .pro_page
                .as_ref()
                .is_some_and(|page| page.logical_units != 0)
                || !self.emitted_pro)
        {
            self.flush_pro_page(terminal)?;
        }
        Ok(())
    }

    fn flush_core_page(&mut self, terminal: bool) -> Result<(), ClaudeNativePathError> {
        let page =
            self.core_page
                .take()
                .ok_or_else(|| ClaudeNativePathError::InvalidCheckpoint {
                    reason: "Claude Core page is unavailable".to_owned(),
                })?;
        let next = CorePageBuilder::new(page.next_safe_frontier.clone());
        let finished = self.finish_core_page(page, terminal)?;
        if self.ready_core.replace(finished).is_some() {
            return Err(ClaudeNativePathError::InvalidCheckpoint {
                reason: "multiple unacknowledged Claude Core pages".to_owned(),
            });
        }
        self.core_page = Some(next);
        self.emitted_core = true;
        Ok(())
    }

    fn flush_pro_page(&mut self, terminal: bool) -> Result<(), ClaudeNativePathError> {
        let page =
            self.pro_page
                .take()
                .ok_or_else(|| ClaudeNativePathError::InvalidCheckpoint {
                    reason: "Claude Pro page is unavailable".to_owned(),
                })?;
        let next = ProPageBuilder::new(page.next_safe_frontier.clone());
        let finished = self.finish_pro_page(page, terminal)?;
        if self.ready_pro.replace(finished).is_some() {
            return Err(ClaudeNativePathError::InvalidCheckpoint {
                reason: "multiple unacknowledged Claude Pro pages".to_owned(),
            });
        }
        self.pro_page = Some(next);
        self.emitted_pro = true;
        Ok(())
    }

    fn finish_core_page(
        &mut self,
        page: CorePageBuilder,
        terminal: bool,
    ) -> Result<ClaudeNativePage, ClaudeNativePathError> {
        revalidate_open_file(&self.source, self.reader.get_ref(), &self.before)?;
        let certificate = page_certificate(&self.source, &page.next_safe_frontier);
        let serialized_bytes = core_encoded_bytes(
            &self.session,
            &page.expected_frontier,
            &page.next_safe_frontier,
            &page.rows,
            &page.rejections,
            page.rejected_records,
            page.logical_units,
            terminal,
            &certificate,
        )?;
        if page.logical_units > CLAUDE_MAX_PAGE_ROWS
            || page.rows.len() > CLAUDE_MAX_PAGE_ROWS
            || serialized_bytes > CLAUDE_MAX_PAGE_BYTES
        {
            return Err(ClaudeNativePathError::InvalidCheckpoint {
                reason: "Claude Core page escaped its certified bounds".to_owned(),
            });
        }
        let identity = core_page_identity(&self.session, &page, terminal, &certificate)?;
        self.stats.emitted_pages = self.stats.emitted_pages.saturating_add(1);
        self.stats.emitted_rows = self
            .stats
            .emitted_rows
            .saturating_add(u64::try_from(page.rows.len()).unwrap_or(u64::MAX));
        self.stats.peak_page_rows = self.stats.peak_page_rows.max(page.rows.len());
        self.stats.peak_page_bytes = self.stats.peak_page_bytes.max(serialized_bytes);
        Ok(ClaudeNativePage {
            identity,
            session: self.session.clone(),
            expected_frontier: page.expected_frontier,
            next_safe_frontier: page.next_safe_frontier,
            rows: page.rows,
            rejections: page.rejections,
            rejected_records: page.rejected_records,
            logical_units: page.logical_units,
            serialized_bytes,
            terminal,
            certificate,
        })
    }

    fn finish_pro_page(
        &mut self,
        page: ProPageBuilder,
        terminal: bool,
    ) -> Result<ClaudeNativeProOutputPage, ClaudeNativePathError> {
        revalidate_open_file(&self.source, self.reader.get_ref(), &self.before)?;
        let certificate = page_certificate(&self.source, &page.next_safe_frontier);
        let serialized_bytes = pro_page_encoded_bytes(&page, &self.source, &certificate)?;
        if page.logical_units > CLAUDE_MAX_PAGE_ROWS
            || page.outputs.len() > CLAUDE_MAX_PAGE_ROWS
            || serialized_bytes > CLAUDE_MAX_PAGE_BYTES
        {
            return Err(ClaudeNativePathError::InvalidCheckpoint {
                reason: "Claude Pro page escaped its certified bounds".to_owned(),
            });
        }
        let identity = pro_page_identity(&page, terminal, &certificate)?;
        self.stats.emitted_pro_pages = self.stats.emitted_pro_pages.saturating_add(1);
        self.stats.emitted_pro_outputs = self
            .stats
            .emitted_pro_outputs
            .saturating_add(u64::try_from(page.outputs.len()).unwrap_or(u64::MAX));
        self.stats.peak_pro_page_outputs = self.stats.peak_pro_page_outputs.max(page.outputs.len());
        self.stats.peak_pro_page_bytes = self.stats.peak_pro_page_bytes.max(serialized_bytes);
        Ok(ClaudeNativeProOutputPage {
            identity,
            expected_frontier: page.expected_frontier,
            next_safe_frontier: page.next_safe_frontier,
            outputs: page.outputs,
            rejections: page.rejections,
            rejected_outputs: page.rejected_outputs,
            logical_units: page.logical_units,
            serialized_bytes,
            terminal,
            certificate,
        })
    }

    fn take_ready(&mut self) -> Result<Option<ClaudeNativeOwnedPage>, ClaudeNativePathError> {
        if self.ready_pro.is_some() || self.ready_core.is_some() {
            // A sibling may have waited while its peer was consumed. Recheck
            // the pinned descriptor and route immediately before every page
            // leaves provider ownership.
            revalidate_open_file(&self.source, self.reader.get_ref(), &self.before)?;
        }
        Ok(self
            .ready_pro
            .take()
            .map(Box::new)
            .map(ClaudeNativeOwnedPage::Pro)
            .or_else(|| {
                self.ready_core
                    .take()
                    .map(Box::new)
                    .map(ClaudeNativeOwnedPage::Core)
            }))
    }

    fn frontier(&self) -> ClaudeNativeFrontier {
        ClaudeNativeFrontier {
            complete_offset: self.offset,
            next_raw_ordinal: self.raw_ordinal,
            complete_record_chain_sha256: self.record_chain,
            boundary_proof_len: u32::try_from(self.boundary_window.bytes.len()).unwrap_or(u32::MAX),
            boundary_proof_sha256: boundary_proof_hash(&self.boundary_window.bytes),
            native_identity_chain_sha256: self.native_identity_chain,
            native_identity_records: self.native_identity_records,
            appendable_boundary: self.offset == 0 || self.last_complete_terminated,
        }
    }

    fn commit_record(
        &mut self,
        raw_line: &RawLine,
        byte_end_exclusive: u64,
        identity_kind: &[u8],
        native_record_id: Option<&str>,
    ) -> Result<(), ClaudeNativePathError> {
        self.record_chain =
            advance_record_chain(&self.record_chain, self.raw_ordinal, &raw_line.raw_sha256);
        self.boundary_window
            .push_line(&raw_line.boundary_tail, raw_line.observed_bytes);
        self.offset = byte_end_exclusive;
        self.native_identity_chain = advance_identity_chain(
            &self.native_identity_chain,
            self.raw_ordinal,
            identity_kind,
            native_record_id,
        );
        if native_record_id.is_some() {
            self.native_identity_records = self
                .native_identity_records
                .checked_add(1)
                .ok_or(ClaudeNativePathError::PositionOverflow)?;
        }
        self.raw_ordinal = self
            .raw_ordinal
            .checked_add(1)
            .ok_or(ClaudeNativePathError::PositionOverflow)?;
        self.last_complete_terminated = true;
        Ok(())
    }

    fn observe_parsed_record(&mut self, parsed: &ParsedClaudeRecord, record_bytes: u64) {
        if parsed.result.is_result() {
            self.stats.native_result_records = self.stats.native_result_records.saturating_add(1);
            self.stats.native_result_record_bytes = self
                .stats
                .native_result_record_bytes
                .saturating_add(record_bytes);
            self.stats.tagged_command_output_records = self
                .stats
                .tagged_command_output_records
                .saturating_add(u64::from(parsed.result.tagged_command_output));
            self.stats.result_block_records = self
                .stats
                .result_block_records
                .saturating_add(u64::from(parsed.result.result_block));
            self.stats.result_like_shape_records = self
                .stats
                .result_like_shape_records
                .saturating_add(u64::from(parsed.result.result_like_shape));
            if self.profile == ClaudeNativeProfile::CoreOnly {
                self.stats.preallocation_excluded_result_records = self
                    .stats
                    .preallocation_excluded_result_records
                    .saturating_add(u64::from(parsed.preallocation_exclusion));
                debug_assert!(parsed.preallocation_exclusion);
            } else {
                self.stats.result_body_bytes_decoded_or_allocated = self
                    .stats
                    .result_body_bytes_decoded_or_allocated
                    .saturating_add(
                        parsed
                            .outputs
                            .iter()
                            .filter_map(|output| output.content.as_ref())
                            .map(|content| u64::try_from(content.len()).unwrap_or(u64::MAX))
                            .sum::<u64>(),
                    );
            }
        }
    }

    fn build_checkpoint(&self, frontier: &ClaudeNativeFrontier, terminal: bool) -> ParseCheckpoint {
        let previous_pro = self
            .previous
            .as_ref()
            .map(ParseCheckpoint::pro_frontier)
            .unwrap_or_else(initial_frontier);
        let previous_core = self
            .previous
            .as_ref()
            .map(ParseCheckpoint::core_frontier)
            .unwrap_or_else(initial_frontier);
        let core = if self.profile.includes_core() {
            frontier.clone()
        } else {
            previous_core
        };
        let pro = if self.profile.includes_pro() {
            frontier.clone()
        } else {
            previous_pro
        };
        let core_terminal = if self.profile.includes_core() {
            terminal
        } else {
            self.previous
                .as_ref()
                .is_some_and(|checkpoint| checkpoint.terminal)
        };
        let pro_terminal = if self.profile.includes_pro() {
            terminal
        } else {
            self.previous
                .as_ref()
                .is_some_and(|checkpoint| checkpoint.pro_terminal)
        };
        let current_observation_sha256 = self.before.observation_sha256();
        let (core_observed_file_len, core_observation_sha256, core_observation_binding_sha256) =
            if self.profile.includes_core() {
                (
                    self.before.len,
                    current_observation_sha256,
                    lane_observation_binding(
                        self.before.len,
                        &current_observation_sha256,
                        &core,
                        core_terminal,
                    ),
                )
            } else {
                self.previous
                    .as_ref()
                    .map(|checkpoint| {
                        (
                            checkpoint.observed_file_len,
                            checkpoint.observation_sha256,
                            checkpoint.core_observation_binding_sha256,
                        )
                    })
                    .unwrap_or_default()
            };
        let (pro_observed_file_len, pro_observation_sha256, pro_observation_binding_sha256) =
            if self.profile.includes_pro() {
                (
                    self.before.len,
                    current_observation_sha256,
                    lane_observation_binding(
                        self.before.len,
                        &current_observation_sha256,
                        &pro,
                        pro_terminal,
                    ),
                )
            } else {
                self.previous
                    .as_ref()
                    .map(|checkpoint| {
                        (
                            checkpoint.pro_observed_file_len,
                            checkpoint.pro_observation_sha256,
                            checkpoint.pro_observation_binding_sha256,
                        )
                    })
                    .unwrap_or_default()
            };
        let (core_parser_revision, core_policy_revision) = if self.profile.includes_core() {
            (
                CLAUDE_NATIVEPATH_PARSER_REVISION,
                CLAUDE_NATIVEPATH_POLICY_REVISION,
            )
        } else {
            self.previous
                .as_ref()
                .map(|checkpoint| (checkpoint.parser_revision, checkpoint.policy_revision))
                .unwrap_or_default()
        };
        let (pro_parser_revision, pro_policy_revision) = if self.profile.includes_pro() {
            (
                CLAUDE_NATIVEPATH_PARSER_REVISION,
                CLAUDE_NATIVEPATH_POLICY_REVISION,
            )
        } else {
            self.previous
                .as_ref()
                .map(|checkpoint| {
                    (
                        checkpoint.pro_parser_revision,
                        checkpoint.pro_policy_revision,
                    )
                })
                .unwrap_or_default()
        };
        ParseCheckpoint {
            parser_revision: core_parser_revision,
            policy_revision: core_policy_revision,
            session_key: self.source.key.clone(),
            canonical_route: self.source.canonical_path.clone(),
            physical_file_id: self.before.physical_file_id,
            observed_file_len: core_observed_file_len,
            observation_sha256: core_observation_sha256,
            core_observation_binding_sha256,
            complete_offset: core.complete_offset,
            next_raw_ordinal: core.next_raw_ordinal,
            complete_record_chain_sha256: core.complete_record_chain_sha256,
            boundary_proof_len: core.boundary_proof_len,
            boundary_proof_sha256: core.boundary_proof_sha256,
            native_identity_chain_sha256: core.native_identity_chain_sha256,
            native_identity_records: core.native_identity_records,
            terminal: core_terminal,
            appendable_boundary: core.appendable_boundary,
            pro_complete_offset: pro.complete_offset,
            pro_next_raw_ordinal: pro.next_raw_ordinal,
            pro_complete_record_chain_sha256: pro.complete_record_chain_sha256,
            pro_boundary_proof_len: pro.boundary_proof_len,
            pro_boundary_proof_sha256: pro.boundary_proof_sha256,
            pro_native_identity_chain_sha256: pro.native_identity_chain_sha256,
            pro_native_identity_records: pro.native_identity_records,
            pro_appendable_boundary: pro.appendable_boundary,
            pro_initialized: self.profile.includes_pro()
                || self
                    .previous
                    .as_ref()
                    .is_some_and(|checkpoint| checkpoint.pro_initialized),
            pro_terminal,
            pro_observed_file_len,
            pro_observation_sha256,
            pro_observation_binding_sha256,
            pro_parser_revision,
            pro_policy_revision,
        }
    }
}

pub(crate) fn parse_session<F>(
    source: &DiscoveredClaudeSession,
    previous: Option<&ParseCheckpoint>,
    mut emit_page: F,
) -> Result<ParseOutput, ClaudeNativePathError>
where
    F: FnMut(ClaudeRowPage) -> Result<(), ClaudeNativePathError>,
{
    let mut scanner =
        ClaudeNativeScanner::new(source.clone(), previous, ClaudeNativeProfile::CoreOnly)?;
    while let Some(page) = scanner.next_page()? {
        match page {
            ClaudeNativeOwnedPage::Core(page) => emit_page(ClaudeRowPage {
                estimated_bytes: page.serialized_bytes,
                rows: page.rows,
            })?,
            ClaudeNativeOwnedPage::Pro(_) => {
                return Err(ClaudeNativePathError::InvalidCheckpoint {
                    reason: "Core-only compatibility scan emitted a Pro page".to_owned(),
                });
            }
        }
    }
    scanner.finish()
}

struct ReadPlan {
    change: ChangeSignal,
    parse: bool,
    frontier: ClaudeNativeFrontier,
    boundary_window: BoundaryWindow,
}

fn plan_read(
    source: &DiscoveredClaudeSession,
    previous: Option<&ParseCheckpoint>,
    profile: ClaudeNativeProfile,
    file: &mut File,
    stats: &mut ParseStats,
) -> Result<ReadPlan, ClaudeNativePathError> {
    let Some(previous) = previous else {
        return Ok(full_read_plan(ChangeSignal::Fresh));
    };
    let selected_checkpoint_matches = match profile {
        ClaudeNativeProfile::CoreOnly => {
            previous.core_revisions_match() && previous.core_observation_binding_matches()
        }
        ClaudeNativeProfile::CoreAndPro => {
            previous.core_revisions_match()
                && previous.pro_revisions_match()
                && previous.core_observation_binding_matches()
                && previous.pro_observation_binding_matches()
        }
        ClaudeNativeProfile::ProReplayOnly => {
            previous.pro_revisions_match() && previous.pro_observation_binding_matches()
        }
    };
    if !selected_checkpoint_matches {
        return Ok(full_read_plan(ChangeSignal::Reparse));
    }
    let same_route = previous.canonical_route == source.canonical_path;
    if previous.session_key != source.key {
        return Ok(full_read_plan(if same_route {
            ChangeSignal::Replacement
        } else {
            ChangeSignal::Fresh
        }));
    }
    let same_physical = match (
        previous.physical_file_id,
        source.fingerprint.physical_file_id,
    ) {
        (Some(previous), Some(current)) => previous == current,
        _ => same_route,
    };
    let previous_route_exists =
        !same_route && std::fs::symlink_metadata(&previous.canonical_route).is_ok();
    if same_route && !same_physical {
        return Ok(full_read_plan(ChangeSignal::Replacement));
    }
    if !same_route && !same_physical {
        return Ok(full_read_plan(if previous_route_exists {
            ChangeSignal::LiveCopy
        } else {
            ChangeSignal::Replacement
        }));
    }

    let selected = match profile {
        ClaudeNativeProfile::CoreOnly => previous.core_frontier(),
        ClaudeNativeProfile::ProReplayOnly => previous.pro_frontier(),
        ClaudeNativeProfile::CoreAndPro => {
            let core = previous.core_frontier();
            let pro = previous.pro_frontier();
            if core != pro || previous.terminal != previous.pro_terminal {
                return Err(ClaudeNativePathError::InvalidCheckpoint {
                    reason:
                        "CoreAndPro requires aligned Core/Pro frontiers; replay Pro independently first"
                            .to_owned(),
                });
            }
            core
        }
    };
    let selected_terminal = match profile {
        ClaudeNativeProfile::CoreOnly => previous.terminal,
        ClaudeNativeProfile::CoreAndPro => previous.terminal && previous.pro_terminal,
        ClaudeNativeProfile::ProReplayOnly => previous.pro_terminal,
    };
    let selected_observed_file_len = match profile {
        ClaudeNativeProfile::CoreOnly => previous.observed_file_len,
        ClaudeNativeProfile::CoreAndPro => previous
            .observed_file_len
            .max(previous.pro_observed_file_len),
        ClaudeNativeProfile::ProReplayOnly => previous.pro_observed_file_len,
    };
    if source.fingerprint.len < selected_observed_file_len
        || source.fingerprint.len < selected.complete_offset
    {
        return Ok(full_read_plan(ChangeSignal::Truncation));
    }
    let verified_prefix = if selected.appendable_boundary {
        verify_committed_prefix(file, &selected, &source.path, stats)?
    } else {
        None
    };
    if verified_prefix.is_none() {
        return Ok(full_read_plan(if same_route {
            ChangeSignal::Rewrite
        } else if previous_route_exists {
            ChangeSignal::LiveCopy
        } else {
            ChangeSignal::Relocation
        }));
    }
    let boundary_window = verified_prefix.unwrap_or_default();
    let current_observation_sha256 = source.fingerprint.observation_sha256();
    let selected_observation_matches = match profile {
        ClaudeNativeProfile::CoreOnly => {
            current_observation_sha256 == previous.observation_sha256
                && source.fingerprint.len == previous.observed_file_len
        }
        ClaudeNativeProfile::CoreAndPro => {
            current_observation_sha256 == previous.observation_sha256
                && source.fingerprint.len == previous.observed_file_len
                && current_observation_sha256 == previous.pro_observation_sha256
                && source.fingerprint.len == previous.pro_observed_file_len
        }
        ClaudeNativeProfile::ProReplayOnly => {
            current_observation_sha256 == previous.pro_observation_sha256
                && source.fingerprint.len == previous.pro_observed_file_len
        }
    };
    let exact_observation = selected_observation_matches
        && (!selected_terminal || selected.complete_offset == source.fingerprint.len)
        && match profile {
            ClaudeNativeProfile::CoreOnly => true,
            ClaudeNativeProfile::CoreAndPro => previous.pro_initialized && selected_terminal,
            ClaudeNativeProfile::ProReplayOnly => previous.pro_initialized,
        };
    if exact_observation {
        return Ok(ReadPlan {
            change: if same_route {
                ChangeSignal::Unchanged
            } else {
                ChangeSignal::Relocation
            },
            parse: false,
            frontier: selected,
            boundary_window,
        });
    }
    if source.fingerprint.len >= selected.complete_offset {
        return Ok(ReadPlan {
            change: if same_route {
                ChangeSignal::Append
            } else if previous_route_exists {
                ChangeSignal::LiveCopy
            } else {
                ChangeSignal::Relocation
            },
            parse: true,
            frontier: selected,
            boundary_window,
        });
    }
    Ok(full_read_plan(if same_route {
        ChangeSignal::Rewrite
    } else if previous_route_exists {
        ChangeSignal::LiveCopy
    } else {
        ChangeSignal::Relocation
    }))
}

fn full_read_plan(change: ChangeSignal) -> ReadPlan {
    ReadPlan {
        change,
        parse: true,
        frontier: initial_frontier(),
        boundary_window: BoundaryWindow::default(),
    }
}

fn initial_frontier() -> ClaudeNativeFrontier {
    ClaudeNativeFrontier {
        complete_offset: 0,
        next_raw_ordinal: 0,
        complete_record_chain_sha256: initial_record_chain(),
        boundary_proof_len: 0,
        boundary_proof_sha256: boundary_proof_hash(&[]),
        native_identity_chain_sha256: initial_identity_chain(),
        native_identity_records: 0,
        appendable_boundary: true,
    }
}

fn lifecycle_from_change(change: ChangeSignal) -> ClaudeSourceLifecycle {
    match change {
        ChangeSignal::Fresh => ClaudeSourceLifecycle::New,
        ChangeSignal::Unchanged | ChangeSignal::ExactRestore => ClaudeSourceLifecycle::Replay,
        ChangeSignal::Append => ClaudeSourceLifecycle::Append,
        ChangeSignal::Rewrite | ChangeSignal::Reparse => ClaudeSourceLifecycle::Rewrite,
        ChangeSignal::Truncation => ClaudeSourceLifecycle::Rewind,
        ChangeSignal::Replacement => ClaudeSourceLifecycle::Replacement,
        ChangeSignal::Relocation => ClaudeSourceLifecycle::Move,
        ChangeSignal::LiveCopy => ClaudeSourceLifecycle::Copy,
        ChangeSignal::ConflictingLiveCopy => ClaudeSourceLifecycle::Ambiguous,
    }
}

fn refine_change_signal(
    signal: ChangeSignal,
    previous: Option<&ParseCheckpoint>,
    current: &ParseCheckpoint,
) -> ChangeSignal {
    let Some(previous) = previous else {
        return signal;
    };
    if signal == ChangeSignal::LiveCopy
        && (previous.complete_offset != current.complete_offset
            || previous.next_raw_ordinal != current.next_raw_ordinal
            || previous.complete_record_chain_sha256 != current.complete_record_chain_sha256)
    {
        ChangeSignal::ConflictingLiveCopy
    } else if signal == ChangeSignal::Replacement
        && previous.canonical_route != current.canonical_route
        && previous.session_key == current.session_key
        && previous.complete_offset == current.complete_offset
        && previous.next_raw_ordinal == current.next_raw_ordinal
        && previous.complete_record_chain_sha256 == current.complete_record_chain_sha256
    {
        ChangeSignal::Relocation
    } else {
        signal
    }
}

fn empty_parsed_record() -> ParsedClaudeRecord {
    ParsedClaudeRecord {
        result: Default::default(),
        preallocation_exclusion: false,
        native_record_id: None,
        session_id: None,
        timestamp: None,
        cwd: None,
        version: None,
        git_branch: None,
        rows: Vec::new(),
        outputs: Vec::new(),
    }
}

fn build_output_observations(
    source: &DiscoveredClaudeSession,
    locator: &ClaudePhysicalLocator,
    parsed: ParsedClaudeRecord,
) -> Vec<ProOutputObservation> {
    let provider_session_id = source.key.provider_session_id();
    let root_session_id = source.key.root_session_id.clone();
    let parent_session_id = source.key.parent_provider_session_id().map(str::to_owned);
    let occurred_at_unix_ms = parsed
        .timestamp
        .as_deref()
        .and_then(|timestamp| timestamp.parse::<DateTime<Utc>>().ok())
        .map(|timestamp| timestamp.timestamp_millis());
    let native_record_id = parsed
        .native_record_id
        .or_else(|| Some(format!("line-{}", locator.line_number)));
    parsed
        .outputs
        .into_iter()
        .map(|output| {
            let mut payload = Vec::with_capacity(20);
            payload.extend_from_slice(&0_u32.to_be_bytes());
            payload.extend_from_slice(&locator.byte_start.to_be_bytes());
            payload.extend_from_slice(&locator.byte_end_exclusive.to_be_bytes());
            let unit_key = if output.subrecord_index == 0 {
                format!("line-{}:output", locator.line_number)
            } else {
                format!(
                    "line-{}:output-{}",
                    locator.line_number, output.subrecord_index
                )
            };
            ProOutputObservation {
                kind: OutputObservationKind::Tool,
                coordinate: OutputNativeCoordinate {
                    unit_key,
                    native_sequence: locator.line_number.saturating_sub(1),
                    native_record_id: native_record_id.clone(),
                    source_record_ordinal: Some(locator.line_number.saturating_sub(1)),
                    source_record_subrecord_index: Some(output.subrecord_index),
                    byte_start: Some(locator.byte_start),
                    byte_end_exclusive: Some(locator.byte_end_exclusive),
                },
                occurred_at_unix_ms,
                associations: OutputAssociations {
                    direct_session_id: provider_session_id.clone(),
                    root_session_id: root_session_id.clone(),
                    parent_session_id: parent_session_id.clone(),
                    provider_session_id: Some(provider_session_id.clone()),
                    agent_id: source.key.agent_id.clone(),
                    repository: None,
                },
                call_id: output.call_id,
                command: None,
                outcome: output.outcome,
                locator: OutputSourceLocator {
                    version: 1,
                    kind: CLAUDE_OUTPUT_LOCATOR_KIND.to_owned(),
                    payload,
                },
                content: output.content.unwrap_or_default(),
            }
        })
        .collect()
}

#[derive(Serialize)]
struct CorePageEncoding<'a> {
    session: &'a ClaudeSessionMetadata,
    expected_frontier: &'a ClaudeNativeFrontier,
    next_safe_frontier: &'a ClaudeNativeFrontier,
    rows: &'a [ClaudeRetainedRow],
    rejections: &'a [RecordRejection],
    rejected_records: u64,
    logical_units: usize,
    terminal: bool,
    certificate: &'a ClaudePageCertificate,
}

#[allow(clippy::too_many_arguments)]
fn core_encoded_bytes(
    session: &ClaudeSessionMetadata,
    expected_frontier: &ClaudeNativeFrontier,
    next_safe_frontier: &ClaudeNativeFrontier,
    rows: &[ClaudeRetainedRow],
    rejections: &[RecordRejection],
    rejected_records: u64,
    logical_units: usize,
    terminal: bool,
    certificate: &ClaudePageCertificate,
) -> Result<usize, ClaudeNativePathError> {
    exact_json_encoded_bytes(&CorePageEncoding {
        session,
        expected_frontier,
        next_safe_frontier,
        rows,
        rejections,
        rejected_records,
        logical_units,
        terminal,
        certificate,
    })?
    .checked_add(CLAUDE_PAGE_ENCODING_ALLOWANCE)
    .ok_or(ClaudeNativePathError::PositionOverflow)
}

fn core_candidate_bytes(
    page: &CorePageBuilder,
    session: &ClaudeSessionMetadata,
    added_row_bytes: usize,
    added_rejection: Option<&RecordRejection>,
    next: &ClaudeNativeFrontier,
    terminal: bool,
    source: &DiscoveredClaudeSession,
) -> Option<usize> {
    let certificate = page_certificate(source, next);
    let added_rejection_bytes = added_rejection
        .map(exact_json_encoded_bytes)
        .transpose()
        .ok()?
        .unwrap_or_default();
    CLAUDE_PAGE_ENCODING_ALLOWANCE
        .checked_add(exact_json_encoded_bytes(session).ok()?)?
        .checked_add(exact_json_encoded_bytes(&page.expected_frontier).ok()?)?
        .checked_add(exact_json_encoded_bytes(next).ok()?)?
        .checked_add(exact_json_encoded_bytes(&certificate).ok()?)?
        .checked_add(page.encoded_row_bytes)?
        .checked_add(added_row_bytes)?
        .checked_add(page.encoded_rejection_bytes)?
        .checked_add(added_rejection_bytes)?
        .checked_add(usize::from(terminal))
}

pub(super) fn exact_json_encoded_bytes<T: Serialize>(
    value: &T,
) -> Result<usize, ClaudeNativePathError> {
    let mut counter = CountingWriter::default();
    serde_json::to_writer(&mut counter, value).map_err(|error| {
        ClaudeNativePathError::InvalidCheckpoint {
            reason: format!("Claude page encoding failed: {error}"),
        }
    })?;
    Ok(counter.bytes)
}

#[derive(Default)]
struct CountingWriter {
    bytes: usize,
}

impl Write for CountingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("Claude encoded byte count overflow"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct DigestWriter<'a>(&'a mut Sha256);

impl Write for DigestWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn core_page_identity(
    session: &ClaudeSessionMetadata,
    page: &CorePageBuilder,
    terminal: bool,
    certificate: &ClaudePageCertificate,
) -> Result<ClaudeNativePageIdentity, ClaudeNativePathError> {
    let mut hasher = Sha256::new();
    hasher.update(CLAUDE_CORE_PAGE_IDENTITY_DOMAIN);
    serde_json::to_writer(
        DigestWriter(&mut hasher),
        &CorePageEncoding {
            session,
            expected_frontier: &page.expected_frontier,
            next_safe_frontier: &page.next_safe_frontier,
            rows: &page.rows,
            rejections: &page.rejections,
            rejected_records: page.rejected_records,
            logical_units: page.logical_units,
            terminal,
            certificate,
        },
    )
    .map_err(|error| ClaudeNativePathError::InvalidCheckpoint {
        reason: format!("Claude Core page identity encoding failed: {error}"),
    })?;
    Ok(ClaudeNativePageIdentity(hasher.finalize().into()))
}

fn pro_candidate_bytes(
    page: &ProPageBuilder,
    parsed: &ParsedClaudeRecord,
    source: &DiscoveredClaudeSession,
) -> Option<usize> {
    let mut bytes = pro_builder_fixed_bytes(page, source)?;
    for output in &parsed.outputs {
        bytes = bytes.checked_add(parsed_output_wire_bytes(output, source)?)?;
    }
    Some(bytes)
}

fn pro_page_encoded_bytes(
    page: &ProPageBuilder,
    source: &DiscoveredClaudeSession,
    certificate: &ClaudePageCertificate,
) -> Result<usize, ClaudeNativePathError> {
    let mut bytes =
        pro_builder_fixed_bytes(page, source).ok_or(ClaudeNativePathError::PositionOverflow)?;
    bytes = bytes
        .checked_add(
            exact_json_encoded_bytes(certificate)?
                .checked_add(CLAUDE_PAGE_ENCODING_ALLOWANCE)
                .ok_or(ClaudeNativePathError::PositionOverflow)?,
        )
        .ok_or(ClaudeNativePathError::PositionOverflow)?;
    Ok(bytes)
}

fn pro_builder_fixed_bytes(
    page: &ProPageBuilder,
    source: &DiscoveredClaudeSession,
) -> Option<usize> {
    CLAUDE_PAGE_ENCODING_ALLOWANCE
        .checked_add(exact_json_encoded_bytes(&page.expected_frontier).ok()?)?
        .checked_add(exact_json_encoded_bytes(&page.next_safe_frontier).ok()?)?
        .checked_add(source.key.provider_session_id().len())?
        .checked_add(source.key.root_session_id.len())?
        .checked_add(page.encoded_output_bytes)?
        .checked_add(page.encoded_rejection_bytes)
}

fn parsed_output_wire_bytes(
    output: &super::record::ParsedClaudeOutput,
    source: &DiscoveredClaudeSession,
) -> Option<usize> {
    let content = output.content.as_ref()?.len();
    CLAUDE_PRO_OUTPUT_ENCODING_ALLOWANCE
        .checked_add(content)?
        .checked_add(output.call_id.as_ref().map_or(0, String::len))?
        .checked_add(source.key.provider_session_id().len())?
        .checked_add(source.key.root_session_id.len())
}

fn output_wire_bytes(output: &ProOutputObservation) -> usize {
    CLAUDE_PRO_OUTPUT_ENCODING_ALLOWANCE
        .saturating_add(output.content.len())
        .saturating_add(output.coordinate.unit_key.len())
        .saturating_add(
            output
                .coordinate
                .native_record_id
                .as_ref()
                .map_or(0, String::len),
        )
        .saturating_add(output.associations.direct_session_id.len())
        .saturating_add(output.associations.root_session_id.len())
        .saturating_add(
            output
                .associations
                .parent_session_id
                .as_ref()
                .map_or(0, String::len),
        )
        .saturating_add(output.call_id.as_ref().map_or(0, String::len))
        .saturating_add(output.locator.kind.len())
        .saturating_add(output.locator.payload.len())
}

fn pro_page_identity(
    page: &ProPageBuilder,
    terminal: bool,
    certificate: &ClaudePageCertificate,
) -> Result<ClaudeNativeProOutputPageIdentity, ClaudeNativePathError> {
    pro_page_identity_claims(
        &page.expected_frontier,
        &page.next_safe_frontier,
        &page.outputs,
        &page.rejections,
        page.rejected_outputs,
        page.logical_units,
        terminal,
        certificate,
    )
}

#[allow(clippy::too_many_arguments)]
fn pro_page_identity_claims(
    expected_frontier: &ClaudeNativeFrontier,
    next_safe_frontier: &ClaudeNativeFrontier,
    outputs: &[ProOutputObservation],
    rejections: &[RecordRejection],
    rejected_outputs: u64,
    logical_units: usize,
    terminal: bool,
    certificate: &ClaudePageCertificate,
) -> Result<ClaudeNativeProOutputPageIdentity, ClaudeNativePathError> {
    let mut hasher = Sha256::new();
    hasher.update(CLAUDE_PRO_PAGE_IDENTITY_DOMAIN);
    hash_canonical_json(&mut hasher, b"expected-frontier\0", expected_frontier)?;
    hash_canonical_json(&mut hasher, b"next-safe-frontier\0", next_safe_frontier)?;
    hash_usize(&mut hasher, logical_units)?;
    hash_usize(&mut hasher, outputs.len())?;
    hasher.update(rejected_outputs.to_be_bytes());
    hash_canonical_json(&mut hasher, b"rejections\0", &rejections)?;
    hash_canonical_json(&mut hasher, b"certificate\0", certificate)?;
    hasher.update([u8::from(terminal)]);
    for output in outputs {
        hash_pro_output_claim(&mut hasher, output)?;
    }
    Ok(ClaudeNativeProOutputPageIdentity(hasher.finalize().into()))
}

#[cfg(test)]
pub(super) fn pro_page_identity_for_test(
    page: &ClaudeNativeProOutputPage,
) -> Result<ClaudeNativeProOutputPageIdentity, ClaudeNativePathError> {
    pro_page_identity_claims(
        &page.expected_frontier,
        &page.next_safe_frontier,
        &page.outputs,
        &page.rejections,
        page.rejected_outputs,
        page.logical_units,
        page.terminal,
        &page.certificate,
    )
}

fn hash_canonical_json<T: Serialize>(
    hasher: &mut Sha256,
    domain: &[u8],
    value: &T,
) -> Result<(), ClaudeNativePathError> {
    hasher.update(domain);
    hasher.update(
        u64::try_from(exact_json_encoded_bytes(value)?)
            .map_err(|_| ClaudeNativePathError::PositionOverflow)?
            .to_be_bytes(),
    );
    serde_json::to_writer(DigestWriter(hasher), value).map_err(|error| {
        ClaudeNativePathError::InvalidCheckpoint {
            reason: format!("Claude Pro identity encoding failed: {error}"),
        }
    })
}

fn hash_pro_output_claim(
    hasher: &mut Sha256,
    output: &ProOutputObservation,
) -> Result<(), ClaudeNativePathError> {
    hasher.update(b"output\0");
    hasher.update([match output.kind {
        OutputObservationKind::Command => 1,
        OutputObservationKind::Tool => 2,
    }]);
    hash_text(hasher, &output.coordinate.unit_key)?;
    hasher.update(output.coordinate.native_sequence.to_be_bytes());
    hash_optional_text(hasher, output.coordinate.native_record_id.as_deref())?;
    hash_optional_u64(hasher, output.coordinate.source_record_ordinal);
    hash_optional_u32(hasher, output.coordinate.source_record_subrecord_index);
    hash_optional_u64(hasher, output.coordinate.byte_start);
    hash_optional_u64(hasher, output.coordinate.byte_end_exclusive);
    hash_optional_i64(hasher, output.occurred_at_unix_ms);

    hash_text(hasher, &output.associations.direct_session_id)?;
    hash_text(hasher, &output.associations.root_session_id)?;
    hash_optional_text(hasher, output.associations.parent_session_id.as_deref())?;
    hash_optional_text(hasher, output.associations.provider_session_id.as_deref())?;
    hash_optional_text(hasher, output.associations.agent_id.as_deref())?;
    hasher.update([u8::from(output.associations.repository.is_some())]);
    if let Some(repository) = &output.associations.repository {
        hash_text(hasher, &repository.repository_id)?;
        hash_optional_text(hasher, repository.checkout_id.as_deref())?;
        hash_optional_text(hasher, repository.worktree_id.as_deref())?;
        hash_optional_text(hasher, repository.object_format.as_deref())?;
    }

    hash_optional_text(hasher, output.call_id.as_deref())?;
    hasher.update([u8::from(output.command.is_some())]);
    if let Some(command) = &output.command {
        hash_text(hasher, &command.tool_name)?;
        hash_text(hasher, &command.command)?;
        hash_optional_text(hasher, command.working_directory.as_deref())?;
    }
    hasher.update([match output.outcome.outcome {
        OutputOutcome::Success => 1,
        OutputOutcome::Failure => 2,
        OutputOutcome::Timeout => 3,
        OutputOutcome::Unknown => 4,
    }]);
    hash_optional_i32(hasher, output.outcome.exit_code);
    hash_optional_u64(hasher, output.outcome.duration_ms);
    hasher.update(output.locator.version.to_be_bytes());
    hash_text(hasher, &output.locator.kind)?;
    hash_bytes(hasher, &output.locator.payload)?;
    hasher.update(
        u64::try_from(output.content.len())
            .map_err(|_| ClaudeNativePathError::PositionOverflow)?
            .to_be_bytes(),
    );
    hasher.update(Sha256::digest(&output.content));
    Ok(())
}

fn hash_optional_text(
    hasher: &mut Sha256,
    value: Option<&str>,
) -> Result<(), ClaudeNativePathError> {
    hasher.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        hash_text(hasher, value)?;
    }
    Ok(())
}

fn hash_text(hasher: &mut Sha256, value: &str) -> Result<(), ClaudeNativePathError> {
    hash_bytes(hasher, value.as_bytes())
}

fn hash_bytes(hasher: &mut Sha256, bytes: &[u8]) -> Result<(), ClaudeNativePathError> {
    hasher.update(
        u64::try_from(bytes.len())
            .map_err(|_| ClaudeNativePathError::PositionOverflow)?
            .to_be_bytes(),
    );
    hasher.update(bytes);
    Ok(())
}

fn hash_usize(hasher: &mut Sha256, value: usize) -> Result<(), ClaudeNativePathError> {
    hasher.update(
        u64::try_from(value)
            .map_err(|_| ClaudeNativePathError::PositionOverflow)?
            .to_be_bytes(),
    );
    Ok(())
}

fn hash_optional_u64(hasher: &mut Sha256, value: Option<u64>) {
    hasher.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        hasher.update(value.to_be_bytes());
    }
}

fn hash_optional_u32(hasher: &mut Sha256, value: Option<u32>) {
    hasher.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        hasher.update(value.to_be_bytes());
    }
}

fn hash_optional_i64(hasher: &mut Sha256, value: Option<i64>) {
    hasher.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        hasher.update(value.to_be_bytes());
    }
}

fn hash_optional_i32(hasher: &mut Sha256, value: Option<i32>) {
    hasher.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        hasher.update(value.to_be_bytes());
    }
}

fn page_certificate(
    source: &DiscoveredClaudeSession,
    frontier: &ClaudeNativeFrontier,
) -> ClaudePageCertificate {
    ClaudePageCertificate {
        canonical_route: source.canonical_path.clone(),
        observation_sha256: source.fingerprint.observation_sha256(),
        physical_file_id: source.fingerprint.physical_file_id,
        certified_prefix_end: frontier.complete_offset,
        certified_prefix_chain_sha256: frontier.complete_record_chain_sha256,
    }
}

#[derive(Default)]
struct BoundaryWindow {
    bytes: Vec<u8>,
}

impl BoundaryWindow {
    fn push_line(&mut self, line_tail: &[u8], observed_bytes: u64) {
        if observed_bytes >= CLAUDE_BOUNDARY_PROOF_BYTES as u64 {
            self.bytes.clear();
            self.bytes.extend_from_slice(line_tail);
        } else {
            push_bounded_tail(&mut self.bytes, line_tail);
        }
    }
}

fn verify_committed_prefix(
    file: &mut File,
    frontier: &ClaudeNativeFrontier,
    path: &std::path::Path,
    stats: &mut ParseStats,
) -> Result<Option<BoundaryWindow>, ClaudeNativePathError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|error| ClaudeNativePathError::Io {
            path: path.to_path_buf(),
            source: error,
        })?;
    let mut reader = BufReader::new(file);
    let mut observed = 0_u64;
    let mut ordinal = 0_u64;
    let mut chain = initial_record_chain();
    let mut boundary_window = BoundaryWindow::default();
    while observed < frontier.complete_offset {
        let Some(raw_line) = read_raw_line(&mut reader, path)? else {
            return Ok(None);
        };
        stats.prefix_verification_bytes = stats
            .prefix_verification_bytes
            .checked_add(raw_line.observed_bytes)
            .ok_or(ClaudeNativePathError::PositionOverflow)?;
        stats.source_bytes_read = stats
            .source_bytes_read
            .checked_add(raw_line.observed_bytes)
            .ok_or(ClaudeNativePathError::PositionOverflow)?;
        stats.prefix_verification_records = stats
            .prefix_verification_records
            .checked_add(1)
            .ok_or(ClaudeNativePathError::PositionOverflow)?;
        observed = observed
            .checked_add(raw_line.observed_bytes)
            .ok_or(ClaudeNativePathError::PositionOverflow)?;
        if observed > frontier.complete_offset || !raw_line.terminated {
            return Ok(None);
        }
        chain = advance_record_chain(&chain, ordinal, &raw_line.raw_sha256);
        boundary_window.push_line(&raw_line.boundary_tail, raw_line.observed_bytes);
        ordinal = ordinal
            .checked_add(1)
            .ok_or(ClaudeNativePathError::PositionOverflow)?;
    }
    let expected_len = usize::try_from(frontier.boundary_proof_len)
        .map_err(|_| ClaudeNativePathError::PositionOverflow)?;
    let matches = observed == frontier.complete_offset
        && ordinal == frontier.next_raw_ordinal
        && chain == frontier.complete_record_chain_sha256
        && expected_len == boundary_window.bytes.len()
        && frontier.boundary_proof_sha256 == boundary_proof_hash(&boundary_window.bytes);
    Ok(matches.then_some(boundary_window))
}

fn boundary_proof_hash(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(CLAUDE_BOUNDARY_PROOF_DOMAIN);
    hasher.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(bytes);
    hasher.finalize().into()
}

struct RawLine {
    buffer: Vec<u8>,
    boundary_tail: Vec<u8>,
    observed_bytes: u64,
    terminated: bool,
    oversized: bool,
    raw_sha256: [u8; 32],
}

fn read_raw_line(
    reader: &mut impl BufRead,
    path: &std::path::Path,
) -> Result<Option<RawLine>, ClaudeNativePathError> {
    let mut buffer = Vec::new();
    let mut boundary_tail = Vec::new();
    let mut observed_bytes = 0_u64;
    let mut terminated = false;
    let mut oversized = false;
    let mut raw_hasher = Sha256::new();
    loop {
        let available = reader
            .fill_buf()
            .map_err(|error| ClaudeNativePathError::Io {
                path: path.to_path_buf(),
                source: error,
            })?;
        if available.is_empty() {
            break;
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consume = newline.map_or(available.len(), |index| index + 1);
        let consumed = &available[..consume];
        raw_hasher.update(consumed);
        push_bounded_tail(&mut boundary_tail, consumed);
        observed_bytes = observed_bytes
            .checked_add(
                u64::try_from(consume).map_err(|_| ClaudeNativePathError::PositionOverflow)?,
            )
            .ok_or(ClaudeNativePathError::PositionOverflow)?;
        if !oversized {
            let next_len = buffer.len().saturating_add(consume);
            if next_len > MAX_PROVIDER_JSONL_LINE_BYTES {
                buffer.clear();
                oversized = true;
            } else {
                buffer.extend_from_slice(consumed);
            }
        }
        reader.consume(consume);
        if newline.is_some() {
            terminated = true;
            break;
        }
    }
    if observed_bytes == 0 {
        return Ok(None);
    }
    Ok(Some(RawLine {
        buffer,
        boundary_tail,
        observed_bytes,
        terminated,
        oversized,
        raw_sha256: raw_hasher.finalize().into(),
    }))
}

fn push_bounded_tail(tail: &mut Vec<u8>, bytes: &[u8]) {
    if bytes.len() >= CLAUDE_BOUNDARY_PROOF_BYTES {
        tail.clear();
        tail.extend_from_slice(&bytes[bytes.len() - CLAUDE_BOUNDARY_PROOF_BYTES..]);
        return;
    }
    let excess = tail
        .len()
        .saturating_add(bytes.len())
        .saturating_sub(CLAUDE_BOUNDARY_PROOF_BYTES);
    if excess > 0 {
        tail.drain(..excess);
    }
    tail.extend_from_slice(bytes);
}

fn observe_parse_io(
    stats: &mut ParseStats,
    observed_bytes: u64,
) -> Result<(), ClaudeNativePathError> {
    stats.parsed_source_bytes = stats
        .parsed_source_bytes
        .checked_add(observed_bytes)
        .ok_or(ClaudeNativePathError::PositionOverflow)?;
    stats.source_bytes_read = stats
        .source_bytes_read
        .checked_add(observed_bytes)
        .ok_or(ClaudeNativePathError::PositionOverflow)?;
    Ok(())
}

fn json_record_bytes(bytes: &[u8]) -> &[u8] {
    let mut end = bytes.len();
    if bytes.get(end.saturating_sub(1)) == Some(&b'\n') {
        end = end.saturating_sub(1);
        if bytes.get(end.saturating_sub(1)) == Some(&b'\r') {
            end = end.saturating_sub(1);
        }
    }
    &bytes[..end]
}

fn initial_record_chain() -> [u8; 32] {
    Sha256::digest(CLAUDE_RECORD_CHAIN_DOMAIN).into()
}

fn advance_record_chain(previous: &[u8; 32], raw_ordinal: u64, raw_sha256: &[u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(CLAUDE_RECORD_CHAIN_DOMAIN);
    hasher.update(previous);
    hasher.update(raw_ordinal.to_be_bytes());
    hasher.update(raw_sha256);
    hasher.finalize().into()
}

fn initial_identity_chain() -> [u8; 32] {
    Sha256::digest(CLAUDE_IDENTITY_HASH_DOMAIN).into()
}

fn advance_identity_chain(
    previous: &[u8; 32],
    raw_ordinal: u64,
    identity_kind: &[u8],
    native_record_id: Option<&str>,
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(CLAUDE_IDENTITY_HASH_DOMAIN);
    hasher.update(previous);
    hasher.update(raw_ordinal.to_be_bytes());
    update_identity_part(&mut hasher, identity_kind);
    update_identity_part(&mut hasher, native_record_id.unwrap_or_default().as_bytes());
    hasher.finalize().into()
}

fn update_identity_part(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(value);
}
