#[cfg(test)]
use std::cell::RefCell;
use std::{fs::File, path::Path, sync::Arc};

use sha2::{Digest, Sha256};

use super::{observe_metadata, PAGE_MAX_BYTES};
use crate::{
    common::io::OpenedProviderSourceFile, CaptureError, Result, MAX_PROVIDER_JSONL_LINE_BYTES,
};

#[cfg(test)]
thread_local! {
    static AFTER_HYDRATION_OBSERVATION_HOOK: RefCell<Option<Box<dyn FnOnce()>>> =
        const { RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_after_jsonl_hydration_observation_hook(hook: impl FnOnce() + 'static) {
    AFTER_HYDRATION_OBSERVATION_HOOK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(hook));
    });
}

#[cfg(test)]
fn run_after_jsonl_hydration_observation_hook() {
    AFTER_HYDRATION_OBSERVATION_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct JsonlHydrationRange {
    pub(crate) byte_offset: u64,
    pub(crate) byte_length: usize,
    pub(crate) record_digest: [u8; 32],
}

impl JsonlHydrationRange {
    pub(crate) fn new(
        byte_offset: u64,
        byte_length: usize,
        record_digest: [u8; 32],
    ) -> Result<Self> {
        if byte_length == 0
            || byte_length > MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(2)
            || byte_length > PAGE_MAX_BYTES
        {
            return Err(CaptureError::InvalidPayload(
                "JSONL hydration range exceeds the bounded record size".to_owned(),
            ));
        }
        Ok(Self {
            byte_offset,
            byte_length,
            record_digest,
        })
    }
}

/// Reads and verifies ordered record ranges from one retained source handle.
///
/// The callback's values are published only if every range, digest, provider
/// extraction, and terminal file revalidation succeeds.
pub(crate) fn visit_verified_ranges<T, E>(
    source_path: &Path,
    source_file: &Arc<OpenedProviderSourceFile>,
    ranges: &[JsonlHydrationRange],
    mut visit: impl FnMut(usize, &[u8]) -> std::result::Result<T, E>,
) -> std::result::Result<Vec<T>, E>
where
    E: From<CaptureError>,
{
    source_file.revalidate_same_object().map_err(E::from)?;
    let observation = observe_metadata(source_path, source_file.file(), source_file.metadata())
        .map_err(E::from)?;
    #[cfg(test)]
    run_after_jsonl_hydration_observation_hook();
    let mut values = Vec::with_capacity(ranges.len());
    for (index, range) in ranges.iter().enumerate() {
        let end = range
            .byte_offset
            .checked_add(range.byte_length as u64)
            .ok_or_else(|| {
                E::from(CaptureError::InvalidPayload(
                    "JSONL hydration range overflows".to_owned(),
                ))
            })?;
        if end > observation.length {
            return Err(E::from(CaptureError::InvalidPayload(
                "JSONL hydration range no longer exists".to_owned(),
            )));
        }
        let mut bytes = vec![0_u8; range.byte_length];
        read_exact_at(source_file.file(), &mut bytes, range.byte_offset).map_err(E::from)?;
        if <[u8; 32]>::from(Sha256::digest(&bytes)) != range.record_digest {
            return Err(E::from(CaptureError::InvalidPayload(
                "JSONL hydration record digest changed".to_owned(),
            )));
        }
        values.push(visit(index, &bytes)?);
    }
    let closing = observe_metadata(
        source_path,
        source_file.file(),
        &source_file
            .file()
            .metadata()
            .map_err(CaptureError::from)
            .map_err(E::from)?,
    )
    .map_err(E::from)?;
    source_file.revalidate_same_object().map_err(E::from)?;
    if closing == observation {
        return Ok(values);
    }
    if !observation.is_same_file_growth_to(&closing) {
        return Err(E::from(CaptureError::SourceChangedDuringCapture));
    }
    for range in ranges {
        let mut bytes = vec![0_u8; range.byte_length];
        read_exact_at(source_file.file(), &mut bytes, range.byte_offset).map_err(E::from)?;
        if <[u8; 32]>::from(Sha256::digest(&bytes)) != range.record_digest {
            return Err(E::from(CaptureError::SourceChangedDuringCapture));
        }
    }
    let final_observation = observe_metadata(
        source_path,
        source_file.file(),
        &source_file
            .file()
            .metadata()
            .map_err(CaptureError::from)
            .map_err(E::from)?,
    )
    .map_err(E::from)?;
    source_file.revalidate_same_object().map_err(E::from)?;
    if final_observation != closing {
        return Err(E::from(CaptureError::SourceChangedDuringCapture));
    }
    Ok(values)
}

#[cfg(unix)]
fn read_exact_at(file: &File, bytes: &mut [u8], offset: u64) -> Result<()> {
    use std::os::unix::fs::FileExt;

    file.read_exact_at(bytes, offset)?;
    Ok(())
}

#[cfg(windows)]
fn read_exact_at(file: &File, bytes: &mut [u8], mut offset: u64) -> Result<()> {
    use std::os::windows::fs::FileExt;

    let mut remaining = bytes;
    while !remaining.is_empty() {
        let read = file.seek_read(remaining, offset)?;
        if read == 0 {
            return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
        }
        offset = offset.saturating_add(read as u64);
        remaining = &mut remaining[read..];
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn read_exact_at(file: &File, bytes: &mut [u8], offset: u64) -> Result<()> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = file.try_clone()?;
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(bytes)?;
    Ok(())
}
