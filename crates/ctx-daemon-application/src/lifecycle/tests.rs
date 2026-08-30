use super::*;
use crate::{SEMANTIC_EMBEDDING_TOKEN_ENDPOINT_ENV, SEMANTIC_EMBEDDING_TOKEN_ENV};
use std::cell::RefCell;

const DAEMON_ENV_PROBE_STAGE: &str = "CTX_DAEMON_ENV_PROBE_STAGE";
const DAEMON_ENV_PRO_CHANNEL: &str = "CTX_PRO_CHANNEL";
const DAEMON_ENV_PROBE_TEST: &str =
    "lifecycle::tests::daemon_child_environment_strips_pro_channel_and_authority";
const DAEMON_ENV_HOSTILE: &str = "CTX_UNTRUSTED_DAEMON_AMBIENT_SECRET";
const DAEMON_ENV_ALLOWED_SENTINEL: &str = "/ctx-daemon-allowed-home";
const DAEMON_ENV_SEMANTIC_TOKEN_SENTINEL: &str = "semantic-bearer-token";
const DAEMON_ENV_SEMANTIC_ENDPOINT_SENTINEL: &str = "https://embeddings.example.test/";
const DAEMON_ENV_UNRELATED_SEMANTIC_TOKEN: &str = "CTX_SEMANTIC_EMBEDDING_FALLBACK_TOKEN";
#[cfg(unix)]
const DETACH_PROBE_STAGE: &str = "CTX_DAEMON_DETACH_PROBE_STAGE";
#[cfg(unix)]
const DETACH_PROBE_TEST: &str =
    "lifecycle::tests::autostart_child_detaches_from_the_invoking_terminal_session";

#[test]
fn daemon_child_environment_strips_pro_channel_and_authority() -> Result<()> {
    match env::var(DAEMON_ENV_PROBE_STAGE).as_deref() {
        Ok("final") => {
            assert_eq!(env::var("HOME").as_deref(), Ok(DAEMON_ENV_ALLOWED_SENTINEL));
            assert_eq!(env::var("GROK_HOME").as_deref(), Ok("/ctx-grok-home"));
            assert_eq!(env::var("DSH_HOME").as_deref(), Ok("/ctx-dsh-home"));
            assert_eq!(
                env::var(SEMANTIC_EMBEDDING_TOKEN_ENV).as_deref(),
                Ok(DAEMON_ENV_SEMANTIC_TOKEN_SENTINEL)
            );
            assert_eq!(
                env::var(SEMANTIC_EMBEDDING_TOKEN_ENDPOINT_ENV).as_deref(),
                Ok(DAEMON_ENV_SEMANTIC_ENDPOINT_SENTINEL)
            );
            assert!(env::var_os(DAEMON_ENV_PRO_CHANNEL).is_none());
            assert!(env::var_os(DAEMON_ENV_HOSTILE).is_none());
            assert!(env::var_os(DAEMON_ENV_UNRELATED_SEMANTIC_TOKEN).is_none());
            assert!(env::var_os("CTX_RELEASE_INHERITED_AUTHORITY").is_none());
            assert!(env::var_os("CTX_RELEASE_CONFIGURED_AUTHORITY").is_none());
            assert!(env::var_os("CTX_PRO_STAGING_ACCESS_CLIENT_SECRET").is_none());
            assert!(env::var_os("CTX_PRO_QUALIFICATION_HELPER_PATH").is_none());
            assert!(env::var_os("CTX_PRO_API_URL").is_none());
            assert!(env::var_os("XAI_API_KEY").is_none());
            assert!(env::var_os("DEEPSEEK_API_KEY").is_none());
            assert!(env::var_os("OPENROUTER_API_KEY").is_none());
            return Ok(());
        }
        Ok("inherited") => {
            assert_eq!(env::var(DAEMON_ENV_HOSTILE).as_deref(), Ok("attacker"));
            assert_eq!(
                env::var("CTX_RELEASE_INHERITED_AUTHORITY").as_deref(),
                Ok("attacker")
            );
            let args: Vec<OsString> = ["--exact", DAEMON_ENV_PROBE_TEST, "--nocapture"]
                .into_iter()
                .map(OsString::from)
                .collect();
            let overrides = BTreeMap::from([(
                OsString::from(DAEMON_ENV_PROBE_STAGE),
                OsString::from("final"),
            )]);
            let mut forbidden = overrides.clone();
            forbidden.insert(
                OsString::from("CTX_RELEASE_CONFIGURED_AUTHORITY"),
                OsString::from("attacker"),
            );
            let forbidden_error =
                normalized_daemon_launch_for_test(env::current_exe()?, args.clone(), forbidden)
                    .expect_err("release authority must be rejected during normalization");
            assert_eq!(forbidden_error.kind(), io::ErrorKind::InvalidInput);
            let descendant =
                normalized_daemon_launch_for_test(env::current_exe()?, args, overrides);
            assert!(spawn_detached_daemon_child(descendant?)?.wait()?.success());
            return Ok(());
        }
        _ => {}
    }

    for channel in [None, Some("stable"), Some("staging"), Some("preview")] {
        let mut inherited = std::process::Command::new(env::current_exe()?);
        inherited
            .args(["--exact", DAEMON_ENV_PROBE_TEST, "--nocapture"])
            .env(DAEMON_ENV_PROBE_STAGE, "inherited")
            .env(DAEMON_ENV_HOSTILE, "attacker")
            .env("CTX_RELEASE_INHERITED_AUTHORITY", "attacker")
            .env("CTX_PRO_STAGING_ACCESS_CLIENT_SECRET", "attacker")
            .env("CTX_PRO_QUALIFICATION_HELPER_PATH", "/attacker/helper")
            .env("CTX_PRO_API_URL", "https://attacker.invalid")
            .env("XAI_API_KEY", "attacker")
            .env("DEEPSEEK_API_KEY", "attacker")
            .env("OPENROUTER_API_KEY", "attacker")
            .env(
                SEMANTIC_EMBEDDING_TOKEN_ENV,
                DAEMON_ENV_SEMANTIC_TOKEN_SENTINEL,
            )
            .env(
                SEMANTIC_EMBEDDING_TOKEN_ENDPOINT_ENV,
                DAEMON_ENV_SEMANTIC_ENDPOINT_SENTINEL,
            )
            .env(DAEMON_ENV_UNRELATED_SEMANTIC_TOKEN, "attacker")
            .env("GROK_HOME", "/ctx-grok-home")
            .env("DSH_HOME", "/ctx-dsh-home")
            .env("HOME", DAEMON_ENV_ALLOWED_SENTINEL)
            .env_remove(DAEMON_ENV_PRO_CHANNEL);
        if let Some(channel) = channel {
            inherited.env(DAEMON_ENV_PRO_CHANNEL, channel);
        }
        assert!(inherited.status()?.success());
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn autostart_child_detaches_from_the_invoking_terminal_session() -> Result<()> {
    if env::var_os(DETACH_PROBE_STAGE).as_deref() == Some(std::ffi::OsStr::new("child")) {
        std::thread::sleep(Duration::from_secs(30));
        return Ok(());
    }

    let launch = normalized_daemon_launch_for_test(
        env::current_exe()?,
        ["--exact", DETACH_PROBE_TEST, "--nocapture"]
            .into_iter()
            .map(OsString::from)
            .collect(),
        BTreeMap::from([(OsString::from(DETACH_PROBE_STAGE), OsString::from("child"))]),
    )?;
    let mut child = spawn_detached_daemon_child(launch)?;
    let child_pid = child.id();
    let child_session = ctx_daemon_runtime::process_session_id(child_pid);
    child.kill()?;
    child.wait()?;
    assert_eq!(child_session?, child_pid);
    Ok(())
}

fn test_config() -> DaemonConfigSnapshot {
    DaemonConfigSnapshot {
        enabled: true,
        mode: DaemonMode::Full,
        semantic_enabled: true,
        semantic_executor: "https://embeddings.example.test/v1/".to_owned(),
        semantic_contract_fingerprint: "sha256:external-space-a".to_owned(),
    }
}

#[test]
fn active_same_binary_daemon_is_reused_without_supervisor_reconciliation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let lock = ctx_daemon_runtime::DaemonLock::acquire(temp.path())?
        .expect("test process should acquire daemon ownership");

    assert!(active_daemon_matches_current_executable(temp.path())?);

    drop(lock);
    assert!(!active_daemon_matches_current_executable(temp.path())?);
    Ok(())
}

#[test]
fn finite_core_worker_launch_is_forced_internal_and_has_no_persistent_timer() {
    let launch = configured_finite_core_worker_command(
        Path::new("/managed/ctx"),
        Path::new("/managed/data"),
        DaemonTrigger::Import,
    )
    .unwrap();
    let args = launch
        .get_args()
        .filter_map(OsStr::to_str)
        .collect::<Vec<_>>();

    assert!(args.contains(&"--finite-core-worker"), "{args:?}");
    assert!(args.contains(&"--force"), "{args:?}");
    assert!(!args.contains(&"--loop-interval-seconds"), "{args:?}");
    assert!(launch.get_envs().any(|(key, value)| {
        key == OsStr::new(DAEMON_BACKGROUND_CHILD_ENV) && value == Some(OsStr::new("1"))
    }));
}

#[test]
fn cancellation_at_the_last_spawn_boundary_starts_no_process() {
    let launch = NormalizedLaunch::new(
        Path::new("/ctx-must-not-spawn-after-cancellation").to_path_buf(),
        Vec::new(),
        BTreeMap::new(),
    );
    let error = spawn_daemon_profile(
        &crate::TestHost,
        launch,
        DaemonLaunchProfile::FiniteCoreWorker,
        &mut || Err(anyhow!("cancelled before spawn")),
    )
    .unwrap_err();

    assert_eq!(error.to_string(), "cancelled before spawn");
}

#[cfg(unix)]
#[test]
fn finite_worker_keeps_the_invoking_terminal_session() -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("record-session.sh");
    let receipt = temp.path().join("session.txt");
    fs::write(
        &executable,
        "#!/bin/sh\nprintf '%s ' \"$$\" >\"$CTX_DAEMON_TEST_RECEIPT\"\nps -o sid= -p \"$$\" >>\"$CTX_DAEMON_TEST_RECEIPT\"\nprintf ' ' >>\"$CTX_DAEMON_TEST_RECEIPT\"\nps -o pgid= -p \"$$\" >>\"$CTX_DAEMON_TEST_RECEIPT\"\n",
    )?;
    let mut permissions = fs::metadata(&executable)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&executable, permissions)?;
    let launch = normalized_daemon_launch_for_test(
        executable,
        Vec::new(),
        BTreeMap::from([(
            OsString::from("CTX_DAEMON_TEST_RECEIPT"),
            receipt.as_os_str().to_os_string(),
        )]),
    )?;
    let mut child = ctx_daemon_runtime::spawn_attached(launch)?;
    assert!(child.wait()?.success());
    let recorded = fs::read_to_string(receipt)?;
    let values = recorded
        .split_whitespace()
        .map(str::parse::<i32>)
        .collect::<std::result::Result<Vec<_>, _>>()?;

    assert_eq!(values.len(), 3);
    assert_eq!(values[0], i32::try_from(child.id())?);
    let parent_session = std::process::Command::new("ps")
        .args(["-o", "sid=", "-p", &std::process::id().to_string()])
        .output()?;
    assert!(parent_session.status.success());
    let parent_session = String::from_utf8(parent_session.stdout)?
        .trim()
        .parse::<i32>()?;
    assert_eq!(values[1], parent_session);
    assert_eq!(values[2], i32::try_from(child.id())?);
    Ok(())
}

#[cfg(unix)]
#[test]
fn finite_lease_distinguishes_owned_direct_child_from_joined_owner() -> Result<()> {
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg("exit 0")
        .spawn()?;
    let pid = child.id();
    let _ = child.wait()?;
    let mut lease = FiniteCoreWorkerLease::from_handoff(
        PathBuf::new(),
        DaemonHandoff {
            pid,
            heartbeat_at_ms: 1,
        },
        Some(child),
    )?;

    let FiniteCoreWorkerLease::Owned(lease) = &mut lease else {
        panic!("matching direct child must retain owned authority");
    };
    assert!(lease.reap_if_exited()?);

    let losing_child = std::process::Command::new("sh")
        .arg("-c")
        .arg("exit 0")
        .spawn()?;
    let losing_pid = losing_child.id();
    let losing_lease = FiniteCoreWorkerLease::from_handoff(
        PathBuf::new(),
        DaemonHandoff {
            pid: losing_pid.saturating_add(1),
            heartbeat_at_ms: 1,
        },
        Some(losing_child),
    )?;
    assert!(matches!(losing_lease, FiniteCoreWorkerLease::Joined(_)));
    Ok(())
}

#[cfg(unix)]
#[test]
fn graceful_delivery_failure_still_escalates_and_reaps_the_exact_child() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let child = std::process::Command::new("sh")
        .args(["-c", "exec sleep 30"])
        .spawn()?;
    let pid = child.id();
    let mut lease = FiniteCoreWorkerLease::from_handoff(
        temp.path().to_path_buf(),
        DaemonHandoff {
            pid,
            heartbeat_at_ms: 1,
        },
        Some(child),
    )?;
    let FiniteCoreWorkerLease::Owned(lease) = &mut lease else {
        panic!("matching direct child must retain owned authority");
    };

    let error = lease
        .interrupt_and_reap_with_signal_for_test(Duration::ZERO, |_| {
            Err(std::io::Error::other("injected group delivery failure"))
        })
        .expect_err("delivery failure remains observable after cleanup");
    assert_eq!(error.to_string(), "injected group delivery failure");
    assert!(lease.reap_if_exited()?);
    assert_eq!(
        ctx_daemon_runtime::process_state(pid),
        ctx_daemon_runtime::ProcessState::NotRunning
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn transient_hard_kill_failure_is_retried_and_the_exact_child_is_reaped() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let child = std::process::Command::new("sh")
        .args(["-c", "exec sleep 30"])
        .spawn()?;
    let pid = child.id();
    let mut lease = FiniteCoreWorkerLease::from_handoff(
        temp.path().to_path_buf(),
        DaemonHandoff {
            pid,
            heartbeat_at_ms: 1,
        },
        Some(child),
    )?;
    let FiniteCoreWorkerLease::Owned(lease) = &mut lease else {
        panic!("matching direct child must retain owned authority");
    };
    let mut kill_attempts = 0;

    let error = lease
        .interrupt_and_reap_with_actions_for_test(
            Duration::ZERO,
            |_| Ok(()),
            |child| {
                kill_attempts += 1;
                if kill_attempts == 1 {
                    Err(std::io::Error::other("injected transient kill failure"))
                } else {
                    child.kill()
                }
            },
        )
        .expect_err("the first delivery error remains diagnostic after exact reap");

    assert_eq!(error.to_string(), "injected transient kill failure");
    assert!(kill_attempts >= 2);
    assert!(lease.reap_if_exited()?);
    assert_eq!(
        ctx_daemon_runtime::process_state(pid),
        ctx_daemon_runtime::ProcessState::NotRunning
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn graceful_interrupt_targets_the_owned_private_group_and_reaps_it() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let interrupted = temp.path().join("interrupted");
    let ready = temp.path().join("ready");
    let launch = NormalizedLaunch::new(
        Path::new("/bin/sh").to_path_buf(),
        vec![
            OsString::from("-c"),
            OsString::from(
                "trap 'printf interrupted >\"$1\"; exit 0' INT; printf ready >\"$2\"; while :; do /bin/sleep 1; done",
            ),
            OsString::from("finite-worker"),
            interrupted.as_os_str().to_os_string(),
            ready.as_os_str().to_os_string(),
        ],
        BTreeMap::new(),
    );
    let child = ctx_daemon_runtime::spawn_attached(launch)?;
    let pid = child.id();
    let deadline = Instant::now() + Duration::from_secs(2);
    while !ready.exists() {
        if Instant::now() >= deadline {
            panic!("finite worker did not install its signal trap");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut lease = FiniteCoreWorkerLease::from_handoff(
        temp.path().to_path_buf(),
        DaemonHandoff {
            pid,
            heartbeat_at_ms: 1,
        },
        Some(child),
    )?;
    let FiniteCoreWorkerLease::Owned(lease) = &mut lease else {
        panic!("matching direct child must retain owned authority");
    };

    lease.interrupt_and_reap(Duration::from_secs(2))?;

    assert_eq!(fs::read_to_string(interrupted)?, "interrupted");
    assert!(lease.reap_if_exited()?);
    assert_eq!(
        ctx_daemon_runtime::process_state(pid),
        ctx_daemon_runtime::ProcessState::NotRunning
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn readiness_interrupt_reaps_the_spawned_candidate() -> Result<()> {
    let child = ctx_daemon_runtime::spawn_attached(NormalizedLaunch::new(
        PathBuf::from("sh"),
        ["-c", "exec sleep 30"]
            .into_iter()
            .map(OsString::from)
            .collect(),
        BTreeMap::new(),
    ))?;
    let pid = child.id();
    let mut checkpoints = 0;
    let mut pauses = 0;
    let error = wait_for_daemon_handoff_with_cancellation(
        10,
        || DaemonHandoffObservation::Pending,
        || Ok(None),
        || {},
        || pauses += 1,
        &mut || {
            checkpoints += 1;
            if checkpoints == 4 {
                Err(anyhow!("cancelled during readiness wait"))
            } else {
                Ok(())
            }
        },
    )
    .unwrap_err();
    let mut child = Some(child);
    let DaemonStartError::Ready(error) =
        daemon_ready_error(DaemonLaunchProfile::FiniteCoreWorker, &mut child, error)
    else {
        panic!("readiness cancellation must remain a ready error");
    };

    assert_eq!(error.to_string(), "cancelled during readiness wait");
    assert_eq!(checkpoints, 4);
    assert_eq!(pauses, 1);
    assert!(child
        .as_mut()
        .expect("candidate child")
        .try_wait()?
        .is_some());
    assert_eq!(
        ctx_daemon_runtime::process_state(pid),
        ctx_daemon_runtime::ProcessState::NotRunning
    );
    Ok(())
}

#[test]
fn authenticated_starting_handoff_remains_cancellable() {
    let mut checkpoints = 0;
    let mut renewals = 0;
    let mut pauses = 0;

    let error = wait_for_daemon_handoff_with_cancellation(
        10,
        || DaemonHandoffObservation::Starting,
        || Ok(None),
        || renewals += 1,
        || pauses += 1,
        &mut || {
            checkpoints += 1;
            if checkpoints == 4 {
                Err(anyhow!("cancelled during authenticated startup"))
            } else {
                Ok(())
            }
        },
    )
    .unwrap_err();

    assert_eq!(error.to_string(), "cancelled during authenticated startup");
    assert_eq!(checkpoints, 4);
    assert_eq!(renewals, 1);
    assert_eq!(pauses, 1);
}

fn test_daemon_owner(owner_id: &str, pid: u32) -> DaemonOwnerIdentity {
    DaemonOwnerIdentity {
        owner_id: owner_id.to_owned(),
        pid,
        started_at_ms: 1_000,
        binary_sha256: "0123456789abcdef".to_owned(),
    }
}

fn running_status(
    owner: &DaemonOwnerIdentity,
    expected: &DaemonConfigSnapshot,
    heartbeat_at_ms: i64,
) -> Value {
    json!({
        "status": "running",
        "pid": owner.pid,
        "started_at_ms": owner.started_at_ms,
        "heartbeat_at_ms": heartbeat_at_ms,
        "config_reload": {
            "status": "applied",
            "applied": {
                "daemon_enabled": expected.enabled,
                "daemon_mode": expected.mode.as_str(),
                "semantic_enabled": expected.semantic_enabled,
                "semantic_executor": expected.semantic_executor.as_str(),
                "semantic_contract_fingerprint": expected.semantic_contract_fingerprint.as_str(),
            },
        },
    })
}

fn lifecycle_response(pid: u32, readiness: &str) -> Value {
    json!({
        "schema_version": 1,
        "ok": true,
        "owner": "daemon",
        "service": "lifecycle",
        "pid": pid,
        "readiness": readiness,
    })
}

fn ready_response(pid: u32) -> Value {
    lifecycle_response(pid, "ready")
}

#[test]
fn blocked_or_fresh_catch_up_without_lifecycle_endpoint_is_pending() {
    let owner = test_daemon_owner("catch-up-owner", 41);
    let expected = test_config();
    let now_ms = 100_000;
    let stale_heartbeat = now_ms - DAEMON_SETUP_HANDOFF_MAX_HEARTBEAT_AGE_MS - 1;

    for (label, heartbeat_at_ms, catch_up) in [
        (
            "blocked",
            stale_heartbeat,
            json!({"status": "running", "progress": {"phase": "blocked"}}),
        ),
        (
            "fresh",
            now_ms,
            json!({"status": "running", "progress": {"phase": "refreshing"}}),
        ),
    ] {
        let mut status = running_status(&owner, &expected, heartbeat_at_ms);
        status["catch_up"] = catch_up;
        let candidate = daemon_handoff_status_observation_from(
            Some(&status),
            Some(&owner),
            Some(owner.pid),
            &expected,
            DaemonReadinessRequirement::Full,
            now_ms,
        );

        assert!(matches!(candidate, DaemonHandoffObservation::Running(_)));
        assert_eq!(
            complete_daemon_handoff_observation(
                candidate,
                Some(&owner),
                Some(&owner),
                DaemonLifecycleEndpointObservation::Unavailable,
            ),
            DaemonHandoffObservation::Pending,
            "{label} catch-up state must not replace a live lifecycle response",
        );
    }
}

#[test]
fn live_ready_endpoint_with_stale_heartbeat_can_succeed() {
    let owner = test_daemon_owner("idle-owner", 42);
    let expected = test_config();
    let now_ms = 100_000;
    let stale_heartbeat = now_ms - DAEMON_SETUP_HANDOFF_MAX_HEARTBEAT_AGE_MS - 1;
    let status = running_status(&owner, &expected, stale_heartbeat);
    let candidate = daemon_handoff_status_observation_from(
        Some(&status),
        Some(&owner),
        Some(owner.pid),
        &expected,
        DaemonReadinessRequirement::Full,
        now_ms,
    );

    assert_eq!(
        complete_daemon_handoff_observation(
            candidate,
            Some(&owner),
            Some(&owner),
            DaemonLifecycleEndpointObservation::Ready,
        ),
        DaemonHandoffObservation::Running(DaemonHandoff {
            pid: owner.pid,
            heartbeat_at_ms: stale_heartbeat,
        })
    );
}

#[test]
fn core_readiness_accepts_semantic_activation_failure_but_full_readiness_does_not() {
    let owner = test_daemon_owner("semantic-degraded-owner", 43);
    let full = test_config();
    let now_ms = 100_000;
    let status = json!({
        "status": "running",
        "pid": owner.pid,
        "started_at_ms": owner.started_at_ms,
        "heartbeat_at_ms": now_ms,
        "config_reload": {
            "status": "activation_failed",
            "requested": {
                "daemon_enabled": full.enabled,
                "daemon_mode": full.mode.as_str(),
                "semantic_enabled": full.semantic_enabled,
                "semantic_executor": full.semantic_executor,
                "semantic_contract_fingerprint": full.semantic_contract_fingerprint,
            },
            "applied": {
                "daemon_enabled": full.enabled,
                "daemon_mode": full.mode.as_str(),
                "semantic_enabled": false,
                "semantic_executor": Value::Null,
                "semantic_contract_fingerprint": Value::Null,
            },
            "last_error": "semantic endpoint unavailable",
        },
    });

    assert!(matches!(
        daemon_handoff_status_observation_from(
            Some(&status),
            Some(&owner),
            Some(owner.pid),
            &full,
            DaemonReadinessRequirement::Core,
            now_ms,
        ),
        DaemonHandoffObservation::Running(_)
    ));
    assert_eq!(
        daemon_handoff_status_observation_from(
            Some(&status),
            Some(&owner),
            Some(owner.pid),
            &full,
            DaemonReadinessRequirement::Full,
            now_ms,
        ),
        DaemonHandoffObservation::Failed("semantic endpoint unavailable".to_owned())
    );
}

#[test]
fn core_readiness_accepts_a_fully_applied_healthy_semantic_runtime() {
    let owner = test_daemon_owner("semantic-healthy-owner", 44);
    let expected = test_config();
    let now_ms = 100_000;
    let status = running_status(&owner, &expected, now_ms);

    assert!(matches!(
        daemon_handoff_status_observation_from(
            Some(&status),
            Some(&owner),
            Some(owner.pid),
            &expected,
            DaemonReadinessRequirement::Core,
            now_ms,
        ),
        DaemonHandoffObservation::Running(_)
    ));
}

#[test]
fn core_readiness_rejects_malformed_semantic_activation_failure_receipts() {
    let owner = test_daemon_owner("semantic-malformed-owner", 45);
    let expected = test_config();
    let now_ms = 100_000;
    let valid = json!({
        "status": "running",
        "pid": owner.pid,
        "started_at_ms": owner.started_at_ms,
        "heartbeat_at_ms": now_ms,
        "config_reload": {
            "status": "activation_failed",
            "requested": {
                "daemon_enabled": expected.enabled,
                "daemon_mode": expected.mode.as_str(),
                "semantic_enabled": expected.semantic_enabled,
                "semantic_executor": expected.semantic_executor,
                "semantic_contract_fingerprint": expected.semantic_contract_fingerprint,
            },
            "applied": {
                "daemon_enabled": expected.enabled,
                "daemon_mode": expected.mode.as_str(),
                "semantic_enabled": false,
                "semantic_executor": Value::Null,
                "semantic_contract_fingerprint": Value::Null,
            },
            "last_error": "semantic endpoint unavailable",
        },
    });
    let mut malformed = Vec::new();

    let mut missing_requested = valid.clone();
    missing_requested["config_reload"]
        .as_object_mut()
        .expect("config reload must be an object")
        .remove("requested");
    malformed.push(missing_requested);

    let mut stale_requested_contract = valid.clone();
    stale_requested_contract["config_reload"]["requested"]["semantic_contract_fingerprint"] =
        json!("stale-contract");
    malformed.push(stale_requested_contract);

    let mut semantic_still_applied = valid.clone();
    semantic_still_applied["config_reload"]["applied"]["semantic_enabled"] = json!(true);
    malformed.push(semantic_still_applied);

    let mut retained_executor = valid.clone();
    retained_executor["config_reload"]["applied"]["semantic_executor"] = json!("builtin");
    malformed.push(retained_executor);

    let mut retained_fingerprint = valid.clone();
    retained_fingerprint["config_reload"]["applied"]["semantic_contract_fingerprint"] =
        json!("stale-contract");
    malformed.push(retained_fingerprint);

    let mut missing_semantic_state = valid;
    missing_semantic_state["config_reload"]["applied"]
        .as_object_mut()
        .expect("applied config must be an object")
        .remove("semantic_enabled");
    malformed.push(missing_semantic_state);

    for status in malformed {
        assert_eq!(
            daemon_handoff_status_observation_from(
                Some(&status),
                Some(&owner),
                Some(owner.pid),
                &expected,
                DaemonReadinessRequirement::Core,
                now_ms,
            ),
            DaemonHandoffObservation::Failed("semantic endpoint unavailable".to_owned()),
            "malformed receipt gained Core readiness authority: {status}",
        );
    }
}

#[test]
fn lifecycle_response_requires_every_strict_field_and_known_active_state() {
    let valid = ready_response(43);
    assert_eq!(
        daemon_lifecycle_response_observation(&valid, 43),
        DaemonLifecycleEndpointObservation::Ready,
    );
    assert_eq!(
        daemon_lifecycle_response_observation(&lifecycle_response(43, "starting"), 43),
        DaemonLifecycleEndpointObservation::Starting,
    );

    let mut invalid_responses = Vec::new();
    for (field, value) in [
        ("schema_version", json!(2)),
        ("ok", json!(false)),
        ("owner", json!("cli")),
        ("service", json!("source_refresh")),
        ("pid", json!(44)),
        ("readiness", json!("stopping")),
    ] {
        let mut response = valid.clone();
        response[field] = value;
        invalid_responses.push(response);
    }
    for field in [
        "schema_version",
        "ok",
        "owner",
        "service",
        "pid",
        "readiness",
    ] {
        let mut response = valid.clone();
        response
            .as_object_mut()
            .expect("test response must be an object")
            .remove(field);
        invalid_responses.push(response);
    }

    for response in invalid_responses {
        assert_eq!(
            daemon_lifecycle_response_observation(&response, 43),
            DaemonLifecycleEndpointObservation::Unavailable,
            "malformed lifecycle response gained authority: {response}",
        );
    }
}

#[test]
fn status_owner_start_time_and_exact_config_including_semantic_contract_are_required_before_probe()
{
    let owner = test_daemon_owner("strict-owner", 44);
    let expected = test_config();
    let status = running_status(&owner, &expected, 50_000);

    assert!(matches!(
        daemon_handoff_status_observation_from(
            Some(&status),
            Some(&owner),
            Some(owner.pid),
            &expected,
            DaemonReadinessRequirement::Full,
            50_000,
        ),
        DaemonHandoffObservation::Running(_)
    ));

    let mut invalid_statuses = Vec::new();
    let mut wrong_pid = status.clone();
    wrong_pid["pid"] = json!(owner.pid + 1);
    invalid_statuses.push(wrong_pid);
    let mut wrong_start = status.clone();
    wrong_start["started_at_ms"] = json!(owner.started_at_ms + 1);
    invalid_statuses.push(wrong_start);
    for (field, value) in [
        ("daemon_enabled", json!(!expected.enabled)),
        ("daemon_mode", json!(DaemonMode::SourceRefreshOnly.as_str())),
        ("semantic_enabled", json!(!expected.semantic_enabled)),
        (
            "semantic_executor",
            json!("https://other-embeddings.example.test/v1/"),
        ),
        (
            "semantic_contract_fingerprint",
            json!("sha256:different-space-at-the-same-endpoint"),
        ),
    ] {
        let mut wrong_config = status.clone();
        wrong_config["config_reload"]["applied"][field] = value;
        invalid_statuses.push(wrong_config);
    }
    let mut missing_executor = status.clone();
    missing_executor["config_reload"]["applied"]
        .as_object_mut()
        .expect("applied config must be an object")
        .remove("semantic_executor");
    invalid_statuses.push(missing_executor);
    let mut missing_contract = status.clone();
    missing_contract["config_reload"]["applied"]
        .as_object_mut()
        .expect("applied config must be an object")
        .remove("semantic_contract_fingerprint");
    invalid_statuses.push(missing_contract);

    for invalid in invalid_statuses {
        let observation = daemon_handoff_status_observation_from(
            Some(&invalid),
            Some(&owner),
            Some(owner.pid),
            &expected,
            DaemonReadinessRequirement::Full,
            50_000,
        );
        assert_eq!(
            complete_daemon_handoff_observation(
                observation,
                Some(&owner),
                Some(&owner),
                DaemonLifecycleEndpointObservation::Ready,
            ),
            DaemonHandoffObservation::Pending,
            "invalid status/config gained readiness authority: {invalid}",
        );
    }
    assert_eq!(
        daemon_handoff_status_observation_from(
            Some(&status),
            None,
            Some(owner.pid),
            &expected,
            DaemonReadinessRequirement::Full,
            50_000,
        ),
        DaemonHandoffObservation::Pending,
    );
}

#[test]
fn owner_replacement_during_probe_rejects_readiness() {
    let owner = test_daemon_owner("probed-owner", 45);
    let expected = test_config();
    let status = running_status(&owner, &expected, 60_000);
    let candidate = daemon_handoff_status_observation_from(
        Some(&status),
        Some(&owner),
        Some(owner.pid),
        &expected,
        DaemonReadinessRequirement::Full,
        60_000,
    );
    let mut replacements = Vec::new();
    let mut changed_owner_id = owner.clone();
    changed_owner_id.owner_id = "replacement-owner".to_owned();
    replacements.push(changed_owner_id);
    let mut changed_pid = owner.clone();
    changed_pid.pid += 1;
    replacements.push(changed_pid);
    let mut changed_start = owner.clone();
    changed_start.started_at_ms += 1;
    replacements.push(changed_start);
    let mut changed_digest = owner.clone();
    changed_digest.binary_sha256 = "fedcba9876543210".to_owned();
    replacements.push(changed_digest);

    for replacement in replacements {
        for endpoint in [
            DaemonLifecycleEndpointObservation::Starting,
            DaemonLifecycleEndpointObservation::Ready,
        ] {
            assert_eq!(
                complete_daemon_handoff_observation(
                    candidate.clone(),
                    Some(&owner),
                    Some(&replacement),
                    endpoint,
                ),
                DaemonHandoffObservation::Pending,
                "changed owner tuple gained lifecycle authority: {replacement:?}",
            );
        }
    }
}

#[test]
fn identity_stable_starting_endpoint_reports_progress_without_readiness() {
    let owner = test_daemon_owner("starting-owner", 46);
    let expected = test_config();
    let status = running_status(&owner, &expected, 61_000);
    let candidate = daemon_handoff_status_observation_from(
        Some(&status),
        Some(&owner),
        Some(owner.pid),
        &expected,
        DaemonReadinessRequirement::Full,
        61_000,
    );

    assert_eq!(
        complete_daemon_handoff_observation(
            candidate,
            Some(&owner),
            Some(&owner),
            DaemonLifecycleEndpointObservation::Starting,
        ),
        DaemonHandoffObservation::Starting,
    );
}

#[test]
fn observation_only_handoff_waits_through_owner_turnover() -> Result<()> {
    let replacement = DaemonHandoff {
        pid: 46,
        heartbeat_at_ms: 61_000,
    };
    let mut observations = [
        DaemonHandoffObservation::Pending,
        DaemonHandoffObservation::Running(replacement),
    ]
    .into_iter();
    let mut observation_count = 0;
    let mut pause_count = 0;

    let observed = wait_for_observed_daemon_handoff_with(
        2,
        || {
            observation_count += 1;
            observations.next().expect("bounded turnover observation")
        },
        || {},
        || pause_count += 1,
    )?;

    assert_eq!(observed, replacement);
    assert_eq!(observation_count, 2);
    assert_eq!(pause_count, 1);
    Ok(())
}

#[test]
fn authenticated_starting_progress_renews_the_handoff_stall_budget() -> Result<()> {
    let ready = DaemonHandoff {
        pid: 47,
        heartbeat_at_ms: 62_000,
    };
    let mut observations = [
        DaemonHandoffObservation::Pending,
        DaemonHandoffObservation::Starting,
        DaemonHandoffObservation::Pending,
        DaemonHandoffObservation::Starting,
        DaemonHandoffObservation::Pending,
        DaemonHandoffObservation::Running(ready),
    ]
    .into_iter();
    let mut renewals = 0;
    let mut pauses = 0;

    let observed = wait_for_observed_daemon_handoff_with(
        2,
        || {
            observations
                .next()
                .expect("progress sequence must terminate")
        },
        || renewals += 1,
        || pauses += 1,
    )?;

    assert_eq!(observed, ready);
    assert_eq!(renewals, 2);
    assert_eq!(pauses, 5);
    Ok(())
}

#[test]
fn terminal_failure_wins_immediately_after_starting_progress() {
    let mut observations = [
        DaemonHandoffObservation::Starting,
        DaemonHandoffObservation::Failed("startup reconciliation failed".to_owned()),
    ]
    .into_iter();
    let mut renewals = 0;
    let mut pauses = 0;

    let error = wait_for_observed_daemon_handoff_with(
        1,
        || {
            observations
                .next()
                .expect("failure sequence must terminate")
        },
        || renewals += 1,
        || pauses += 1,
    )
    .expect_err("terminal startup failure must fail the handoff");

    assert_eq!(error.to_string(), "startup reconciliation failed");
    assert_eq!(renewals, 1);
    assert_eq!(pauses, 1);
}

#[test]
fn child_exit_wins_immediately_during_starting_progress() {
    let mut renewals = 0;
    let mut child_checks = 0;
    let mut pauses = 0;

    let error = wait_for_daemon_handoff_with(
        1,
        || DaemonHandoffObservation::Starting,
        || {
            child_checks += 1;
            Ok(Some("daemon child exited".to_owned()))
        },
        || renewals += 1,
        || pauses += 1,
    )
    .expect_err("child exit must fail an authenticated starting handoff");

    assert_eq!(error.to_string(), "daemon child exited");
    assert_eq!(renewals, 1);
    assert_eq!(child_checks, 1);
    assert_eq!(pauses, 0);
}

#[test]
fn pending_handoff_exhausts_the_exact_stall_budget() {
    let mut observations = 0;
    let mut pauses = 0;

    let error = wait_for_observed_daemon_handoff_with(
        3,
        || {
            observations += 1;
            DaemonHandoffObservation::Pending
        },
        || panic!("pending is not authenticated progress"),
        || pauses += 1,
    )
    .expect_err("unresponsive handoff must remain bounded");

    assert!(error.is::<DaemonHandoffTimeout>());
    assert_eq!(observations, 3);
    assert_eq!(pauses, 2);
}

#[test]
fn fresh_spawned_failure_requires_the_full_active_owner_identity() {
    let expected = test_config();
    let now_ms = 70_000;
    let owner = test_daemon_owner("spawned-failure", 46);
    let fresh_failure = json!({
        "status": "failed",
        "pid": owner.pid,
        "started_at_ms": owner.started_at_ms,
        "heartbeat_at_ms": now_ms,
        "last_error": "query service failed",
    });
    assert_eq!(
        daemon_handoff_status_observation_from(
            Some(&fresh_failure),
            Some(&owner),
            Some(owner.pid),
            &expected,
            DaemonReadinessRequirement::Full,
            now_ms,
        ),
        DaemonHandoffObservation::Failed("query service failed".to_owned()),
    );
    assert_eq!(
        daemon_handoff_status_observation_from(
            Some(&fresh_failure),
            None,
            Some(owner.pid),
            &expected,
            DaemonReadinessRequirement::Full,
            now_ms,
        ),
        DaemonHandoffObservation::Pending,
    );

    let mut stale_failure = fresh_failure.clone();
    stale_failure["heartbeat_at_ms"] =
        json!(now_ms - DAEMON_SETUP_HANDOFF_MAX_HEARTBEAT_AGE_MS - 1);
    assert_eq!(
        daemon_handoff_status_observation_from(
            Some(&stale_failure),
            Some(&owner),
            Some(owner.pid),
            &expected,
            DaemonReadinessRequirement::Full,
            now_ms,
        ),
        DaemonHandoffObservation::Pending,
    );
}

#[test]
fn fresh_existing_owner_failure_matches_full_lifecycle_identity() {
    let owner = test_daemon_owner("failed-owner", 47);
    let expected = test_config();
    let now_ms = 80_000;
    let failure = json!({
        "status": "failed",
        "pid": owner.pid,
        "started_at_ms": owner.started_at_ms,
        "heartbeat_at_ms": now_ms,
        "last_error": "existing daemon failed",
    });

    assert_eq!(
        daemon_handoff_status_observation_from(
            Some(&failure),
            Some(&owner),
            None,
            &expected,
            DaemonReadinessRequirement::Full,
            now_ms,
        ),
        DaemonHandoffObservation::Failed("existing daemon failed".to_owned()),
    );
}

#[test]
fn fresh_failure_from_reused_pid_does_not_match_existing_owner() {
    let owner = test_daemon_owner("current-owner", 48);
    let expected = test_config();
    let now_ms = 90_000;
    let failure = json!({
        "status": "failed",
        "pid": owner.pid,
        "started_at_ms": owner.started_at_ms - 1,
        "heartbeat_at_ms": now_ms,
        "last_error": "previous process failed",
    });

    assert_eq!(
        daemon_handoff_status_observation_from(
            Some(&failure),
            Some(&owner),
            None,
            &expected,
            DaemonReadinessRequirement::Full,
            now_ms,
        ),
        DaemonHandoffObservation::Pending,
    );
}

#[test]
fn recovery_probes_first_then_revalidates_the_full_owner_before_termination() -> Result<()> {
    let owner = test_daemon_owner("unusable-owner", 47);
    let events = RefCell::new(Vec::new());

    let terminated = recover_unusable_daemon_owner_with(
        &owner,
        || {
            events.borrow_mut().push("probe");
            Ok(false)
        },
        || {
            events.borrow_mut().push("revalidate");
            Ok(Some(owner.clone()))
        },
        |owner_id| {
            assert_eq!(owner_id, owner.owner_id.as_str());
            events.borrow_mut().push("terminate");
            Ok(())
        },
        || Ok(()),
    )?;

    assert!(terminated);
    assert_eq!(
        events.borrow().as_slice(),
        &["probe", "revalidate", "terminate"]
    );
    Ok(())
}

#[test]
fn recovery_never_terminates_an_owner_replaced_during_the_probe() -> Result<()> {
    let owner = test_daemon_owner("unusable-owner", 48);
    let mut replacement = owner.clone();
    replacement.binary_sha256 = "replacement-binary-digest".to_owned();
    let events = RefCell::new(Vec::new());

    let terminated = recover_unusable_daemon_owner_with(
        &owner,
        || {
            events.borrow_mut().push("probe");
            Ok(false)
        },
        || {
            events.borrow_mut().push("revalidate");
            Ok(Some(replacement.clone()))
        },
        |_| {
            events.borrow_mut().push("terminate");
            Ok(())
        },
        || Ok(()),
    )?;

    assert!(!terminated);
    assert_eq!(events.borrow().as_slice(), &["probe", "revalidate"]);
    Ok(())
}

#[test]
fn recovery_preserves_a_daemon_with_a_live_usable_endpoint() -> Result<()> {
    let owner = test_daemon_owner("usable-owner", 49);
    let events = RefCell::new(Vec::new());

    let terminated = recover_unusable_daemon_owner_with(
        &owner,
        || {
            events.borrow_mut().push("probe");
            Ok(true)
        },
        || {
            events.borrow_mut().push("revalidate");
            Ok(Some(owner.clone()))
        },
        |_| {
            events.borrow_mut().push("terminate");
            Ok(())
        },
        || Ok(()),
    )?;

    assert!(!terminated);
    assert_eq!(events.borrow().as_slice(), &["probe"]);
    Ok(())
}

#[test]
fn cancellation_after_recovery_probe_prevents_owner_termination() {
    let owner = test_daemon_owner("cancelled-recovery-owner", 50);
    let events = RefCell::new(Vec::new());
    let mut checkpoints = 0;

    let error = recover_unusable_daemon_owner_with(
        &owner,
        || {
            events.borrow_mut().push("probe");
            Ok(false)
        },
        || {
            events.borrow_mut().push("revalidate");
            Ok(Some(owner.clone()))
        },
        |_| {
            events.borrow_mut().push("terminate");
            Ok(())
        },
        || {
            checkpoints += 1;
            if checkpoints == 2 {
                Err(anyhow!("cancelled after recovery probe"))
            } else {
                Ok(())
            }
        },
    )
    .unwrap_err();

    assert_eq!(error.to_string(), "cancelled after recovery probe");
    assert_eq!(events.borrow().as_slice(), &["probe"]);
}

#[test]
fn cancellation_after_mismatch_probe_preserves_the_existing_owner() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let expected = temp.path().join("replacement-ctx");
    fs::write(&expected, b"different binary identity")?;
    let lock = ctx_daemon_runtime::DaemonLock::acquire(temp.path())?
        .expect("test process owns the existing daemon lock");
    let mut checkpoints = 0;

    let error = handoff_mismatched_daemon_owner_with_cancellation(
        &crate::TestHost,
        temp.path(),
        &expected,
        &mut || {
            checkpoints += 1;
            if checkpoints == 2 {
                Err(anyhow!("cancelled after mismatch probe"))
            } else {
                Ok(())
            }
        },
    )
    .unwrap_err();

    assert_eq!(error.to_string(), "cancelled after mismatch probe");
    assert!(daemon_lock_is_active(temp.path()));
    assert_eq!(
        ctx_daemon_runtime::process_state(std::process::id()),
        ctx_daemon_runtime::ProcessState::Running
    );
    drop(lock);
    Ok(())
}

#[test]
fn daemon_handoff_stall_without_authenticated_progress_is_bounded_to_five_seconds() {
    let pauses = DAEMON_SETUP_HANDOFF_STALL_POLL_ATTEMPTS.saturating_sub(1);
    let maximum_wait = DAEMON_UPGRADE_POLL_INTERVAL
        .checked_mul(u32::try_from(pauses).expect("bounded test attempt count"))
        .expect("bounded handoff duration");
    assert_eq!(maximum_wait, Duration::from_secs(5));
    assert_eq!(DAEMON_SETUP_HANDOFF_STALL_TIMEOUT, maximum_wait);
    assert!(DAEMON_HEALTH_TIMEOUT < DAEMON_SETUP_HANDOFF_STALL_TIMEOUT);
}
