use notify::{Config, EventKindMask};

pub(super) fn config() -> Config {
    // Refresh reads every archive member. Filtering at native subscription
    // keeps those reads out of the watch queue while retaining write-close.
    Config::default().with_event_kinds(EventKindMask::CORE | EventKindMask::ACCESS_CLOSE)
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    use std::{
        collections::BTreeMap,
        sync::{mpsc, Arc},
        time::Duration,
    };

    use super::*;
    #[cfg(target_os = "linux")]
    use crate::{watch::WatchWatermark, CoalescingWakePayload, NativeFileWatcher};

    #[cfg(target_os = "linux")]
    #[derive(Clone, Debug, Default, Eq, PartialEq)]
    struct TestPayload(Option<WatchWatermark>);

    #[cfg(target_os = "linux")]
    impl CoalescingWakePayload for TestPayload {
        fn is_empty(&self) -> bool {
            self.0.is_none()
        }

        fn merge(&mut self, other: Self) {
            if let Some(watermark) = other.0 {
                self.0 = Some(self.0.map_or(watermark, |current| current.max(watermark)));
            }
        }
    }

    #[test]
    fn subscribes_to_changes_and_write_close_only() {
        let event_kinds = config().event_kinds();
        assert!(event_kinds.contains(EventKindMask::CORE));
        assert!(event_kinds.contains(EventKindMask::ACCESS_CLOSE));
        assert!(!event_kinds
            .intersects(EventKindMask::ACCESS_OPEN | EventKindMask::ACCESS_CLOSE_NOWRITE));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn source_reads_do_not_enter_native_watch_ingress() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("archive.jsonl");
        std::fs::write(&source, b"archival provider history\n").unwrap();
        let (ignored_tx, ignored_rx) = mpsc::channel();
        let mut watcher = NativeFileWatcher::start(
            "ctx-watch-source-read-test",
            Arc::new(move |event| {
                if event.ignored_kind().is_some() {
                    ignored_tx.send(()).unwrap();
                    true
                } else {
                    false
                }
            }),
            Arc::new(|_, _| TestPayload::default()),
            Arc::new(|_| {}),
            Arc::new(|watermark| TestPayload(Some(watermark))),
            Arc::new(|_| {}),
            Arc::new(|_| {}),
        )
        .unwrap();
        watcher
            .reconcile_paths(BTreeMap::from([(temp.path().to_path_buf(), true)]), false)
            .unwrap();

        for _ in 0..32 {
            assert_eq!(
                std::fs::read(&source).unwrap(),
                b"archival provider history\n"
            );
        }

        assert!(matches!(
            ignored_rx.recv_timeout(Duration::from_millis(250)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
    }
}
