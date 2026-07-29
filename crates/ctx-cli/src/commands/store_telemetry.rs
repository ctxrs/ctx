use std::{fs, path::Path};

use anyhow::Result;
use ctx_history_store::Store;

use crate::analytics::{self, StoreTelemetry};

pub(crate) fn indexed_history_item_count(store: &Store) -> Result<usize> {
    Ok(store.indexed_history_item_count()?)
}

pub(crate) fn analytics_preflight<T>(
    enabled: bool,
    query: impl FnOnce() -> Result<T>,
) -> Option<T> {
    enabled.then(query)?.ok()
}

pub(crate) fn insert_store_analytics_counts(
    telemetry: &mut StoreTelemetry,
    store: &Store,
    enabled: bool,
) -> Option<usize> {
    let counts = analytics_preflight(enabled, || Ok(store.indexed_history_counts()?))?;
    telemetry.indexed_sessions = Some(analytics::count_bucket(counts.sessions as u64));
    telemetry.indexed_events = Some(analytics::count_bucket(counts.events as u64));
    telemetry.indexed_items = Some(analytics::count_bucket(counts.items() as u64));
    Some(counts.items())
}

pub(crate) fn insert_db_size_bucket(telemetry: &mut StoreTelemetry, db_path: &Path) {
    let bytes = fs::metadata(db_path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    telemetry.db_size = Some(analytics::bytes_bucket(bytes));
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn analytics_preflight_is_disabled_without_running_the_query() {
        let called = Cell::new(false);
        let value = analytics_preflight(false, || {
            called.set(true);
            Ok::<_, anyhow::Error>(42)
        });
        assert_eq!(value, None);
        assert!(!called.get());
    }

    #[test]
    fn analytics_preflight_errors_become_unknown() {
        let value = analytics_preflight(true, || Err::<usize, _>(anyhow::anyhow!("preflight")));
        assert_eq!(value, None);
    }
}
