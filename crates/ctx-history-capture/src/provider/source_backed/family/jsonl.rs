use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    common::io::OpenedProviderSourceFile, CaptureError, Result, MAX_PROVIDER_JSONL_LINE_BYTES,
};

mod checkpoint;
mod identity;
mod revalidation;
mod route;

pub(crate) use checkpoint::{
    bounded_checkpoint_fits, decode_bounded_checkpoint, encode_bounded_checkpoint,
};
use identity::observe_metadata;
use revalidation::hash_prefix;
#[cfg(test)]
pub(crate) use revalidation::{
    jsonl_prefix_hash_bytes, reset_jsonl_prefix_hash_bytes, set_after_final_jsonl_prefix_hash_hook,
    set_after_jsonl_prefix_hash_hook, set_after_second_jsonl_prefix_hash_hook,
};
pub(crate) use revalidation::{
    observe_opened_file, revalidate_frozen_prefix, revalidate_frozen_prefix_sha256,
};
pub(crate) use route::{
    jsonl_family_driver, JsonlFamilyAdapter, JsonlFamilyAppendMode, JsonlFamilyBaseScope,
    JsonlFamilyInventory, JsonlFamilyInventoryMode, JsonlFamilyLeaf,
    JsonlFamilyMembershipObservation, JsonlFamilyOptimizedLeafOutcome, JsonlFamilyProjectionMode,
    JsonlFamilyProjector, JsonlFamilyPublication, JsonlFamilyRejectedLeaf,
    JsonlFamilyRootMissingMode, JsonlFamilyTerminalProof, JsonlFamilyWorkerContext,
};
const PREFIX_HASH_DOMAIN: &[u8] = b"ctx-direct-jsonl-nativepath-prefix-v1\0";
const PAGE_MAX_RECORDS: usize = 64;
const PAGE_MAX_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct JsonlSourceIdentity {
    provider: String,
    parser_revision: String,
    policy_revision: String,
    source_descriptor_digest: [u8; 32],
    source_path: PathBuf,
}

impl JsonlSourceIdentity {
    pub(crate) fn new(
        provider: impl Into<String>,
        parser_revision: impl Into<String>,
        policy_revision: impl Into<String>,
        source_descriptor_digest: [u8; 32],
        source_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            provider: provider.into(),
            parser_revision: parser_revision.into(),
            policy_revision: policy_revision.into(),
            source_descriptor_digest,
            source_path: source_path.into(),
        }
    }

    pub(crate) fn source_descriptor_digest(&self) -> &[u8; 32] {
        &self.source_descriptor_digest
    }

    pub(crate) fn source_path(&self) -> &PathBuf {
        &self.source_path
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct JsonlObservedTime {
    before_epoch: bool,
    seconds: u64,
    nanos: u32,
}

impl JsonlObservedTime {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct JsonlFileObservation {
    length: u64,
    modified: JsonlObservedTime,
    readonly: bool,
    stable_identity: Option<[u8; 32]>,
    change_identity: Option<[u8; 32]>,
}

impl JsonlFileObservation {
    fn same_stable_file(&self, current: &Self) -> bool {
        match (self.stable_identity, current.stable_identity) {
            (Some(previous), Some(current)) => previous == current,
            _ => false,
        }
    }

    pub(crate) fn supports_exact_revalidation(&self) -> bool {
        self.stable_identity.is_some() && self.change_identity.is_some()
    }

    /// Whether `current` can still contain the exact frozen bytes represented
    /// by this observation. Content is not trusted until the caller separately
    /// verifies its certified prefix digest.
    pub(crate) fn admits_frozen_prefix_in(&self, current: &Self) -> bool {
        self == current
            || (current.length >= self.length
                && self.supports_exact_revalidation()
                && self.same_stable_file(current))
    }
}

#[derive(Debug, Clone)]
pub(crate) struct JsonlProbe {
    observation: JsonlFileObservation,
    prefix_hasher: Sha256,
    complete_prefix_end: u64,
    next_physical_ordinal: u64,
}

impl JsonlProbe {
    pub(crate) fn next_physical_ordinal(&self) -> u64 {
        self.next_physical_ordinal
    }

    pub(crate) fn observation(&self) -> &JsonlFileObservation {
        &self.observation
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct JsonlCheckpoint {
    version: u32,
    identity: JsonlSourceIdentity,
    source_observation: JsonlFileObservation,
    complete_prefix_end: u64,
    complete_prefix_sha256: [u8; 32],
    next_physical_ordinal: u64,
    terminal: bool,
}

impl JsonlCheckpoint {
    const VERSION: u32 = 1;

    pub(crate) fn identity(&self) -> &JsonlSourceIdentity {
        &self.identity
    }

    pub(crate) fn source_observation(&self) -> &JsonlFileObservation {
        &self.source_observation
    }

    pub(crate) fn complete_prefix_end(&self) -> u64 {
        self.complete_prefix_end
    }

    pub(crate) fn complete_prefix_sha256(&self) -> &[u8; 32] {
        &self.complete_prefix_sha256
    }

    pub(crate) fn next_physical_ordinal(&self) -> u64 {
        self.next_physical_ordinal
    }

    pub(crate) fn terminal(&self) -> bool {
        self.terminal
    }

    pub(crate) fn is_internally_consistent(&self) -> bool {
        let empty_prefix = self.complete_prefix_end == 0;
        let empty_prefix_is_exact = self.next_physical_ordinal == 0
            && self.complete_prefix_sha256 == prefix_digest(&new_prefix_hasher());
        let nonempty_prefix_is_possible = self.next_physical_ordinal > 0
            && self.next_physical_ordinal <= self.complete_prefix_end;
        self.version == Self::VERSION
            && self.complete_prefix_end <= self.source_observation.length
            && if empty_prefix {
                empty_prefix_is_exact
            } else {
                nonempty_prefix_is_possible
            }
            && (!self.terminal || self.complete_prefix_end == self.source_observation.length)
    }

    fn supports(&self, identity: &JsonlSourceIdentity) -> bool {
        self.is_internally_consistent() && self.identity == *identity
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JsonlSourceChange {
    Cold,
    Unchanged,
    Append,
    Replace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JsonlOversizedRecordPolicy {
    RejectSource,
    RejectRecord,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct JsonlRecordEvidence {
    physical_ordinal: u64,
    byte_start: u64,
    byte_end_exclusive: u64,
    record_digest: [u8; 32],
}

impl JsonlRecordEvidence {
    pub(crate) fn physical_ordinal(self) -> u64 {
        self.physical_ordinal
    }

    pub(crate) fn byte_start(self) -> u64 {
        self.byte_start
    }

    pub(crate) fn byte_end_exclusive(self) -> u64 {
        self.byte_end_exclusive
    }

    pub(crate) fn record_digest(self) -> [u8; 32] {
        self.record_digest
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct JsonlRecordRef<'record> {
    bytes: &'record [u8],
    evidence: JsonlRecordEvidence,
    oversized: bool,
}

impl<'record> JsonlRecordRef<'record> {
    #[cfg(test)]
    pub(crate) fn for_test(bytes: &'record [u8], physical_ordinal: u64) -> Self {
        Self {
            bytes,
            evidence: JsonlRecordEvidence {
                physical_ordinal,
                byte_start: 0,
                byte_end_exclusive: bytes.len() as u64,
                record_digest: Sha256::digest(bytes).into(),
            },
            oversized: false,
        }
    }

    pub(crate) fn bytes(self) -> &'record [u8] {
        self.bytes
    }

    pub(crate) fn evidence(self) -> JsonlRecordEvidence {
        self.evidence
    }

    pub(crate) fn oversized(self) -> bool {
        self.oversized
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct JsonlPage;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JsonlScanOutcome {
    checkpoint: JsonlCheckpoint,
}

impl JsonlScanOutcome {
    pub(crate) fn checkpoint(&self) -> &JsonlCheckpoint {
        &self.checkpoint
    }
}

pub(crate) struct JsonlReader {
    identity: JsonlSourceIdentity,
    observation: JsonlFileObservation,
    source_file: Arc<OpenedProviderSourceFile>,
    reader: BufReader<File>,
    prefix_hasher: Sha256,
    complete_prefix_end: u64,
    next_physical_ordinal: u64,
    source_change: JsonlSourceChange,
    skip_scan: bool,
    unchanged_checkpoint: Option<JsonlCheckpoint>,
    finished: bool,
    outcome: Option<JsonlScanOutcome>,
    record_buffer: Vec<u8>,
    whole_record: bool,
    append_log: bool,
    oversized_record_policy: JsonlOversizedRecordPolicy,
}

impl JsonlReader {
    pub(crate) fn open(
        identity: JsonlSourceIdentity,
        source_file: Arc<OpenedProviderSourceFile>,
        previous: Option<&JsonlCheckpoint>,
        probe: Option<JsonlProbe>,
    ) -> Result<Self> {
        Self::open_with_framing(identity, source_file, previous, probe, false)
    }

    pub(crate) fn open_whole_record(
        identity: JsonlSourceIdentity,
        source_file: Arc<OpenedProviderSourceFile>,
        previous: Option<&JsonlCheckpoint>,
    ) -> Result<Self> {
        Self::open_with_framing(identity, source_file, previous, None, true)
    }

    fn open_with_framing(
        identity: JsonlSourceIdentity,
        source_file: Arc<OpenedProviderSourceFile>,
        previous: Option<&JsonlCheckpoint>,
        probe: Option<JsonlProbe>,
        whole_record: bool,
    ) -> Result<Self> {
        source_file.revalidate_same_object()?;
        let current_metadata = source_file.file().metadata()?;
        let observation = observe_metadata(
            identity.source_path(),
            source_file.file(),
            &current_metadata,
        )?;
        let mut file = source_file.reopen_same_object()?;
        if observe_metadata(identity.source_path(), &file, &file.metadata()?)? != observation {
            return Err(CaptureError::SourceChangedDuringCapture);
        }

        let mut prefix_hasher = new_prefix_hasher();
        let mut complete_prefix_end = 0_u64;
        let mut next_physical_ordinal = 0_u64;
        let mut source_change = if previous.is_some() {
            JsonlSourceChange::Replace
        } else {
            JsonlSourceChange::Cold
        };
        let mut skip_scan = false;
        let mut unchanged_checkpoint = None;

        if let Some(previous) = previous.filter(|checkpoint| checkpoint.supports(&identity)) {
            let previous_observation = previous.source_observation();
            let same_file = previous_observation.same_stable_file(&observation);
            if same_file
                && previous_observation.supports_exact_revalidation()
                && previous_observation == &observation
                && previous.terminal()
            {
                complete_prefix_end = previous.complete_prefix_end();
                next_physical_ordinal = previous.next_physical_ordinal();
                source_change = JsonlSourceChange::Unchanged;
                skip_scan = true;
                unchanged_checkpoint = Some(previous.clone());
            } else if same_file && observation.length >= previous.complete_prefix_end() {
                let observed_prefix = hash_prefix(
                    &mut file,
                    previous.complete_prefix_end(),
                    new_prefix_hasher(),
                )?;
                if prefix_digest(&observed_prefix) == *previous.complete_prefix_sha256() {
                    prefix_hasher = observed_prefix;
                    complete_prefix_end = previous.complete_prefix_end();
                    next_physical_ordinal = previous.next_physical_ordinal();
                    if previous.terminal() && observation.length == previous.complete_prefix_end() {
                        source_change = JsonlSourceChange::Unchanged;
                        skip_scan = true;
                        unchanged_checkpoint = Some(previous.clone());
                    } else {
                        source_change = JsonlSourceChange::Append;
                    }
                }
            }
        }

        if matches!(
            source_change,
            JsonlSourceChange::Cold | JsonlSourceChange::Replace
        ) {
            if let Some(probe) = probe {
                if probe.observation != observation {
                    if !probe.observation.admits_frozen_prefix_in(&observation) {
                        return Err(CaptureError::SourceChangedDuringCapture);
                    }
                    revalidate_frozen_prefix(
                        identity.source_path(),
                        source_file.as_ref(),
                        &probe.observation,
                        probe.complete_prefix_end,
                        prefix_digest(&probe.prefix_hasher),
                    )?;
                }
                prefix_hasher = probe.prefix_hasher;
                complete_prefix_end = probe.complete_prefix_end;
                next_physical_ordinal = probe.next_physical_ordinal;
            }
        }
        file.seek(SeekFrom::Start(complete_prefix_end))?;
        Ok(Self {
            identity,
            observation,
            source_file,
            reader: BufReader::new(file),
            prefix_hasher,
            complete_prefix_end,
            next_physical_ordinal,
            source_change,
            skip_scan,
            unchanged_checkpoint,
            finished: false,
            outcome: None,
            record_buffer: Vec::new(),
            whole_record,
            append_log: !whole_record,
            oversized_record_policy: JsonlOversizedRecordPolicy::RejectSource,
        })
    }

    pub(crate) fn set_oversized_record_policy(&mut self, policy: JsonlOversizedRecordPolicy) {
        self.oversized_record_policy = policy;
    }

    pub(crate) fn source_change(&self) -> JsonlSourceChange {
        self.source_change
    }

    pub(crate) fn outcome(&self) -> Option<&JsonlScanOutcome> {
        self.outcome.as_ref()
    }

    pub(crate) fn visit_page<E>(
        &mut self,
        visit: &mut impl FnMut(JsonlRecordRef<'_>) -> std::result::Result<(), E>,
    ) -> std::result::Result<Option<JsonlPage>, E>
    where
        E: From<CaptureError>,
    {
        if self.finished {
            return Ok(None);
        }
        if self.skip_scan {
            self.finish(true).map_err(E::from)?;
            return Ok(None);
        }
        if self.whole_record {
            return self.visit_whole_record(visit);
        }

        let mut records = 0_usize;
        let mut page_bytes = 0_usize;
        while records < PAGE_MAX_RECORDS {
            let start = self.complete_prefix_end;
            let ordinal = self.next_physical_ordinal;
            let hasher_before = self.prefix_hasher.clone();
            let line = read_bounded_line(
                &mut self.reader,
                &mut self.record_buffer,
                &mut self.prefix_hasher,
                self.observation.length,
                start,
            )
            .map_err(E::from)?;
            let (end, record_digest, wire_bytes, oversized) = match line {
                RawLine::Complete {
                    end,
                    record_digest,
                    wire_bytes,
                } => (end, record_digest, wire_bytes, false),
                RawLine::Oversized {
                    end,
                    record_digest,
                    wire_bytes,
                } if self.oversized_record_policy == JsonlOversizedRecordPolicy::RejectRecord => {
                    (end, record_digest, wire_bytes, true)
                }
                RawLine::Oversized { .. } => {
                    return Err(E::from(CaptureError::InvalidPayload(format!(
                        "{}:{} exceeds the {} byte JSONL record limit",
                        self.identity.source_path.display(),
                        ordinal.saturating_add(1),
                        MAX_PROVIDER_JSONL_LINE_BYTES
                    ))));
                }
                RawLine::EndOfFile => {
                    self.finish(true).map_err(E::from)?;
                    break;
                }
                RawLine::IncompleteTail => {
                    self.prefix_hasher = hasher_before;
                    self.reader
                        .seek(SeekFrom::Start(start))
                        .map_err(CaptureError::from)
                        .map_err(E::from)?;
                    self.finish(false).map_err(E::from)?;
                    break;
                }
            };

            if records != 0 && page_bytes.saturating_add(wire_bytes) > PAGE_MAX_BYTES {
                self.prefix_hasher = hasher_before;
                self.reader
                    .seek(SeekFrom::Start(start))
                    .map_err(CaptureError::from)
                    .map_err(E::from)?;
                break;
            }

            let evidence = JsonlRecordEvidence {
                physical_ordinal: ordinal,
                byte_start: start,
                byte_end_exclusive: end,
                record_digest,
            };
            visit(JsonlRecordRef {
                bytes: &self.record_buffer,
                evidence,
                oversized,
            })?;
            self.complete_prefix_end = end;
            self.next_physical_ordinal = self.next_physical_ordinal.saturating_add(1);
            records = records.saturating_add(1);
            page_bytes = page_bytes.saturating_add(wire_bytes);
        }

        if records == 0 {
            return Ok(None);
        }
        Ok(Some(JsonlPage))
    }

    fn visit_whole_record<E>(
        &mut self,
        visit: &mut impl FnMut(JsonlRecordRef<'_>) -> std::result::Result<(), E>,
    ) -> std::result::Result<Option<JsonlPage>, E>
    where
        E: From<CaptureError>,
    {
        if self.complete_prefix_end != 0 || self.next_physical_ordinal != 0 {
            return Err(E::from(CaptureError::InvalidPayload(
                "whole-record JSON source has a non-empty scan frontier".to_owned(),
            )));
        }
        if self.observation.length == 0 {
            self.finish(true).map_err(E::from)?;
            return Ok(None);
        }
        let length = usize::try_from(self.observation.length).map_err(|_| {
            E::from(CaptureError::InvalidPayload(
                "whole-record JSON source exceeds platform limits".to_owned(),
            ))
        })?;
        if length > MAX_PROVIDER_JSONL_LINE_BYTES {
            return Err(E::from(CaptureError::InvalidPayload(format!(
                "{} exceeds the {} byte whole-record JSON limit",
                self.identity.source_path.display(),
                MAX_PROVIDER_JSONL_LINE_BYTES
            ))));
        }
        self.record_buffer.resize(length, 0);
        self.reader
            .read_exact(&mut self.record_buffer)
            .map_err(CaptureError::from)
            .map_err(E::from)?;
        self.prefix_hasher.update(&self.record_buffer);
        let evidence = JsonlRecordEvidence {
            physical_ordinal: 0,
            byte_start: 0,
            byte_end_exclusive: self.observation.length,
            record_digest: Sha256::digest(&self.record_buffer).into(),
        };
        visit(JsonlRecordRef {
            bytes: &self.record_buffer,
            evidence,
            oversized: false,
        })?;
        self.complete_prefix_end = self.observation.length;
        self.next_physical_ordinal = 1;
        self.finish(true).map_err(E::from)?;
        Ok(Some(JsonlPage))
    }

    fn checkpoint(&self, terminal: bool) -> JsonlCheckpoint {
        JsonlCheckpoint {
            version: JsonlCheckpoint::VERSION,
            identity: self.identity.clone(),
            source_observation: self.observation.clone(),
            complete_prefix_end: self.complete_prefix_end,
            complete_prefix_sha256: prefix_digest(&self.prefix_hasher),
            next_physical_ordinal: self.next_physical_ordinal,
            terminal,
        }
    }

    fn finish(&mut self, terminal: bool) -> Result<()> {
        let current = observe_metadata(
            self.identity.source_path(),
            self.reader.get_ref(),
            &self.reader.get_ref().metadata()?,
        )?;
        if current == self.observation {
            if self.append_log {
                // The retained authority may have been opened before an
                // identity probe observed a legitimate append. The scan is
                // bound to `self.observation`, so require that exact
                // observation plus same-object routing rather than the
                // authority handle's older, metadata-sensitive stamp.
                self.source_file.revalidate_same_object()?;
            } else {
                self.source_file.revalidate()?;
            }
        } else {
            if !self.append_log {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            revalidate_frozen_prefix(
                self.identity.source_path(),
                self.source_file.as_ref(),
                &self.observation,
                self.complete_prefix_end,
                prefix_digest(&self.prefix_hasher),
            )?;
        }
        self.outcome = Some(JsonlScanOutcome {
            checkpoint: self
                .unchanged_checkpoint
                .clone()
                .unwrap_or_else(|| self.checkpoint(terminal)),
        });
        self.finished = true;
        Ok(())
    }
}

/// Projects the first complete physical record and returns its prefix state.
///
/// Cold and replacement scans resume after this record, so the provider parser
/// sees every physical record at most once. Append and unchanged scans discard
/// the probe state after binding identity.
pub(crate) fn probe_first_record<T, E>(
    source_path: &Path,
    source_file: &Arc<OpenedProviderSourceFile>,
    visit: impl FnOnce(JsonlRecordRef<'_>) -> std::result::Result<T, E>,
) -> std::result::Result<(T, JsonlProbe), E>
where
    E: From<CaptureError>,
{
    let mut visit = Some(visit);
    probe_records_until(source_path, source_file, 1, |record| {
        visit.take().ok_or_else(|| {
            E::from(CaptureError::SystemInvariant(
                "provider identity probe visited more than one record",
            ))
        })?(record)
        .map(Some)
    })?
    .ok_or_else(|| {
        E::from(CaptureError::InvalidPayload(
            "provider identity record is missing or incomplete".to_owned(),
        ))
    })
}

pub(crate) fn probe_records_until<T, E>(
    source_path: &Path,
    source_file: &Arc<OpenedProviderSourceFile>,
    max_records: usize,
    mut visit: impl FnMut(JsonlRecordRef<'_>) -> std::result::Result<Option<T>, E>,
) -> std::result::Result<Option<(T, JsonlProbe)>, E>
where
    E: From<CaptureError>,
{
    if max_records == 0 || max_records > PAGE_MAX_RECORDS {
        return Err(E::from(CaptureError::SystemInvariant(
            "provider identity probe record bound is invalid",
        )));
    }
    source_file.revalidate_same_object().map_err(E::from)?;
    let observation = observe_metadata(
        source_path,
        source_file.file(),
        &source_file
            .file()
            .metadata()
            .map_err(CaptureError::from)
            .map_err(E::from)?,
    )
    .map_err(E::from)?;
    let mut file = source_file.reopen_same_object().map_err(E::from)?;
    file.seek(SeekFrom::Start(0))
        .map_err(CaptureError::from)
        .map_err(E::from)?;
    let mut reader = BufReader::new(file);
    let mut hasher = new_prefix_hasher();
    let mut buffer = Vec::new();
    let mut start = 0_u64;
    for ordinal in 0..max_records {
        let (end, record_digest, _wire_bytes) = match read_bounded_line(
            &mut reader,
            &mut buffer,
            &mut hasher,
            observation.length,
            start,
        )
        .map_err(E::from)?
        {
            RawLine::Complete {
                end,
                record_digest,
                wire_bytes,
            } => (end, record_digest, wire_bytes),
            RawLine::EndOfFile | RawLine::IncompleteTail => break,
            RawLine::Oversized { .. } => {
                return Err(E::from(CaptureError::InvalidPayload(format!(
                    "provider identity record exceeds the {} byte JSONL record limit",
                    MAX_PROVIDER_JSONL_LINE_BYTES
                ))));
            }
        };
        let physical_ordinal = u64::try_from(ordinal).map_err(|_| {
            E::from(CaptureError::SystemInvariant(
                "provider identity probe ordinal exceeds u64",
            ))
        })?;
        if let Some(value) = visit(JsonlRecordRef {
            bytes: &buffer,
            evidence: JsonlRecordEvidence {
                physical_ordinal,
                byte_start: start,
                byte_end_exclusive: end,
                record_digest,
            },
            oversized: false,
        })? {
            let closing = revalidate_frozen_prefix(
                source_path,
                source_file.as_ref(),
                &observation,
                end,
                prefix_digest(&hasher),
            )
            .map_err(E::from)?;
            return Ok(Some((
                value,
                JsonlProbe {
                    observation: closing,
                    prefix_hasher: hasher,
                    complete_prefix_end: end,
                    next_physical_ordinal: physical_ordinal.saturating_add(1),
                },
            )));
        }
        start = end;
    }
    revalidate_frozen_prefix(
        source_path,
        source_file.as_ref(),
        &observation,
        start,
        prefix_digest(&hasher),
    )
    .map_err(E::from)?;
    Ok(None)
}

enum RawLine {
    EndOfFile,
    IncompleteTail,
    Oversized {
        end: u64,
        record_digest: [u8; 32],
        wire_bytes: usize,
    },
    Complete {
        end: u64,
        record_digest: [u8; 32],
        wire_bytes: usize,
    },
}

fn read_bounded_line(
    reader: &mut BufReader<File>,
    bytes: &mut Vec<u8>,
    hasher: &mut Sha256,
    frozen_length: u64,
    start: u64,
) -> Result<RawLine> {
    bytes.clear();
    if start >= frozen_length {
        return Ok(RawLine::EndOfFile);
    }
    let mut total = 0_u64;
    let mut oversized = false;
    let mut record_hasher = Sha256::new();
    loop {
        let remaining = frozen_length.saturating_sub(start.saturating_add(total));
        if remaining == 0 {
            return Ok(RawLine::IncompleteTail);
        }
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let available_len = usize::try_from(remaining.min(available.len() as u64))
            .map_err(|_| CaptureError::SystemInvariant("JSONL read bound exceeds usize"))?;
        let available = &available[..available_len];
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index.saturating_add(1));
        let chunk = &available[..take];
        hasher.update(chunk);
        record_hasher.update(chunk);
        total = total.saturating_add(chunk.len() as u64);
        if !oversized {
            if bytes.len().saturating_add(chunk.len())
                > MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(2)
            {
                oversized = true;
                bytes.clear();
            } else {
                bytes.extend_from_slice(chunk);
            }
        }
        let complete = chunk.last() == Some(&b'\n');
        reader.consume(take);
        if complete {
            let end = start
                .checked_add(total)
                .ok_or(CaptureError::SystemInvariant(
                    "JSONL byte offset overflowed",
                ))?;
            if oversized {
                return Ok(RawLine::Oversized {
                    end,
                    record_digest: record_hasher.finalize().into(),
                    wire_bytes: usize::try_from(total).unwrap_or(usize::MAX),
                });
            }
            let record_digest = record_hasher.finalize().into();
            let wire_bytes = bytes.len();
            if bytes.last() == Some(&b'\n') {
                bytes.pop();
                if bytes.last() == Some(&b'\r') {
                    bytes.pop();
                }
            }
            if bytes.len() > MAX_PROVIDER_JSONL_LINE_BYTES {
                bytes.clear();
                return Ok(RawLine::Oversized {
                    end,
                    record_digest,
                    wire_bytes,
                });
            }
            return Ok(RawLine::Complete {
                end,
                record_digest,
                wire_bytes,
            });
        }
    }
}

fn new_prefix_hasher() -> Sha256 {
    let mut hasher = Sha256::new();
    hasher.update(PREFIX_HASH_DOMAIN);
    hasher
}

fn prefix_digest(hasher: &Sha256) -> [u8; 32] {
    hasher.clone().finalize().into()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::common::io::open_provider_source_file;

    fn drain(reader: &mut JsonlReader) -> Result<Vec<Vec<u8>>> {
        let mut records = Vec::new();
        while reader
            .visit_page(&mut |record| -> Result<()> {
                records.push(record.bytes().to_vec());
                Ok(())
            })?
            .is_some()
        {}
        Ok(records)
    }

    #[test]
    fn readers_opened_from_one_retained_source_drain_independently() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let source_path = temp.path().join("source.jsonl");
        fs::write(
            &source_path,
            b"{\"message\":\"first\"}\n{\"message\":\"second\"}\n",
        )
        .unwrap();
        let source_file = Arc::new(open_provider_source_file(&source_path).unwrap());
        let identity = JsonlSourceIdentity::new(
            "test",
            "independent-reader-v1",
            "independent-reader-policy-v1",
            [7; 32],
            source_path,
        );
        let mut first =
            JsonlReader::open(identity.clone(), Arc::clone(&source_file), None, None).unwrap();
        let mut second = JsonlReader::open(identity, source_file, None, None).unwrap();
        let expected = vec![
            br#"{"message":"first"}"#.to_vec(),
            br#"{"message":"second"}"#.to_vec(),
        ];

        assert_eq!(drain(&mut first).unwrap(), expected);
        assert_eq!(drain(&mut second).unwrap(), expected);
    }
}
