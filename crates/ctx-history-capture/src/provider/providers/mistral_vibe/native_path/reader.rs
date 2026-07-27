use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ObservedTime {
    pub(super) before_epoch: bool,
    pub(super) seconds: u64,
    pub(super) nanos: u32,
}

impl ObservedTime {
    pub(super) fn from_system_time(value: SystemTime) -> Self {
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
pub(super) struct FileStamp {
    pub(super) length: u64,
    pub(super) modified: ObservedTime,
    pub(super) readonly: bool,
    pub(super) device: Option<u64>,
    pub(super) inode: Option<u64>,
}

impl FileStamp {
    pub(super) fn from_metadata(metadata: &Metadata) -> Result<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        #[cfg(unix)]
        let (device, inode) = (Some(metadata.dev()), Some(metadata.ino()));
        #[cfg(not(unix))]
        let (device, inode) = (None, None);

        Ok(Self {
            length: metadata.len(),
            modified: ObservedTime::from_system_time(metadata.modified()?),
            readonly: metadata.permissions().readonly(),
            device,
            inode,
        })
    }

    pub(super) fn same_physical_file(&self, current: &Self) -> bool {
        match (self.device, self.inode, current.device, current.inode) {
            (Some(device), Some(inode), Some(current_device), Some(current_inode)) => {
                device == current_device && inode == current_inode
            }
            _ => self.modified == current.modified && self.readonly == current.readonly,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SourceObservation {
    pub(super) canonical_metadata_path: PathBuf,
    pub(super) canonical_messages_path: PathBuf,
    pub(super) metadata: FileStamp,
    pub(super) messages: FileStamp,
    pub(super) metadata_sha256: [u8; 32],
    pub(super) exact_content_revision: String,
}

impl SourceObservation {
    pub(super) fn read(source: &MistralVibeSessionSource) -> Result<Self> {
        let canonical_metadata_path = fs::canonicalize(&source.metadata_path)?;
        let canonical_messages_path = fs::canonicalize(&source.messages_path)?;
        let metadata_file = File::open(&canonical_metadata_path)?;
        let messages_file = File::open(&canonical_messages_path)?;
        let metadata = FileStamp::from_metadata(&metadata_file.metadata()?)?;
        let messages = FileStamp::from_metadata(&messages_file.metadata()?)?;
        if metadata.length > MAX_PROVIDER_JSONL_LINE_BYTES as u64 {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: source.metadata_path.clone(),
                reason: "Mistral Vibe meta.json exceeds the supported size",
            });
        }
        let metadata_sha256 = hash_file_prefix(&canonical_metadata_path, metadata.length)?;
        let exact_content_revision =
            super::super::source::mistral_vibe_complete_content_revision_from_admitted(
                &metadata_file.metadata()?,
                &messages_file.metadata()?,
            )?;
        Ok(Self {
            canonical_metadata_path,
            canonical_messages_path,
            metadata,
            messages,
            metadata_sha256,
            exact_content_revision,
        })
    }

    pub(super) fn source_revision(&self, inventory_token: Option<&str>) -> String {
        let mut digest = Sha256::new();
        digest.update(SOURCE_REVISION_DOMAIN);
        digest.update(MISTRAL_VIBE_CAPTURE_REVISION.to_be_bytes());
        digest.update(MISTRAL_VIBE_POLICY_REVISION.to_be_bytes());
        hash_stamp(&mut digest, &self.metadata);
        hash_stamp(&mut digest, &self.messages);
        digest.update(self.metadata_sha256);
        if let Some(token) = inventory_token {
            digest.update((token.len() as u64).to_be_bytes());
            digest.update(token.as_bytes());
        }
        format!("mistral-vibe-nativepath-sha256-v1:{:x}", digest.finalize())
    }

    pub(super) fn generation_identity(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(b"ctx-mistral-vibe-generation-v1\0");
        digest.update(self.metadata_sha256);
        match (self.messages.device, self.messages.inode) {
            (Some(device), Some(inode)) => {
                digest.update(device.to_be_bytes());
                digest.update(inode.to_be_bytes());
            }
            _ => {
                digest.update(self.messages.modified.seconds.to_be_bytes());
                digest.update(self.messages.modified.nanos.to_be_bytes());
            }
        }
        digest.finalize().into()
    }

    pub(super) fn revalidate(&self, source: &MistralVibeSessionSource) -> Result<bool> {
        match Self::read(source) {
            Ok(current) => Ok(&current == self),
            Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(false)
            }
            Err(CaptureError::InvalidProviderTranscriptPath { .. }) => Ok(false),
            Err(error) => Err(error),
        }
    }
}

pub(super) fn next_core_page(opened: &mut OpenedSource) -> Result<Option<Page>> {
    if opened.lifecycle == SourceLifecycle::NoOp {
        return Ok(None);
    }
    let expected = opened.checkpoint.clone();
    let mut next = expected.clone();
    let mut events = Vec::new();
    let mut detached_touches = Vec::new();
    let mut rejections = Vec::new();
    let mut physical_records = 0_usize;
    let mut logical_units = 0_usize;
    let mut serialized_bytes =
        PAGE_BASE_BYTES.saturating_add(checkpoint_rejection_bytes(&expected));

    next.canonical_metadata_path = opened.observation.canonical_metadata_path.clone();
    next.canonical_messages_path = opened.observation.canonical_messages_path.clone();
    next.metadata_stamp = opened.observation.metadata.clone();
    next.messages_stamp = opened.observation.messages.clone();
    next.metadata_sha256 = opened.observation.metadata_sha256;
    next.source_revision = opened.target_source_revision.clone();
    next.generation_identity = opened.observation.generation_identity();
    next.canonical_source_identity = opened.target_source_identity.clone();
    next.session = opened.target_session.clone();
    next.terminal = false;

    if !next.metadata_failure_reported {
        if let Some(failure) = opened.metadata_failure.clone() {
            record_checkpoint_rejection(
                &mut next,
                &mut rejections,
                ProviderImportFailure {
                    line: 0,
                    error: failure,
                },
            );
            serialized_bytes = serialized_bytes.saturating_add(
                rejections
                    .last()
                    .map_or(128, |failure| failure.error.len().saturating_add(128)),
            );
            logical_units = logical_units.saturating_add(1);
        }
        next.metadata_failure_reported = true;
    }

    while physical_records < PAGE_MAX_UNITS && logical_units < PAGE_MAX_UNITS {
        let start = next.complete_prefix_end;
        let ordinal = next.next_ordinal;
        let hasher_before = opened.hasher.clone();
        let line = read_bounded_line(
            &mut opened.reader,
            &mut opened.hasher,
            opened.observation.messages.length,
            start,
        )?;
        let (bytes, end) = match line {
            Line::EndOfFile => {
                next.terminal = true;
                break;
            }
            Line::IncompleteTail => {
                opened.hasher = hasher_before;
                opened.reader.seek(SeekFrom::Start(start))?;
                next.terminal = false;
                break;
            }
            Line::Oversized { end } => {
                let failure = ProviderImportFailure {
                    line: usize::try_from(ordinal)
                        .unwrap_or(usize::MAX)
                        .saturating_add(1),
                    error: format!(
                        "{}:{} exceeds the {} byte JSONL record limit",
                        opened.source.messages_path.display(),
                        ordinal.saturating_add(1),
                        MAX_PROVIDER_JSONL_LINE_BYTES
                    ),
                };
                let failure_bytes = failure.error.len().saturating_add(128);
                if physical_records != 0
                    && serialized_bytes.saturating_add(failure_bytes) > PAGE_MAX_BYTES
                {
                    opened.hasher = hasher_before;
                    opened.reader.seek(SeekFrom::Start(start))?;
                    break;
                }
                next.complete_prefix_end = end;
                next.next_ordinal = next.next_ordinal.saturating_add(1);
                record_checkpoint_rejection(&mut next, &mut rejections, failure);
                physical_records = physical_records.saturating_add(1);
                logical_units = logical_units.saturating_add(1);
                serialized_bytes = serialized_bytes.saturating_add(failure_bytes);
                continue;
            }
            Line::Complete { bytes, end } => (bytes, end),
        };

        let projected = project_core_record(opened, &bytes, ordinal, start, end)?;
        opened.target_session.started_at =
            opened.target_session.started_at.min(projected.occurred_at);
        next.session.started_at = opened.target_session.started_at;
        let projected_units = projected
            .event
            .as_ref()
            .map_or(0, |event| 1_usize.saturating_add(event.touches.len()))
            .saturating_add(projected.detached_touches.len())
            .saturating_add(usize::from(projected.rejection.is_some()));
        let projected_bytes = projected.serialized_bytes;
        if physical_records != 0
            && (logical_units.saturating_add(projected_units) > PAGE_MAX_UNITS
                || serialized_bytes.saturating_add(projected_bytes) > PAGE_MAX_BYTES)
        {
            opened.hasher = hasher_before;
            opened.reader.seek(SeekFrom::Start(start))?;
            break;
        }
        if projected_units > PAGE_MAX_UNITS || projected_bytes > PAGE_MAX_BYTES {
            let failure = ProviderImportFailure {
                line: usize::try_from(ordinal)
                    .unwrap_or(usize::MAX)
                    .saturating_add(1),
                error: format!(
                    "{}:{} expands past a Mistral Vibe NativePath page",
                    opened.source.messages_path.display(),
                    ordinal.saturating_add(1)
                ),
            };
            next.complete_prefix_end = end;
            next.next_ordinal = next.next_ordinal.saturating_add(1);
            record_checkpoint_rejection(&mut next, &mut rejections, failure);
            physical_records = physical_records.saturating_add(1);
            logical_units = logical_units.saturating_add(1);
            serialized_bytes = serialized_bytes.saturating_add(128);
            continue;
        }

        next.complete_prefix_end = end;
        next.next_ordinal = next.next_ordinal.saturating_add(1);
        next.accepted_events = next
            .accepted_events
            .saturating_add(u64::from(projected.event.is_some()));
        next.accepted_file_touches = next
            .accepted_file_touches
            .saturating_add(
                projected
                    .event
                    .as_ref()
                    .map_or(0, |event| event.touches.len() as u64),
            )
            .saturating_add(projected.detached_touches.len() as u64);
        if let Some(failure) = projected.rejection {
            record_checkpoint_rejection(&mut next, &mut rejections, failure);
        }
        if let Some(event) = projected.event {
            events.push(event);
        }
        if !projected.detached_touches.is_empty() {
            detached_touches.push(DetachedTouches {
                ordinal,
                occurred_at: projected.occurred_at,
                touches: projected.detached_touches,
            });
        }
        physical_records = physical_records.saturating_add(1);
        logical_units = logical_units.saturating_add(projected_units);
        serialized_bytes = serialized_bytes.saturating_add(projected_bytes);
    }

    next.complete_prefix_sha256 = prefix_digest(&opened.hasher);
    next.messages_stamp = opened.observation.messages.clone();
    next.metadata_stamp = opened.observation.metadata.clone();
    if next.complete_prefix_end == opened.observation.messages.length {
        next.terminal = true;
    }
    let checkpoint_changed = next != expected;
    opened.checkpoint = next.clone();
    if !checkpoint_changed && physical_records == 0 && !opened.force_publication {
        return Ok(None);
    }
    opened.force_publication = false;
    Ok(Some(Page {
        expected,
        next,
        events,
        detached_touches,
        rejections,
        physical_records,
        conservative_serialized_bytes: serialized_bytes,
    }))
}

pub(super) enum Line {
    EndOfFile,
    IncompleteTail,
    Oversized { end: u64 },
    Complete { bytes: Vec<u8>, end: u64 },
}

pub(super) fn read_bounded_line(
    reader: &mut BufReader<File>,
    hasher: &mut Sha256,
    frozen_length: u64,
    start: u64,
) -> Result<Line> {
    if start >= frozen_length {
        return Ok(Line::EndOfFile);
    }
    let mut bytes = Vec::new();
    let mut total = 0_u64;
    let mut oversized = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(if total == 0 {
                Line::EndOfFile
            } else {
                Line::IncompleteTail
            });
        }
        let remaining = frozen_length.saturating_sub(start.saturating_add(total));
        if remaining == 0 {
            return Ok(Line::IncompleteTail);
        }
        let bounded = &available[..available
            .len()
            .min(usize::try_from(remaining).unwrap_or(usize::MAX))];
        let take = bounded
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(bounded.len(), |index| index.saturating_add(1));
        let chunk = &bounded[..take];
        hasher.update(chunk);
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
            let end = start.saturating_add(total);
            if oversized {
                return Ok(Line::Oversized { end });
            }
            bytes.pop();
            if bytes.last() == Some(&b'\r') {
                bytes.pop();
            }
            return Ok(Line::Complete { bytes, end });
        }
        if start.saturating_add(total) == frozen_length {
            return Ok(Line::IncompleteTail);
        }
    }
}
