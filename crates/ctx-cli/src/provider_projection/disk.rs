use std::path::Path;

use anyhow::{anyhow, Context, Result};

use crate::progress::format_bytes;

/// A re-derived generation is built alongside the installed one, so both exist
/// at once. The rebuilt database is close to the size of the one it replaces;
/// the margin covers the write-ahead log, FTS construction, and corpora that
/// grew since the original import.
const REBUILD_MARGIN_NUMERATOR: u64 = 5;
const REBUILD_MARGIN_DENOMINATOR: u64 = 4;
/// Floor for a small store, where a proportional margin is not meaningful.
const MINIMUM_REQUIRED_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Headroom {
    pub(crate) installed_bytes: u64,
    pub(crate) required_bytes: u64,
    pub(crate) available_bytes: u64,
}

impl Headroom {
    pub(crate) const fn is_sufficient(self) -> bool {
        self.available_bytes >= self.required_bytes
    }
}

pub(crate) fn required_bytes(installed_bytes: u64) -> u64 {
    installed_bytes
        .saturating_mul(REBUILD_MARGIN_NUMERATOR)
        .checked_div(REBUILD_MARGIN_DENOMINATOR)
        .unwrap_or(u64::MAX)
        .max(MINIMUM_REQUIRED_BYTES)
}

#[cfg(test)]
std::thread_local! {
    static AVAILABLE_SPACE_OVERRIDE: std::cell::Cell<Option<u64>> =
        const { std::cell::Cell::new(None) };
}

/// Runs `body` as if the filesystem holding the index had `bytes` free.
#[cfg(test)]
pub(crate) fn with_available_space<T>(bytes: u64, body: impl FnOnce() -> T) -> T {
    AVAILABLE_SPACE_OVERRIDE.with(|slot| slot.set(Some(bytes)));
    let outcome = body();
    AVAILABLE_SPACE_OVERRIDE.with(|slot| slot.set(None));
    outcome
}

fn available_space(parent: &Path) -> Result<u64> {
    #[cfg(test)]
    if let Some(bytes) = AVAILABLE_SPACE_OVERRIDE.with(|slot| slot.get()) {
        return Ok(bytes);
    }
    fs2::available_space(parent)
        .with_context(|| format!("read available disk space for {}", parent.display()))
}

pub(crate) fn headroom(db_path: &Path) -> Result<Headroom> {
    let installed_bytes = installed_store_bytes(db_path)?;
    let parent = db_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let available_bytes = available_space(parent)?;
    Ok(Headroom {
        installed_bytes,
        required_bytes: required_bytes(installed_bytes),
        available_bytes,
    })
}

/// Refuses the rebuild rather than filling the disk.
pub(crate) fn ensure_headroom(db_path: &Path) -> Result<Headroom> {
    let headroom = headroom(db_path)?;
    if headroom.is_sufficient() {
        return Ok(headroom);
    }
    Err(insufficient_disk_error(db_path, headroom))
}

pub(crate) fn insufficient_disk_error(db_path: &Path, headroom: Headroom) -> anyhow::Error {
    let shortfall = headroom
        .required_bytes
        .saturating_sub(headroom.available_bytes);
    anyhow!(
        "not enough free disk space to rebuild the provider index: {} required, {} available on {} \
         ({} short). The rebuild is built alongside the current index of {} and only replaces it \
         once it is complete, so both exist at the same time. Free space and run the rebuild \
         again; the current index is untouched.",
        format_bytes(headroom.required_bytes),
        format_bytes(headroom.available_bytes),
        db_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .display(),
        format_bytes(shortfall),
        format_bytes(headroom.installed_bytes),
    )
}

fn installed_store_bytes(db_path: &Path) -> Result<u64> {
    let mut total = 0_u64;
    for suffix in ["", "-wal"] {
        let mut value = db_path.as_os_str().to_owned();
        value.push(suffix);
        match std::fs::metadata(std::path::PathBuf::from(value)) {
            Ok(metadata) => total = total.saturating_add(metadata.len()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("measure ctx index {}", db_path.display()))
            }
        }
    }
    Ok(total)
}
