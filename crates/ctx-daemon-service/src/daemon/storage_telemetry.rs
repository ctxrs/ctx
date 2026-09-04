use std::path::Path;

use ctx_client_observability::analytics::DaemonStorageFactsV1;
use ctx_history_refresh::source_backed_index_root;

use super::CoreRefreshEngine;
use crate::DaemonObservationPort;

pub(super) fn collect_if_enabled(
    observation: &dyn DaemonObservationPort,
    data_root: &Path,
    refresh: Option<&CoreRefreshEngine>,
) -> Option<DaemonStorageFactsV1> {
    collect_if_enabled_with(observation, data_root, || collect(data_root, refresh))
}

fn collect_if_enabled_with<T>(
    observation: &dyn DaemonObservationPort,
    data_root: &Path,
    collect: impl FnOnce() -> Option<T>,
) -> Option<T> {
    observation
        .analytics_enabled(data_root)
        .then(collect)
        .flatten()
}

fn collect(data_root: &Path, refresh: Option<&CoreRefreshEngine>) -> Option<DaemonStorageFactsV1> {
    DaemonStorageFactsV1::from_exact(
        filesystem_storage(data_root),
        active_core_storage(data_root, refresh),
    )
}

fn filesystem_storage(data_root: &Path) -> Option<(u64, u64)> {
    filesystem_storage_with(data_root, |path| {
        let stats = fs2::statvfs(path).ok()?;
        Some((stats.total_space(), stats.available_space()))
    })
}

fn filesystem_storage_with<F>(data_root: &Path, probe: F) -> Option<(u64, u64)>
where
    F: FnOnce(&Path) -> Option<(u64, u64)>,
{
    let mut candidate = data_root;
    loop {
        match std::fs::metadata(candidate) {
            Ok(_) => return probe(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                candidate = candidate.parent()?;
            }
            Err(_) => return None,
        }
    }
}

fn active_core_storage(
    data_root: &Path,
    refresh: Option<&CoreRefreshEngine>,
) -> Option<(u64, u64)> {
    let publication = refresh?.pinned_core_publication()?;
    let verified = publication.verified_index_ref();
    let storage =
        ctx_history_index::active_generation_storage_metadata(&source_backed_index_root(data_root))
            .ok()??;
    (storage.generation_id() == verified.generation_id()).then_some((
        storage.logical_bytes(),
        verified.manifest().certified_source_bytes,
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use ctx_client_observability::analytics::PublicEventV1;
    use serde_json::Value;

    use super::*;

    struct PolicyObservation(bool);

    impl DaemonObservationPort for PolicyObservation {
        fn analytics_enabled(&self, _data_root: &Path) -> bool {
            self.0
        }

        fn provider_refresh_event(
            &self,
            _job: &Value,
            _successor_pending: bool,
        ) -> Option<PublicEventV1> {
            None
        }

        fn append(&self, _data_root: &Path, _events: &[PublicEventV1]) {}

        fn append_and_upload(&self, _data_root: &Path, _events: &[PublicEventV1]) {}
    }

    #[test]
    fn disabled_analytics_skips_storage_collection() {
        let root = tempfile::tempdir().unwrap();
        let calls = AtomicUsize::new(0);

        let result = collect_if_enabled_with(&PolicyObservation(false), root.path(), || {
            calls.fetch_add(1, Ordering::Relaxed);
            Some(())
        });

        assert_eq!(result, None);
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn filesystem_probe_failure_omits_the_group() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(filesystem_storage_with(root.path(), |_| None), None);
    }

    #[test]
    fn filesystem_probe_resolves_an_existing_ancestor() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("not-created/child");
        let measured = filesystem_storage_with(&missing, |path| {
            assert_eq!(path, root.path());
            Some((10, 4))
        });
        assert_eq!(measured, Some((10, 4)));
    }

    #[test]
    fn no_generation_omits_active_core_storage() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(active_core_storage(root.path(), None), None);
    }
}
