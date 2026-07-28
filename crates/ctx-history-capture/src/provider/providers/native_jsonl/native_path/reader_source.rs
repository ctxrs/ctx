use super::*;

pub(super) enum DirectLine {
    EndOfFile,
    IncompleteTail,
    Oversized {
        end: u64,
    },
    Complete {
        bytes: Vec<u8>,
        end: u64,
        record_digest: [u8; 32],
    },
}

pub(super) fn read_bounded_jsonl_line(
    reader: &mut BufReader<File>,
    hasher: &mut Sha256,
    frozen_length: u64,
    start: u64,
) -> Result<DirectLine> {
    if start >= frozen_length {
        return Ok(DirectLine::EndOfFile);
    }
    let mut bytes = Vec::new();
    let mut total = 0_u64;
    let mut oversized = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(if total == 0 {
                DirectLine::EndOfFile
            } else {
                DirectLine::IncompleteTail
            });
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index.saturating_add(1));
        let chunk = &available[..take];
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
                return Ok(DirectLine::Oversized { end });
            }
            let record_digest = Sha256::digest(&bytes).into();
            if bytes.last() == Some(&b'\n') {
                bytes.pop();
                if bytes.last() == Some(&b'\r') {
                    bytes.pop();
                }
            }
            return Ok(DirectLine::Complete {
                bytes,
                end,
                record_digest,
            });
        }
    }
}

pub(crate) fn observe_file(path: &Path) -> Result<DirectJsonlFileObservation> {
    let opened = crate::common::io::open_provider_source_file(path)?;
    let observation = observe_opened_file(&opened)?;
    opened.revalidate()?;
    Ok(observation)
}

pub(crate) fn observe_opened_file(
    opened: &crate::common::io::OpenedProviderSourceFile,
) -> Result<DirectJsonlFileObservation> {
    observe_metadata(opened.metadata())
}

pub(crate) fn direct_jsonl_source_revision(observation: &DirectJsonlFileObservation) -> String {
    let side = if observation.modified.before_epoch {
        '-'
    } else {
        '+'
    };
    format!(
        "native-jsonl-metadata-v1:length={};modified={side}{}.{:09};readonly={};device={};inode={}",
        observation.length,
        observation.modified.seconds,
        observation.modified.nanos,
        observation.readonly,
        observation
            .device
            .map_or_else(|| "none".to_owned(), |value| value.to_string()),
        observation
            .inode
            .map_or_else(|| "none".to_owned(), |value| value.to_string()),
    )
}

pub(crate) fn direct_jsonl_prefix_sha256_opened(
    opened: &crate::common::io::OpenedProviderSourceFile,
    length: u64,
) -> Result<[u8; 32]> {
    let mut file = opened.file().try_clone()?;
    let digest = prefix_digest(&hash_prefix(&mut file, length, new_prefix_hasher())?);
    opened.revalidate()?;
    Ok(digest)
}

pub(super) fn observe_metadata(metadata: &Metadata) -> Result<DirectJsonlFileObservation> {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;

    #[cfg(unix)]
    let (device, inode) = (Some(metadata.dev()), Some(metadata.ino()));
    #[cfg(not(unix))]
    let (device, inode) = (None, None);
    Ok(DirectJsonlFileObservation {
        length: metadata.len(),
        modified: DirectJsonlObservedTime::from_system_time(metadata.modified()?),
        readonly: metadata.permissions().readonly(),
        device,
        inode,
    })
}

pub(super) fn same_file_identity(
    previous: &DirectJsonlFileObservation,
    current: &DirectJsonlFileObservation,
) -> bool {
    match (
        previous.device,
        previous.inode,
        current.device,
        current.inode,
    ) {
        (Some(previous_device), Some(previous_inode), Some(device), Some(inode)) => {
            previous_device == device && previous_inode == inode
        }
        _ => previous.modified == current.modified && previous.readonly == current.readonly,
    }
}

pub(super) fn new_prefix_hasher() -> Sha256 {
    let mut hasher = Sha256::new();
    hasher.update(DIRECT_JSONL_PREFIX_HASH_DOMAIN);
    hasher
}

pub(super) fn hash_prefix(file: &mut File, length: u64, mut hasher: Sha256) -> Result<Sha256> {
    file.seek(SeekFrom::Start(0))?;
    let mut remaining = length;
    let mut buffer = [0_u8; 64 * 1024];
    while remaining > 0 {
        let requested = usize::try_from(remaining.min(buffer.len() as u64)).map_err(|_| {
            CaptureError::SystemInvariant("direct JSONL prefix read length exceeds usize")
        })?;
        let read = file.read(&mut buffer[..requested])?;
        if read == 0 {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        hasher.update(&buffer[..read]);
        remaining = remaining.saturating_sub(read as u64);
    }
    Ok(hasher)
}

pub(super) fn prefix_digest(hasher: &Sha256) -> [u8; 32] {
    hasher.clone().finalize().into()
}

pub(super) fn rejection_wire_bytes(rejection: &DirectJsonlRejection) -> usize {
    128_usize.saturating_add(rejection.reason.len())
}

pub(super) fn event_wire_bytes(event: &DirectJsonlEvent) -> usize {
    DIRECT_JSONL_EVENT_ENVELOPE_BYTES
        .saturating_add(event.provider_event_hash.len())
        .saturating_add(event.cursor.len())
        .saturating_add(serde_json::to_vec(&event.payload).map_or(usize::MAX, |value| value.len()))
        .saturating_add(serde_json::to_vec(&event.metadata).map_or(usize::MAX, |value| value.len()))
        .saturating_add(
            event
                .touches
                .iter()
                .map(|touch| {
                    touch
                        .path
                        .len()
                        .saturating_add(touch.old_path.as_deref().map_or(0, str::len))
                })
                .sum::<usize>(),
        )
}

pub(super) fn output_wire_bytes(output: &DirectJsonlOutput) -> usize {
    512_usize
        .saturating_add(output.call_id.as_deref().map_or(0, str::len))
        .saturating_add(output.tool_name.as_deref().map_or(0, str::len))
        .saturating_add(output.content.len())
}
