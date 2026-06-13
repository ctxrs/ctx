use super::launch::{daemon_env_unset_args, strip_automation_env};
use super::path_env::{
    build_effective_daemon_path, extract_shell_path, parse_local_daemon_path_probe_output,
    probe_local_daemon_path_via_shell, read_login_shell_path, resolve_daemon_path_env,
    DAEMON_PATH_SENTINEL_BEGIN, DAEMON_PATH_SENTINEL_END, LOCAL_DAEMON_PATH_PROBE_END,
    LOCAL_DAEMON_PATH_PROBE_START,
};
use super::resources::{configured_bundle_dir, select_bundle_dir_path, select_optional_bin_path};
use super::*;
use crate::desktop_local_daemon::{
    apply_validated_local_connection, connect_local_with_sources, lock_local_connect_gate,
};
use crate::desktop_ssh::normalized_ssh_config_override;
use std::sync::{Mutex, OnceLock};

static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn env_lock() -> &'static Mutex<()> {
    ENV_LOCK.get_or_init(|| Mutex::new(()))
}

struct EnvVarGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let previous = std::env::var_os(key);
        // SAFETY: Tests mutate process env serially under ENV_LOCK.
        unsafe { std::env::set_var(key, value) };
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.as_ref() {
            // SAFETY: Tests mutate process env serially under ENV_LOCK.
            unsafe { std::env::set_var(self.key, previous) };
        } else {
            // SAFETY: Tests mutate process env serially under ENV_LOCK.
            unsafe { std::env::remove_var(self.key) };
        }
    }
}

#[test]
fn ssh_config_override_normalization() {
    assert_eq!(
        normalized_ssh_config_override(Some(" /tmp/ctx-fixture-ssh-config ")),
        Some("/tmp/ctx-fixture-ssh-config".to_string())
    );
    assert_eq!(normalized_ssh_config_override(Some("   ")), None);
}

#[test]
fn daemon_env_unset_args_match_blocklist() {
    let rendered = daemon_env_unset_args()
        .into_iter()
        .map(|arg| arg.to_string_lossy().to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        rendered,
        vec![
            "-u".to_string(),
            "AUTOMATION_LIBRARY_PATH".to_string(),
            "-u".to_string(),
            "AUTOMATION_PORT".to_string(),
            "-u".to_string(),
            "REMOTE_WEBDRIVER_URL".to_string(),
            "-u".to_string(),
            "TAURI_DRIVER_PORT".to_string(),
            "-u".to_string(),
            "TEST_RUNNER_BACKEND_PORT".to_string(),
        ]
    );
}

#[test]
#[cfg(unix)]
fn strip_automation_env_removes_desktop_driver_vars_from_local_daemon_children() {
    let _guard = env_lock().lock().expect("env lock poisoned");
    let _automation_library_path = EnvVarGuard::set("AUTOMATION_LIBRARY_PATH", "/tmp/bindings");
    let _automation_port = EnvVarGuard::set("AUTOMATION_PORT", "17643");
    let _remote_webdriver_url = EnvVarGuard::set("REMOTE_WEBDRIVER_URL", "http://127.0.0.1:3000");
    let _tauri_driver_port = EnvVarGuard::set("TAURI_DRIVER_PORT", "4444");
    let _test_runner_backend_port = EnvVarGuard::set("TEST_RUNNER_BACKEND_PORT", "3000");
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(
        "printf '%s|%s|%s|%s|%s' \
         \"$AUTOMATION_LIBRARY_PATH\" \
         \"$AUTOMATION_PORT\" \
         \"$REMOTE_WEBDRIVER_URL\" \
         \"$TAURI_DRIVER_PORT\" \
         \"$TEST_RUNNER_BACKEND_PORT\"",
    );
    strip_automation_env(&mut cmd);
    let output = cmd.output().expect("run env-strip probe");
    assert!(
        output.status.success(),
        "env-strip probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "||||",
        "local daemon children should not inherit desktop automation driver env"
    );
}

#[test]
fn select_optional_bin_path_prefers_bundled_avf_helper_for_macos_debug_bundles() {
    let bundled = PathBuf::from("/tmp/ctx.app/Contents/Resources/ctx-avf-linux-helper");
    let dev = PathBuf::from("/tmp/dev-bin/ctx-avf-linux-helper");
    assert_eq!(
        select_optional_bin_path(
            "ctx-avf-linux-helper",
            Some(bundled.clone()),
            Some(dev.clone()),
            true,
            true,
        ),
        Some(bundled)
    );
    let bundled_mcp = PathBuf::from("/tmp/bundle/ctx-mcp");
    let dev_mcp = PathBuf::from("/tmp/dev-bin/ctx-mcp");
    assert_eq!(
        select_optional_bin_path(
            "ctx-mcp",
            Some(bundled_mcp),
            Some(dev_mcp.clone()),
            true,
            true
        ),
        Some(dev_mcp)
    );
}

#[test]
fn select_bundle_dir_path_prefers_configured_override() {
    let configured = PathBuf::from("/tmp/configured-bundle");
    let bundled = PathBuf::from("/tmp/bundled-bundle");
    let dev = PathBuf::from("/tmp/dev-bundle");
    assert_eq!(
        select_bundle_dir_path(
            Some(configured.clone()),
            Some(bundled.clone()),
            Some(dev.clone()),
        ),
        Some(configured)
    );
    assert_eq!(
        select_bundle_dir_path(None, Some(bundled.clone()), Some(dev.clone())),
        Some(bundled)
    );
    assert_eq!(
        select_bundle_dir_path(None, None, Some(dev.clone())),
        Some(dev)
    );
}

#[test]
fn configured_bundle_dir_uses_existing_ctx_bundle_dir_override() {
    let _guard = env_lock().lock().expect("env lock poisoned");
    let temp = std::env::temp_dir().join(format!("ctx-bundle-dir-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp).expect("create temp bundle dir");
    let _bundle_dir = EnvVarGuard::set(DESKTOP_BUNDLE_DIR_ENV, temp.as_os_str());
    assert_eq!(configured_bundle_dir(), Some(temp.clone()));
    std::fs::remove_dir_all(temp).ok();
}

#[test]
fn configured_bundle_dir_ignores_missing_ctx_bundle_dir_override() {
    let _guard = env_lock().lock().expect("env lock poisoned");
    let missing = std::env::temp_dir().join(format!("ctx-missing-bundle-{}", uuid::Uuid::new_v4()));
    let _bundle_dir = EnvVarGuard::set(DESKTOP_BUNDLE_DIR_ENV, missing.as_os_str());
    assert_eq!(configured_bundle_dir(), None);
}

#[test]
fn parse_local_daemon_path_probe_output_extracts_marker_payload() {
    let output = format!(
        "noise before\n{start}/home/test/.local/bin:/usr/bin{end}\nnoise after\n",
        start = LOCAL_DAEMON_PATH_PROBE_START,
        end = LOCAL_DAEMON_PATH_PROBE_END,
    );
    assert_eq!(
        parse_local_daemon_path_probe_output(&output),
        Some("/home/test/.local/bin:/usr/bin".to_string())
    );
    assert_eq!(
        parse_local_daemon_path_probe_output("missing markers"),
        None
    );
}

#[cfg(unix)]
fn write_shell_probe_script(contents: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let path = std::env::temp_dir().join(format!("ctx-shell-probe-{}", uuid::Uuid::new_v4()));
    std::fs::write(&path, contents).expect("write shell probe script");
    let mut perms = std::fs::metadata(&path)
        .expect("stat shell probe script")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod shell probe script");
    path
}

#[test]
#[cfg(unix)]
fn probe_local_daemon_path_via_shell_reads_marker_payload() {
    let shell_path = write_shell_probe_script(&format!(
        "#!/bin/sh\nprintf 'boot noise\\n'\nprintf '%s/tmp/ctx-user-bin:%s%s\\n' '{start}' '/usr/bin' '{end}'\n",
        start = LOCAL_DAEMON_PATH_PROBE_START,
        end = LOCAL_DAEMON_PATH_PROBE_END,
    ));
    let parsed = probe_local_daemon_path_via_shell(&shell_path);
    assert_eq!(parsed, Some("/tmp/ctx-user-bin:/usr/bin".to_string()));
    std::fs::remove_file(shell_path).ok();
}

#[test]
#[cfg(unix)]
fn probe_local_daemon_path_via_shell_returns_none_without_marker() {
    let shell_path = write_shell_probe_script("#!/bin/sh\nprintf '/usr/bin\\n'\n");
    let parsed = probe_local_daemon_path_via_shell(&shell_path);
    assert_eq!(parsed, None);
    std::fs::remove_file(shell_path).ok();
}

#[test]
fn replacement_validation_failure_keeps_existing_active_connection() {
    let state = ConnectionManager::default();
    state.set_local_attached(
        "http://127.0.0.1:4399".to_string(),
        "existing-token".to_string(),
        None,
        LocalConnectionSource::ExistingCompatibleDaemon,
    );

    let err =
        apply_validated_local_connection(&state, Err(anyhow!("spawn validation failed")), None)
            .expect_err("spawn failure should not replace an existing healthy connection");
    assert!(format!("{err:#}").contains("spawn validation failed"));

    let info = state.info();
    assert!(matches!(info.kind, DesktopConnectionKind::Local));
    assert_eq!(info.base_url.as_deref(), Some("http://127.0.0.1:4399"));
    assert_eq!(info.token.as_deref(), Some("existing-token"));
}

#[test]
fn local_connect_gate_serializes_callers() {
    let first_guard = lock_local_connect_gate().expect("lock first gate holder");
    let ready = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let entered = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let ready_for_thread = std::sync::Arc::clone(&ready);
    let entered_for_thread = std::sync::Arc::clone(&entered);
    let handle = std::thread::spawn(move || {
        ready_for_thread.store(true, std::sync::atomic::Ordering::SeqCst);
        let _second_guard = lock_local_connect_gate().expect("lock second gate holder");
        entered_for_thread.store(true, std::sync::atomic::Ordering::SeqCst);
    });

    while !ready.load(std::sync::atomic::Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(80));
    assert!(
        !entered.load(std::sync::atomic::Ordering::SeqCst),
        "second caller should block while the shared local-connect gate is held"
    );

    drop(first_guard);
    handle.join().expect("join gate waiter");
    assert!(
        entered.load(std::sync::atomic::Ordering::SeqCst),
        "second caller should proceed once the shared local-connect gate is released"
    );
}

#[cfg(unix)]
fn pid_is_alive(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[cfg(unix)]
fn wait_for_pid_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !pid_is_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(80));
    }
    !pid_is_alive(pid)
}

#[test]
#[cfg(unix)]
fn desktop_connect_local_spawn_failure_preserves_existing_owned_connection() {
    let state = ConnectionManager::default();
    let child = Command::new("sh")
        .arg("-c")
        .arg("sleep 60")
        .spawn()
        .expect("spawn sleep child");
    let child_pid = child.id();
    assert!(
        pid_is_alive(child_pid),
        "owned child should be alive before replacement attempt"
    );
    state.set_local(
        "http://127.0.0.1:4399".to_string(),
        "existing-token".to_string(),
        child,
        false,
    );

    let resolve_existing_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let err = connect_local_with_sources(
        &state,
        |_, _| false,
        || Ok(None),
        |_, _| Ok(()),
        {
            let resolve_existing_calls = std::sync::Arc::clone(&resolve_existing_calls);
            move || {
                resolve_existing_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(None)
            }
        },
        || Err(anyhow!("spawn validation failed")),
    )
    .expect_err("spawn failure should preserve the existing owned connection");

    assert!(format!("{err:#}").contains("spawn validation failed"));
    assert_eq!(
        resolve_existing_calls.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "spawn failure path should retry existing-daemon resolution before returning"
    );
    assert!(
        pid_is_alive(child_pid),
        "existing owned daemon child should remain alive after replacement failure"
    );

    let info = state.info();
    assert!(matches!(info.kind, DesktopConnectionKind::Local));
    assert_eq!(info.base_url.as_deref(), Some("http://127.0.0.1:4399"));
    assert_eq!(info.token.as_deref(), Some("existing-token"));

    state.disconnect();
    assert!(
        wait_for_pid_exit(child_pid, Duration::from_secs(3)),
        "owned child should be cleaned up during test teardown"
    );
}

#[test]
fn desktop_connect_local_lock_race_waits_for_existing_daemon_to_become_visible() {
    let state = ConnectionManager::default();
    let resolve_existing_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let info = connect_local_with_sources(
        &state,
        |_, _| false,
        || Ok(None),
        |_, _| Ok(()),
        {
            let resolve_existing_calls = std::sync::Arc::clone(&resolve_existing_calls);
            move || {
                let call_index =
                    resolve_existing_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if call_index < 2 {
                    return Ok(None);
                }
                Ok(Some((
                    "http://127.0.0.1:4401".to_string(),
                    "lock-race-token".to_string(),
                    None,
                )))
            }
        },
        || {
            Err(anyhow!(
                "ctx daemon already running (lockfile /tmp/ctx/daemon.lock)"
            ))
        },
    )
    .expect("lock race should attach to the daemon that won startup");

    assert!(
        resolve_existing_calls.load(std::sync::atomic::Ordering::SeqCst) >= 3,
        "spawn lock race should wait for existing daemon auth/health to become visible"
    );
    assert!(matches!(info.kind, DesktopConnectionKind::Local));
    assert_eq!(info.base_url.as_deref(), Some("http://127.0.0.1:4401"));
    assert_eq!(info.token.as_deref(), Some("lock-race-token"));
}

#[test]
#[cfg(unix)]
fn desktop_connect_local_spawn_race_reattaches_same_owned_local_daemon_without_killing_it() {
    let state = ConnectionManager::default();
    let child = Command::new("sh")
        .arg("-c")
        .arg("sleep 60")
        .spawn()
        .expect("spawn local child placeholder");
    let child_pid = child.id();
    assert!(
        pid_is_alive(child_pid),
        "existing local child should start alive"
    );
    state.set_local(
        "http://127.0.0.1:4301".to_string(),
        "same-token".to_string(),
        child,
        false,
    );

    let resolve_existing_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let info = connect_local_with_sources(
        &state,
        |_, _| false,
        || Ok(None),
        |_, _| Ok(()),
        {
            let resolve_existing_calls = std::sync::Arc::clone(&resolve_existing_calls);
            move || {
                let call_index =
                    resolve_existing_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if call_index == 0 {
                    return Ok(None);
                }
                Ok(Some((
                    "http://127.0.0.1:4301".to_string(),
                    "same-token".to_string(),
                    Some(child_pid),
                )))
            }
        },
        || Err(anyhow!("spawn lost race")),
    )
    .expect("same-daemon handoff should preserve the owned local daemon");

    assert_eq!(
        resolve_existing_calls.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "spawn failure path should retry existing-daemon resolution before reattaching"
    );
    assert!(
        pid_is_alive(child_pid),
        "same-daemon handoff must not kill the process being reattached"
    );
    assert!(matches!(info.kind, DesktopConnectionKind::Local));
    assert_eq!(info.base_url.as_deref(), Some("http://127.0.0.1:4301"));
    assert_eq!(info.token.as_deref(), Some("same-token"));

    state.disconnect();
    assert!(
        wait_for_pid_exit(child_pid, Duration::from_secs(3)),
        "reattached owned local daemon should still be terminated on disconnect"
    );
}

#[test]
#[cfg(unix)]
fn desktop_connect_local_spawn_race_switches_to_validated_local_daemon_over_existing_local_connection(
) {
    let state = ConnectionManager::default();
    let child = Command::new("sh")
        .arg("-c")
        .arg("sleep 60")
        .spawn()
        .expect("spawn local child placeholder");
    let child_pid = child.id();
    assert!(
        pid_is_alive(child_pid),
        "existing local child should start alive"
    );
    state.set_local(
        "http://127.0.0.1:4301".to_string(),
        "stale-token".to_string(),
        child,
        false,
    );

    let resolve_existing_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let info = connect_local_with_sources(
        &state,
        |_, _| false,
        || Ok(None),
        |_, _| Ok(()),
        {
            let resolve_existing_calls = std::sync::Arc::clone(&resolve_existing_calls);
            move || {
                let call_index =
                    resolve_existing_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if call_index == 0 {
                    return Ok(None);
                }
                Ok(Some((
                    "http://127.0.0.1:4399".to_string(),
                    "replacement-token".to_string(),
                    None,
                )))
            }
        },
        || Err(anyhow!("spawn lost race")),
    )
    .expect("validated local fallback should replace the stale local connection");

    assert_eq!(
        resolve_existing_calls.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "spawn failure path should retry existing-daemon resolution before reattaching"
    );
    assert!(
        wait_for_pid_exit(child_pid, Duration::from_secs(3)),
        "stale local child should be cleaned up once the validated local fallback replaces it"
    );
    assert!(matches!(info.kind, DesktopConnectionKind::Local));
    assert_eq!(info.base_url.as_deref(), Some("http://127.0.0.1:4399"));
    assert_eq!(info.token.as_deref(), Some("replacement-token"));
}

#[test]
#[cfg(unix)]
fn desktop_connect_local_spawn_race_switches_to_validated_local_daemon_over_ssh_connection() {
    let state = ConnectionManager::default();
    let tunnel = Command::new("sh")
        .arg("-c")
        .arg("sleep 60")
        .spawn()
        .expect("spawn ssh tunnel placeholder");
    let tunnel_pid = tunnel.id();
    assert!(
        pid_is_alive(tunnel_pid),
        "ssh tunnel placeholder should start alive"
    );
    state.set_ssh(
        "http://127.0.0.1:5401".to_string(),
        Some("ssh-token".to_string()),
        tunnel,
        "example.test".to_string(),
        Some("dev".to_string()),
        2222,
        None,
        SshRuntimeMetadata {
            managed_ctx_bin: "~/.ctx/bin/ctx".to_string(),
            active_ctx_bin: Some("~/.ctx/bin/ctx".to_string()),
            ssh_password_once: None,
            admin_password_once: None,
        },
    );

    let resolve_existing_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let info = connect_local_with_sources(
        &state,
        |_, _| false,
        || Ok(None),
        |_, _| Ok(()),
        {
            let resolve_existing_calls = std::sync::Arc::clone(&resolve_existing_calls);
            move || {
                let call_index =
                    resolve_existing_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if call_index == 0 {
                    return Ok(None);
                }
                Ok(Some((
                    "http://127.0.0.1:4399".to_string(),
                    "local-token".to_string(),
                    None,
                )))
            }
        },
        || Err(anyhow!("spawn lost race")),
    )
    .expect("validated local fallback should replace an active non-local connection");

    assert_eq!(
        resolve_existing_calls.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "spawn failure path should retry existing-daemon resolution before attaching"
    );
    assert!(
        wait_for_pid_exit(tunnel_pid, Duration::from_secs(3)),
        "ssh tunnel placeholder should be cleaned up when the validated local daemon replaces it"
    );
    assert!(matches!(info.kind, DesktopConnectionKind::Local));
    assert_eq!(info.base_url.as_deref(), Some("http://127.0.0.1:4399"));
    assert_eq!(info.token.as_deref(), Some("local-token"));
}

#[test]
fn build_effective_daemon_path_merges_current_shell_and_common_dirs() {
    let home_dir =
        std::env::temp_dir().join(format!("ctx-daemon-path-home-{}", uuid::Uuid::new_v4()));
    let current = std::ffi::OsString::from("/usr/bin:/bin");
    let shell = std::ffi::OsString::from("/tmp/custom/bin:/usr/bin");

    let merged = build_effective_daemon_path(
        Some(current.as_os_str()),
        Some(shell.as_os_str()),
        Some(home_dir.as_path()),
    )
    .expect("merged path");
    let parts = std::env::split_paths(&merged).collect::<Vec<_>>();

    assert_eq!(parts[0], PathBuf::from("/usr/bin"));
    assert_eq!(parts[1], PathBuf::from("/bin"));
    assert!(parts.contains(&PathBuf::from("/tmp/custom/bin")));
    assert!(parts.contains(&home_dir.join(".local").join("bin")));
    assert!(parts.contains(&PathBuf::from("/opt/homebrew/bin")));
    assert_eq!(
        parts
            .iter()
            .filter(|entry| **entry == PathBuf::from("/usr/bin"))
            .count(),
        1
    );
}

#[test]
fn extract_shell_path_reads_sentinel_payload() {
    let raw = b"noise before __CTX_DAEMON_PATH_BEGIN__/tmp/alpha:/tmp/beta__CTX_DAEMON_PATH_END__ trailing";
    let parsed = extract_shell_path(raw).expect("parsed path");
    assert_eq!(parsed, std::ffi::OsString::from("/tmp/alpha:/tmp/beta"));
}

#[test]
#[cfg(unix)]
fn read_login_shell_path_uses_shell_output_markers() {
    use std::os::unix::fs::PermissionsExt;

    let temp = std::env::temp_dir().join(format!("ctx-daemon-shell-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp).expect("create temp dir");
    let shell_path = temp.join("fake-shell");
    std::fs::write(
        &shell_path,
        format!(
            "#!/bin/sh\nprintf 'prefix {DAEMON_PATH_SENTINEL_BEGIN}/tmp/fake-cursor:/usr/bin{DAEMON_PATH_SENTINEL_END} suffix'\n"
        ),
    )
    .expect("write fake shell");
    let mut perms = std::fs::metadata(&shell_path)
        .expect("stat fake shell")
        .permissions();
    perms.set_mode(perms.mode() | 0o111);
    std::fs::set_permissions(&shell_path, perms).expect("chmod fake shell");

    let resolved = read_login_shell_path(&shell_path).expect("resolved shell path");
    assert_eq!(
        resolved,
        std::ffi::OsString::from("/tmp/fake-cursor:/usr/bin")
    );
    std::fs::remove_dir_all(&temp).ok();
}

#[test]
#[cfg(unix)]
fn resolve_daemon_path_env_prefers_current_path_and_shell_discovery() {
    use std::os::unix::fs::PermissionsExt;

    let _guard = env_lock().lock().expect("lock env");
    let temp = std::env::temp_dir().join(format!("ctx-daemon-shell-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp).expect("create temp dir");
    let shell_path = temp.join("fake-shell");
    std::fs::write(
        &shell_path,
        format!(
            "#!/bin/sh\nprintf '{DAEMON_PATH_SENTINEL_BEGIN}/tmp/fake-cursor:/usr/bin{DAEMON_PATH_SENTINEL_END}'\n"
        ),
    )
    .expect("write fake shell");
    let mut perms = std::fs::metadata(&shell_path)
        .expect("stat fake shell")
        .permissions();
    perms.set_mode(perms.mode() | 0o111);
    std::fs::set_permissions(&shell_path, perms).expect("chmod fake shell");

    let _shell = EnvVarGuard::set("SHELL", shell_path.as_os_str());
    let _path = EnvVarGuard::set("PATH", std::ffi::OsStr::new("/usr/bin:/bin"));
    let _home = EnvVarGuard::set("HOME", temp.as_os_str());

    let resolved = resolve_daemon_path_env().expect("resolved daemon path");
    let parts = std::env::split_paths(&resolved).collect::<Vec<_>>();
    assert_eq!(parts[0], PathBuf::from("/usr/bin"));
    assert_eq!(parts[1], PathBuf::from("/bin"));
    assert!(parts.contains(&PathBuf::from("/tmp/fake-cursor")));
    std::fs::remove_dir_all(&temp).ok();
}
