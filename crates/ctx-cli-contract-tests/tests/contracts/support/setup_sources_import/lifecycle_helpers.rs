use crate::support::*;
use std::{
    io::Read,
    process::{Child, Command as StdCommand, Stdio},
};

#[cfg(target_os = "linux")]
pub(super) struct FakeSystemdDaemon {
    pid_file: std::path::PathBuf,
    data_root: std::path::PathBuf,
    executable: std::path::PathBuf,
}

#[cfg(target_os = "linux")]
impl Drop for FakeSystemdDaemon {
    fn drop(&mut self) {
        let Some(pid) = fs::read_to_string(&self.pid_file)
            .ok()
            .and_then(|pid| pid.trim().parse::<u32>().ok())
        else {
            return;
        };
        let Some(lock) = fs::read(self.data_root.join("daemon/daemon.lock"))
            .ok()
            .and_then(|body| serde_json::from_slice::<Value>(&body).ok())
        else {
            return;
        };
        let recorded_binary = lock.get("binary").and_then(Value::as_str).map(Path::new);
        let process_binary = fs::read_link(format!("/proc/{pid}/exe")).ok();
        if lock.get("pid").and_then(Value::as_u64) != Some(u64::from(pid))
            || lock.get("data_root").and_then(Value::as_str) != self.data_root.to_str()
            || recorded_binary.and_then(|path| fs::canonicalize(path).ok())
                != fs::canonicalize(&self.executable).ok()
            || process_binary.and_then(|path| fs::canonicalize(path).ok())
                != fs::canonicalize(&self.executable).ok()
        {
            return;
        }
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
    }
}

#[cfg(target_os = "linux")]
pub(super) fn fake_operational_systemd_user_manager(
    temp: &TempDir,
    binary: &std::path::Path,
    managed_root: &std::path::Path,
    clean_exit_before_manager_restart: bool,
) -> (std::path::PathBuf, FakeSystemdDaemon) {
    use std::os::unix::fs::PermissionsExt as _;

    let manager_bin = temp.path().join("fake-systemd-bin");
    fs::create_dir(&manager_bin).unwrap();
    let systemctl = manager_bin.join("systemctl");
    let pid_file = temp.path().join("fake-systemd-main.pid");
    let enabled_file = temp.path().join("fake-systemd-enabled");
    let stdout_file = temp.path().join("fake-systemd-daemon.stdout");
    let stderr_file = temp.path().join("fake-systemd-daemon.stderr");
    let unit_file = temp.path().join(".config/systemd/user/ctx.service");
    fs::write(
        &systemctl,
        format!(
            r#"#!/bin/sh
pid_file='{pid_file}'
enabled_file='{enabled_file}'
unit_file='{unit_file}'
environment_file='{managed_root}/daemon/supervisor-environment.json'
clean_exit_before_manager_restart='{clean_exit_before_manager_restart}'
case "$*" in
  "--user show --property=Version --value")
    printf '255\n'
    exit 0
    ;;
  "--user daemon-reload")
    exit 0
    ;;
  "--user enable ctx.service")
    : > "$enabled_file"
    exit 0
    ;;
  "--user start ctx.service")
    if [ -f '{managed_root}/daemon/daemon.lock' ]; then
      lock_pid=$(sed -n 's/.*"pid"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' '{managed_root}/daemon/daemon.lock' | sed -n '1p')
      if [ -n "$lock_pid" ] && kill -0 "$lock_pid" 2>/dev/null; then
        i=0
        while [ "$i" -lt 200 ]; do
          if [ -S '{managed_root}/daemon/source-refresh.sock' ]; then exit 0; fi
          if ! kill -0 "$lock_pid" 2>/dev/null; then break; fi
          i=$((i + 1))
          sleep 0.05
        done
        exit 0
      fi
      rm -f '{managed_root}/daemon/daemon.lock'
    fi
    nohup setsid /usr/bin/env -i "CTX_INTERNAL_SUPERVISOR_ENVIRONMENT_FILE=$environment_file" '{binary}' --data-root '{managed_root}' daemon run --format=json </dev/null >'{stdout_file}' 2>'{stderr_file}' &
    printf '%s\n' "$!" > "$pid_file"
    i=0
    while [ "$i" -lt 100 ]; do
      if [ -S '{managed_root}/daemon/source-refresh.sock' ] && [ -f '{managed_root}/daemon/daemon.lock' ]; then break; fi
      i=$((i + 1))
      sleep 0.05
    done
    exit 0
    ;;
  "--user restart ctx.service")
    current_pid="$(sed -n '1p' "$pid_file")"
    if [ -f '{managed_root}/daemon/daemon.lock' ]; then
      lock_pid=$(sed -n 's/.*"pid"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' '{managed_root}/daemon/daemon.lock' | sed -n '1p')
      if [ -n "$lock_pid" ]; then current_pid="$lock_pid"; fi
    fi
    if [ -n "$current_pid" ]; then kill "$current_pid" 2>/dev/null || true; fi
    rm -f "$pid_file"
    i=0
    while [ "$i" -lt 100 ] && [ -f '{managed_root}/daemon/daemon.lock' ]; do
      i=$((i + 1))
      sleep 0.05
    done
    if [ "$clean_exit_before_manager_restart" = 1 ]; then
      if grep -Fxq 'Restart=always' "$unit_file"; then
        sleep 0.1
        nohup setsid /usr/bin/env -i "CTX_INTERNAL_SUPERVISOR_ENVIRONMENT_FILE=$environment_file" '{binary}' --data-root '{managed_root}' daemon run --format=json </dev/null >'{stdout_file}' 2>'{stderr_file}' &
        printf '%s\n' "$!" > "$pid_file"
        i=0
        while [ "$i" -lt 100 ]; do
          if [ -S '{managed_root}/daemon/source-refresh.sock' ] && [ -f '{managed_root}/daemon/daemon.lock' ]; then break; fi
          i=$((i + 1))
          sleep 0.05
        done
      fi
      exit 0
    fi
    nohup setsid /usr/bin/env -i "CTX_INTERNAL_SUPERVISOR_ENVIRONMENT_FILE=$environment_file" '{binary}' --data-root '{managed_root}' daemon run --format=json </dev/null >'{stdout_file}' 2>'{stderr_file}' &
    printf '%s\n' "$!" > "$pid_file"
    i=0
    while [ "$i" -lt 100 ]; do
      if [ -S '{managed_root}/daemon/source-refresh.sock' ] && [ -f '{managed_root}/daemon/daemon.lock' ]; then break; fi
      i=$((i + 1))
      sleep 0.05
    done
    exit 0
    ;;
  "--user is-enabled ctx.service")
    if [ -f "$enabled_file" ]; then printf 'enabled\n'; exit 0; fi
    exit 1
    ;;
  "--user is-active ctx.service")
    if [ -f '{managed_root}/daemon/daemon.lock' ]; then
      lock_pid=$(sed -n 's/.*"pid"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' '{managed_root}/daemon/daemon.lock' | sed -n '1p')
      if [ -n "$lock_pid" ] && kill -0 "$lock_pid" 2>/dev/null; then
        printf 'active\n'
        exit 0
      fi
    fi
    exit 1
    ;;
  "--user show ctx.service --property=MainPID --value")
    i=0
    while [ "$i" -lt 100 ]; do
      if [ -f '{managed_root}/daemon/daemon.lock' ]; then
      lock_pid=$(sed -n 's/.*"pid"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' '{managed_root}/daemon/daemon.lock' | sed -n '1p')
        if [ -n "$lock_pid" ]; then printf '%s\n' "$lock_pid"; exit 0; fi
      fi
      i=$((i + 1))
      sleep 0.05
    done
    exit 1
    ;;
  "--user disable --now ctx.service")
    current_pid="$(sed -n '1p' "$pid_file")"
    if [ -f '{managed_root}/daemon/daemon.lock' ]; then
      lock_pid=$(sed -n 's/.*"pid"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' '{managed_root}/daemon/daemon.lock' | sed -n '1p')
      if [ -n "$lock_pid" ]; then current_pid="$lock_pid"; fi
    fi
    if [ -n "$current_pid" ]; then kill "$current_pid" 2>/dev/null || true; fi
    rm -f "$pid_file" "$enabled_file"
    exit 0
    ;;
esac
printf 'unexpected fake systemctl invocation: %s\n' "$*" >&2
exit 2
"#,
            pid_file = pid_file.display(),
            enabled_file = enabled_file.display(),
            unit_file = unit_file.display(),
            clean_exit_before_manager_restart =
                if clean_exit_before_manager_restart { 1 } else { 0 },
            binary = binary.display(),
            managed_root = managed_root.display(),
            stdout_file = stdout_file.display(),
            stderr_file = stderr_file.display(),
        ),
    )
    .unwrap();
    fs::set_permissions(&systemctl, fs::Permissions::from_mode(0o700)).unwrap();
    (
        manager_bin,
        FakeSystemdDaemon {
            pid_file: pid_file.clone(),
            data_root: managed_root.to_path_buf(),
            executable: binary.to_path_buf(),
        },
    )
}

pub(super) struct SourceRefreshDaemon {
    child: Option<Child>,
}

impl Drop for SourceRefreshDaemon {
    fn drop(&mut self) {
        if let Err(error) =
            terminate_and_reap_test_child(&mut self.child, "setup source-refresh daemon")
        {
            if std::thread::panicking() {
                eprintln!("setup daemon teardown also failed: {error}");
            } else {
                panic!("setup daemon teardown failed: {error}");
            }
        }
    }
}

pub(super) fn start_full_source_refresh_daemon(temp: &TempDir) -> SourceRefreshDaemon {
    start_source_refresh_daemon(temp, "full")
}

pub(super) fn start_core_only_source_refresh_daemon(temp: &TempDir) -> SourceRefreshDaemon {
    start_source_refresh_daemon(temp, "source-refresh-only")
}

fn start_source_refresh_daemon(temp: &TempDir, mode: &str) -> SourceRefreshDaemon {
    let data_root = data_root(temp);
    fs::create_dir_all(&data_root).unwrap();
    fs::write(
        data_root.join("config.toml"),
        format!("[daemon]\nenabled = true\nmode = \"{mode}\"\n\n[search]\nsemantic = false\n"),
    )
    .unwrap();
    let binary = copied_ctx_binary(temp);
    let prepared = ctx_from_binary(temp, &binary);
    let mut command = StdCommand::new(prepared.get_program());
    for (name, value) in prepared.get_envs() {
        match value {
            Some(value) => {
                command.env(name, value);
            }
            None => {
                command.env_remove(name);
            }
        }
    }
    command
        .args(["daemon", "run", "--force", "--loop-interval-seconds", "600"])
        .env("CTX_DAEMON_MODE", mode)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let child = command
        .spawn()
        .unwrap_or_else(|error| panic!("start isolated source-refresh daemon: {error}"));
    let mut daemon = SourceRefreshDaemon { child: Some(child) };
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(exit) = daemon.child.as_mut().unwrap().try_wait().unwrap() {
            let mut stderr = String::new();
            daemon
                .child
                .as_mut()
                .unwrap()
                .stderr
                .as_mut()
                .unwrap()
                .read_to_string(&mut stderr)
                .unwrap();
            panic!("{mode} source-refresh daemon exited before becoming ready ({exit}): {stderr}");
        }
        let status = ctx(temp)
            .args(["daemon", "status", "--format=json"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| serde_json::from_slice::<Value>(&output.stdout).ok());
        if status.as_ref().is_some_and(|status| {
            status["daemon"]["running"] == true
                && status["daemon"]["core_refresh_endpoint"]["available"] == true
        }) {
            return daemon;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {mode} source-refresh daemon readiness: {status:#?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

pub(super) fn wait_for_core_generation(temp: &TempDir, generation: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = json_output(ctx(temp).args(["status", "--format=json"]));
        if status["history_epoch"]["status"] == "ready"
            && status["lexical"]["status"] == "ready"
            && status["lexical"]["generation_id"] == generation
        {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for Core generation {generation}: {status:#}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

pub(super) fn ready_setup(temp: &TempDir) -> Value {
    let binary = copied_ctx_binary(temp);
    let mut command = ctx_with_enabled_daemon(temp);
    command.env("CTX_DAEMON_AUTOSTART_EXE", binary);
    json_output(command.args(["setup", "--wait", "--format=json", "--progress", "none"]))
}

pub(super) fn write_large_codex_setup_sessions(
    temp: &TempDir,
    sessions: usize,
    messages_per_session: usize,
    payload_bytes: usize,
) {
    let sessions_dir = temp.path().join(".codex/sessions/2026/07/12");
    fs::create_dir_all(&sessions_dir).unwrap();
    let payload = "provider source checkpoint bounded lexical generation "
        .repeat(payload_bytes / "provider source checkpoint bounded lexical generation ".len() + 1);
    for session_index in 0..sessions {
        let session_id = format!("codex-setup-history-{session_index}");
        let path = sessions_dir.join(format!("{session_id}.jsonl"));
        let mut file = fs::File::create(path).unwrap();
        writeln!(
            file,
            "{}",
            json!({
                "timestamp": "2026-07-12T10:00:00.000Z",
                "type": "session_meta",
                "payload": {
                    "id": session_id,
                    "timestamp": "2026-07-12T10:00:00.000Z",
                    "cwd": "/repo/setup",
                    "originator": "codex-cli",
                    "cli_version": "0.200.0",
                    "source": "cli",
                    "model_provider": "openai"
                }
            })
        )
        .unwrap();
        for message_index in 0..messages_per_session {
            writeln!(
                file,
                "{}",
                json!({
                    "timestamp": "2026-07-12T10:00:01.000Z",
                    "type": "response_item",
                    "payload": {
                        "type": "message",
                        "role": "user",
                        "content": [{
                            "type": "input_text",
                            "text": format!(
                                "codex-setup-history session {session_index} message {message_index} {payload}"
                            )
                        }]
                    }
                })
            )
            .unwrap();
        }
    }
}

pub(super) fn write_large_hermes_setup_db(temp: &TempDir, messages: usize, payload_bytes: usize) {
    let hermes_dir = temp.path().join(".hermes");
    fs::create_dir_all(&hermes_dir).unwrap();
    let mut conn = Connection::open(hermes_dir.join("state.db")).unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (
            id TEXT PRIMARY KEY,
            source TEXT NOT NULL,
            started_at REAL NOT NULL
        );
        CREATE TABLE messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL,
            role TEXT NOT NULL,
            content TEXT,
            timestamp REAL NOT NULL,
            active INTEGER NOT NULL DEFAULT 1,
            compacted INTEGER NOT NULL DEFAULT 0
        );
        INSERT INTO sessions VALUES ('hermes-setup-current', 'acp', 1782259200.0);",
    )
    .unwrap();
    let payload = "provider import Core recovery bounded checkpoint "
        .repeat(payload_bytes / "provider import Core recovery bounded checkpoint ".len() + 1);
    let transaction = conn.transaction().unwrap();
    for index in 0..messages {
        transaction
            .execute(
                "INSERT INTO messages (session_id, role, content, timestamp)
                 VALUES ('hermes-setup-current', ?1, ?2, ?3)",
                params![
                    if index % 2 == 0 { "user" } else { "assistant" },
                    format!("hermes-setup-current message {index} {payload}"),
                    1782259201.0 + index as f64,
                ],
            )
            .unwrap();
    }
    transaction.commit().unwrap();
}
