use super::*;
use std::collections::HashMap;
use std::sync::Arc;

struct EnvGuard {
    key: &'static str,
    prev: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let prev = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, prev }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(value) = self.prev.take() {
            std::env::set_var(self.key, value);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

fn managed_source(version: &str, sha256: &str) -> bundled_assets::ManagedRuntimeSource {
    bundled_assets::ManagedRuntimeSource {
        uri: "file:///tmp/rootfs.raw.zst".to_string(),
        sha256: sha256.to_string(),
        version: version.to_string(),
        bin: "rootfs.raw".to_string(),
        helpers: HashMap::from([
            (
                AVF_LINUX_KERNEL_HELPER.to_string(),
                bundled_assets::ManagedArtifactSource {
                    uri: "file:///tmp/kernel".to_string(),
                    sha256: "1".repeat(64),
                },
            ),
            (
                AVF_LINUX_INITRD_HELPER.to_string(),
                bundled_assets::ManagedArtifactSource {
                    uri: "file:///tmp/initrd".to_string(),
                    sha256: "2".repeat(64),
                },
            ),
            (
                AVF_LINUX_GUEST_AGENT_HELPER.to_string(),
                bundled_assets::ManagedArtifactSource {
                    uri: "file:///tmp/guest-agent".to_string(),
                    sha256: "3".repeat(64),
                },
            ),
            (
                AVF_LINUX_EGRESS_PROXY_HELPER.to_string(),
                bundled_assets::ManagedArtifactSource {
                    uri: "file:///tmp/egress-proxy".to_string(),
                    sha256: "4".repeat(64),
                },
            ),
            (
                AVF_LINUX_CONTAINER_STACK_HELPER.to_string(),
                bundled_assets::ManagedArtifactSource {
                    uri: "file:///tmp/container-stack".to_string(),
                    sha256: "5".repeat(64),
                },
            ),
        ]),
    }
}

#[test]
fn managed_runtime_root_is_bound_to_version_and_source_hash() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source_a = managed_source("runtime-v1", &"a".repeat(64));
    let source_b = managed_source("runtime-v1", &"b".repeat(64));
    let mut source_c = source_a.clone();
    source_c
        .helpers
        .get_mut(AVF_LINUX_KERNEL_HELPER)
        .expect("kernel helper")
        .sha256 = "6".repeat(64);
    let root_a = managed_avf_linux_runtime_root(temp.path(), &source_a);
    let root_b = managed_avf_linux_runtime_root(temp.path(), &source_b);
    let root_c = managed_avf_linux_runtime_root(temp.path(), &source_c);
    let identity_a = managed_avf_linux_runtime_source_identity(&source_a);

    assert_ne!(root_a, root_b);
    assert_ne!(root_a, root_c);
    assert!(root_a
        .to_string_lossy()
        .contains("runtime-v1-source-sha256-"));
    assert!(root_a.to_string_lossy().contains(&identity_a));
}

fn write_staged_runtime(runtime_root: &Path, version: &str) {
    std::fs::create_dir_all(runtime_root.join("helpers")).expect("create staged runtime helpers");
    std::fs::write(runtime_root.join("rootfs.raw"), b"rootfs").expect("write staged rootfs");
    std::fs::write(runtime_root.join("version.txt"), version).expect("write staged version");
    for helper in [
        AVF_LINUX_KERNEL_HELPER,
        AVF_LINUX_INITRD_HELPER,
        AVF_LINUX_GUEST_AGENT_HELPER,
        AVF_LINUX_EGRESS_PROXY_HELPER,
        AVF_LINUX_CONTAINER_STACK_HELPER,
    ] {
        let helper_path =
            managed_avf_linux_helper_path(runtime_root, helper).expect("staged helper path");
        std::fs::write(helper_path, helper.as_bytes())
            .unwrap_or_else(|err| panic!("write staged helper {helper}: {err}"));
    }
}

#[derive(Default)]
struct RecordingObserver {
    phases: StdMutex<Vec<(HarnessSetupPhase, String)>>,
    logs: StdMutex<Vec<(HarnessSetupPhase, HarnessSetupLogLevel, String)>>,
}

impl HarnessSetupObserver for RecordingObserver {
    fn on_phase(&self, phase: HarnessSetupPhase, message: &str) {
        self.phases
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push((phase, message.to_string()));
    }

    fn on_log(&self, phase: HarnessSetupPhase, level: HarnessSetupLogLevel, message: &str) {
        self.logs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push((phase, level, message.to_string()));
    }
}

#[tokio::test]
async fn ensure_managed_avf_linux_guest_runtime_prefers_explicit_staged_dir() {
    let _serial = crate::test_support::sandbox_cli_env_test_lock()
        .lock()
        .await;
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime_root = temp.path().join("runtime");
    write_staged_runtime(&runtime_root, "staged-override");
    let _runtime_dir = EnvGuard::set(
        AVF_LINUX_GUEST_RUNTIME_DIR_ENV,
        runtime_root.to_str().expect("runtime root utf8"),
    );
    let _source =
        override_managed_avf_linux_runtime_source_for_test(bundled_assets::ManagedRuntimeSource {
            uri: "locked://runtimes/avf-linux-guest/macos/aarch64/rootfs.raw.zst".to_string(),
            sha256: "0".repeat(64),
            version: "lock-version".to_string(),
            bin: "rootfs.raw".to_string(),
            helpers: HashMap::from([
                (
                    AVF_LINUX_KERNEL_HELPER.to_string(),
                    bundled_assets::ManagedArtifactSource {
                        uri: "locked://kernel".to_string(),
                        sha256: "1".repeat(64),
                    },
                ),
                (
                    AVF_LINUX_INITRD_HELPER.to_string(),
                    bundled_assets::ManagedArtifactSource {
                        uri: "locked://initrd".to_string(),
                        sha256: "2".repeat(64),
                    },
                ),
                (
                    AVF_LINUX_GUEST_AGENT_HELPER.to_string(),
                    bundled_assets::ManagedArtifactSource {
                        uri: "locked://guest-agent".to_string(),
                        sha256: "3".repeat(64),
                    },
                ),
                (
                    AVF_LINUX_EGRESS_PROXY_HELPER.to_string(),
                    bundled_assets::ManagedArtifactSource {
                        uri: "locked://egress-proxy".to_string(),
                        sha256: "4".repeat(64),
                    },
                ),
                (
                    AVF_LINUX_CONTAINER_STACK_HELPER.to_string(),
                    bundled_assets::ManagedArtifactSource {
                        uri: "locked://container-stack".to_string(),
                        sha256: "5".repeat(64),
                    },
                ),
            ]),
        });

    let runtime = ensure_managed_avf_linux_guest_runtime(temp.path(), None, None)
        .await
        .expect("staged runtime should be used before lock download");

    assert_eq!(runtime.runtime_root, runtime_root);
    assert_eq!(runtime.version, "staged-override");
    assert!(!runtime.managed);
}

#[tokio::test]
async fn explicit_staged_avf_linux_guest_runtime_dir_must_be_ready() {
    let _serial = crate::test_support::sandbox_cli_env_test_lock()
        .lock()
        .await;
    let temp = tempfile::tempdir().expect("tempdir");
    let runtime_root = temp.path().join("runtime");
    std::fs::create_dir_all(runtime_root.join("helpers")).expect("create staged helpers");
    std::fs::write(runtime_root.join("rootfs.raw"), b"rootfs").expect("write staged rootfs");
    let _runtime_dir = EnvGuard::set(
        AVF_LINUX_GUEST_RUNTIME_DIR_ENV,
        runtime_root.to_str().expect("runtime root utf8"),
    );

    let err = ensure_managed_avf_linux_guest_runtime(temp.path(), None, None)
        .await
        .expect_err("incomplete staged runtime should fail closed");

    assert!(
        err.to_string()
            .contains("explicit staged AVF Linux guest runtime dir is incomplete or not ready"),
        "unexpected error: {err:#}"
    );
}

#[tokio::test]
async fn ensure_managed_avf_linux_guest_runtime_reports_artifact_wait_before_shared_install_lock() {
    let _serial = crate::test_support::sandbox_cli_env_test_lock()
        .lock()
        .await;
    let _install_guard = managed_avf_linux_install_lock().lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let observer = Arc::new(RecordingObserver::default());
    let source = bundled_assets::ManagedRuntimeSource {
        uri: "https://example.test/runtimes/avf-linux-guest/rootfs.raw.zst".to_string(),
        sha256: "a".repeat(64),
        version: "ubuntu-noble-arm64-test".to_string(),
        bin: "rootfs.raw".to_string(),
        helpers: HashMap::new(),
    };

    let task = tokio::spawn({
        let data_root = temp.path().to_path_buf();
        let observer = observer.clone();
        async move {
            let _ = ensure_managed_avf_linux_guest_runtime_with_override(
                &data_root,
                Some(&source),
                Some(&*observer),
                None,
            )
            .await;
        }
    });

    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let saw_phase = observer
                .phases
                .lock()
                .unwrap_or_else(|poisoned: std::sync::PoisonError<_>| poisoned.into_inner())
                .iter()
                .any(|(phase, message)| {
                    *phase == HarnessSetupPhase::ArtifactDownload
                        && message == "waiting for managed AVF Linux guest runtime preparation"
                });
            let saw_log = observer
                .logs
                .lock()
                .unwrap_or_else(|poisoned: std::sync::PoisonError<_>| poisoned.into_inner())
                .iter()
                .any(
                    |(phase, level, message): &(
                        HarnessSetupPhase,
                        HarnessSetupLogLevel,
                        String,
                    )| {
                        *phase == HarnessSetupPhase::ArtifactDownload
                            && *level == HarnessSetupLogLevel::Info
                            && message.contains("waiting for another launch")
                    },
                );
            if saw_phase && saw_log {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("observer should report artifact wait before acquiring shared install lock");

    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn ensure_managed_avf_linux_guest_runtime_reports_shared_wait_duration_after_lock_release() {
    let _serial = crate::test_support::sandbox_cli_env_test_lock()
        .lock()
        .await;
    let install_guard = managed_avf_linux_install_lock().lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let observer = Arc::new(RecordingObserver::default());
    let source = bundled_assets::ManagedRuntimeSource {
        uri: "https://example.test/runtimes/avf-linux-guest/rootfs.raw.zst".to_string(),
        sha256: "a".repeat(64),
        version: "ubuntu-noble-arm64-test".to_string(),
        bin: "rootfs.raw".to_string(),
        helpers: HashMap::new(),
    };

    let task = tokio::spawn({
        let data_root = temp.path().to_path_buf();
        let observer = observer.clone();
        async move {
            let _ = ensure_managed_avf_linux_guest_runtime_with_override(
                &data_root,
                Some(&source),
                Some(&*observer),
                None,
            )
            .await;
        }
    });

    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let saw_wait = observer
                .logs
                .lock()
                .unwrap_or_else(|poisoned: std::sync::PoisonError<_>| poisoned.into_inner())
                .iter()
                .any(
                    |(phase, level, message): &(
                        HarnessSetupPhase,
                        HarnessSetupLogLevel,
                        String,
                    )| {
                        *phase == HarnessSetupPhase::ArtifactDownload
                            && *level == HarnessSetupLogLevel::Info
                            && message.contains("waiting for another launch")
                    },
                );
            if saw_wait {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("observer should report shared-install wait before lock release");

    drop(install_guard);

    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let saw_elapsed = observer
                .logs
                .lock()
                .unwrap_or_else(|poisoned: std::sync::PoisonError<_>| poisoned.into_inner())
                .iter()
                .any(
                    |(phase, level, message): &(
                        HarnessSetupPhase,
                        HarnessSetupLogLevel,
                        String,
                    )| {
                        *phase == HarnessSetupPhase::ArtifactDownload
                            && *level == HarnessSetupLogLevel::Info
                            && message
                                .contains("shared AVF Linux runtime preparation wait finished in")
                    },
                );
            if saw_elapsed {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("observer should report shared-install wait duration after lock release");

    let _ = task.await;
}

#[test]
fn managed_avf_linux_archive_path_preserves_zstd_extension() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = bundled_assets::ManagedRuntimeSource {
        uri: "https://example.test/runtimes/avf-linux-guest/rootfs.raw.zst".to_string(),
        sha256: "a".repeat(64),
        version: "ubuntu-noble-arm64-test".to_string(),
        bin: "rootfs.raw".to_string(),
        helpers: HashMap::new(),
    };

    let archive_path = managed_avf_linux_archive_path(temp.path(), &source);
    assert_eq!(
        archive_path
            .extension()
            .and_then(|ext| ext.to_str())
            .expect("zst extension"),
        "zst"
    );
}
