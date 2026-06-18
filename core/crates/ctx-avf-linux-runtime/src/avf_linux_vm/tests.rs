use super::helper_wrappers::{shared_vm_state, start_shared_vm, stop_shared_vm};
use super::runtime_install as runtime_assets;
use super::*;
use crate::{
    default_container_image, ContainerExecutionSettings, ContainerRuntimeKind,
    SharedSubstrateLifecycleManager, SubstrateShutdownOutcome, SubstrateShutdownReason,
    SubstrateStartupOutcome, SubstrateStartupReason, SubstrateStartupSelection,
    CTX_HARNESS_SANDBOX_CLI_PATH_ENV,
};
use ctx_bundled_assets as bundled_assets;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

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

#[derive(Default)]
struct RecordingObserver {
    phases: std::sync::Mutex<Vec<(HarnessSetupPhase, String)>>,
}

impl HarnessSetupObserver for RecordingObserver {
    fn on_phase(&self, phase: HarnessSetupPhase, message: &str) {
        self.phases
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push((phase, message.to_string()));
    }

    fn on_log(&self, _phase: HarnessSetupPhase, _level: HarnessSetupLogLevel, _message: &str) {}
}

fn helper_env_test_lock() -> &'static tokio::sync::Mutex<()> {
    crate::test_support::sandbox_cli_env_test_lock()
}

fn process_env_test_lock() -> &'static tokio::sync::Mutex<()> {
    crate::test_support::process_env_test_lock()
}

fn build_test_plain_archive(file_name: &str, payload: &[u8]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_mode(0o644);
    header.set_size(payload.len() as u64);
    header.set_cksum();
    builder
        .append_data(&mut header, file_name, payload)
        .expect("append plain archive payload");
    builder.into_inner().expect("finalize plain archive")
}

fn write_probe_helper(dir: &Path) -> PathBuf {
    let helper = dir.join("ctx-avf-linux-helper");
    std::fs::write(
        &helper,
        r#"#!/bin/sh
set -eu
case "${1:-}" in
  probe)
    cat <<'JSON'
{"protocol_version":1,"protocol_schema":"ctx.avf_linux_helper.v1","helper_version":"test-helper","host_os":"macos","host_arch":"aarch64","supported":true,"save_restore_supported":true,"rosetta_supported":true,"notes":["ready"]}
JSON
    ;;
  *)
    echo "unsupported" >&2
    exit 1
    ;;
esac
"#,
    )
    .expect("write probe helper");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&helper)
            .expect("probe helper metadata")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&helper, perms).expect("chmod probe helper");
    }
    helper
}

fn write_python_helper(helper: &Path, script: &str, label: &str) {
    std::fs::write(
        helper,
        r#"#!/bin/sh
unset PYTHONHOME PYTHONPATH
exec python3 -E "$0.py" "$@"
"#,
    )
    .unwrap_or_else(|error| panic!("write {label} helper wrapper: {error}"));
    let mut script_path = helper.to_path_buf().into_os_string();
    script_path.push(".py");
    std::fs::write(PathBuf::from(script_path), script)
        .unwrap_or_else(|error| panic!("write {label} helper script: {error}"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(helper)
            .unwrap_or_else(|error| panic!("{label} helper metadata: {error}"))
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(helper, perms)
            .unwrap_or_else(|error| panic!("chmod {label} helper: {error}"));
    }
}

fn write_lifecycle_helper(dir: &Path) -> PathBuf {
    let helper = dir.join("ctx-avf-linux-helper");
    write_python_helper(
        &helper,
        r#"import json
import os
import pathlib
import sys
from typing import Optional

PROTOCOL_VERSION = 1
PROTOCOL_SCHEMA = "ctx.avf_linux_helper.v1"
STATE_FILE = "shared-vm-state.json"
STATE_LOG = "shared-vm.log"

def vm_root(data_root: str) -> pathlib.Path:
    return pathlib.Path(data_root) / "avf-linux-vm" / "shared-vm"

def state_path(data_root: str) -> pathlib.Path:
    return vm_root(data_root) / STATE_FILE

def write_state(data_root: str, state: str, transition: Optional[str] = None):
    root = vm_root(data_root)
    root.mkdir(parents=True, exist_ok=True)
    payload = {
        "protocol_version": PROTOCOL_VERSION,
        "protocol_schema": PROTOCOL_SCHEMA,
        "state": state,
        "vm_root": str(root),
        "logs_root": str(root / "logs"),
        "state_path": str(root / STATE_FILE),
        "saved_state_path": str(root / "saved-machine-state.vzvmsave"),
        "saved_state_exists": state != "missing",
        "runtime_root": str(root / "runtime"),
        "rootfs_image": str(root / "runtime" / "rootfs.raw"),
        "kernel_path": str(root / "runtime" / "helpers" / "kernel"),
        "initrd_path": str(root / "runtime" / "helpers" / "initrd"),
        "runtime_version": "test-runtime",
        "log_path": str(root / STATE_LOG),
        "simulated": True,
        "notes": ["test helper"],
    }
    if state == "running" and transition is None:
        transition = "ready"
    if transition is not None:
        payload["transition_status"] = transition
    state_path(data_root).write_text(json.dumps(payload), encoding="utf-8")

cmd = sys.argv[1]
if cmd == "probe":
    print(json.dumps({
        "protocol_version": PROTOCOL_VERSION,
        "protocol_schema": PROTOCOL_SCHEMA,
        "helper_version": "test-helper",
        "host_os": "macos",
        "host_arch": "aarch64",
        "supported": True,
        "save_restore_supported": True,
        "rosetta_supported": True,
        "notes": ["ready"],
    }))
elif cmd == "prepare-runtime-layout":
    data_root = sys.argv[2]
    root = vm_root(data_root)
    root.mkdir(parents=True, exist_ok=True)
    (root / "logs").mkdir(exist_ok=True)
    print(json.dumps({
        "protocol_version": PROTOCOL_VERSION,
        "protocol_schema": PROTOCOL_SCHEMA,
        "vm_root": str(root),
        "logs_root": str(root / "logs"),
        "state_path": str(root / STATE_FILE),
        "layout_status": "prepared",
        "notes": [],
    }))
elif cmd == "shared-vm-state" or cmd == "workspace-vm-state":
    data_root = sys.argv[2]
    path = state_path(data_root)
    if path.exists():
        print(path.read_text(encoding="utf-8"))
    else:
        root = vm_root(data_root)
        print(json.dumps({
            "protocol_version": PROTOCOL_VERSION,
            "protocol_schema": PROTOCOL_SCHEMA,
            "state": "missing",
            "vm_root": str(root),
            "logs_root": str(root / "logs"),
            "state_path": str(root / STATE_FILE),
            "saved_state_exists": False,
            "simulated": True,
            "notes": [],
        }))
elif cmd == "start-shared-vm" or cmd == "start-workspace-vm":
    data_root = sys.argv[2]
    write_state(data_root, "running", "ready")
    print(state_path(data_root).read_text(encoding="utf-8"))
elif cmd == "stop-shared-vm" or cmd == "stop-workspace-vm":
    data_root = sys.argv[2]
    write_state(data_root, "stopped", "stopped")
    print(state_path(data_root).read_text(encoding="utf-8"))
else:
    print(f"unsupported command: {cmd}", file=sys.stderr)
    sys.exit(1)
"#,
        "lifecycle",
    );
    helper
}

async fn install_bundled_runtime_fixture(
    dir: &Path,
) -> (EnvGuard, EnvGuard, JoinHandle<Result<()>>) {
    let bundle_root = dir.join("bundle");
    let runtime_root = bundle_root
        .join("runtimes")
        .join("avf-linux-guest")
        .join(format!(
            "{}/{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        ));
    let helpers_root = runtime_root.join("helpers");
    let images_root = bundle_root.join("images");
    std::fs::create_dir_all(&helpers_root).expect("create bundled runtime helpers");
    std::fs::create_dir_all(&images_root).expect("create bundled images");
    std::fs::write(runtime_root.join("rootfs.raw"), b"rootfs").expect("write bundled rootfs");
    std::fs::write(helpers_root.join("kernel"), b"kernel").expect("write bundled kernel");
    std::fs::write(helpers_root.join("initrd"), b"initrd").expect("write bundled initrd");
    std::fs::write(helpers_root.join("guest-agent"), b"guest-agent")
        .expect("write bundled guest agent");
    std::fs::write(helpers_root.join("egress-proxy"), b"egress-proxy")
        .expect("write bundled egress proxy");
    std::fs::write(
        helpers_root.join("container-stack.tar.gz"),
        b"container-stack",
    )
    .expect("write bundled container stack");
    let image_bytes = build_test_plain_archive("ctx-harness-image", b"ctx-harness-image");
    std::fs::write(images_root.join("ctx-harness.tar"), &image_bytes)
        .expect("write bundled image tar");
    let default_image = default_container_image();
    let (image_url, image_server) = spawn_static_http_server(image_bytes.clone(), 1)
        .await
        .expect("spawn bundled harness image server");

    let manifest_path = bundle_root.join("manifest.json");
    std::fs::create_dir_all(manifest_path.parent().expect("bundle manifest parent"))
        .expect("create bundled manifest parent");
    std::fs::write(
        &manifest_path,
        serde_json::json!({
            "version": 1,
            "runtimes": [{
                "id": AVF_LINUX_GUEST_RUNTIME_ID,
                "os": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
                "version": "bundled-runtime",
                "sha256": "bundled-sha256",
                "root": format!(
                    "runtimes/{}/{}/{}",
                    AVF_LINUX_GUEST_RUNTIME_ID,
                    std::env::consts::OS,
                    std::env::consts::ARCH
                ),
                "bin": "rootfs.raw"
            }],
            "providers": [],
            "images": [{
                "id": "ctx-harness",
                "version": "bundled-image",
                "os": "linux",
                "arch": std::env::consts::ARCH,
                "sha256": "bundled-image-sha256",
                "tar": "images/ctx-harness.tar",
                "image": default_image,
            }],
            "daemons": []
        })
        .to_string(),
    )
    .expect("write bundled manifest");
    let runtime_lock_path = bundle_root.join("runtime_lock.v2.json");
    std::fs::write(
        &runtime_lock_path,
        serde_json::json!({
            "version": 2,
            "profiles": {
                "parity": {
                    "allowed_source_types": ["http"]
                }
            },
            "components": [{
                "kind": "image",
                "id": "ctx-harness",
                "os": "linux",
                "arch": std::env::consts::ARCH,
                "version": "bundled-image",
                "sources": [{
                    "source_type": "http",
                    "uri": image_url.to_string(),
                    "sha256": sha256_hex(&image_bytes),
                }]
            }]
        })
        .to_string(),
    )
    .expect("write bundled runtime lock");
    let bundle_dir = EnvGuard::set("CTX_BUNDLE_DIR", bundle_root.to_str().unwrap());
    let bundle_manifest = EnvGuard::set("CTX_BUNDLE_MANIFEST", manifest_path.to_str().unwrap());
    (bundle_dir, bundle_manifest, image_server)
}

fn write_stateful_lifecycle_helper(dir: &Path) -> (PathBuf, PathBuf) {
    let helper = dir.join("ctx-avf-linux-helper");
    let log_path = dir.join("shared-vm-lifecycle.log");
    let log_path_literal = log_path.display().to_string().replace('\\', "\\\\");
    let script = r#"import json
import os
import pathlib
import sys

PROTOCOL_VERSION = 1
PROTOCOL_SCHEMA = "ctx.avf_linux_helper.v1"
STATE_FILE = "helper-shared-vm-state.json"
STATE_JSON = "shared-vm-state.json"
STATE_LOG = "shared-vm.log"
LOG_PATH = pathlib.Path("__LOG_PATH__")

def start_scenario():
    return os.environ.get("CTX_TEST_AVF_START_SCENARIO", "cold_boot")

def stop_mode():
    return os.environ.get("CTX_TEST_AVF_STOP_MODE", "saved")

def restore_supported():
    raw = os.environ.get("CTX_TEST_AVF_RESTORE_SUPPORTED", "1").strip().lower()
    return raw not in ("0", "false", "no")

def vm_root(data_root: str) -> pathlib.Path:
    return pathlib.Path(data_root) / "avf-linux-vm" / "shared-vm"

def helper_state_path(data_root: str) -> pathlib.Path:
    return pathlib.Path(data_root) / STATE_FILE

def seed_state():
    scenario = start_scenario()
    if scenario == "reuse":
        return {
            "state": "running",
            "transition_status": "ready",
            "saved_state_exists": False,
            "last_start_outcome": "already_running",
        }
    if scenario == "running_not_ready":
        return {
            "state": "running",
            "transition_status": "scaffolded",
            "saved_state_exists": False,
            "last_start_outcome": "cold_boot",
            "state_poll_count": 0,
        }
    if scenario == "start_not_ready":
        return {
            "state": "stopped",
            "saved_state_exists": False,
        }
    if scenario in ("restore", "restore_failure"):
        return {
            "state": "stopped",
            "saved_state_exists": True,
            "last_stop_outcome": "saved_state_written",
        }
    return {
        "state": "stopped",
        "saved_state_exists": False,
    }

def load_state(data_root: str):
    path = helper_state_path(data_root)
    if path.exists():
        return json.loads(path.read_text(encoding="utf-8"))
    state = seed_state()
    save_state(data_root, state)
    return state

def save_state(data_root: str, state):
    path = helper_state_path(data_root)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(state), encoding="utf-8")

def payload(data_root: str, state):
    root = vm_root(data_root)
    logs_root = root / "logs"
    result = {
        "protocol_version": PROTOCOL_VERSION,
        "protocol_schema": PROTOCOL_SCHEMA,
        "state": state["state"],
        "vm_root": str(root),
        "logs_root": str(logs_root),
        "state_path": str(root / STATE_JSON),
        "log_path": str(root / STATE_LOG),
        "saved_state_exists": bool(state.get("saved_state_exists", False)),
        "simulated": True,
        "notes": [f"scenario:{start_scenario()}"],
    }
    if state.get("saved_state_exists"):
        result["saved_state_path"] = str(root / "saved-machine-state.vzvmsave")
    for key in (
        "runtime_root",
        "rootfs_image",
        "kernel_path",
        "initrd_path",
        "runtime_version",
        "transition_status",
        "last_start_outcome",
        "last_stop_outcome",
        "last_restore_error",
        "last_save_error",
    ):
        value = state.get(key)
        if value is not None:
            result[key] = value
    return result

def log_invocation():
    LOG_PATH.parent.mkdir(parents=True, exist_ok=True)
    with LOG_PATH.open("a", encoding="utf-8") as handle:
        handle.write(" ".join(sys.argv[1:]) + "\n")

log_invocation()
cmd = sys.argv[1]
if cmd == "probe":
    print(json.dumps({
        "protocol_version": PROTOCOL_VERSION,
        "protocol_schema": PROTOCOL_SCHEMA,
        "helper_version": "stateful-test-helper",
        "host_os": "macos",
        "host_arch": "aarch64",
        "supported": True,
        "save_restore_supported": restore_supported(),
        "rosetta_supported": True,
        "notes": ["ready"],
    }))
elif cmd == "prepare-runtime-layout":
    data_root = sys.argv[2]
    root = vm_root(data_root)
    root.mkdir(parents=True, exist_ok=True)
    (root / "logs").mkdir(exist_ok=True)
    load_state(data_root)
    print(json.dumps({
        "protocol_version": PROTOCOL_VERSION,
        "protocol_schema": PROTOCOL_SCHEMA,
        "vm_root": str(root),
        "logs_root": str(root / "logs"),
        "state_path": str(root / STATE_JSON),
        "layout_status": "prepared",
        "notes": [],
    }))
elif cmd == "shared-vm-state" or cmd == "workspace-vm-state":
    data_root = sys.argv[2]
    state = load_state(data_root)
    if start_scenario() in ("running_not_ready", "start_not_ready") and state.get("transition_status") == "scaffolded":
        poll_count = int(state.get("state_poll_count", 0)) + 1
        state["state_poll_count"] = poll_count
        if poll_count >= 2:
            state["transition_status"] = "ready"
        save_state(data_root, state)
    print(json.dumps(payload(data_root, state)))
elif cmd == "start-shared-vm" or cmd == "start-workspace-vm":
    data_root = sys.argv[2]
    runtime_root, rootfs_image, kernel_path, initrd_path, runtime_version = sys.argv[3:8]
    state = load_state(data_root)
    scenario = start_scenario()
    if scenario == "restore":
        outcome = "restored"
        restore_error = None
    elif scenario == "restore_failure":
        outcome = "cold_boot_after_restore_failure"
        restore_error = "restore failed"
    elif scenario == "reuse":
        outcome = "already_running"
        restore_error = None
    else:
        outcome = "cold_boot"
        restore_error = None
    transition_status = "ready"
    if scenario == "start_not_ready":
        transition_status = "scaffolded"
    state.update({
        "state": "running",
        "saved_state_exists": False,
        "runtime_root": runtime_root,
        "rootfs_image": rootfs_image,
        "kernel_path": kernel_path,
        "initrd_path": initrd_path,
        "runtime_version": runtime_version,
        "transition_status": transition_status,
        "last_start_outcome": outcome,
        "last_restore_error": restore_error,
    })
    if scenario == "start_not_ready":
        state["state_poll_count"] = 0
    save_state(data_root, state)
    print(json.dumps(payload(data_root, state)))
elif cmd == "stop-shared-vm" or cmd == "stop-workspace-vm":
    data_root = sys.argv[2]
    state = load_state(data_root)
    mode = stop_mode()
    if mode == "unsupported":
        stop_outcome = "cold_stop_save_unsupported"
        save_error = None
        saved_state_exists = False
    elif mode == "failure":
        stop_outcome = "cold_stop_after_save_failure"
        save_error = "save failed"
        saved_state_exists = False
    else:
        stop_outcome = "saved_state_written"
        save_error = None
        saved_state_exists = True
    state.update({
        "state": "stopped",
        "saved_state_exists": saved_state_exists,
        "transition_status": "stopped",
        "last_stop_outcome": stop_outcome,
        "last_save_error": save_error,
    })
    save_state(data_root, state)
    print(json.dumps(payload(data_root, state)))
else:
    print(f"unsupported command: {cmd}", file=sys.stderr)
    sys.exit(1)
"#
    .replace("__LOG_PATH__", &log_path_literal);
    write_python_helper(&helper, &script, "stateful lifecycle");
    (helper, log_path)
}

fn write_ready_runtime_sandbox_cli_shim(dir: &Path) -> PathBuf {
    let sandbox_cli_path = dir.join("sandbox-cli.sh");
    let marker_path = dir.join("image-present");
    std::fs::write(
        &sandbox_cli_path,
        format!(
            "#!/bin/sh\nset -eu\nmarker='{}'\nif [ \"$1\" = \"info\" ]; then\n  printf '{{}}\\n'\n  exit 0\nfi\nif [ \"$1\" = \"image\" ] && [ \"$2\" = \"inspect\" ]; then\n  if [ -f \"$marker\" ]; then\n    printf '[{{}}]\\n'\n    exit 0\n  fi\n  exit 1\nfi\nif [ \"$1\" = \"load\" ] && [ \"$2\" = \"-i\" ]; then\n  : > \"$marker\"\n  printf 'Loaded image\\n'\n  exit 0\nfi\necho \"unexpected sandbox CLI invocation: $*\" >&2\nexit 1\n",
            marker_path.display()
        ),
    )
    .expect("write sandbox CLI shim");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&sandbox_cli_path)
            .expect("sandbox cli metadata")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&sandbox_cli_path, perms).expect("chmod sandbox cli shim");
    }
    sandbox_cli_path
}

fn write_guest_exec_helper(dir: &Path) -> (PathBuf, PathBuf) {
    let helper = dir.join("ctx-avf-linux-helper");
    let capture_file = dir.join("guest-exec-capture.json");
    let capture_path = capture_file.display().to_string().replace('\\', "\\\\");
    let script = format!(
        r#"import json
import pathlib
import shutil
import sys

PROTOCOL_VERSION = 1
PROTOCOL_SCHEMA = "ctx.avf_linux_helper.v1"
CAPTURE_PATH = pathlib.Path("{capture_path}")

def workspace_vm_root(data_root: str, workspace_id: str) -> pathlib.Path:
    return pathlib.Path(data_root) / "avf-linux-vm" / "workspaces" / workspace_id

def worktree_root(data_root: str, workspace_id: str, worktree_id: str) -> pathlib.Path:
    return workspace_vm_root(data_root, workspace_id) / "worktrees" / worktree_id

cmd = sys.argv[1]
if cmd == "probe":
    print(json.dumps({{
        "protocol_version": PROTOCOL_VERSION,
        "protocol_schema": PROTOCOL_SCHEMA,
        "helper_version": "guest-exec-helper",
        "host_os": "macos",
        "host_arch": "aarch64",
        "supported": True,
        "save_restore_supported": True,
        "rosetta_supported": True,
        "notes": ["ready"],
    }}))
elif cmd == "prepare-runtime-layout":
    data_root = sys.argv[2]
    root = pathlib.Path(data_root) / "avf-linux-vm" / "shared-vm"
    root.mkdir(parents=True, exist_ok=True)
    (root / "logs").mkdir(exist_ok=True)
    print(json.dumps({{
        "protocol_version": PROTOCOL_VERSION,
        "protocol_schema": PROTOCOL_SCHEMA,
        "vm_root": str(root),
        "logs_root": str(root / "logs"),
        "state_path": str(root / "shared-vm-state.json"),
        "layout_status": "prepared",
        "notes": [],
    }}))
elif cmd == "start-shared-vm" or cmd == "start-workspace-vm":
    data_root = sys.argv[2]
    root = pathlib.Path(data_root) / "avf-linux-vm" / "shared-vm"
    root.mkdir(parents=True, exist_ok=True)
    (root / "logs").mkdir(exist_ok=True)
    print(json.dumps({{
        "protocol_version": PROTOCOL_VERSION,
        "protocol_schema": PROTOCOL_SCHEMA,
        "state": "running",
        "vm_root": str(root),
        "logs_root": str(root / "logs"),
        "state_path": str(root / "shared-vm-state.json"),
        "saved_state_exists": True,
        "saved_state_path": str(root / "saved-machine-state.vzvmsave"),
        "runtime_root": str(root / "runtime"),
        "rootfs_image": str(root / "runtime" / "rootfs.raw"),
        "kernel_path": str(root / "runtime" / "helpers" / "kernel"),
        "initrd_path": str(root / "runtime" / "helpers" / "initrd"),
        "runtime_version": "test-runtime",
        "log_path": str(root / "shared-vm.log"),
        "simulated": True,
        "notes": ["guest exec ready"],
    }}))
elif cmd == "shared-vm-state" or cmd == "workspace-vm-state":
    data_root = sys.argv[2]
    root = pathlib.Path(data_root) / "avf-linux-vm" / "shared-vm"
    print(json.dumps({{
        "protocol_version": PROTOCOL_VERSION,
        "protocol_schema": PROTOCOL_SCHEMA,
        "state": "missing",
        "vm_root": str(root),
        "logs_root": str(root / "logs"),
        "state_path": str(root / "shared-vm-state.json"),
        "saved_state_exists": False,
        "simulated": True,
        "notes": [],
    }}))
elif cmd == "prepare-guest-worktree":
    data_root = sys.argv[2]
    workspace_id = sys.argv[3]
    worktree_id = sys.argv[4]
    host_workspace_root = pathlib.Path(sys.argv[5])
    branch_name = sys.argv[7]
    root = worktree_root(data_root, workspace_id, worktree_id)
    shadow = root / "shadow-root"
    if shadow.exists():
        shutil.rmtree(shadow)
    shutil.copytree(host_workspace_root, shadow)
    metadata_path = root / "worktree.json"
    payload = {{
        "protocol_version": PROTOCOL_VERSION,
        "protocol_schema": PROTOCOL_SCHEMA,
        "workspace_id": workspace_id,
        "worktree_id": worktree_id,
        "guest_root": str(pathlib.Path("/ctx/ws/worktrees") / worktree_id),
        "host_shadow_root": str(shadow),
        "metadata_path": str(metadata_path),
        "status": "prepared",
        "simulated": True,
        "notes": [branch_name],
    }}
    metadata_path.parent.mkdir(parents=True, exist_ok=True)
    metadata_path.write_text(json.dumps(payload), encoding="utf-8")
    print(json.dumps(payload))
elif cmd == "exec":
    args = sys.argv[2:]
    container_name = ""
    cwd = ""
    env = {{}}
    interactive = False
    idx = 0
    while idx < len(args):
        arg = args[idx]
        if arg == "--interactive":
            interactive = True
            idx += 1
        elif arg == "--tty":
            idx += 1
        elif arg == "--user":
            idx += 2
        elif arg == "--workdir":
            cwd = args[idx + 1]
            idx += 2
        else:
            break
    if idx >= len(args):
        raise SystemExit("missing sandbox exec container name")
    container_name = args[idx]
    idx += 1
    while idx < len(args) and args[idx] == "--env":
        key, value = args[idx + 1].split("=", 1)
        env[key] = value
        idx += 2
    if idx >= len(args):
        raise SystemExit("missing sandbox exec command")
    command = args[idx]
    passthrough = args[idx + 1:]
    payload = {{
        "interactive": interactive,
        "container_name": container_name,
        "cwd": cwd,
        "command": command,
        "args": passthrough,
        "env": env,
    }}
    CAPTURE_PATH.write_text(json.dumps(payload, indent=2), encoding="utf-8")
    print("guest-exec-ok")
elif cmd == "guest-exec":
    args = sys.argv[2:]
    data_root = ""
    workspace_id = ""
    worktree_id = ""
    cwd = ""
    command = ""
    env = {{}}
    passthrough = []
    idx = 0
    while idx < len(args):
        arg = args[idx]
        if arg == "--data-root":
            data_root = args[idx + 1]
            idx += 2
        elif arg == "--workspace-id":
            workspace_id = args[idx + 1]
            idx += 2
        elif arg == "--worktree-id":
            worktree_id = args[idx + 1]
            idx += 2
        elif arg == "--cwd":
            cwd = args[idx + 1]
            idx += 2
        elif arg == "--command":
            command = args[idx + 1]
            idx += 2
        elif arg == "--env":
            key, value = args[idx + 1].split("=", 1)
            env[key] = value
            idx += 2
        elif arg == "--user":
            idx += 2
        elif arg == "--pty":
            idx += 1
        elif arg == "--":
            passthrough = args[idx + 1:]
            break
        else:
            raise SystemExit(f"unexpected guest-exec arg: {{arg}}")
    payload = {{
        "data_root": data_root,
        "workspace_id": workspace_id,
        "worktree_id": worktree_id,
        "cwd": cwd,
        "command": command,
        "args": passthrough,
        "env": env,
    }}
    CAPTURE_PATH.write_text(json.dumps(payload, indent=2), encoding="utf-8")
    print("guest-exec-ok")
else:
    print(f"unsupported command: {{cmd}}", file=sys.stderr)
    sys.exit(1)
"#,
    );
    write_python_helper(&helper, &script, "guest exec");
    (helper, capture_file)
}

fn runtime_archive_bytes() -> Vec<u8> {
    let temp = tempfile::tempdir().expect("temp runtime archive dir");
    let root = temp.path().join("avf-linux-runtime");
    std::fs::create_dir_all(root.join("helpers")).expect("create helpers");
    std::fs::write(root.join("rootfs.raw"), b"rootfs\n").expect("write rootfs");
    std::fs::write(root.join("helpers").join("kernel"), b"kernel\n").expect("write kernel");
    std::fs::write(root.join("helpers").join("initrd"), b"initrd\n").expect("write initrd");
    std::fs::write(root.join("helpers").join("guest-agent"), b"guest-agent\n")
        .expect("write guest agent");
    std::fs::write(root.join("helpers").join("egress-proxy"), b"egress-proxy\n")
        .expect("write egress proxy");
    std::fs::write(
        root.join("helpers").join("container-stack.tar.gz"),
        b"container-stack\n",
    )
    .expect("write container stack");
    std::fs::write(
        root.join("version.txt"),
        "version=managed-runtime\nubuntu-release=noble\nubuntu-arch=arm64\n",
    )
    .expect("write version metadata");

    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut archive = tar::Builder::new(encoder);
    archive
        .append_dir_all("avf-linux-runtime", &root)
        .expect("append runtime dir to tar archive");
    let encoder = archive.into_inner().expect("finalize tar archive");
    encoder.finish().expect("finish tar.gz archive")
}

async fn spawn_static_http_server(
    body: Vec<u8>,
    requests: usize,
) -> Result<(url::Url, JoinHandle<Result<()>>)> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("bind static http server")?;
    let addr = listener
        .local_addr()
        .context("static http server local addr")?;
    let url = url::Url::parse(&format!("http://127.0.0.1:{}/runtime.tar.gz", addr.port()))
        .context("static http server url")?;
    let handle = tokio::spawn(async move {
        for _ in 0..requests {
            let (mut stream, _) = listener.accept().await.context("accept static http")?;
            let mut request_buf = vec![0_u8; 4096];
            let _ = stream.read(&mut request_buf).await;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .await
                .context("write static http headers")?;
            stream
                .write_all(&body)
                .await
                .context("write static http body")?;
        }
        Ok(())
    });
    Ok((url, handle))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn shared_vm_settings() -> ContainerExecutionSettings {
    ContainerExecutionSettings {
        runtime: ContainerRuntimeKind::SharedVmContainer,
        ..ContainerExecutionSettings::default()
    }
}

#[test]
fn helper_probe_uses_configured_helper_binary() {
    let _serial = helper_env_test_lock().blocking_lock();
    let temp = tempfile::tempdir().unwrap();
    let helper = write_probe_helper(temp.path());
    let _guard = EnvGuard::set(AVF_LINUX_HELPER_PATH_ENV, helper.to_str().unwrap());

    let probe = probe_helper().unwrap();
    assert!(probe.supported);
    assert_eq!(probe.helper_version, "test-helper");
    assert_eq!(probe.host_os, "macos");
    assert_eq!(probe.host_arch, "aarch64");
}

#[tokio::test]
async fn ensure_workspace_vm_ready_reports_machine_check_after_runtime_download() {
    let _serial = helper_env_test_lock().lock().await;
    let temp = tempfile::tempdir().unwrap();
    let helper = write_lifecycle_helper(temp.path());
    let _helper_guard = EnvGuard::set(AVF_LINUX_HELPER_PATH_ENV, helper.to_str().unwrap());
    let (_bundle_root_guard, _bundle_manifest_guard, image_server) =
        install_bundled_runtime_fixture(temp.path()).await;
    let observer = std::sync::Arc::new(RecordingObserver::default());

    ensure_workspace_vm_ready_with_observer(
        temp.path(),
        WorkspaceId::new(),
        &shared_vm_settings(),
        Some(&*observer),
    )
    .await
    .unwrap();

    let phases = observer
        .phases
        .lock()
        .unwrap_or_else(|poisoned: std::sync::PoisonError<_>| poisoned.into_inner())
        .clone();
    assert!(
        phases.iter().any(|(phase, message)| {
            *phase == HarnessSetupPhase::MachineCheck
                && message == "checking AVF Linux workspace VM state"
        }),
        "expected a machine-check phase after runtime preparation, saw: {phases:?}"
    );
    assert!(
        phases.iter().any(|(phase, message)| {
            *phase == HarnessSetupPhase::MachineStartOrInit
                && message == "starting AVF Linux workspace VM"
        }),
        "expected a machine-start phase after machine-check, saw: {phases:?}"
    );
    image_server.abort();
}

#[tokio::test]
async fn managed_avf_linux_runtime_downloads_archive_and_helpers() {
    let _helper_lock = helper_env_test_lock().lock().await;
    let temp = tempfile::tempdir().unwrap();
    let archive = runtime_archive_bytes();
    let archive_sha = sha256_hex(&archive);
    let (archive_url, server_handle) = spawn_static_http_server(archive.clone(), 1).await.unwrap();

    let helper_bytes = b"#!/bin/sh\nexit 0\n".to_vec();
    let helper_sha = sha256_hex(&helper_bytes);
    let (helper_url, helper_server_handle) = spawn_static_http_server(helper_bytes.clone(), 5)
        .await
        .unwrap();

    let source = bundled_assets::ManagedRuntimeSource {
        version: "managed-runtime".to_string(),
        uri: archive_url.to_string(),
        sha256: archive_sha.clone(),
        bin: "rootfs.raw".to_string(),
        helpers: HashMap::from([
            (
                AVF_LINUX_KERNEL_HELPER.to_string(),
                bundled_assets::ManagedArtifactSource {
                    uri: helper_url.to_string(),
                    sha256: helper_sha.clone(),
                },
            ),
            (
                AVF_LINUX_INITRD_HELPER.to_string(),
                bundled_assets::ManagedArtifactSource {
                    uri: helper_url.to_string(),
                    sha256: helper_sha.clone(),
                },
            ),
            (
                AVF_LINUX_GUEST_AGENT_HELPER.to_string(),
                bundled_assets::ManagedArtifactSource {
                    uri: helper_url.to_string(),
                    sha256: helper_sha.clone(),
                },
            ),
            (
                AVF_LINUX_EGRESS_PROXY_HELPER.to_string(),
                bundled_assets::ManagedArtifactSource {
                    uri: helper_url.to_string(),
                    sha256: helper_sha.clone(),
                },
            ),
            (
                AVF_LINUX_CONTAINER_STACK_HELPER.to_string(),
                bundled_assets::ManagedArtifactSource {
                    uri: helper_url.to_string(),
                    sha256: helper_sha.clone(),
                },
            ),
        ]),
    };

    let runtime = runtime_assets::ensure_managed_avf_linux_guest_runtime_with_override(
        temp.path(),
        Some(&source),
        None,
        None,
    )
    .await
    .unwrap();

    server_handle.await.unwrap().unwrap();
    helper_server_handle.await.unwrap().unwrap();

    assert_eq!(runtime.version, "managed-runtime");
    assert!(runtime.managed);
    assert!(runtime.runtime_root.exists());
    assert!(runtime.rootfs_image.exists());
    assert!(runtime.kernel_path.exists());
    assert!(runtime.initrd_path.exists());
    let guest_agent_path = runtime
        .guest_agent_path
        .as_ref()
        .expect("guest agent helper should exist");
    assert!(guest_agent_path.exists());
    let egress_proxy_path = runtime
        .egress_proxy_path
        .as_ref()
        .expect("egress proxy helper should exist");
    assert!(egress_proxy_path.exists());
    assert!(runtime.container_stack_path.exists());

    let archive_path = runtime_assets::managed_avf_linux_archive_path(temp.path(), &source);
    assert!(archive_path.exists());
    let ready_marker = tokio::fs::read_to_string(
        runtime_assets::managed_avf_linux_runtime_ready_marker_path(&runtime.runtime_root),
    )
    .await
    .unwrap();
    let ready_json: serde_json::Value = serde_json::from_str(&ready_marker).unwrap();
    assert_eq!(ready_json["runtime_id"], "avf-linux-guest");
    assert_eq!(ready_json["source_version"], "managed-runtime");
    assert_eq!(ready_json["source_sha256"], source.sha256);
    assert_eq!(
        ready_json["source_identity_sha256"],
        runtime_assets::managed_avf_linux_runtime_source_identity(&source)
    );
    assert!(runtime_assets::avf_linux_runtime_is_ready(&runtime));
    assert_eq!(
        tokio::fs::read(&runtime.kernel_path).await.unwrap(),
        helper_bytes
    );
    assert_eq!(
        tokio::fs::read(&runtime.initrd_path).await.unwrap(),
        helper_bytes
    );
    assert_eq!(
        tokio::fs::read(guest_agent_path).await.unwrap(),
        helper_bytes
    );
    assert_eq!(
        tokio::fs::read(egress_proxy_path).await.unwrap(),
        helper_bytes
    );
    assert_eq!(
        tokio::fs::read(&runtime.container_stack_path)
            .await
            .unwrap(),
        helper_bytes
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let kernel_mode = tokio::fs::metadata(&runtime.kernel_path)
            .await
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(kernel_mode, 0o644);

        let initrd_mode = tokio::fs::metadata(&runtime.initrd_path)
            .await
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(initrd_mode, 0o644);

        let guest_agent_mode = tokio::fs::metadata(guest_agent_path)
            .await
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(guest_agent_mode, 0o755);

        let egress_proxy_mode = tokio::fs::metadata(egress_proxy_path)
            .await
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(egress_proxy_mode, 0o755);

        let container_stack_mode = tokio::fs::metadata(&runtime.container_stack_path)
            .await
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(container_stack_mode, 0o644);
    }
}

#[tokio::test]
async fn ensure_avf_linux_runtime_prefers_bundled_guest_runtime_over_managed_source() {
    let _process_env = process_env_test_lock().lock().await;
    let _helper_lock = helper_env_test_lock().lock().await;
    let temp = tempfile::tempdir().unwrap();
    let bundle_root = temp.path().join("bundle");
    let runtime_root = bundle_root
        .join("runtimes")
        .join("avf-linux-guest")
        .join(format!(
            "{}/{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        ));
    let helpers_root = runtime_root.join("helpers");
    std::fs::create_dir_all(&helpers_root).unwrap();
    let rootfs_path = runtime_root.join("rootfs.raw");
    let kernel_path = helpers_root.join("kernel");
    let initrd_path = helpers_root.join("initrd");
    let guest_agent_path = helpers_root.join("guest-agent");
    let egress_proxy_path = helpers_root.join("egress-proxy");
    let container_stack_path = helpers_root.join("container-stack.tar.gz");
    std::fs::write(&rootfs_path, b"rootfs").unwrap();
    std::fs::write(&kernel_path, b"kernel").unwrap();
    std::fs::write(&initrd_path, b"initrd").unwrap();
    std::fs::write(&guest_agent_path, b"guest-agent").unwrap();
    std::fs::write(&egress_proxy_path, b"egress-proxy").unwrap();
    std::fs::write(&container_stack_path, b"container-stack").unwrap();
    let manifest_path = bundle_root.join("manifest.json");
    std::fs::create_dir_all(manifest_path.parent().unwrap()).unwrap();
    std::fs::write(
        &manifest_path,
        serde_json::json!({
            "version": 1,
            "runtimes": [{
                "id": AVF_LINUX_GUEST_RUNTIME_ID,
                "os": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
                "version": "bundled-runtime",
                "sha256": "bundled-sha256",
                "root": format!(
                    "runtimes/{}/{}/{}",
                    AVF_LINUX_GUEST_RUNTIME_ID,
                    std::env::consts::OS,
                    std::env::consts::ARCH
                ),
                "bin": "rootfs.raw"
            }],
            "providers": [],
            "images": [],
            "daemons": []
        })
        .to_string(),
    )
    .unwrap();
    let _bundle_dir = EnvGuard::set("CTX_BUNDLE_DIR", bundle_root.to_str().unwrap());
    let _bundle_manifest = EnvGuard::set("CTX_BUNDLE_MANIFEST", manifest_path.to_str().unwrap());
    assert!(
        bundled_assets::bundled_avf_linux_guest_runtime().is_some(),
        "bundled AVF Linux runtime should resolve from the test bundle manifest"
    );
    let source = bundled_assets::ManagedRuntimeSource {
        version: "managed-runtime".to_string(),
        uri: "https://example.invalid/runtime.tar.gz".to_string(),
        sha256: "deadbeef".to_string(),
        bin: "rootfs.raw".to_string(),
        helpers: HashMap::new(),
    };
    let _source_override = override_managed_avf_linux_runtime_source_for_test(source.clone());

    let runtime = runtime_assets::ensure_managed_avf_linux_guest_runtime_with_override(
        temp.path(),
        None,
        None,
        None,
    )
    .await
    .unwrap();

    assert_eq!(runtime.version, "bundled-runtime");
    assert!(!runtime.managed);
    assert_eq!(runtime.runtime_root, runtime_root);
    assert_eq!(runtime.rootfs_image, rootfs_path);
    assert_eq!(runtime.kernel_path, kernel_path);
    assert_eq!(runtime.initrd_path, initrd_path);
    assert_eq!(
        runtime.guest_agent_path.as_deref(),
        Some(guest_agent_path.as_path())
    );
    assert_eq!(
        runtime.egress_proxy_path.as_deref(),
        Some(egress_proxy_path.as_path())
    );
    assert_eq!(runtime.container_stack_path, container_stack_path);
}

#[test]
fn helper_lifecycle_commands_round_trip_structured_state() {
    let _serial = helper_env_test_lock().blocking_lock();
    let temp = tempfile::tempdir().unwrap();
    let helper = write_lifecycle_helper(temp.path());
    let _guard = EnvGuard::set(AVF_LINUX_HELPER_PATH_ENV, helper.to_str().unwrap());

    let layout = prepare_runtime_layout(temp.path()).unwrap();
    assert_eq!(layout.layout_status, AvfLinuxRuntimeLayoutStatus::Prepared);
    assert!(layout.vm_root.ends_with("shared-vm"));

    let initial = shared_vm_state(temp.path()).unwrap();
    assert_eq!(initial.state, AvfLinuxSharedVmLifecycleState::Missing);
    assert!(initial.simulated);

    let runtime = AvfLinuxGuestRuntime {
        runtime_root: temp.path().join("runtime"),
        rootfs_image: temp.path().join("runtime/rootfs.raw"),
        kernel_path: temp.path().join("runtime/helpers/kernel"),
        initrd_path: temp.path().join("runtime/helpers/initrd"),
        guest_agent_path: Some(temp.path().join("runtime/helpers/guest-agent")),
        egress_proxy_path: Some(temp.path().join("runtime/helpers/egress-proxy")),
        container_stack_path: temp.path().join("runtime/helpers/container-stack.tar.gz"),
        version: "test-runtime".to_string(),
        managed: false,
    };
    let started = start_shared_vm(temp.path(), &runtime).unwrap();
    assert_eq!(started.state, AvfLinuxSharedVmLifecycleState::Running);
    assert_eq!(started.runtime_version.as_deref(), Some("test-runtime"));

    let state_after_start = shared_vm_state(temp.path()).unwrap();
    assert_eq!(
        state_after_start.state,
        AvfLinuxSharedVmLifecycleState::Running
    );

    let stopped = stop_shared_vm(temp.path()).unwrap();
    assert_eq!(stopped.state, AvfLinuxSharedVmLifecycleState::Stopped);
    assert_eq!(
        stopped.transition_status,
        Some(AvfLinuxSharedVmTransitionStatus::Stopped)
    );

    let state_after_stop = shared_vm_state(temp.path()).unwrap();
    assert_eq!(
        state_after_stop.state,
        AvfLinuxSharedVmLifecycleState::Stopped
    );
}

#[tokio::test]
async fn shared_substrate_lifecycle_manager_reports_cold_boot_startup() {
    let _process_env = process_env_test_lock().lock().await;
    let _helper_lock = helper_env_test_lock().lock().await;
    let temp = tempfile::tempdir().unwrap();
    let (helper, log_path) = write_stateful_lifecycle_helper(temp.path());
    let sandbox_cli_path = write_ready_runtime_sandbox_cli_shim(temp.path());
    let _helper_guard = EnvGuard::set(AVF_LINUX_HELPER_PATH_ENV, helper.to_str().unwrap());
    let _sandbox_cli_guard = EnvGuard::set(
        CTX_HARNESS_SANDBOX_CLI_PATH_ENV,
        sandbox_cli_path.to_str().unwrap(),
    );
    let (_bundle_dir, _bundle_manifest, image_server) =
        install_bundled_runtime_fixture(temp.path()).await;
    let _start_scenario = EnvGuard::set("CTX_TEST_AVF_START_SCENARIO", "cold_boot");

    let record = SharedSubstrateLifecycleManager::new(temp.path())
        .ensure_shared_runtime_ready(&shared_vm_settings(), None)
        .await
        .unwrap();

    assert_eq!(
        record.startup_selection,
        Some(SubstrateStartupSelection::ColdBoot)
    );
    assert_eq!(
        record.startup_outcome,
        Some(SubstrateStartupOutcome::ColdBoot)
    );
    assert_eq!(record.startup_reason, None);
    assert!(!record.restore_attempted);
    assert!(!record.restore_error_present);
    let log = std::fs::read_to_string(log_path).unwrap();
    assert!(
        log.lines()
            .any(|line| line.starts_with("start-workspace-vm ")),
        "expected cold boot start invocation in log:\n{log}"
    );
    image_server.abort();
}

#[tokio::test]
async fn shared_substrate_lifecycle_manager_reuses_running_vm_without_start() {
    let _helper_lock = helper_env_test_lock().lock().await;
    let temp = tempfile::tempdir().unwrap();
    let (helper, log_path) = write_stateful_lifecycle_helper(temp.path());
    let _helper_guard = EnvGuard::set(AVF_LINUX_HELPER_PATH_ENV, helper.to_str().unwrap());
    let _start_scenario = EnvGuard::set("CTX_TEST_AVF_START_SCENARIO", "reuse");

    let record = SharedSubstrateLifecycleManager::new(temp.path())
        .ensure_shared_runtime_ready(&shared_vm_settings(), None)
        .await
        .unwrap();

    assert_eq!(
        record.startup_selection,
        Some(SubstrateStartupSelection::Reuse)
    );
    assert_eq!(record.startup_outcome, Some(SubstrateStartupOutcome::Reuse));
    assert_eq!(record.startup_reason, None);
    assert!(!record.restore_attempted);
    assert!(!record.restore_error_present);
    let log = std::fs::read_to_string(log_path).unwrap();
    assert!(
        !log.lines()
            .any(|line| line.starts_with("start-workspace-vm ")),
        "reuse path should not start the VM:\n{log}"
    );
}

#[tokio::test]
async fn shared_substrate_lifecycle_manager_joins_running_vm_until_launch_ready() {
    let _process_env = process_env_test_lock().lock().await;
    let _helper_lock = helper_env_test_lock().lock().await;
    let temp = tempfile::tempdir().unwrap();
    let (helper, log_path) = write_stateful_lifecycle_helper(temp.path());
    let sandbox_cli_path = write_ready_runtime_sandbox_cli_shim(temp.path());
    let _helper_guard = EnvGuard::set(AVF_LINUX_HELPER_PATH_ENV, helper.to_str().unwrap());
    let _sandbox_cli_guard = EnvGuard::set(
        CTX_HARNESS_SANDBOX_CLI_PATH_ENV,
        sandbox_cli_path.to_str().unwrap(),
    );
    let (_bundle_dir, _bundle_manifest, image_server) =
        install_bundled_runtime_fixture(temp.path()).await;
    let _start_scenario = EnvGuard::set("CTX_TEST_AVF_START_SCENARIO", "running_not_ready");

    let record = SharedSubstrateLifecycleManager::new(temp.path())
        .ensure_shared_runtime_ready(&shared_vm_settings(), None)
        .await
        .unwrap();

    assert_eq!(
        record.startup_selection,
        Some(SubstrateStartupSelection::ColdBoot)
    );
    assert_eq!(
        record.startup_outcome,
        Some(SubstrateStartupOutcome::ColdBoot)
    );
    let log = std::fs::read_to_string(log_path).unwrap();
    assert!(
        !log.lines()
            .any(|line| line.starts_with("start-workspace-vm ")),
        "running-but-not-ready state should be joined instead of restarted:\n{log}"
    );
    image_server.abort();
}

#[tokio::test]
async fn ensure_shared_vm_ready_waits_for_fresh_start_to_become_launch_ready() {
    let _process_env = process_env_test_lock().lock().await;
    let _helper_lock = helper_env_test_lock().lock().await;
    let temp = tempfile::tempdir().unwrap();
    let (helper, _log_path) = write_stateful_lifecycle_helper(temp.path());
    let sandbox_cli_path = write_ready_runtime_sandbox_cli_shim(temp.path());
    let _helper_guard = EnvGuard::set(AVF_LINUX_HELPER_PATH_ENV, helper.to_str().unwrap());
    let _sandbox_cli_guard = EnvGuard::set(
        CTX_HARNESS_SANDBOX_CLI_PATH_ENV,
        sandbox_cli_path.to_str().unwrap(),
    );
    let (_bundle_dir, _bundle_manifest, image_server) =
        install_bundled_runtime_fixture(temp.path()).await;
    let _start_scenario = EnvGuard::set("CTX_TEST_AVF_START_SCENARIO", "start_not_ready");

    let state = ensure_shared_vm_ready_with_observer(temp.path(), &shared_vm_settings(), None)
        .await
        .unwrap();

    assert!(shared_vm_is_launch_ready(&state));
    image_server.abort();
}

#[tokio::test]
async fn shared_substrate_lifecycle_manager_reports_restore_startup() {
    let _process_env = process_env_test_lock().lock().await;
    let _helper_lock = helper_env_test_lock().lock().await;
    let temp = tempfile::tempdir().unwrap();
    let (helper, _log_path) = write_stateful_lifecycle_helper(temp.path());
    let sandbox_cli_path = write_ready_runtime_sandbox_cli_shim(temp.path());
    let _helper_guard = EnvGuard::set(AVF_LINUX_HELPER_PATH_ENV, helper.to_str().unwrap());
    let _sandbox_cli_guard = EnvGuard::set(
        CTX_HARNESS_SANDBOX_CLI_PATH_ENV,
        sandbox_cli_path.to_str().unwrap(),
    );
    let (_bundle_dir, _bundle_manifest, image_server) =
        install_bundled_runtime_fixture(temp.path()).await;
    let _start_scenario = EnvGuard::set("CTX_TEST_AVF_START_SCENARIO", "restore");
    let _restore_supported = EnvGuard::set("CTX_TEST_AVF_RESTORE_SUPPORTED", "1");

    let record = SharedSubstrateLifecycleManager::new(temp.path())
        .ensure_shared_runtime_ready(&shared_vm_settings(), None)
        .await
        .unwrap();

    assert_eq!(
        record.startup_selection,
        Some(SubstrateStartupSelection::Restore)
    );
    assert_eq!(
        record.startup_outcome,
        Some(SubstrateStartupOutcome::Restore)
    );
    assert_eq!(record.startup_reason, None);
    assert!(record.restore_attempted);
    assert!(!record.restore_error_present);
    image_server.abort();
}

#[tokio::test]
async fn shared_substrate_lifecycle_manager_normalizes_restore_failure_to_cold_boot_with_reason() {
    let _process_env = process_env_test_lock().lock().await;
    let _helper_lock = helper_env_test_lock().lock().await;
    let temp = tempfile::tempdir().unwrap();
    let (helper, _log_path) = write_stateful_lifecycle_helper(temp.path());
    let sandbox_cli_path = write_ready_runtime_sandbox_cli_shim(temp.path());
    let _helper_guard = EnvGuard::set(AVF_LINUX_HELPER_PATH_ENV, helper.to_str().unwrap());
    let _sandbox_cli_guard = EnvGuard::set(
        CTX_HARNESS_SANDBOX_CLI_PATH_ENV,
        sandbox_cli_path.to_str().unwrap(),
    );
    let (_bundle_dir, _bundle_manifest, image_server) =
        install_bundled_runtime_fixture(temp.path()).await;
    let _start_scenario = EnvGuard::set("CTX_TEST_AVF_START_SCENARIO", "restore_failure");
    let _restore_supported = EnvGuard::set("CTX_TEST_AVF_RESTORE_SUPPORTED", "1");

    let record = SharedSubstrateLifecycleManager::new(temp.path())
        .ensure_shared_runtime_ready(&shared_vm_settings(), None)
        .await
        .unwrap();

    assert_eq!(
        record.startup_selection,
        Some(SubstrateStartupSelection::Restore)
    );
    assert_eq!(
        record.startup_outcome,
        Some(SubstrateStartupOutcome::ColdBoot)
    );
    assert_eq!(
        record.startup_reason,
        Some(SubstrateStartupReason::RestoreFailed)
    );
    assert!(record.restore_attempted);
    assert!(record.restore_error_present);
    image_server.abort();
}

#[tokio::test]
async fn shared_substrate_lifecycle_manager_reports_saved_shutdown() {
    let _helper_lock = helper_env_test_lock().lock().await;
    let temp = tempfile::tempdir().unwrap();
    let (helper, log_path) = write_stateful_lifecycle_helper(temp.path());
    let _helper_guard = EnvGuard::set(AVF_LINUX_HELPER_PATH_ENV, helper.to_str().unwrap());
    let _start_scenario = EnvGuard::set("CTX_TEST_AVF_START_SCENARIO", "reuse");
    let _stop_mode = EnvGuard::set("CTX_TEST_AVF_STOP_MODE", "saved");

    let record = SharedSubstrateLifecycleManager::new(temp.path())
        .save_or_stop_shared_runtime(&shared_vm_settings())
        .await
        .unwrap();

    assert_eq!(
        record.shutdown_outcome,
        Some(SubstrateShutdownOutcome::Saved)
    );
    assert_eq!(record.shutdown_reason, None);
    assert!(!record.save_error_present);
    assert!(record.saved_state_written_on_shutdown);
    let log = std::fs::read_to_string(log_path).unwrap();
    assert!(
        log.lines()
            .any(|line| line.starts_with("stop-workspace-vm ")),
        "expected save-or-stop invocation in log:\n{log}"
    );
}

#[tokio::test]
async fn shared_substrate_lifecycle_manager_maps_unsupported_save_to_cold_stop() {
    let _helper_lock = helper_env_test_lock().lock().await;
    let temp = tempfile::tempdir().unwrap();
    let (helper, _log_path) = write_stateful_lifecycle_helper(temp.path());
    let _helper_guard = EnvGuard::set(AVF_LINUX_HELPER_PATH_ENV, helper.to_str().unwrap());
    let _start_scenario = EnvGuard::set("CTX_TEST_AVF_START_SCENARIO", "reuse");
    let _stop_mode = EnvGuard::set("CTX_TEST_AVF_STOP_MODE", "unsupported");

    let record = SharedSubstrateLifecycleManager::new(temp.path())
        .save_or_stop_shared_runtime(&shared_vm_settings())
        .await
        .unwrap();

    assert_eq!(
        record.shutdown_outcome,
        Some(SubstrateShutdownOutcome::ColdStop)
    );
    assert_eq!(
        record.shutdown_reason,
        Some(SubstrateShutdownReason::SaveUnsupported)
    );
    assert!(!record.save_error_present);
    assert!(!record.saved_state_written_on_shutdown);
}

#[tokio::test]
async fn shared_substrate_lifecycle_manager_reports_cold_stop_after_save_failure() {
    let _helper_lock = helper_env_test_lock().lock().await;
    let temp = tempfile::tempdir().unwrap();
    let (helper, _log_path) = write_stateful_lifecycle_helper(temp.path());
    let _helper_guard = EnvGuard::set(AVF_LINUX_HELPER_PATH_ENV, helper.to_str().unwrap());
    let _start_scenario = EnvGuard::set("CTX_TEST_AVF_START_SCENARIO", "reuse");
    let _stop_mode = EnvGuard::set("CTX_TEST_AVF_STOP_MODE", "failure");

    let record = SharedSubstrateLifecycleManager::new(temp.path())
        .save_or_stop_shared_runtime(&shared_vm_settings())
        .await
        .unwrap();

    assert_eq!(
        record.shutdown_outcome,
        Some(SubstrateShutdownOutcome::ColdStopAfterSaveFailure)
    );
    assert_eq!(
        record.shutdown_reason,
        Some(SubstrateShutdownReason::SaveFailed)
    );
    assert!(record.save_error_present);
    assert!(!record.saved_state_written_on_shutdown);
}

#[tokio::test]
async fn helper_prepare_guest_worktree_round_trips_structured_state() {
    let _serial = helper_env_test_lock().lock().await;
    let temp = tempfile::tempdir().unwrap();
    let (helper, capture_path) = write_guest_exec_helper(temp.path());
    let _guard = EnvGuard::set(AVF_LINUX_HELPER_PATH_ENV, helper.to_str().unwrap());
    let _sandbox_cli = EnvGuard::set(CTX_HARNESS_SANDBOX_CLI_PATH_ENV, helper.to_str().unwrap());

    let workspace_id = WorkspaceId::new();
    let worktree_id = WorktreeId::new();
    let host_root = temp.path().join("workspace");
    std::fs::create_dir_all(host_root.join(".git")).unwrap();
    std::fs::write(host_root.join("README.md"), "hello\n").unwrap();

    let prepared = prepare_guest_worktree(
        temp.path(),
        workspace_id,
        worktree_id,
        &host_root,
        "abc123def456",
        "ctx/test-branch",
    )
    .unwrap();
    assert_eq!(prepared.status, AvfLinuxGuestWorktreeStatus::Prepared);
    assert!(prepared.simulated);
    assert_eq!(prepared.notes, vec!["ctx/test-branch"]);

    let expected_guest_root = PathBuf::from("/ctx/ws/worktrees").join(worktree_id.0.to_string());
    assert_eq!(prepared.guest_root, expected_guest_root);
    assert!(prepared.host_shadow_root.exists());
    assert!(prepared.host_shadow_root.join("README.md").exists());
    assert!(prepared.host_shadow_root.join(".git").exists());

    let state = workspace_vm_state(temp.path(), workspace_id).unwrap();
    assert_eq!(state.state, AvfLinuxSharedVmLifecycleState::Missing);

    let output = run_guest_exec_capture(
        temp.path(),
        workspace_id,
        worktree_id,
        Path::new(&host_root),
        "python3",
        &["-c".to_string(), "print('ok')".to_string()],
        &HashMap::from([
            ("CTX_CUSTOM".to_string(), "value".to_string()),
            ("PATH".to_string(), "/custom/bin".to_string()),
        ]),
        None,
        false,
    )
    .await
    .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "guest-exec-ok"
    );

    let captured: serde_json::Value =
        serde_json::from_slice(&std::fs::read(capture_path).unwrap()).unwrap();
    assert_eq!(captured["interactive"], true);
    assert_eq!(
        captured["container_name"],
        format!("ctx-harness-{}", workspace_id.0)
    );
    assert_eq!(captured["cwd"], host_root.display().to_string());
    assert_eq!(captured["command"], "python3");
    assert_eq!(captured["args"], serde_json::json!(["-c", "print('ok')"]));
    assert_eq!(captured["env"]["CTX_CUSTOM"], "value");
    assert_eq!(captured["env"]["PATH"], "/custom/bin");
}

#[tokio::test]
async fn run_guest_exec_capture_invokes_helper_with_expected_args() {
    let _helper_lock = helper_env_test_lock().lock().await;
    let temp = tempfile::tempdir().unwrap();
    let (helper, capture_path) = write_guest_exec_helper(temp.path());
    let _guard = EnvGuard::set(AVF_LINUX_HELPER_PATH_ENV, helper.to_str().unwrap());
    let _sandbox_cli = EnvGuard::set(CTX_HARNESS_SANDBOX_CLI_PATH_ENV, helper.to_str().unwrap());

    let workspace_id = WorkspaceId::new();
    let worktree_id = WorktreeId::new();
    let host_root = temp.path().join("workspace");
    tokio::fs::create_dir_all(host_root.join(".git"))
        .await
        .unwrap();
    tokio::fs::write(host_root.join("README.md"), b"hello\n")
        .await
        .unwrap();

    let output = run_guest_exec_capture(
        temp.path(),
        workspace_id,
        worktree_id,
        Path::new(&host_root),
        "python3",
        &["-c".to_string(), "print('ok')".to_string()],
        &HashMap::from([("CTX_SAMPLE".to_string(), "value".to_string())]),
        None,
        false,
    )
    .await
    .unwrap();

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "guest-exec-ok"
    );

    let captured: serde_json::Value =
        serde_json::from_slice(&tokio::fs::read(capture_path).await.unwrap()).unwrap();
    assert_eq!(captured["interactive"], true);
    assert_eq!(
        captured["container_name"],
        format!("ctx-harness-{}", workspace_id.0)
    );
    assert_eq!(captured["cwd"], host_root.display().to_string());
    assert_eq!(captured["command"], "python3");
    assert_eq!(captured["args"], serde_json::json!(["-c", "print('ok')"]));
    assert_eq!(captured["env"]["CTX_SAMPLE"], "value");
}
