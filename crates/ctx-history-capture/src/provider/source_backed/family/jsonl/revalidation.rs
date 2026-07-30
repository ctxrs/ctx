#[cfg(test)]
use std::cell::{Cell, RefCell};
use std::{fs::File, io::Read, path::Path};

use sha2::Sha256;

use super::{identity::observe_metadata, new_prefix_hasher, prefix_digest, JsonlFileObservation};
use crate::{common::io::OpenedProviderSourceFile, CaptureError, Result};

#[cfg(test)]
thread_local! {
    static PREFIX_HASH_BYTES: Cell<u64> = const { Cell::new(0) };
    static AFTER_PREFIX_HASH_HOOK: RefCell<Option<Box<dyn FnOnce()>>> =
        const { RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn reset_jsonl_prefix_hash_bytes() {
    PREFIX_HASH_BYTES.set(0);
}

#[cfg(test)]
pub(crate) fn jsonl_prefix_hash_bytes() -> u64 {
    PREFIX_HASH_BYTES.get()
}

#[cfg(test)]
pub(crate) fn set_after_jsonl_prefix_hash_hook(hook: impl FnOnce() + 'static) {
    AFTER_PREFIX_HASH_HOOK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(hook));
    });
}

#[cfg(test)]
fn run_after_jsonl_prefix_hash_hook() {
    AFTER_PREFIX_HASH_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

pub(crate) fn observe_opened_file(
    source_path: &Path,
    opened: &OpenedProviderSourceFile,
) -> Result<JsonlFileObservation> {
    opened.revalidate()?;
    let observation = observe_metadata(source_path, opened.file(), opened.metadata())?;
    opened.revalidate()?;
    Ok(observation)
}

/// Observes one stable metadata instant while permitting the retained
/// append-log file to have grown since its authority handle was acquired.
pub(crate) fn observe_opened_file_same_object(
    source_path: &Path,
    opened: &OpenedProviderSourceFile,
) -> Result<JsonlFileObservation> {
    for _ in 0..2 {
        let before = observe_metadata(source_path, opened.file(), &opened.file().metadata()?)?;
        opened.revalidate_same_object()?;
        let after = observe_metadata(source_path, opened.file(), &opened.file().metadata()?)?;
        if before == after {
            return Ok(after);
        }
    }
    Err(CaptureError::SourceChangedDuringCapture)
}

pub(crate) fn revalidate_frozen_prefix(
    source_path: &Path,
    source_file: &OpenedProviderSourceFile,
    frozen: &JsonlFileObservation,
    prefix_length: u64,
    expected_prefix_digest: [u8; 32],
) -> Result<JsonlFileObservation> {
    if prefix_length > frozen.length {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    for _ in 0..2 {
        let before = observe_metadata(
            source_path,
            source_file.file(),
            &source_file.file().metadata()?,
        )?;
        if !frozen.admits_frozen_prefix_in(&before) {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        source_file.revalidate_same_object()?;

        // An unchanged strong observation already binds the retained bytes.
        // Avoid turning exact no-op refresh into an O(total source bytes)
        // prefix rehash.
        if &before == frozen {
            let after = observe_metadata(
                source_path,
                source_file.file(),
                &source_file.file().metadata()?,
            )?;
            source_file.revalidate_same_object()?;
            if after == before {
                return Ok(after);
            }
            continue;
        }

        let observed = hash_prefix(
            &mut source_file.file().try_clone()?,
            prefix_length,
            new_prefix_hasher(),
        )?;
        #[cfg(test)]
        run_after_jsonl_prefix_hash_hook();
        let after = observe_metadata(
            source_path,
            source_file.file(),
            &source_file.file().metadata()?,
        )?;
        source_file.revalidate_same_object()?;
        if after != before {
            continue;
        }
        if prefix_digest(&observed) != expected_prefix_digest {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        // A second digest closes the interval between the first prefix read
        // and its metadata observation even on filesystems whose change-time
        // granularity can coalesce two writes. Growth is the only path that
        // pays this cost; exact no-op remains metadata-only above.
        let confirmed = hash_prefix(
            &mut source_file.file().try_clone()?,
            prefix_length,
            new_prefix_hasher(),
        )?;
        let final_observation = observe_metadata(
            source_path,
            source_file.file(),
            &source_file.file().metadata()?,
        )?;
        source_file.revalidate_same_object()?;
        if final_observation != after {
            continue;
        }
        if prefix_digest(&confirmed) != expected_prefix_digest {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        return Ok(final_observation);
    }
    Err(CaptureError::SourceChangedDuringCapture)
}

pub(super) fn hash_prefix(file: &mut File, length: u64, mut hasher: Sha256) -> Result<Sha256> {
    use sha2::Digest;
    use std::io::{Seek, SeekFrom};

    file.seek(SeekFrom::Start(0))?;
    let mut remaining = length;
    let mut buffer = [0_u8; 64 * 1024];
    while remaining > 0 {
        let requested = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| CaptureError::SystemInvariant("JSONL prefix length exceeds usize"))?;
        let read = file.read(&mut buffer[..requested])?;
        if read == 0 {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        #[cfg(test)]
        PREFIX_HASH_BYTES.with(|bytes| {
            bytes.set(bytes.get().saturating_add(read as u64));
        });
        hasher.update(&buffer[..read]);
        remaining = remaining.saturating_sub(read as u64);
    }
    Ok(hasher)
}
