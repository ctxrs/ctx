use std::path::{Path, PathBuf};

use super::sandbox_cli_shim::write_lifecycle_sandbox_cli_shim;

pub(in crate::daemon::workspace_runtime::tests) fn write_avf_linux_lifecycle_helper(
    dir: &Path,
) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let path = dir.join("ctx-avf-linux-helper-runtime-manager-test.sh");
    let host_os = std::env::consts::OS;
    let host_arch = std::env::consts::ARCH;
    let sandbox_cli_path = write_lifecycle_sandbox_cli_shim(dir, host_os, host_arch);
    let script = lifecycle_helper_script(host_os, host_arch, &sandbox_cli_path);
    std::fs::write(&path, script).expect("write AVF lifecycle helper shim");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod AVF lifecycle helper shim");
    path
}

fn lifecycle_helper_script(host_os: &str, host_arch: &str, sandbox_cli_path: &Path) -> String {
    format!(
        r#"#!/bin/sh
cmd="$1"
shift
case "$cmd" in
  probe)
    printf '%s\n' '{{"protocol_version":1,"protocol_schema":"ctx.avf_linux_helper.v1","helper_version":"0.0.0-test","host_os":"{host_os}","host_arch":"{host_arch}","supported":true,"save_restore_supported":true,"rosetta_supported":true,"notes":["test helper"]}}'
    ;;
  prepare-runtime-layout)
    data_root="$1"
    vm_root="$data_root/managed/vms/avf-linux/{host_os}/{host_arch}/shared"
    logs_root="$vm_root/logs"
    state_path="$vm_root/shared-vm-state.json"
    mkdir -p "$logs_root"
    status_file="$vm_root/helper-status.txt"
    if [ ! -f "$status_file" ]; then
      printf 'stopped' > "$status_file"
    fi
    printf '{{"protocol_version":1,"protocol_schema":"ctx.avf_linux_helper.v1","vm_root":"%s","logs_root":"%s","state_path":"%s","layout_status":"prepared","notes":["layout ready"]}}\n' "$vm_root" "$logs_root" "$state_path"
    ;;
  workspace-vm-state|shared-vm-state)
    data_root="$1"
    vm_root="$data_root/managed/vms/avf-linux/{host_os}/{host_arch}/shared"
    logs_root="$vm_root/logs"
    state_path="$vm_root/shared-vm-state.json"
    log_path="$logs_root/shared-vm.log"
    status_file="$vm_root/helper-status.txt"
    version_file="$vm_root/runtime-version.txt"
    state=$(cat "$status_file" 2>/dev/null || printf 'missing')
    runtime_version=$(cat "$version_file" 2>/dev/null || true)
    if [ "$state" = "running" ]; then
      printf '{{"protocol_version":1,"protocol_schema":"ctx.avf_linux_helper.v1","state":"running","vm_root":"%s","logs_root":"%s","state_path":"%s","log_path":"%s","runtime_version":"%s","transition_status":"ready","last_start_outcome":"already_running","simulated":true,"notes":["state ready"]}}\n' "$vm_root" "$logs_root" "$state_path" "$log_path" "$runtime_version"
    else
      printf '{{"protocol_version":1,"protocol_schema":"ctx.avf_linux_helper.v1","state":"%s","vm_root":"%s","logs_root":"%s","state_path":"%s","log_path":"%s","simulated":true,"notes":["state ready"]}}\n' "$state" "$vm_root" "$logs_root" "$state_path" "$log_path"
    fi
    ;;
  start-workspace-vm|start-shared-vm)
    data_root="$1"
    runtime_root="$2"
    rootfs_image="$3"
    kernel_path="$4"
    initrd_path="$5"
    runtime_version="$6"
    vm_root="$data_root/managed/vms/avf-linux/{host_os}/{host_arch}/shared"
    logs_root="$vm_root/logs"
    state_path="$vm_root/shared-vm-state.json"
    log_path="$logs_root/shared-vm.log"
    mkdir -p "$logs_root"
    printf 'running' > "$vm_root/helper-status.txt"
    printf '%s' "$runtime_version" > "$vm_root/runtime-version.txt"
    printf '{{"protocol_version":1,"protocol_schema":"ctx.avf_linux_helper.v1","state":"running","vm_root":"%s","logs_root":"%s","state_path":"%s","log_path":"%s","runtime_root":"%s","rootfs_image":"%s","kernel_path":"%s","initrd_path":"%s","runtime_version":"%s","transition_status":"ready","last_start_outcome":"cold_boot","simulated":true,"notes":["launch ready"]}}\n' "$vm_root" "$logs_root" "$state_path" "$log_path" "$runtime_root" "$rootfs_image" "$kernel_path" "$initrd_path" "$runtime_version"
    ;;
  stop-workspace-vm|stop-shared-vm)
    data_root="$1"
    vm_root="$data_root/managed/vms/avf-linux/{host_os}/{host_arch}/shared"
    logs_root="$vm_root/logs"
    state_path="$vm_root/shared-vm-state.json"
    log_path="$logs_root/shared-vm.log"
    mkdir -p "$logs_root"
    printf 'stopped' > "$vm_root/helper-status.txt"
    rm -f "$vm_root/runtime-version.txt"
    printf '{{"protocol_version":1,"protocol_schema":"ctx.avf_linux_helper.v1","state":"stopped","vm_root":"%s","logs_root":"%s","state_path":"%s","log_path":"%s","transition_status":"stopped","simulated":true,"notes":["stopped"]}}\n' "$vm_root" "$logs_root" "$state_path" "$log_path"
    ;;
  prepare-guest-worktree)
    data_root="$1"
    workspace_id="$2"
    worktree_id="$3"
    vm_root="$data_root/managed/vms/avf-linux/{host_os}/{host_arch}/shared"
    host_shadow_root="$vm_root/worktrees/$workspace_id/$worktree_id/shadow-root"
    metadata_path="$vm_root/worktrees/$workspace_id/$worktree_id/worktree.json"
    guest_root="/ctx/ws/worktrees/$worktree_id"
    mkdir -p "$host_shadow_root"
    if [ ! -f "$metadata_path" ]; then
      mkdir -p "$(dirname "$metadata_path")"
      printf '{{"workspace_id":"%s","worktree_id":"%s"}}\n' "$workspace_id" "$worktree_id" > "$metadata_path"
      status="prepared"
      note="prepared guest worktree"
    else
      status="already_present"
      note="existing guest worktree"
    fi
    printf '{{"protocol_version":1,"protocol_schema":"ctx.avf_linux_helper.v1","workspace_id":"%s","worktree_id":"%s","guest_root":"%s","host_shadow_root":"%s","metadata_path":"%s","status":"%s","simulated":true,"notes":["%s"]}}\n' "$workspace_id" "$worktree_id" "$guest_root" "$host_shadow_root" "$metadata_path" "$status" "$note"
    ;;
  guest-exec)
    data_root=""
    workspace_id=""
    worktree_id=""
    cwd=""
    guest_command=""
    while [ $# -gt 0 ]; do
      case "$1" in
        --data-root) data_root="$2"; shift 2 ;;
        --workspace-id) workspace_id="$2"; shift 2 ;;
        --worktree-id) worktree_id="$2"; shift 2 ;;
        --cwd) cwd="$2"; shift 2 ;;
        --command) guest_command="$2"; shift 2 ;;
        --user) shift 2 ;;
        --pty) shift ;;
        --env)
          kv="$2"
          key=${{kv%%=*}}
          value=${{kv#*=}}
          export "$key=$value"
          shift 2
          ;;
        --) shift; break ;;
        *) echo "unexpected guest-exec arg: $1" >&2; exit 1 ;;
      esac
    done
    vm_root="$data_root/managed/vms/avf-linux/{host_os}/{host_arch}/shared"
    host_shadow_root="$vm_root/worktrees/$workspace_id/$worktree_id/shadow-root"
    guest_root="/ctx/ws/worktrees/$worktree_id"
    case "$cwd" in
      "$guest_root") host_cwd="$host_shadow_root" ;;
      "$guest_root"/*) host_cwd="$host_shadow_root/${{cwd#"$guest_root"/}}" ;;
      *) echo "invalid guest cwd: $cwd" >&2; exit 1 ;;
    esac
    mkdir -p "$host_cwd"
    (cd "$host_cwd" && exec "$guest_command" "$@")
    ;;
  shared-vm-exec)
    data_root=""
    shared_command=""
    while [ $# -gt 0 ]; do
      case "$1" in
        --data-root) data_root="$2"; shift 2 ;;
        --cwd) shift 2 ;;
        --command) shared_command="$2"; shift 2 ;;
        --user) shift 2 ;;
        --pty) shift ;;
        --env)
          kv="$2"
          key=$(printf '%s' "$kv" | sed 's/=.*//')
          value=$(printf '%s' "$kv" | sed 's/^[^=]*=//')
          export "$key=$value"
          shift 2
          ;;
        --) shift; break ;;
        *) echo "unexpected shared-vm-exec arg: $1" >&2; exit 1 ;;
      esac
    done
    if [ "$shared_command" = "sandbox-cli" ] || [ "$shared_command" = "/usr/local/bin/nerdctl" ]; then
      exec "{sandbox_cli_shim}" "$data_root" "$@"
    fi
    exec "$shared_command" "$@"
    ;;
  *)
    echo "unexpected helper invocation: $cmd $*" >&2
    exit 1
    ;;
esac
"#,
        sandbox_cli_shim = sandbox_cli_path.display()
    )
}
