use std::{
    fs::File,
    io::{self, BufRead, BufReader, Seek, SeekFrom, Write},
};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::MAX_PROVIDER_JSONL_LINE_BYTES;

use super::{
    checkpoint::{
        lane_observation_binding, ChangeSignal, ClaudeNativeFrontier, ParseCheckpoint,
        CLAUDE_NATIVEPATH_PARSER_REVISION, CLAUDE_NATIVEPATH_POLICY_REVISION,
    },
    record::{parse_native_record, ParsedClaudeRecord},
    rows::{
        ClaudePhysicalLocator, ClaudeRetainedRow, ClaudeSessionMetadata, ParseStats,
        RecordRejection, RejectionKind, RejectionSummary, CLAUDE_MAX_PAGE_BYTES,
        CLAUDE_MAX_PAGE_ROWS,
    },
    source::{
        open_discovered_file, revalidate_open_file, ClaudeFileFingerprint, ClaudeNativePathError,
        ClaudePhysicalFileId, DiscoveredClaudeSession,
    },
};

mod page;
mod page_identity;
mod page_queue;
mod raw_io;
mod read_plan;

pub(crate) use page::{ClaudeNativePage, ClaudePageCertificate, ParseOutput};
pub(super) use page_identity::exact_json_encoded_bytes;
use page_identity::*;
use raw_io::*;
use read_plan::{empty_parsed_record, plan_read, refine_change_signal};

const CLAUDE_RECORD_CHAIN_DOMAIN: &[u8] = b"ctx-claude-nativepath-record-chain-v1\0";
const CLAUDE_BOUNDARY_PROOF_DOMAIN: &[u8] = b"ctx-claude-nativepath-boundary-proof-v1\0";
const CLAUDE_IDENTITY_HASH_DOMAIN: &[u8] = b"ctx-claude-nativepath-native-identity-v1\0";
const CLAUDE_BOUNDARY_PROOF_BYTES: usize = 64 * 1024;
const CLAUDE_PAGE_ENCODING_ALLOWANCE: usize = 4 * 1024;

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
    change: ChangeSignal,
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
    has_incomplete_tail: bool,
    stats: ParseStats,
    core_page: CorePageBuilder,
    ready_core: Option<ClaudeNativePage>,
    pending: Option<PendingRecord>,
    exhausted: bool,
    replay: bool,
    emitted_core: bool,
}

impl ClaudeNativeScanner {
    pub(crate) fn new(
        source: DiscoveredClaudeSession,
        previous: Option<&ParseCheckpoint>,
    ) -> Result<Self, ClaudeNativePathError> {
        let mut file = open_discovered_file(&source)?;
        let before = source.fingerprint.clone();
        let mut stats = ParseStats::default();
        let plan = plan_read(&source, previous, &mut file, &mut stats)?;
        if plan.parse {
            file.seek(SeekFrom::Start(plan.frontier.complete_offset))
                .map_err(|error| ClaudeNativePathError::Io {
                    path: source.path.clone(),
                    source: error,
                })?;
        }
        let initial = plan.frontier.clone();
        let replay = !plan.parse;
        let has_incomplete_tail = replay && previous.is_some_and(|checkpoint| !checkpoint.terminal);
        Ok(Self {
            session: ClaudeSessionMetadata::new(source.key.clone()),
            source,
            before,
            reader: BufReader::new(file),
            change: plan.change,
            previous: previous.cloned(),
            offset: initial.complete_offset,
            raw_ordinal: initial.next_raw_ordinal,
            record_chain: initial.complete_record_chain_sha256,
            boundary_window: plan.boundary_window,
            native_identity_chain: initial.native_identity_chain_sha256,
            native_identity_records: initial.native_identity_records,
            last_complete_terminated: initial.appendable_boundary,
            rejections: RejectionSummary::default(),
            has_incomplete_tail,
            stats,
            core_page: CorePageBuilder::new(initial),
            ready_core: None,
            pending: None,
            exhausted: replay,
            replay,
            emitted_core: false,
        })
    }

    pub(crate) fn next_page(&mut self) -> Result<Option<ClaudeNativePage>, ClaudeNativePathError> {
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
            let record_sha256 = Sha256::digest(json_record_bytes(&raw_line.buffer)).into();
            let locator = ClaudePhysicalLocator {
                path: self.source.canonical_path.clone(),
                byte_start,
                byte_end_exclusive,
                line_number: self
                    .raw_ordinal
                    .checked_add(1)
                    .ok_or(ClaudeNativePathError::PositionOverflow)?,
                record_sha256,
            };

            if !raw_line.terminated {
                self.has_incomplete_tail = true;
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
                    match parse_native_record(json, self.raw_ordinal, &locator) {
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

    pub(crate) fn finish(mut self) -> Result<ParseOutput, ClaudeNativePathError> {
        if !self.exhausted
            || self.pending.is_some()
            || self.ready_core.is_some()
            || self.core_page.logical_units != 0
        {
            return Err(ClaudeNativePathError::InvalidCheckpoint {
                reason: "Claude NativePath scan must drain all owned pages before certification"
                    .to_owned(),
            });
        }
        revalidate_open_file(&self.source, self.reader.get_ref(), &self.before)?;
        let frontier = self.frontier();
        let terminal = !self.has_incomplete_tail;
        let checkpoint = self.build_checkpoint(&frontier, terminal);
        self.change = refine_change_signal(self.change, self.previous.as_ref(), &checkpoint);
        if self.replay {
            self.stats.metadata_only_noop = true;
        }
        Ok(ParseOutput {
            change: self.change,
            rejections: self.rejections,
            checkpoint,
            stats: self.stats,
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
        let candidate_core_row_bytes = if core_record_rejection.is_some() {
            0
        } else {
            pending.core_rows_encoded_bytes
        };
        let core_needs_flush = self.core_page.logical_units != 0
            && core_candidate_bytes(
                &self.core_page,
                &prospective_session,
                candidate_core_row_bytes,
                core_record_rejection.as_ref(),
                &pending.after,
                false,
                &self.source,
            )
            .is_none_or(|bytes| bytes > CLAUDE_MAX_PAGE_BYTES);
        if core_needs_flush {
            self.flush_core_page(false)?;
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
        let mut rows = std::mem::take(&mut pending.parsed.rows);
        if let Some(rejection) = core_record_rejection {
            rows.clear();
            self.core_page.rejected_records = self.core_page.rejected_records.saturating_add(1);
            if self.core_page.rejections.len() < super::rows::CLAUDE_MAX_REJECTION_SAMPLES {
                self.core_page.encoded_rejection_bytes = self
                    .core_page
                    .encoded_rejection_bytes
                    .saturating_add(exact_json_encoded_bytes(&rejection).unwrap_or(usize::MAX));
                self.core_page.rejections.push(rejection);
            }
        }
        for row in &rows {
            self.stats.observe_row(row);
        }
        self.core_page.encoded_row_bytes = self
            .core_page
            .encoded_row_bytes
            .saturating_add(candidate_core_row_bytes);
        self.core_page.rows.extend(rows);
        self.core_page.logical_units = self.core_page.logical_units.saturating_add(1);
        self.core_page.next_safe_frontier = pending.after;
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
            self.stats.preallocation_excluded_result_records = self
                .stats
                .preallocation_excluded_result_records
                .saturating_add(u64::from(parsed.preallocation_exclusion));
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
        }
    }

    fn build_checkpoint(&self, frontier: &ClaudeNativeFrontier, terminal: bool) -> ParseCheckpoint {
        let observation_sha256 = self.before.observation_sha256();
        ParseCheckpoint {
            parser_revision: CLAUDE_NATIVEPATH_PARSER_REVISION,
            policy_revision: CLAUDE_NATIVEPATH_POLICY_REVISION,
            session_key: self.source.key.clone(),
            canonical_route: self.source.canonical_path.clone(),
            physical_file_id: self.before.physical_file_id,
            observed_file_len: self.before.len,
            observation_sha256,
            core_observation_binding_sha256: lane_observation_binding(
                self.before.len,
                &observation_sha256,
                frontier,
                terminal,
            ),
            complete_offset: frontier.complete_offset,
            next_raw_ordinal: frontier.next_raw_ordinal,
            complete_record_chain_sha256: frontier.complete_record_chain_sha256,
            boundary_proof_len: frontier.boundary_proof_len,
            boundary_proof_sha256: frontier.boundary_proof_sha256,
            native_identity_chain_sha256: frontier.native_identity_chain_sha256,
            native_identity_records: frontier.native_identity_records,
            terminal,
            appendable_boundary: frontier.appendable_boundary,
        }
    }
}

struct ReadPlan {
    change: ChangeSignal,
    parse: bool,
    frontier: ClaudeNativeFrontier,
    boundary_window: BoundaryWindow,
}

struct RawLine {
    buffer: Vec<u8>,
    boundary_tail: Vec<u8>,
    observed_bytes: u64,
    terminated: bool,
    oversized: bool,
    raw_sha256: [u8; 32],
}
