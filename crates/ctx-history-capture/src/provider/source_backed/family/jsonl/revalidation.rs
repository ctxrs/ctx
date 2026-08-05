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
    static AFTER_SECOND_PREFIX_HASH_HOOK: RefCell<Option<Box<dyn FnOnce()>>> =
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
pub(crate) fn set_after_second_jsonl_prefix_hash_hook(hook: impl FnOnce() + 'static) {
    AFTER_SECOND_PREFIX_HASH_HOOK.with(|slot| {
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

#[cfg(test)]
fn run_after_second_jsonl_prefix_hash_hook() {
    AFTER_SECOND_PREFIX_HASH_HOOK.with(|slot| {
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

pub(crate) fn revalidate_frozen_prefix(
    source_path: &Path,
    source_file: &OpenedProviderSourceFile,
    frozen: &JsonlFileObservation,
    prefix_length: u64,
    expected_prefix_digest: [u8; 32],
) -> Result<JsonlFileObservation> {
    revalidate_frozen_prefix_with_hasher(
        source_path,
        source_file,
        frozen,
        prefix_length,
        expected_prefix_digest,
        new_prefix_hasher(),
    )
}

pub(crate) fn revalidate_frozen_prefix_sha256(
    source_path: &Path,
    source_file: &OpenedProviderSourceFile,
    frozen: &JsonlFileObservation,
    prefix_length: u64,
    expected_prefix_digest: [u8; 32],
) -> Result<JsonlFileObservation> {
    revalidate_frozen_prefix_with_hasher(
        source_path,
        source_file,
        frozen,
        prefix_length,
        expected_prefix_digest,
        Sha256::default(),
    )
}

fn revalidate_frozen_prefix_with_hasher(
    source_path: &Path,
    source_file: &OpenedProviderSourceFile,
    frozen: &JsonlFileObservation,
    prefix_length: u64,
    expected_prefix_digest: [u8; 32],
    prefix_hasher: Sha256,
) -> Result<JsonlFileObservation> {
    if prefix_length > frozen.length {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let before = observe_metadata(
        source_path,
        source_file.file(),
        &source_file.file().metadata()?,
    )?;
    if !frozen.admits_frozen_prefix_in(&before) {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    source_file.revalidate_same_object()?;

    // An unchanged strong observation already binds the retained bytes. This
    // keeps an exact no-op metadata-only instead of rehashing the whole corpus.
    if &before == frozen {
        return Ok(before);
    }

    verify_prefix_digest(
        source_file,
        prefix_length,
        expected_prefix_digest,
        prefix_hasher.clone(),
    )?;
    #[cfg(test)]
    run_after_jsonl_prefix_hash_hook();
    let middle = observe_metadata(
        source_path,
        source_file.file(),
        &source_file.file().metadata()?,
    )?;
    source_file.revalidate_same_object()?;
    if !before.admits_frozen_prefix_in(&middle) {
        return Err(CaptureError::SourceChangedDuringCapture);
    }

    // Exact prefix equality plus monotonic same-object observations admits a
    // continuously growing append log. Requiring metadata to stop changing
    // would make an active terminal source impossible to certify.
    verify_prefix_digest(
        source_file,
        prefix_length,
        expected_prefix_digest,
        prefix_hasher.clone(),
    )?;
    #[cfg(test)]
    run_after_second_jsonl_prefix_hash_hook();
    let after = observe_metadata(
        source_path,
        source_file.file(),
        &source_file.file().metadata()?,
    )?;
    source_file.revalidate_same_object()?;
    if !middle.admits_frozen_prefix_in(&after) {
        return Err(CaptureError::SourceChangedDuringCapture);
    }

    // End on content proof so rewrite-plus-append after the preceding metadata
    // observation cannot be mistaken for deferred growth.
    verify_prefix_digest(
        source_file,
        prefix_length,
        expected_prefix_digest,
        prefix_hasher,
    )?;
    Ok(after)
}

fn verify_prefix_digest(
    source_file: &OpenedProviderSourceFile,
    prefix_length: u64,
    expected_prefix_digest: [u8; 32],
    prefix_hasher: Sha256,
) -> Result<()> {
    let observed = hash_prefix(
        &mut source_file.file().try_clone()?,
        prefix_length,
        prefix_hasher,
    )?;
    if prefix_digest(&observed) != expected_prefix_digest {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    Ok(())
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
