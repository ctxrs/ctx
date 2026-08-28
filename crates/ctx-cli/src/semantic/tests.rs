use std::{
    borrow::Cow,
    ffi::OsString,
    fs,
    sync::{
        atomic::{AtomicBool, Ordering as AtomicOrdering},
        Arc,
    },
    time::Duration,
};

use anyhow::Result;
use ctx_client_observability::analytics::{
    DaemonOperationV1, OperationCompletedV1, Outcome, PublicEventV1,
};
use ctx_terminal::{RenderContext, StreamKind, TestContext};

use super::*;

#[test]
fn companion_maintenance_wake_coalesces_without_losing_a_publication() {
    let state = AtomicU8::new(0);

    assert!(request_companion_maintenance_worker(&state));
    assert!(!request_companion_maintenance_worker(&state));
    take_companion_maintenance_request(&state);

    assert!(!request_companion_maintenance_worker(&state));
    assert!(companion_maintenance_should_continue(&state));
    take_companion_maintenance_request(&state);
    assert!(!companion_maintenance_should_continue(&state));
    assert_eq!(state.load(Ordering::Acquire), 0);
}

#[test]
fn daemon_shutdown_cancels_and_joins_companion_maintenance_worker() {
    let state = AtomicU8::new(COMPANION_MAINTENANCE_WAKE_RUNNING);
    let cancellation = CancellationToken::new();
    let worker_cancellation = cancellation.clone();
    let stopped = Arc::new(AtomicBool::new(false));
    let worker_stopped = Arc::clone(&stopped);
    let handle = std::thread::spawn(move || {
        while !worker_cancellation.is_cancelled() {
            std::thread::yield_now();
        }
        worker_stopped.store(true, AtomicOrdering::Release);
    });
    let worker = Mutex::new(Some(CompanionMaintenanceWorker {
        cancellation,
        handle,
    }));

    stop_companion_maintenance_worker_in(&state, &worker);

    assert!(stopped.load(AtomicOrdering::Acquire));
    assert_eq!(state.load(Ordering::Acquire), 0);
    assert!(worker
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .is_none());
}

struct RestoreEnvironment {
    name: &'static str,
    previous: Option<OsString>,
}

impl RestoreEnvironment {
    fn set(name: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let previous = std::env::var_os(name);
        std::env::set_var(name, value);
        Self { name, previous }
    }
}

impl Drop for RestoreEnvironment {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var(self.name, value),
            None => std::env::remove_var(self.name),
        }
    }
}

#[test]
fn daemon_cli_config_borrows_passive_upgrade_policy() {
    let mut config = crate::config::AppConfig::default();
    config.upgrade.auto = "off".to_owned();

    let mapped = daemon_cli_config(&config);

    assert!(!mapped.auto_upgrade_enabled());
    assert!(matches!(&mapped.upgrade.channel, Cow::Borrowed(_)));
    assert!(std::ptr::eq(
        mapped.upgrade_channel().as_ptr(),
        config.upgrade.channel.as_ptr(),
    ));
}

#[test]
fn nonempty_daemon_observation_batch_loads_config_once() -> Result<()> {
    let _env_lock = crate::config::TEST_LOCAL_USAGE_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _dry_run = RestoreEnvironment::set("CTX_ANALYTICS_DRY_RUN", "1");
    initialize()?;
    let root = tempfile::tempdir()?;
    fs::write(
        root.path().join(crate::config::CONFIG_FILE),
        "[analytics]\nenabled = true\n\n[upgrade]\nauto = \"off\"\n",
    )?;
    let event = PublicEventV1::OperationCompleted(OperationCompletedV1::for_daemon(
        DaemonOperationV1::Status,
        Outcome::Success,
        Duration::ZERO,
    ));

    let (_, empty_loads) =
        crate::config::count_app_config_loads(|| deliver_daemon_events(root.path(), &[]));
    assert_eq!(empty_loads, 0);
    let (_, nonempty_loads) = crate::config::count_app_config_loads(|| {
        deliver_daemon_events(root.path(), std::slice::from_ref(&event));
    });
    assert_eq!(nonempty_loads, 1);
    Ok(())
}

#[test]
fn post_lock_initialization_failure_retains_restart_intent() -> Result<()> {
    let _env_lock = crate::config::TEST_LOCAL_USAGE_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    initialize()?;
    let installation = tempfile::tempdir()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(installation.path(), fs::Permissions::from_mode(0o700))?;
    }
    let installation_executable =
        installation
            .path()
            .join(if cfg!(windows) { "ctx.exe" } else { "ctx" });
    fs::write(&installation_executable, b"test ctx executable")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&installation_executable, fs::Permissions::from_mode(0o700))?;
    }
    let _upgrade_target =
        RestoreEnvironment::set("CTX_UPGRADE_TEST_TARGET", &installation_executable);
    let _background_child = RestoreEnvironment::set("CTX_DAEMON_BACKGROUND_CHILD", "1");

    let root = tempfile::tempdir()?;
    ctx_daemon_cli::publish_daemon_restart_intent(
        root.path(),
        ctx_daemon_cli::DaemonTriggerCommandArg::Search,
        "ua_01890f3e-2c80-7000-8000-00000000000b",
    )?;
    fs::write(
        root.path().join(".fail-daemon-before-ready-for-test"),
        b"fail",
    )?;

    let stdout_context = RenderContext::for_test(TestContext::pipe(StreamKind::Stdout));
    let stderr_context = RenderContext::for_test(TestContext::pipe(StreamKind::Stderr));
    let mut ui = crate::ui::Ui::with_writers(
        std::io::sink(),
        stdout_context,
        std::io::sink(),
        stderr_context,
    );
    let error = run_daemon_command(
        crate::DaemonArgs {
            command: crate::DaemonCommand::Run(crate::cli::DaemonRunArgs {
                foreground: false,
                finite_core_worker: false,
                loop_interval_seconds: None,
                max_chunks: None,
                force: false,
                start_mode: Some(crate::DaemonStartModeArg::Auto),
                trigger_command: Some(crate::DaemonTriggerCommandArg::Search),
                format: crate::output::JsonOutputFormat::Text,
            }),
        },
        root.path().to_path_buf(),
        &crate::config::AppConfig::default(),
        &mut ui,
    )
    .expect_err("the injected post-lock initialization failure must surface");

    let rendered_error = error.to_string();
    assert!(
        rendered_error.contains("injected daemon failure before readiness"),
        "unexpected daemon initialization error: {rendered_error}"
    );
    assert!(ctx_daemon_cli::daemon_restart_intent_pending(root.path()));
    Ok(())
}
