use std::{
    collections::BTreeSet,
    fs::{self, File, Metadata},
    io::{BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    common::io::ensure_regular_provider_transcript_file,
    provider::{
        importer::provider_path_identity,
        native_ingestion::{
            process_pro_replay_only, NativePageAccounting, NativeProOutputPage,
            NativeProReplayPage, NativeSafeFrontier, NativeSourceIdentity,
        },
    },
    CaptureError, OutputAssociations, OutputNativeCoordinate, OutputObservationKind,
    OutputSourceIdentity, OutputSourceLocator, ProOutputObservation, ProOutputProgress,
    ProOutputSink, ProOutputSourceDisposition, Result,
};

use super::{
    checkpoint::{CursorCheckpoint, CursorCheckpointDisposition},
    layout::{CursorRootInventory, CursorTranscriptPath},
    parser::{
        scan_cursor_output_pages, scan_cursor_reader, CursorOutputFact, CursorOutputPage,
        CursorOutputScanOutcome, CursorParserOutcome, CursorParserPlan, CursorParserStats,
        CursorRecordRejection, CursorRejectionKind, CursorRejectionSummary,
    },
    projection::{CursorNativeSession, CursorPublicationSink},
};

const CURSOR_OUTPUT_FRONTIER_VERSION: u32 = 1;
const CURSOR_OUTPUT_PARSER_REVISION: &str = "cursor-nativepath-output-v1";

#[cfg(test)]
use super::projection::{CursorNativeEvent, CursorPublicationPage};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CursorObservedTime {
    pub(crate) before_epoch: bool,
    pub(crate) seconds: u64,
    pub(crate) nanos: u32,
}

impl CursorObservedTime {
    fn from_system_time(value: SystemTime) -> Self {
        match value.duration_since(UNIX_EPOCH) {
            Ok(duration) => Self {
                before_epoch: false,
                seconds: duration.as_secs(),
                nanos: duration.subsec_nanos(),
            },
            Err(error) => {
                let duration = error.duration();
                Self {
                    before_epoch: true,
                    seconds: duration.as_secs(),
                    nanos: duration.subsec_nanos(),
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CursorFileIdentity {
    pub(crate) device: u64,
    pub(crate) inode: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CursorSourceObservation {
    pub(crate) path: PathBuf,
    pub(crate) locator_identity: String,
    pub(crate) proposed_source_identity: String,
    pub(crate) native_session_id: String,
    pub(crate) length: u64,
    /// Private control-plane proof; never published as an event or result hash.
    pub(crate) content_sha256: [u8; 32],
    pub(crate) modified: CursorObservedTime,
    pub(crate) changed: Option<CursorObservedTime>,
    pub(crate) readonly: bool,
    pub(crate) file_identity: Option<CursorFileIdentity>,
}

#[derive(Debug, Clone)]
pub(crate) struct CursorFrozenSource {
    transcript: CursorTranscriptPath,
    observation: CursorSourceObservation,
}

impl CursorFrozenSource {
    pub(crate) fn transcript(&self) -> &CursorTranscriptPath {
        &self.transcript
    }

    pub(crate) fn observation(&self) -> &CursorSourceObservation {
        &self.observation
    }

    fn open(&self) -> Result<File> {
        let (file, observed) =
            open_observed_cursor_file(&self.observation.path, &self.observation.native_session_id)?;
        if !observed.same_strong_snapshot(&self.observation) {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        Ok(file)
    }

    pub(crate) fn revalidate(&self) -> Result<()> {
        ensure_regular_provider_transcript_file(&self.observation.path)?;
        let (_, observed) =
            open_observed_cursor_file(&self.observation.path, &self.observation.native_session_id)?;
        if !observed.same_strong_snapshot(&self.observation) {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        Ok(())
    }

    pub(crate) fn replay_outputs(
        &self,
        source_root: &Path,
        canonical_source_identity: &str,
        core_checkpoint: &CursorCheckpoint,
        observed_revision: &str,
        sink: &dyn ProOutputSink,
    ) -> Result<()> {
        let output_source = OutputSourceIdentity {
            provider: ctx_history_core::CaptureProvider::Cursor
                .as_str()
                .to_owned(),
            namespace_id: source_root.display().to_string(),
            source_id: self.observation.locator_identity.clone(),
        };
        let progress = match sink.observe_source(&output_source) {
            Ok(progress) => progress,
            Err(error) => {
                sink.mark_behind(error);
                return Ok(());
            }
        };
        let same_physical_source = progress.as_ref().is_none_or(|progress| {
            cursor_output_revision_allows_resume(&progress.observed_revision, observed_revision)
        });
        let resume = same_physical_source
            .then(|| resumable_cursor_output_checkpoint(progress.as_ref(), sink))
            .flatten();
        if progress.as_ref().is_some_and(|progress| {
            progress.observed_revision == observed_revision
                && progress.terminal == core_checkpoint.terminal
                && resume.as_ref() == Some(core_checkpoint)
        }) {
            return Ok(());
        }

        let append_or_resume = progress.is_some() && resume.is_some();
        let mut state =
            CursorOutputState::new(output_source.clone(), progress.as_ref(), append_or_resume)?;
        let outcome = self.scan_and_replay_outputs(
            canonical_source_identity,
            core_checkpoint,
            observed_revision,
            resume.as_ref(),
            sink,
            &mut state,
        )?;
        if outcome == CursorOutputScanOutcome::PrefixMismatch && append_or_resume {
            state = CursorOutputState::new(output_source, progress.as_ref(), false)?;
            let retry = self.scan_and_replay_outputs(
                canonical_source_identity,
                core_checkpoint,
                observed_revision,
                None,
                sink,
                &mut state,
            )?;
            if retry == CursorOutputScanOutcome::PrefixMismatch {
                return Err(CaptureError::InvalidPayload(
                    "Cursor full output replay did not reproduce committed Core".to_owned(),
                ));
            }
        }
        Ok(())
    }

    fn scan_and_replay_outputs(
        &self,
        canonical_source_identity: &str,
        core_checkpoint: &CursorCheckpoint,
        observed_revision: &str,
        resume: Option<&CursorCheckpoint>,
        sink: &dyn ProOutputSink,
        state: &mut CursorOutputState,
    ) -> Result<CursorOutputScanOutcome> {
        let file = self.open()?;
        let mut reader = BufReader::new(file);
        let source_identity = NativeSourceIdentity::new(
            ctx_history_core::CaptureProvider::Cursor.as_str(),
            canonical_source_identity,
        );
        let mut emit = |page: CursorOutputPage| {
            let expected_frontier = cursor_output_frontier(&page.expected_checkpoint)?;
            let next_frontier = cursor_output_frontier(&page.next_checkpoint)?;
            let observations = page
                .outputs
                .into_iter()
                .map(|output| cursor_output_observation(&self.observation, output))
                .collect::<Result<Vec<_>>>()?;
            let output = NativeProOutputPage {
                inventory_generation: sink.inventory_generation(),
                source: state.source.clone(),
                source_epoch: state.source_epoch,
                observed_revision: observed_revision.to_owned(),
                parser_revision: CURSOR_OUTPUT_PARSER_REVISION.to_owned(),
                materializer_revision: sink.materializer_revision().to_owned(),
                disposition: state.disposition,
                expected_prior_source_epoch: state.expected_source_epoch,
                expected_prior_frontier: state.expected_sink_frontier.clone(),
                observations,
            };
            let replay = NativeProReplayPage::new_with_source_identity(
                source_identity.clone(),
                expected_frontier,
                next_frontier.clone(),
                page.terminal,
                NativePageAccounting {
                    logical_units: page.logical_units,
                    conservative_serialized_bytes: page.conservative_serialized_bytes,
                },
                output,
            )
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
            self.revalidate()?;
            if process_pro_replay_only(replay, sink).is_err() {
                return Ok(false);
            }
            state.advance(next_frontier);
            Ok(true)
        };
        let outcome = scan_cursor_output_pages(&mut reader, resume, core_checkpoint, &mut emit)?;
        self.revalidate()?;
        Ok(outcome)
    }
}

struct CursorOutputState {
    source: OutputSourceIdentity,
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_sink_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
}

impl CursorOutputState {
    fn new(
        source: OutputSourceIdentity,
        progress: Option<&ProOutputProgress>,
        append_or_resume: bool,
    ) -> Result<Self> {
        let Some(progress) = progress else {
            return Ok(Self {
                source,
                source_epoch: 0,
                expected_source_epoch: None,
                expected_sink_frontier: None,
                disposition: ProOutputSourceDisposition::NewSource,
            });
        };
        let expected_sink_frontier = progress
            .cursor
            .as_ref()
            .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload.clone()))
            .transpose()
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        Ok(Self {
            source,
            source_epoch: if append_or_resume {
                progress.source_epoch
            } else {
                progress
                    .source_epoch
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Cursor output source epoch exhausted",
                    ))?
            },
            expected_source_epoch: Some(progress.source_epoch),
            expected_sink_frontier,
            disposition: if append_or_resume {
                ProOutputSourceDisposition::AppendOrResume
            } else {
                ProOutputSourceDisposition::Rewrite
            },
        })
    }

    fn advance(&mut self, next_frontier: NativeSafeFrontier) {
        self.expected_source_epoch = Some(self.source_epoch);
        self.expected_sink_frontier = Some(next_frontier);
        self.disposition = ProOutputSourceDisposition::AppendOrResume;
    }
}

fn resumable_cursor_output_checkpoint(
    progress: Option<&ProOutputProgress>,
    sink: &dyn ProOutputSink,
) -> Option<CursorCheckpoint> {
    let progress = progress?;
    if progress.parser_revision != CURSOR_OUTPUT_PARSER_REVISION
        || progress.materializer_revision != sink.materializer_revision()
    {
        return None;
    }
    let cursor = progress.cursor.as_ref()?;
    if cursor.version != CURSOR_OUTPUT_FRONTIER_VERSION {
        return None;
    }
    serde_json::from_slice::<CursorCheckpoint>(&cursor.payload)
        .ok()
        .filter(CursorCheckpoint::is_supported)
}

fn cursor_output_frontier(checkpoint: &CursorCheckpoint) -> Result<NativeSafeFrontier> {
    NativeSafeFrontier::new(
        CURSOR_OUTPUT_FRONTIER_VERSION,
        serde_json::to_vec(checkpoint)?,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

fn cursor_output_revision_allows_resume(previous: &str, current: &str) -> bool {
    match (
        cursor_output_revision_file_identity(previous),
        cursor_output_revision_file_identity(current),
    ) {
        (Some(previous), Some(current)) => previous == current,
        _ => true,
    }
}

fn cursor_output_revision_file_identity(revision: &str) -> Option<(&str, &str)> {
    let (_, identity) = revision.rsplit_once(";device=")?;
    let (device, inode) = identity.split_once(";inode=")?;
    (device != "none" && inode != "none").then_some((device, inode))
}

fn cursor_output_observation(
    source: &CursorSourceObservation,
    output: CursorOutputFact,
) -> Result<ProOutputObservation> {
    let locator = serde_json::to_vec(&serde_json::json!({
        "path": source.path,
        "locator_identity": source.locator_identity,
        "semantic_ordinal": output.semantic_ordinal,
        "subrecord_index": output.subrecord_index,
        "byte_start": output.byte_start,
        "byte_end_exclusive": output.byte_end_exclusive,
    }))?;
    Ok(ProOutputObservation {
        kind: OutputObservationKind::Tool,
        coordinate: OutputNativeCoordinate {
            unit_key: format!(
                "cursor-semantic-v1:{}:{}",
                output.semantic_ordinal, output.subrecord_index
            ),
            native_sequence: output.semantic_ordinal,
            native_record_id: output.call_id.clone(),
            source_record_ordinal: Some(output.semantic_ordinal),
            source_record_subrecord_index: Some(output.subrecord_index),
            byte_start: Some(output.byte_start),
            byte_end_exclusive: Some(output.byte_end_exclusive),
        },
        occurred_at_unix_ms: output.occurred_at_unix_ms,
        associations: OutputAssociations {
            direct_session_id: source.native_session_id.clone(),
            root_session_id: source.native_session_id.clone(),
            parent_session_id: None,
            provider_session_id: Some(source.native_session_id.clone()),
            agent_id: None,
            repository: None,
        },
        call_id: output.call_id,
        command: None,
        outcome: output.outcome,
        locator: OutputSourceLocator {
            version: 1,
            kind: "cursor/nativepath/jsonl-result".to_owned(),
            payload: locator,
        },
        content: output.content,
    })
}

pub(crate) fn freeze_cursor_source(
    transcript: &CursorTranscriptPath,
) -> Result<CursorFrozenSource> {
    ensure_regular_provider_transcript_file(transcript.path())?;
    let canonical_path = fs::canonicalize(transcript.path())?;
    let (_, observation) =
        open_observed_cursor_file(&canonical_path, transcript.native_session_id())?;
    Ok(CursorFrozenSource {
        transcript: transcript.clone(),
        observation,
    })
}

fn observation_from_metadata(
    path: &Path,
    native_session_id: &str,
    metadata: &Metadata,
    content_sha256: [u8; 32],
) -> Result<CursorSourceObservation> {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;

    let locator_identity = provider_path_identity(path)?;
    #[cfg(unix)]
    let (file_identity, changed) = (
        Some(CursorFileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        }),
        unix_changed_time(metadata),
    );
    #[cfg(not(unix))]
    let (file_identity, changed) = (None, None);
    Ok(CursorSourceObservation {
        path: path.to_path_buf(),
        proposed_source_identity: format!("cursor-native-path-v1:{locator_identity}"),
        locator_identity,
        native_session_id: native_session_id.to_owned(),
        length: metadata.len(),
        content_sha256,
        modified: CursorObservedTime::from_system_time(metadata.modified()?),
        changed,
        readonly: metadata.permissions().readonly(),
        file_identity,
    })
}

impl CursorSourceObservation {
    fn same_strong_snapshot(&self, other: &Self) -> bool {
        self.path == other.path
            && self.locator_identity == other.locator_identity
            && self.native_session_id == other.native_session_id
            && self.length == other.length
            && self.content_sha256 == other.content_sha256
    }
}

fn open_observed_cursor_file(
    path: &Path,
    native_session_id: &str,
) -> Result<(File, CursorSourceObservation)> {
    let mut file = File::open(path)?;
    let before = file.metadata()?;
    let content_sha256 = hash_cursor_file(&mut file)?;
    let after = file.metadata()?;
    let before_observation =
        observation_from_metadata(path, native_session_id, &before, content_sha256)?;
    let after_observation =
        observation_from_metadata(path, native_session_id, &after, content_sha256)?;
    if before_observation != after_observation {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    file.seek(SeekFrom::Start(0))?;
    Ok((file, after_observation))
}

fn hash_cursor_file(file: &mut File) -> Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().into())
}

#[cfg(unix)]
fn unix_changed_time(metadata: &Metadata) -> Option<CursorObservedTime> {
    use std::os::unix::fs::MetadataExt;

    let seconds = metadata.ctime();
    let nanos = metadata.ctime_nsec();
    if !(0..1_000_000_000).contains(&nanos) {
        return None;
    }
    if seconds >= 0 {
        Some(CursorObservedTime {
            before_epoch: false,
            seconds: seconds as u64,
            nanos: nanos as u32,
        })
    } else {
        Some(CursorObservedTime {
            before_epoch: true,
            seconds: seconds.unsigned_abs(),
            nanos: nanos as u32,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CursorPriorObservation {
    pub(crate) canonical_source_key: String,
    pub(crate) observation: CursorSourceObservation,
    pub(crate) checkpoint: CursorCheckpoint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum CursorSourceMutation {
    NewPathScopedSource,
    ExactReplay,
    AppendCandidate,
    Rewrite,
    Truncation,
}

pub(super) fn cursor_source_mutation(
    current: &CursorSourceObservation,
    prior: Option<&CursorPriorObservation>,
) -> CursorSourceMutation {
    let Some(prior) = prior.filter(|prior| {
        prior.observation.locator_identity == current.locator_identity
            && prior.observation.native_session_id == current.native_session_id
    }) else {
        return CursorSourceMutation::NewPathScopedSource;
    };
    if prior.observation.same_strong_snapshot(current) {
        return CursorSourceMutation::ExactReplay;
    }
    match current.length.cmp(&prior.observation.length) {
        std::cmp::Ordering::Less => CursorSourceMutation::Truncation,
        std::cmp::Ordering::Greater => CursorSourceMutation::AppendCandidate,
        std::cmp::Ordering::Equal => CursorSourceMutation::Rewrite,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CursorSourceRejection {
    pub(crate) physical_line: u64,
    pub(crate) kind: CursorRejectionKind,
    pub(crate) observed_bytes: u64,
}

impl From<CursorRecordRejection> for CursorSourceRejection {
    fn from(value: CursorRecordRejection) -> Self {
        Self {
            physical_line: value.physical_line,
            kind: value.kind,
            observed_bytes: value.observed_bytes,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CursorSourceRejections {
    pub(crate) total: u64,
    pub(crate) samples: Vec<CursorSourceRejection>,
}

impl From<CursorRejectionSummary> for CursorSourceRejections {
    fn from(value: CursorRejectionSummary) -> Self {
        Self {
            total: value.total,
            samples: value.samples.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CursorGenerationStats {
    pub(crate) source_opens: u64,
    pub(crate) source_revalidations: u64,
    pub(crate) full_rescans_after_prefix_mismatch: u64,
    pub(crate) bytes_read: u64,
    pub(crate) verification_bytes_read: u64,
    pub(crate) projected_bytes_read: u64,
    pub(crate) complete_records: u64,
    pub(crate) malformed_records: u64,
    pub(crate) oversized_records: u64,
    pub(crate) incomplete_tail_records: u64,
    pub(crate) native_result_records: u64,
    pub(crate) native_result_bytes: u64,
    pub(crate) result_body_bytes_decoded_or_allocated: u64,
    pub(crate) result_hashes_created: u64,
    pub(crate) result_previews_created: u64,
    pub(crate) result_touches_created: u64,
    pub(crate) result_fts_created: u64,
    pub(crate) result_handoffs_created: u64,
    pub(crate) retained_messages: u64,
    pub(crate) retained_summaries: u64,
    pub(crate) retained_notices: u64,
    pub(crate) retained_tool_calls: u64,
    pub(crate) retained_body_bytes: u64,
    pub(crate) max_line_buffer_bytes: usize,
    pub(crate) publication_pages: u64,
    pub(crate) nativepath_publication_rows: u64,
    pub(crate) publication_serialized_bytes: u64,
    pub(crate) max_publication_page_rows: usize,
    pub(crate) max_publication_page_bytes: usize,
}

impl CursorGenerationStats {
    fn from_parser(parser: CursorParserStats) -> Self {
        Self {
            source_opens: 1,
            source_revalidations: 1,
            full_rescans_after_prefix_mismatch: 0,
            bytes_read: parser.bytes_read,
            verification_bytes_read: parser.verification_bytes_read,
            projected_bytes_read: parser.projected_bytes_read,
            complete_records: parser.complete_records,
            malformed_records: parser.malformed_records,
            oversized_records: parser.oversized_records,
            incomplete_tail_records: parser.incomplete_tail_records,
            native_result_records: parser.native_result_records,
            native_result_bytes: parser.native_result_bytes,
            result_body_bytes_decoded_or_allocated: parser.result_body_bytes_decoded_or_allocated,
            result_hashes_created: parser.result_hashes_created,
            result_previews_created: parser.result_previews_created,
            result_touches_created: parser.result_touches_created,
            result_fts_created: parser.result_fts_created,
            result_handoffs_created: parser.result_handoffs_created,
            retained_messages: parser.retained_messages,
            retained_summaries: parser.retained_summaries,
            retained_notices: parser.retained_notices,
            retained_tool_calls: parser.retained_tool_calls,
            retained_body_bytes: parser.retained_body_bytes,
            max_line_buffer_bytes: parser.max_line_buffer_bytes,
            publication_pages: parser.publication_pages,
            nativepath_publication_rows: parser.nativepath_publication_rows,
            publication_serialized_bytes: parser.publication_serialized_bytes,
            max_publication_page_rows: parser.max_publication_page_rows,
            max_publication_page_bytes: parser.max_publication_page_bytes,
        }
    }

    fn merge_retry(&mut self, retry: CursorParserStats) {
        self.source_opens = self.source_opens.saturating_add(1);
        self.full_rescans_after_prefix_mismatch =
            self.full_rescans_after_prefix_mismatch.saturating_add(1);
        self.bytes_read = self.bytes_read.saturating_add(retry.bytes_read);
        self.verification_bytes_read = self
            .verification_bytes_read
            .saturating_add(retry.verification_bytes_read);
        self.projected_bytes_read = retry.projected_bytes_read;
        self.complete_records = retry.complete_records;
        self.malformed_records = retry.malformed_records;
        self.oversized_records = retry.oversized_records;
        self.incomplete_tail_records = retry.incomplete_tail_records;
        self.native_result_records = retry.native_result_records;
        self.native_result_bytes = retry.native_result_bytes;
        self.retained_messages = retry.retained_messages;
        self.retained_summaries = retry.retained_summaries;
        self.retained_notices = retry.retained_notices;
        self.retained_tool_calls = retry.retained_tool_calls;
        self.retained_body_bytes = retry.retained_body_bytes;
        self.max_line_buffer_bytes = self.max_line_buffer_bytes.max(retry.max_line_buffer_bytes);
        self.publication_pages = retry.publication_pages;
        self.nativepath_publication_rows = retry.nativepath_publication_rows;
        self.publication_serialized_bytes = retry.publication_serialized_bytes;
        self.max_publication_page_rows = retry.max_publication_page_rows;
        self.max_publication_page_bytes = retry.max_publication_page_bytes;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CursorSourceGeneration {
    pub(crate) observation: CursorSourceObservation,
    pub(crate) session: Option<CursorNativeSession>,
    pub(crate) mutation: CursorSourceMutation,
    #[cfg(test)]
    pub(crate) events: Vec<CursorNativeEvent>,
    pub(crate) rejections: CursorSourceRejections,
    pub(crate) checkpoint: CursorCheckpoint,
    pub(crate) stats: CursorGenerationStats,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CursorUnchangedSource {
    pub(crate) observation: CursorSourceObservation,
    pub(crate) checkpoint: CursorCheckpoint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CursorReadOutcome {
    Unchanged(Box<CursorUnchangedSource>),
    Generation(Box<CursorSourceGeneration>),
}

pub(crate) fn scan_cursor_source_into(
    frozen: &CursorFrozenSource,
    prior: Option<&CursorPriorObservation>,
    sink: &mut dyn CursorPublicationSink,
) -> Result<CursorReadOutcome> {
    sink.begin_cursor_publication()?;
    let result = scan_cursor_source_into_transaction(frozen, prior, sink);
    match result {
        Ok(outcome) => match sink.commit_cursor_publication() {
            Ok(()) => Ok(outcome),
            Err(error) => {
                sink.abort_cursor_publication();
                Err(error)
            }
        },
        Err(error) => {
            sink.abort_cursor_publication();
            Err(error)
        }
    }
}

fn scan_cursor_source_into_transaction(
    frozen: &CursorFrozenSource,
    prior: Option<&CursorPriorObservation>,
    sink: &mut dyn CursorPublicationSink,
) -> Result<CursorReadOutcome> {
    let mutation = cursor_source_mutation(frozen.observation(), prior);
    if mutation == CursorSourceMutation::ExactReplay {
        if let Some(prior) = prior.filter(|prior| prior.checkpoint.is_supported()) {
            return Ok(CursorReadOutcome::Unchanged(Box::new(
                CursorUnchangedSource {
                    observation: frozen.observation.clone(),
                    checkpoint: prior.checkpoint.clone(),
                },
            )));
        }
    }
    let resume_checkpoint = prior
        .filter(|_| mutation == CursorSourceMutation::AppendCandidate)
        .map(|prior| &prior.checkpoint)
        .filter(|checkpoint| {
            checkpoint.is_supported()
                && checkpoint.disposition == CursorCheckpointDisposition::Publish
        });
    let file = frozen.open()?;
    let mut reader = BufReader::new(file);
    let plan = resume_checkpoint.map_or(CursorParserPlan::FullSnapshot, |checkpoint| {
        CursorParserPlan::VerifyPrefixAndResume(checkpoint)
    });
    let first = scan_cursor_reader(&mut reader, plan, sink)?;
    let (parsed, mut stats, resumed) = match first {
        CursorParserOutcome::Parsed(parsed) => {
            let parsed = *parsed;
            let stats = CursorGenerationStats::from_parser(parsed.stats.clone());
            let resumed = parsed.resumed;
            (parsed, stats, resumed)
        }
        CursorParserOutcome::PrefixMismatch(first_stats) => {
            let first_stats = *first_stats;
            let file = frozen.open()?;
            let mut reader = BufReader::new(file);
            let CursorParserOutcome::Parsed(parsed) =
                scan_cursor_reader(&mut reader, CursorParserPlan::FullSnapshot, sink)?
            else {
                return Err(CaptureError::SystemInvariant(
                    "Cursor full snapshot unexpectedly reported a prefix mismatch",
                ));
            };
            let parsed = *parsed;
            let mut stats = CursorGenerationStats::from_parser(first_stats);
            stats.merge_retry(parsed.stats.clone());
            (parsed, stats, false)
        }
    };
    frozen.revalidate()?;
    if !resumed && mutation == CursorSourceMutation::AppendCandidate {
        stats.full_rescans_after_prefix_mismatch = stats.full_rescans_after_prefix_mismatch.max(1);
    }
    let has_retained_events = parsed.stats.retained_messages > 0
        || parsed.stats.retained_summaries > 0
        || parsed.stats.retained_notices > 0
        || parsed.stats.retained_tool_calls > 0;
    let session = (has_retained_events
        || parsed.checkpoint.session.started_at.is_some()
        || parsed.checkpoint.session.title.is_some())
    .then(|| CursorNativeSession {
        native_session_id: frozen.observation.native_session_id.clone(),
        project: frozen.transcript.project().to_path_buf(),
        started_at: parsed.checkpoint.session.started_at,
        ended_at: parsed.checkpoint.session.ended_at,
        title: parsed.checkpoint.session.title.clone(),
    });
    Ok(CursorReadOutcome::Generation(Box::new(
        CursorSourceGeneration {
            observation: frozen.observation.clone(),
            session,
            mutation: if !resumed && mutation == CursorSourceMutation::AppendCandidate {
                CursorSourceMutation::Rewrite
            } else {
                mutation
            },
            #[cfg(test)]
            events: parsed.events,
            rejections: parsed.rejections.into(),
            checkpoint: parsed.checkpoint,
            stats,
        },
    )))
}

#[cfg(test)]
struct CursorCollectingPublicationSink {
    events: Vec<CursorNativeEvent>,
}

#[cfg(test)]
impl CursorPublicationSink for CursorCollectingPublicationSink {
    fn begin_cursor_publication(&mut self) -> Result<()> {
        self.events.clear();
        Ok(())
    }

    fn stage_cursor_page(&mut self, page: CursorPublicationPage) -> Result<()> {
        self.events.extend(page.events);
        Ok(())
    }

    fn abort_cursor_publication(&mut self) {
        self.events.clear();
    }

    fn commit_cursor_publication(&mut self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
pub(crate) fn scan_cursor_source(
    frozen: &CursorFrozenSource,
    prior: Option<&CursorPriorObservation>,
) -> Result<CursorReadOutcome> {
    let mut sink = CursorCollectingPublicationSink { events: Vec::new() };
    let outcome = scan_cursor_source_into(frozen, prior, &mut sink)?;
    match outcome {
        CursorReadOutcome::Generation(mut generation) => {
            // Preserve the historical test helper contract. The production
            // source path hands pages directly to its sink instead.
            generation.events = sink.events;
            Ok(CursorReadOutcome::Generation(generation))
        }
        unchanged => Ok(unchanged),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CursorKnownSource {
    pub(crate) canonical_source_key: String,
    pub(crate) locator_identity: String,
    pub(crate) native_session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CursorCompletedExactInventory {
    live_sources: BTreeSet<(String, String)>,
}

impl CursorCompletedExactInventory {
    pub(crate) fn from_discovery(
        inventory: &CursorRootInventory,
        observations: &[CursorSourceObservation],
    ) -> Option<Self> {
        let root_metadata = fs::symlink_metadata(&inventory.input).ok()?;
        if !inventory.completed
            || inventory.input.file_name() != Some(std::ffi::OsStr::new("projects"))
            || !root_metadata.file_type().is_dir()
            || crate::common::io::provider_metadata_is_link_like(&root_metadata)
            || inventory
                .transcripts
                .iter()
                .any(|transcript| transcript.projects_root() != inventory.input)
            || inventory.transcripts.len() != observations.len()
        {
            return None;
        }
        let mut expected = BTreeSet::new();
        for transcript in &inventory.transcripts {
            let path = fs::canonicalize(transcript.path()).ok()?;
            let locator_identity = provider_path_identity(&path).ok()?;
            expected.insert((locator_identity, transcript.native_session_id().to_owned()));
        }
        let observed = observations
            .iter()
            .map(|observation| {
                (
                    observation.locator_identity.clone(),
                    observation.native_session_id.clone(),
                )
            })
            .collect::<BTreeSet<_>>();
        (expected == observed && observed.len() == observations.len()).then_some(Self {
            live_sources: observed,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CursorMissingSourceDisposition {
    RetainWithoutCompletedInventory {
        canonical_source_key: String,
    },
    Present {
        canonical_source_key: String,
    },
    RouteUnavailableCandidate {
        canonical_source_key: String,
        locator_identity: String,
    },
}

pub(crate) fn resolve_cursor_missing_sources(
    known: &[CursorKnownSource],
    inventory: Option<&CursorCompletedExactInventory>,
) -> Vec<CursorMissingSourceDisposition> {
    known
        .iter()
        .map(|source| {
            let Some(inventory) = inventory else {
                return CursorMissingSourceDisposition::RetainWithoutCompletedInventory {
                    canonical_source_key: source.canonical_source_key.clone(),
                };
            };
            if inventory.live_sources.contains(&(
                source.locator_identity.clone(),
                source.native_session_id.clone(),
            )) {
                CursorMissingSourceDisposition::Present {
                    canonical_source_key: source.canonical_source_key.clone(),
                }
            } else {
                CursorMissingSourceDisposition::RouteUnavailableCandidate {
                    canonical_source_key: source.canonical_source_key.clone(),
                    locator_identity: source.locator_identity.clone(),
                }
            }
        })
        .collect()
}
