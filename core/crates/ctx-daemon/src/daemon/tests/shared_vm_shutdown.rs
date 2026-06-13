use super::*;

#[cfg(target_os = "macos")]
pub(super) struct EnvGuard {
    key: &'static str,
    prev: Option<String>,
}

#[cfg(target_os = "macos")]
impl EnvGuard {
    pub(super) fn set(key: &'static str, value: &str) -> Self {
        let prev = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, prev }
    }

    pub(super) fn remove(key: &'static str) -> Self {
        let prev = std::env::var(key).ok();
        std::env::remove_var(key);
        Self { key, prev }
    }
}

#[cfg(target_os = "macos")]
impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(value) = self.prev.take() {
            std::env::set_var(self.key, value);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

#[cfg(target_os = "macos")]
pub(super) fn write_shared_vm_shutdown_helper(dir: &Path) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;

    let helper_path = dir.join("ctx-avf-linux-helper-daemon-shutdown-test.sh");
    let log_path = dir.join("ctx-avf-linux-helper-daemon-shutdown.log");
    let state_path = dir.join("ctx-avf-linux-helper-daemon-shutdown.state");
    let script = format!(
        r#"#!/bin/sh
LOG="{log}"
STATE="{state}"
printf '%s\n' "$*" >> "$LOG"
case "$1" in
  probe)
    printf '{{"protocol_version":1,"protocol_schema":"ctx.avf_linux_helper.v1","helper_version":"test","host_os":"darwin","host_arch":"arm64","supported":true,"save_restore_supported":true,"rosetta_supported":false,"notes":[]}}\n'
    ;;
  workspace-vm-state)
    if [ -f "$STATE" ] && [ "$(cat "$STATE")" = "stopped" ]; then
      printf '{{"protocol_version":1,"protocol_schema":"ctx.avf_linux_helper.v1","state":"stopped","vm_root":"%s","logs_root":"%s","state_path":"%s/state.json","transition_status":"stopped","last_stop_outcome":"saved_state_written","simulated":true,"notes":["stopped"]}}\n' "$2" "$2" "$2"
    else
      printf '{{"protocol_version":1,"protocol_schema":"ctx.avf_linux_helper.v1","state":"running","vm_root":"%s","logs_root":"%s","state_path":"%s/state.json","last_start_outcome":"restored","simulated":true,"notes":["running"]}}\n' "$2" "$2" "$2"
    fi
    ;;
  stop-workspace-vm)
    printf 'stopped' > "$STATE"
    printf '{{"protocol_version":1,"protocol_schema":"ctx.avf_linux_helper.v1","state":"stopped","vm_root":"%s","logs_root":"%s","state_path":"%s/state.json","transition_status":"stopped","last_stop_outcome":"saved_state_written","simulated":true,"notes":["stopped"]}}\n' "$2" "$2" "$2"
    ;;
  *)
    echo "unexpected helper invocation: $*" >&2
    exit 1
    ;;
esac
"#,
        log = log_path.display(),
        state = state_path.display(),
    );
    std::fs::write(&helper_path, script).expect("write AVF helper shim");
    let mut perms = std::fs::metadata(&helper_path)
        .expect("helper shim metadata")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&helper_path, perms).expect("chmod AVF helper shim");
    (helper_path, log_path)
}
