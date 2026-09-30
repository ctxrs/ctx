use std::{ffi::OsString, fs, net::TcpListener};

use crate::analytics::{DaemonOperationV1, OperationCompletedV1, Outcome};

use super::{consent_tests::isolate_analytics_environment, *};

pub(crate) struct RestoreEnvironment {
    key: &'static str,
    previous: Option<OsString>,
}

impl RestoreEnvironment {
    pub(super) fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }

    pub(super) fn remove(key: &'static str) -> Self {
        let previous = std::env::var_os(key);
        std::env::remove_var(key);
        Self { key, previous }
    }
}

impl Drop for RestoreEnvironment {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            std::env::set_var(self.key, previous);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

pub(super) fn daemon_event() -> PublicEventV1 {
    PublicEventV1::OperationCompleted(OperationCompletedV1::for_daemon(
        DaemonOperationV1::Status,
        Outcome::Success,
        Duration::ZERO,
    ))
}

#[test]
fn enabled_root_cannot_deliver_a_disabled_roots_queued_batch() {
    let _env_lock = ctx_app_config::TEST_LOCAL_USAGE_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let sandbox = tempfile::tempdir().unwrap();
    let _environment = isolate_analytics_environment(sandbox.path());
    let root_a = sandbox.path().join("root-a");
    let root_b = sandbox.path().join("root-b");
    let received = sandbox.path().join("received.jsonl");
    let endpoint = url::Url::from_file_path(&received).unwrap().to_string();
    let _endpoint = RestoreEnvironment::set("CTX_ANALYTICS_ENDPOINT", &endpoint);

    append_analytics_batch(&root_a, &[daemon_event()]).unwrap();
    append_analytics_batch(&root_b, &[daemon_event()]).unwrap();
    let id_a = crate::identity::installation_id(&root_a).unwrap();
    let id_b = crate::identity::installation_id(&root_b).unwrap();
    assert_ne!(id_a, id_b);
    let path = crate::identity::device_state_path(ANALYTICS_OUTBOX_FILE, &root_a).unwrap();
    assert!(!path.starts_with(&root_a) && !path.starts_with(&root_b));
    ctx_history_platform::platform_security::verify_private_file(&path).unwrap();
    let outbox_b = crate::analytics_outbox::AnalyticsOutbox::open(path.clone(), &id_b).unwrap();
    let queued_b = outbox_b.snapshot(&endpoint).unwrap().remove(0);
    consent_tests::configure(&root_a, false, &endpoint);

    // The file transport exercises the production drain without a daemon or network.
    drain_analytics_outbox(&root_b, Duration::from_secs(1)).unwrap();
    let bodies = fs::read_to_string(&received).unwrap();
    assert!(
        !bodies.contains(&id_a),
        "enabled B uploaded opted-out A's batch"
    );
    assert!(
        bodies.contains(&id_b),
        "enabled B must deliver its own batch"
    );
    assert_eq!(bodies.trim_end().as_bytes(), queued_b.payload());
    assert!(!bodies.contains(&sandbox.path().to_string_lossy().to_string()));

    append_analytics_batch(&root_b, &[daemon_event()]).unwrap();
    let next_b = outbox_b.snapshot(&endpoint).unwrap().remove(0);
    drain_analytics_outbox(&root_a, Duration::from_secs(1)).unwrap();
    assert_eq!(bodies, fs::read_to_string(&received).unwrap());
    assert_eq!(
        outbox_b.snapshot(&endpoint).unwrap()[0].payload(),
        next_b.payload()
    );
    let outbox_a = crate::analytics_outbox::AnalyticsOutbox::open(path, &id_a).unwrap();
    assert!(outbox_a.snapshot(&endpoint).unwrap().is_empty());
    assert_eq!(
        crate::identity::existing_installation_id(&root_a)
            .unwrap()
            .as_deref(),
        Some(id_a.as_str())
    );

    let absent_root = sandbox.path().join("absent-root");
    let _disabled = RestoreEnvironment::set("CTX_ANALYTICS_ENABLED", "false");
    drain_analytics_outbox(&absent_root, Duration::from_secs(1)).unwrap();
    assert!(
        !absent_root.exists(),
        "opt-out must not create a root identity"
    );
    assert_eq!(
        outbox_b.snapshot(&endpoint).unwrap()[0].payload(),
        next_b.payload()
    );
}

#[test]
fn storage_authority_grants_only_the_exact_legacy_database_path() {
    let root = Path::new("/tmp/ctx-observability-authority-test");
    assert_eq!(
        local_usage_storage_authority(root).database_path(),
        root.join("usage.sqlite")
    );
}

#[test]
fn opt_out_precedes_dry_run_policy() {
    assert_eq!(analytics_policy_for(false, true), AnalyticsPolicy::Purge);
    assert_eq!(analytics_policy_for(true, true), AnalyticsPolicy::DryRun);
    assert_eq!(analytics_policy_for(true, false), AnalyticsPolicy::Active);
}

#[test]
fn foreground_append_opens_no_network_connection_and_dry_run_creates_no_backlog() {
    let _env_lock = ctx_app_config::TEST_LOCAL_USAGE_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let data_root = tempfile::tempdir().unwrap();
    let device_root = tempfile::tempdir().unwrap();
    let _environment = isolate_analytics_environment(device_root.path());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}/events", listener.local_addr().unwrap());
    let _endpoint = RestoreEnvironment::set("CTX_ANALYTICS_ENDPOINT", &endpoint);
    append_analytics_batch(data_root.path(), &[daemon_event()]).unwrap();

    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    let path = crate::identity::device_state_path(ANALYTICS_OUTBOX_FILE, data_root.path()).unwrap();
    let outbox = crate::analytics_outbox::AnalyticsOutbox::open(
        path.clone(),
        &crate::identity::installation_id(data_root.path()).unwrap(),
    )
    .unwrap();
    assert_eq!(outbox.snapshot(&endpoint).unwrap().len(), 1);

    let _dry_run = RestoreEnvironment::set("CTX_ANALYTICS_DRY_RUN", "1");
    purge_analytics_outbox(data_root.path(), &path).unwrap();
    append_analytics_batch(data_root.path(), &[daemon_event()]).unwrap();
    assert!(!path.exists());

    let _disabled = RestoreEnvironment::set("CTX_ANALYTICS_ENABLED", "false");
    crate::identity::write_private_file(&path, b"must be purged").unwrap();
    append_analytics_batch(data_root.path(), &[daemon_event()]).unwrap();
    assert!(!path.exists(), "opt-out must purge even during dry-run");
}
