pub(super) fn daemon_autostart_exe() -> Result<PathBuf> {
    env::var("CTX_DAEMON_AUTOSTART_EXE")
        .ok()
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(|| env::current_exe().context("resolve ctx daemon autostart executable"))
}

pub(super) fn write_daemon_autostart_status(
    data_root: &Path,
    trigger: DaemonTriggerCommandArg,
    status: &str,
    reason: Option<&str>,
    last_error: Option<String>,
    pid: Option<u32>,
) -> Result<()> {
    let now = utc_now().timestamp_millis();
    write_daemon_status(
        data_root,
        &compact_json(json!({
            "schema_version": 1,
            "status": status,
            "reason": reason,
            "pid": pid,
            "started_at_ms": Value::Null,
            "heartbeat_at_ms": now,
            "finished_at_ms": now,
            "start_mode": DaemonStartModeArg::Auto.as_str(),
            "trigger_command": trigger.as_str(),
            "last_error": last_error,
        })),
    )
}

pub(super) fn daemon_autostart_u64_env(name: &str, default: u64, max: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .map(|value| value.min(max))
        .unwrap_or(default)
}

pub(crate) fn maybe_autostart_daemon(
    data_root: &Path,
    config: &AppConfig,
    trigger: DaemonTriggerCommandArg,
    json_output: bool,
) {
    maybe_autostart_daemon_inner(data_root, config, trigger, json_output, false);
}

pub(crate) fn maybe_autostart_daemon_for_search(data_root: &Path, config: &AppConfig) {
    maybe_autostart_daemon_inner(
        data_root,
        config,
        DaemonTriggerCommandArg::Search,
        false,
        true,
    );
}

pub(super) fn maybe_autostart_daemon_inner(
    data_root: &Path,
    config: &AppConfig,
    trigger: DaemonTriggerCommandArg,
    json_output: bool,
    allow_json_output: bool,
) {
    if semantic_env_flag(DAEMON_BACKGROUND_CHILD_ENV) {
        return;
    }
    if !database_path(data_root.to_path_buf()).exists() {
        return;
    }
    if !config.daemon.enabled {
        return;
    }
    if semantic_env_flag(DAEMON_AUTOSTART_OFF_ENV) {
        return;
    }
    if json_output && !allow_json_output {
        return;
    }
    if semantic_env_flag("CI") {
        return;
    }
    let lock_path = daemon_lock_path(data_root);
    if lock_path.exists() && !daemon_lock_is_stale(&lock_path) {
        return;
    }
    let exe = match daemon_autostart_exe() {
        Ok(exe) => exe,
        Err(error) => {
            let _ = write_daemon_autostart_status(
                data_root,
                trigger,
                "failed",
                Some("current_exe"),
                Some(format!("{error:#}")),
                None,
            );
            return;
        }
    };
    let idle_exit = daemon_autostart_u64_env(
        "CTX_DAEMON_AUTOSTART_IDLE_EXIT_SECONDS",
        DAEMON_AUTOSTART_IDLE_EXIT_SECONDS_DEFAULT,
        DAEMON_IDLE_EXIT_SECONDS_CAP,
    );
    let loop_interval = daemon_autostart_u64_env(
        "CTX_DAEMON_AUTOSTART_LOOP_INTERVAL_SECONDS",
        DAEMON_AUTOSTART_LOOP_INTERVAL_SECONDS_DEFAULT,
        3_600,
    );
    match daemon_autostart_command(&exe, data_root, trigger, idle_exit, loop_interval).spawn() {
        Ok(_child) => {}
        Err(error) => {
            let _ = write_daemon_autostart_status(
                data_root,
                trigger,
                "failed",
                Some("spawn_failed"),
                Some(error.to_string()),
                None,
            );
        }
    }
}

fn daemon_autostart_command(
    exe: &Path,
    data_root: &Path,
    trigger: DaemonTriggerCommandArg,
    idle_exit: u64,
    loop_interval: u64,
) -> Command {
    let mut command = Command::new(exe);
    command
        .arg("--data-root")
        .arg(data_root)
        .arg("daemon")
        .arg("run")
        .arg("--idle-exit-seconds")
        .arg(idle_exit.to_string())
        .arg("--loop-interval-seconds")
        .arg(loop_interval.to_string())
        .arg("--start-mode")
        .arg(DaemonStartModeArg::Auto.as_str())
        .arg("--trigger-command")
        .arg(trigger.as_str())
        .arg("--json")
        .env(DAEMON_BACKGROUND_CHILD_ENV, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

#[cfg(test)]
mod telemetry_tests {
    use super::*;

    #[test]
    fn autostart_child_inherits_effective_analytics_policy() {
        let command = daemon_autostart_command(
            Path::new("ctx"),
            Path::new("/tmp/ctx-daemon-telemetry-test"),
            DaemonTriggerCommandArg::Search,
            5,
            5,
        );
        let env = command
            .get_envs()
            .map(|(key, value)| (key.to_owned(), value.map(ToOwned::to_owned)))
            .collect::<Vec<_>>();
        assert!(env.iter().any(|(key, value)| {
            key == DAEMON_BACKGROUND_CHILD_ENV
                && value.as_deref() == Some(std::ffi::OsStr::new("1"))
        }));
        assert!(env
            .iter()
            .all(|(key, _)| key != std::ffi::OsStr::new("CTX_ANALYTICS_ENABLED")));
    }
}
use std::{
    env,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result};
use ctx_history_core::{database_path, utc_now};
use serde_json::{json, Value};

use crate::{compact_json, config::AppConfig, DaemonStartModeArg, DaemonTriggerCommandArg};

use super::{
    health_search::semantic_env_flag,
    paths_status::{daemon_lock_is_stale, daemon_lock_path, write_daemon_status},
    runtime_limits::{
        DAEMON_AUTOSTART_IDLE_EXIT_SECONDS_DEFAULT, DAEMON_AUTOSTART_LOOP_INTERVAL_SECONDS_DEFAULT,
        DAEMON_AUTOSTART_OFF_ENV, DAEMON_BACKGROUND_CHILD_ENV, DAEMON_IDLE_EXIT_SECONDS_CAP,
    },
};
