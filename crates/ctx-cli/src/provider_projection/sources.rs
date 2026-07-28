use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    str::FromStr,
};

use anyhow::Result;
use rusqlite::{Connection, OpenFlags};

use ctx_history_core::CaptureProvider;

use crate::provider_sources::{discovered_sources, source_for_path, SourceInfo};

/// Bound on recovered roots. A store with more distinct provider roots than
/// this is not a normal upgrade and is better handled by an explicit import.
const MAX_RECOVERED_ROOTS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecordedRoot {
    pub(crate) provider: CaptureProvider,
    pub(crate) path: PathBuf,
}

impl RecordedRoot {
    fn sort_key(&self) -> (&'static str, &Path) {
        (self.provider.as_str(), self.path.as_path())
    }
}

/// Provider roots the superseded store proves it indexed.
///
/// Discovery alone is not enough to reproduce an installed index: ctx supports
/// importing from an explicit path that discovery never finds, and re-deriving
/// without those roots would silently drop history the user already had. The
/// superseded store records each root it projected, so the rebuild replays
/// exactly that set, unioned with whatever discovery finds now.
pub(crate) fn recorded_roots(db_path: &Path) -> Result<Vec<RecordedRoot>> {
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let mut recorded = Vec::<RecordedRoot>::new();
    let mut seen = BTreeSet::<(String, PathBuf)>::new();
    for sql in [
        "SELECT DISTINCT provider, source_root FROM capture_sources
         WHERE source_root IS NOT NULL AND source_root <> ''",
        "SELECT DISTINCT provider, source_root FROM catalog_sessions
         WHERE source_root IS NOT NULL AND source_root <> ''",
    ] {
        let mut statement = conn.prepare(sql)?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (provider, source_root) = row?;
            let Ok(capture_provider) = CaptureProvider::from_str(&provider) else {
                continue;
            };
            let path = PathBuf::from(source_root);
            if !seen.insert((provider, path.clone())) {
                continue;
            }
            recorded.push(RecordedRoot {
                provider: capture_provider,
                path,
            });
            if recorded.len() >= MAX_RECOVERED_ROOTS {
                recorded.sort_by(|left, right| left.sort_key().cmp(&right.sort_key()));
                return Ok(recorded);
            }
        }
    }
    recorded.sort_by(|left, right| left.sort_key().cmp(&right.sort_key()));
    Ok(recorded)
}

#[cfg(test)]
std::thread_local! {
    static DISCOVERY_OVERRIDE: std::cell::RefCell<Option<Vec<SourceInfo>>> =
        const { std::cell::RefCell::new(None) };
}

/// Runs `body` with provider discovery replaced, so a test never reads the
/// developer's own provider history.
#[cfg(test)]
pub(crate) fn with_discovery<T>(sources: Vec<SourceInfo>, body: impl FnOnce() -> T) -> T {
    DISCOVERY_OVERRIDE.with(|slot| *slot.borrow_mut() = Some(sources));
    let outcome = body();
    DISCOVERY_OVERRIDE.with(|slot| *slot.borrow_mut() = None);
    outcome
}

/// Discovered sources a fresh install of this ctx would import.
pub(crate) fn discovered_importable_sources() -> Vec<SourceInfo> {
    #[cfg(test)]
    if let Some(sources) = DISCOVERY_OVERRIDE.with(|slot| slot.borrow().clone()) {
        return sources;
    }
    discovered_sources()
        .into_iter()
        .filter(|source| {
            source.exists
                && source.import_support.is_auto_importable()
                && source.status == ctx_history_capture::ProviderSourceStatus::Available
        })
        .collect()
}

/// Recorded roots that discovery does not already cover and that still exist.
///
/// A root whose provider data has since been deleted is dropped rather than
/// failed: complete-content retrieval already fails closed when a source has
/// vanished, and refusing the whole rebuild would leave the user with an index
/// they cannot import into.
pub(crate) fn replayable_roots(
    recorded: &[RecordedRoot],
    discovered: &[SourceInfo],
) -> Vec<SourceInfo> {
    recorded
        .iter()
        .filter(|root| {
            !discovered
                .iter()
                .any(|source| source.provider == root.provider && source.path == root.path)
        })
        .map(|root| source_for_path(root.provider, root.path.clone()))
        .filter(|source| source.exists && source.import_support.is_importable())
        .collect()
}

/// Orders sources so the largest Codex session tree is imported first.
///
/// Only an absent destination takes the cold builder, so whichever source runs
/// first decides how much of the rebuild gets the one-shot path. Sorting by
/// on-disk size puts the dominant corpus there, which is the same choice the
/// ordinary CLI import makes.
pub(crate) fn ordered_for_cold_build(mut sources: Vec<SourceInfo>) -> Vec<SourceInfo> {
    sources.sort_by(|left, right| {
        source_weight(right)
            .cmp(&source_weight(left))
            .then_with(|| left.path.cmp(&right.path))
    });
    sources
}

fn source_weight(source: &SourceInfo) -> u64 {
    if source.provider != CaptureProvider::Codex || !source.path.is_dir() {
        return 0;
    }
    directory_bytes(&source.path)
}

fn directory_bytes(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .filter_map(|entry| entry.metadata().ok())
        .filter(|metadata| metadata.is_file())
        .fold(0_u64, |total, metadata| {
            total.saturating_add(metadata.len())
        })
}
