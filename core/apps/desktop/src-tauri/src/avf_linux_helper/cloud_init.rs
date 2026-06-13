use super::*;
const SHARED_VM_CLOUD_INIT_ISO_LABEL: &str = "CIDATA";
const SHARED_VM_CLOUD_INIT_SOURCE_DIR: &str = "source";

#[path = "cloud_init/disk_image.rs"]
mod disk_image;
#[path = "cloud_init/user_data.rs"]
mod user_data;

pub(super) use disk_image::*;
pub(super) use user_data::render_shared_vm_cloud_init_user_data;

pub(super) fn wrap_cloud_init_base64(bytes: &[u8]) -> String {
    let encoded = BASE64_STANDARD.encode(bytes);
    let mut wrapped = String::new();
    for chunk in encoded.as_bytes().chunks(76) {
        if !wrapped.is_empty() {
            wrapped.push('\n');
        }
        wrapped.push_str(std::str::from_utf8(chunk).unwrap_or_default());
    }
    wrapped
}

pub(super) fn indent_cloud_init_block(content: &str, spaces: usize) -> String {
    let prefix = " ".repeat(spaces);
    content
        .lines()
        .map(|line| format!("{prefix}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn render_shared_vm_guest_agent_launcher_script(
    ready_marker_path: &Path,
    failure_marker_path: &Path,
    guest_agent_log_path: &Path,
) -> String {
    let ready_marker = shell_escape_single_quotes(&ready_marker_path.display().to_string());
    let failure_marker = shell_escape_single_quotes(&failure_marker_path.display().to_string());
    let guest_agent_log = shell_escape_single_quotes(&guest_agent_log_path.display().to_string());
    format!(
        "#!/bin/sh\nset -eu\nready_marker='{ready_marker}'\nfailure_marker='{failure_marker}'\nlog_path='{guest_agent_log}'\nagent_bin='/usr/local/bin/ctx-avf-linux-guest-agent'\nready_timeout_sec={ready_timeout_sec}\nlog() {{\n  message=\"$1\"\n  printf '%s\\n' \"$message\" >> \"$log_path\"\n  printf '%s\\n' \"$message\" >/dev/hvc0\n}}\nfail() {{\n  message=\"$1\"\n  rm -f \"$ready_marker\"\n  printf '%s\\n' \"$message\" > \"$failure_marker\"\n  log \"$message\"\n  exit 1\n}}\nmkdir -p \"$(dirname \"$ready_marker\")\" \"$(dirname \"$failure_marker\")\" \"$(dirname \"$log_path\")\"\n: > \"$log_path\"\nrm -f \"$ready_marker\" \"$failure_marker\"\nlog \"[ctx-avf-linux] guest-agent launcher starting\"\nif [ ! -x \"$agent_bin\" ]; then\n  fail \"[ctx-avf-linux] guest-agent binary missing or not executable: $agent_bin\"\nfi\nprobe_path=\"${{ready_marker}}.probe\"\nif ! touch \"$probe_path\" >/dev/null 2>&1; then\n  fail \"[ctx-avf-linux] guest-agent ready-marker parent is not writable: $(dirname \"$ready_marker\")\"\nfi\nrm -f \"$probe_path\"\nif [ ! -e /dev/vsock ]; then\n  log \"[ctx-avf-linux] /dev/vsock is not present before guest-agent exec\"\nfi\nCTX_AVF_GUEST_CONTROL_READY_MARKER=\"$ready_marker\" \"$agent_bin\" >> \"$log_path\" 2>&1 &\nagent_pid=$!\nlog \"[ctx-avf-linux] guest-agent started as pid $agent_pid; waiting for ready marker\"\nremaining=\"$ready_timeout_sec\"\nwhile [ \"$remaining\" -gt 0 ]; do\n  if [ -f \"$ready_marker\" ]; then\n    log \"[ctx-avf-linux] guest-agent published ready marker\"\n    wait \"$agent_pid\"\n    status=$?\n    fail \"[ctx-avf-linux] guest-agent exited after ready with status $status\"\n  fi\n  if ! kill -0 \"$agent_pid\" 2>/dev/null; then\n    status=1\n    wait \"$agent_pid\" || status=$?\n    fail \"[ctx-avf-linux] guest-agent exited before ready with status $status\"\n  fi\n  sleep 1\n  remaining=$((remaining - 1))\ndone\nkill \"$agent_pid\" >/dev/null 2>&1 || true\nwait \"$agent_pid\" >/dev/null 2>&1 || true\nfail \"[ctx-avf-linux] guest-agent did not publish ready marker within {ready_timeout_sec}s\"\n",
        ready_timeout_sec = SHARED_VM_GUEST_AGENT_READY_TIMEOUT_SECONDS,
    )
}

pub(super) fn render_shared_vm_guest_agent_service(
    ready_marker_path: &Path,
    failure_marker_path: &Path,
    guest_agent_log_path: &Path,
) -> String {
    let prepare_script = shell_escape_single_quotes(&format!(
        "rm -f '{ready_marker}' && echo \"[ctx-avf-linux] starting guest-agent\" >/dev/hvc0 && echo \"[ctx-avf-linux] ensuring vsock kernel modules are loaded\" >/dev/hvc0 && /usr/sbin/modprobe vsock >/dev/hvc0 2>&1 && /usr/sbin/modprobe vmw_vsock_virtio_transport_common >/dev/hvc0 2>&1 && /usr/sbin/modprobe vmw_vsock_virtio_transport >/dev/hvc0 2>&1",
        ready_marker = ready_marker_path.display(),
    ));
    let launcher_script = render_shared_vm_guest_agent_launcher_script(
        ready_marker_path,
        failure_marker_path,
        guest_agent_log_path,
    );
    format!(
        "[Unit]\nDescription=ctx AVF Linux Guest Agent\nAfter={data_disk_service} {host_data_service}\nRequires={data_disk_service} {host_data_service}\n\n[Service]\nType=simple\nEnvironment=RUST_BACKTRACE=1\nExecStartPre=/bin/sh -lc '{prepare_script}'\nExecStart={launcher_path}\nStandardOutput=journal+console\nStandardError=journal+console\nRestart=no\n\n[Install]\nWantedBy=multi-user.target\n# {guest_agent_service}\n# guest-agent-launcher\n{launcher_script_comment}",
        data_disk_service = SHARED_VM_DATA_DISK_SERVICE_NAME,
        host_data_service = SHARED_VM_HOST_DATA_SERVICE_NAME,
        prepare_script = prepare_script,
        launcher_path = SHARED_VM_GUEST_AGENT_LAUNCHER_PATH,
        guest_agent_service = SHARED_VM_GUEST_AGENT_SERVICE_NAME,
        launcher_script_comment = launcher_script
            .lines()
            .map(|line| format!("# {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

pub(super) fn render_shared_vm_host_data_mount_service() -> String {
    let mount_root = SHARED_VM_GUEST_HOST_DATA_ROOT;
    let escaped_mount_root = shell_escape_single_quotes(mount_root);
    let escaped_tag = shell_escape_single_quotes(SHARED_VM_DATA_ROOT_SHARE_TAG);
    format!(
        "[Unit]\nDescription=ctx AVF Host Data Mount\nDefaultDependencies=no\nAfter=local-fs.target\nBefore={guest_agent_service}\n\n[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/sh -lc 'mkdir -p '\\''{mount_root}'\\'' && mountpoint -q '\\''{mount_root}'\\'' || mount -t virtiofs '\\''{tag}'\\'' '\\''{mount_root}'\\''' \nExecStop=/bin/sh -lc 'mountpoint -q '\\''{mount_root}'\\'' && umount '\\''{mount_root}'\\'' || true'\n\n[Install]\nWantedBy=multi-user.target\n# {service_name}\n",
        guest_agent_service = SHARED_VM_GUEST_AGENT_SERVICE_NAME,
        mount_root = escaped_mount_root,
        tag = escaped_tag,
        service_name = SHARED_VM_HOST_DATA_SERVICE_NAME,
    )
}

pub(super) fn render_shared_vm_data_disk_script() -> String {
    format!(
        "#!/bin/sh\nset -eu\nmount_root='{writable_root}'\nworktrees_root='{worktrees_root}'\nhome_root='{home_root}'\ncache_root='{cache_root}'\ntmp_root='{tmp_root}'\nroot_home='{root_home}'\nroot_xdg_config='{root_xdg_config}'\nroot_xdg_data='{root_xdg_data}'\nroot_xdg_cache='{root_xdg_cache}'\nroot_xdg_runtime='{root_xdg_runtime}'\nlog_root='{log_root}'\ncontainerd_root='{containerd_root}'\nbuildkit_root='{buildkit_root}'\nnerdctl_root='{nerdctl_root}'\ncni_config_root='{cni_config_root}'\ncni_state_root='{cni_state_root}'\ndata_label='{data_label}'\nmarker_name='.ctx-avf-data-disk-ready'\nroot_device=\"$(findmnt -n -o SOURCE /)\"\nif [ -z \"$root_device\" ]; then\n  echo \"[ctx-avf-linux] could not determine root device\" >/dev/hvc0\n  exit 1\nfi\nroot_device=\"$(readlink -f \"$root_device\" 2>/dev/null || printf '%s' \"$root_device\")\"\nroot_disk=\"$(lsblk -nro PKNAME \"$root_device\" | head -n1)\"\nif [ -z \"$root_disk\" ]; then\n  echo \"[ctx-avf-linux] could not resolve parent disk for $root_device\" >/dev/hvc0\n  exit 1\nfi\ndata_device=\"$(lsblk -dnbo NAME,SIZE,RO,TYPE | awk -v root_disk=\"$root_disk\" '$4 == \"disk\" && $1 != root_disk && $3 == 0 && $2 >= 1073741824 {{ print \"/dev/\" $1; exit }}')\"\nif [ -z \"$data_device\" ]; then\n  echo \"[ctx-avf-linux] could not locate writable data disk\" >/dev/hvc0\n  exit 1\nfi\nmkdir -p \"$mount_root\"\nif ! blkid -s TYPE -o value \"$data_device\" >/dev/null 2>&1; then\n  mkfs.ext4 -F -L \"$data_label\" \"$data_device\" >/dev/hvc0 2>&1\nfi\ncurrent_mount_source=\"$(findmnt -n -o SOURCE \"$mount_root\" 2>/dev/null || true)\"\nif [ -n \"$current_mount_source\" ] && [ \"$current_mount_source\" != \"$data_device\" ]; then\n  umount \"$mount_root\" >/dev/null 2>&1 || true\n  current_mount_source=\"\"\nfi\nif [ \"$current_mount_source\" != \"$data_device\" ]; then\n  mount \"$data_device\" \"$mount_root\" >/dev/hvc0 2>&1\nfi\nmkdir -p \"$worktrees_root\" \"$home_root\" \"$cache_root\" \"$tmp_root\" \"$root_home\" \"$root_xdg_config\" \"$root_xdg_data\" \"$root_xdg_cache\" \"$root_xdg_runtime\" \"$log_root\" \"$containerd_root\" \"$buildkit_root\" \"$nerdctl_root\" \"$cni_config_root\" \"$cni_state_root\" /root /var/log /var/lib/containerd /var/lib/buildkit /var/lib/nerdctl /var/lib/cni /etc/cni/net.d /tmp /var/tmp\nchmod 1777 \"$tmp_root\"\nchmod 0700 \"$root_home\" \"$root_xdg_config\" \"$root_xdg_data\" \"$root_xdg_cache\" \"$root_xdg_runtime\"\ncurrent_root_source=\"$(findmnt -n -o SOURCE /root 2>/dev/null || true)\"\nif [ \"$current_root_source\" != \"$root_home\" ]; then\n  mountpoint -q /root && umount /root >/dev/null 2>&1 || true\n  mount --bind \"$root_home\" /root >/dev/hvc0 2>&1\nfi\nchmod 0700 /root\ncurrent_tmp_source=\"$(findmnt -n -o SOURCE /tmp 2>/dev/null || true)\"\nif [ \"$current_tmp_source\" != \"$tmp_root\" ]; then\n  mountpoint -q /tmp && umount /tmp >/dev/null 2>&1 || true\n  mount --bind \"$tmp_root\" /tmp >/dev/hvc0 2>&1\nfi\nchmod 1777 /tmp\ncurrent_var_tmp_source=\"$(findmnt -n -o SOURCE /var/tmp 2>/dev/null || true)\"\nif [ \"$current_var_tmp_source\" != \"$tmp_root\" ]; then\n  mountpoint -q /var/tmp && umount /var/tmp >/dev/null 2>&1 || true\n  mount --bind \"$tmp_root\" /var/tmp >/dev/hvc0 2>&1\nfi\nchmod 1777 /var/tmp\ncurrent_var_log_source=\"$(findmnt -n -o SOURCE /var/log 2>/dev/null || true)\"\nif [ \"$current_var_log_source\" != \"$log_root\" ]; then\n  mountpoint -q /var/log && umount /var/log >/dev/null 2>&1 || true\n  mount --bind \"$log_root\" /var/log >/dev/hvc0 2>&1\nfi\nif [ ! -f \"$mount_root/$marker_name\" ]; then\n  printf 'ready\\n' > \"$mount_root/$marker_name\"\nfi\necho \"[ctx-avf-linux] mounted data disk $data_device at $mount_root\" >/dev/hvc0\ncurrent_containerd_source=\"$(findmnt -n -o SOURCE /var/lib/containerd 2>/dev/null || true)\"\nif [ \"$current_containerd_source\" != \"$containerd_root\" ]; then\n  mountpoint -q /var/lib/containerd && umount /var/lib/containerd >/dev/null 2>&1 || true\n  mount --bind \"$containerd_root\" /var/lib/containerd >/dev/hvc0 2>&1\nfi\ncurrent_buildkit_source=\"$(findmnt -n -o SOURCE /var/lib/buildkit 2>/dev/null || true)\"\nif [ \"$current_buildkit_source\" != \"$buildkit_root\" ]; then\n  mountpoint -q /var/lib/buildkit && umount /var/lib/buildkit >/dev/null 2>&1 || true\n  mount --bind \"$buildkit_root\" /var/lib/buildkit >/dev/hvc0 2>&1\nfi\ncurrent_nerdctl_source=\"$(findmnt -n -o SOURCE /var/lib/nerdctl 2>/dev/null || true)\"\nif [ \"$current_nerdctl_source\" != \"$nerdctl_root\" ]; then\n  mountpoint -q /var/lib/nerdctl && umount /var/lib/nerdctl >/dev/null 2>&1 || true\n  mount --bind \"$nerdctl_root\" /var/lib/nerdctl >/dev/hvc0 2>&1\nfi\ncurrent_cni_config_source=\"$(findmnt -n -o SOURCE /etc/cni/net.d 2>/dev/null || true)\"\nif [ \"$current_cni_config_source\" != \"$cni_config_root\" ]; then\n  mountpoint -q /etc/cni/net.d && umount /etc/cni/net.d >/dev/null 2>&1 || true\n  mount --bind \"$cni_config_root\" /etc/cni/net.d >/dev/hvc0 2>&1\nfi\ncurrent_cni_state_source=\"$(findmnt -n -o SOURCE /var/lib/cni 2>/dev/null || true)\"\nif [ \"$current_cni_state_source\" != \"$cni_state_root\" ]; then\n  mountpoint -q /var/lib/cni && umount /var/lib/cni >/dev/null 2>&1 || true\n  mount --bind \"$cni_state_root\" /var/lib/cni >/dev/hvc0 2>&1\nfi\n",
        writable_root = SHARED_VM_GUEST_WRITABLE_ROOT,
        worktrees_root = SHARED_VM_GUEST_WORKTREES_ROOT,
        home_root = SHARED_VM_GUEST_HOME_ROOT,
        cache_root = SHARED_VM_GUEST_CACHE_ROOT,
        tmp_root = SHARED_VM_GUEST_TMP_ROOT,
        root_home = SHARED_VM_GUEST_ROOT_HOME,
        root_xdg_config = SHARED_VM_GUEST_ROOT_XDG_CONFIG_ROOT,
        root_xdg_data = SHARED_VM_GUEST_ROOT_XDG_DATA_ROOT,
        root_xdg_cache = SHARED_VM_GUEST_ROOT_XDG_CACHE_ROOT,
        root_xdg_runtime = SHARED_VM_GUEST_ROOT_XDG_RUNTIME_ROOT,
        log_root = SHARED_VM_GUEST_LOG_ROOT,
        containerd_root = SHARED_VM_GUEST_CONTAINERD_ROOT,
        buildkit_root = SHARED_VM_GUEST_BUILDKIT_ROOT,
        nerdctl_root = SHARED_VM_GUEST_NERDCTL_ROOT,
        cni_config_root = SHARED_VM_GUEST_CNI_CONFIG_ROOT,
        cni_state_root = SHARED_VM_GUEST_CNI_STATE_ROOT,
        data_label = SHARED_VM_DATA_DISK_LABEL,
    )
}

pub(super) fn render_shared_vm_guest_policy_script() -> String {
    let masked_units = SHARED_VM_GUEST_POLICY_MASKED_UNITS
        .iter()
        .map(|unit| format!("  '{unit}'"))
        .collect::<Vec<_>>()
        .join(" \\\n");
    format!(
        "#!/bin/sh\nset -eu\nmkdir -p /etc/systemd/system\nfor unit in \\\n{masked_units}\ndo\n  mask_path=\"/etc/systemd/system/$unit\"\n  current_target=\"$(readlink \"$mask_path\" 2>/dev/null || true)\"\n  if [ \"$current_target\" != \"/dev/null\" ]; then\n    echo \"[ctx-avf-linux] guest policy masking $unit\" >/dev/hvc0\n    rm -f \"$mask_path\"\n    ln -s /dev/null \"$mask_path\"\n  else\n    echo \"[ctx-avf-linux] guest policy already masked $unit\" >/dev/hvc0\n  fi\n  systemctl stop \"$unit\" >/dev/hvc0 2>&1 || true\n  systemctl reset-failed \"$unit\" >/dev/hvc0 2>&1 || true\ndone\nsystemctl daemon-reload >/dev/hvc0 2>&1\n"
    )
}

pub(super) fn render_shared_vm_early_bootcmd() -> String {
    format!(
        "mkdir -p /usr/local/lib/ctx\nprintf '%s' '{data_disk_script}' > {data_disk_install_path}\nchmod 0755 {data_disk_install_path}\n{data_disk_install_path} >/dev/hvc0 2>&1\nprintf '%s' '{guest_policy_script}' > {policy_path}\nchmod 0755 {policy_path}\n{policy_path} >/dev/hvc0 2>&1",
        data_disk_script = shell_escape_single_quotes(&render_shared_vm_data_disk_script()),
        data_disk_install_path = SHARED_VM_DATA_DISK_INSTALL_PATH,
        guest_policy_script = shell_escape_single_quotes(&render_shared_vm_guest_policy_script()),
        policy_path = SHARED_VM_GUEST_POLICY_INSTALL_PATH,
    )
}

pub(super) fn shared_vm_writable_surface_contract_digest(data_root: &Path) -> Result<String> {
    let guest_ready_marker_path =
        shared_vm_guest_host_share_path(data_root, &shared_vm_guest_control_ready_path(data_root))
            .context("guest ready-marker path should live under shared data root")?;
    let guest_failure_marker_path =
        shared_vm_guest_host_share_path(data_root, &shared_vm_guest_control_failed_path(data_root))
            .context("guest failure-marker path should live under shared data root")?;
    let guest_agent_log_path =
        shared_vm_guest_host_share_path(data_root, &shared_vm_guest_agent_log_path(data_root))
            .context("guest agent log path should live under shared data root")?;
    let mut hasher = Sha256::new();
    for rendered in [
        render_shared_vm_host_data_mount_service(),
        render_shared_vm_data_disk_script(),
        render_shared_vm_guest_policy_script(),
        render_shared_vm_early_bootcmd(),
        render_shared_vm_data_disk_service(),
        render_shared_vm_containerd_service(),
        render_shared_vm_buildkit_service(),
        render_shared_vm_guest_agent_service(
            &guest_ready_marker_path,
            &guest_failure_marker_path,
            &guest_agent_log_path,
        ),
    ] {
        hasher.update(rendered.as_bytes());
        hasher.update(b"\0");
    }
    Ok(hex::encode(hasher.finalize()))
}

pub(super) fn render_shared_vm_data_disk_service() -> String {
    format!(
        "[Unit]\nDescription=ctx AVF Data Disk Setup\nAfter=local-fs.target\nBefore={containerd_service} {buildkit_service} {guest_agent_service}\n\n[Service]\nType=oneshot\nExecStart=/bin/sh -lc 'exec {script_path}'\nRemainAfterExit=yes\n\n[Install]\nWantedBy=multi-user.target\n# {service_name}\n",
        containerd_service = SHARED_VM_CONTAINERD_SERVICE_NAME,
        buildkit_service = SHARED_VM_BUILDKIT_SERVICE_NAME,
        guest_agent_service = SHARED_VM_GUEST_AGENT_SERVICE_NAME,
        script_path = SHARED_VM_DATA_DISK_INSTALL_PATH,
        service_name = SHARED_VM_DATA_DISK_SERVICE_NAME,
    )
}

pub(super) fn render_shared_vm_containerd_service() -> String {
    format!(
        "[Unit]\nDescription=containerd Container Runtime\nAfter=network-online.target local-fs.target {data_disk_service}\nWants=network-online.target\nRequires={data_disk_service}\n\n[Service]\nType=simple\nExecStartPre=/bin/sh -lc 'mkdir -p /run/containerd /var/lib/containerd'\nExecStart=/usr/local/bin/containerd\nRestart=always\nRestartSec=1\nKillMode=process\nDelegate=yes\n\n[Install]\nWantedBy=multi-user.target\n# {containerd_service}\n",
        data_disk_service = SHARED_VM_DATA_DISK_SERVICE_NAME,
        containerd_service = SHARED_VM_CONTAINERD_SERVICE_NAME,
    )
}

pub(super) fn render_shared_vm_buildkit_service() -> String {
    format!(
        "[Unit]\nDescription=BuildKit\nAfter={containerd_service} network-online.target local-fs.target\nWants=network-online.target\nRequires={containerd_service}\n\n[Service]\nType=simple\nExecStartPre=/bin/sh -lc 'mkdir -p /run/buildkit /var/lib/buildkit /etc/buildkit'\nExecStart=/usr/local/bin/buildkitd --config /etc/buildkit/buildkitd.toml --addr {buildkit_socket}\nRestart=always\nRestartSec=1\n\n[Install]\nWantedBy=multi-user.target\n# {buildkit_service}\n",
        containerd_service = SHARED_VM_CONTAINERD_SERVICE_NAME,
        buildkit_service = SHARED_VM_BUILDKIT_SERVICE_NAME,
        buildkit_socket = SHARED_VM_GUEST_BUILDKIT_SOCKET,
    )
}

pub(super) fn render_shared_vm_container_stack_install_script(
    container_stack_host_path: &Path,
    container_stack_sha256: &str,
) -> String {
    let escaped_payload_path =
        shell_escape_single_quotes(&container_stack_host_path.display().to_string());
    let escaped_expected_sha = shell_escape_single_quotes(container_stack_sha256);
    let escaped_marker_path =
        shell_escape_single_quotes(SHARED_VM_GUEST_CONTAINER_STACK_MARKER_PATH);
    format!(
        "#!/bin/sh\nset -eu\npayload='{payload_path}'\nexpected_sha='{expected_sha}'\nmarker='{marker_path}'\nif [ ! -f \"$payload\" ]; then\n  echo \"[ctx-avf-linux] missing guest container-stack payload at $payload\" >/dev/hvc0\n  exit 1\nfi\nactual_sha=\"$(sha256sum \"$payload\" | awk '{{print $1}}')\"\nif [ \"$actual_sha\" != \"$expected_sha\" ]; then\n  echo \"[ctx-avf-linux] guest container-stack sha mismatch: expected $expected_sha got $actual_sha\" >/dev/hvc0\n  exit 1\nfi\nif [ -f \"$marker\" ] && [ \"$(cat \"$marker\" 2>/dev/null || true)\" = \"$expected_sha\" ]; then\n  exit 0\nfi\nmkdir -p /usr/local /usr/local/lib/ctx /etc/containerd /etc/buildkit /etc/cni/net.d /var/lib/containerd /var/lib/buildkit /var/lib/cni /run/containerd /run/buildkit\ntar -xzf \"$payload\" -C /usr/local\ncat > /etc/containerd/config.toml <<'EOF'\nversion = 2\nroot = \"/var/lib/containerd\"\nstate = \"/run/containerd\"\n[grpc]\n  address = \"/run/containerd/containerd.sock\"\nEOF\ncat > /etc/buildkit/buildkitd.toml <<'EOF'\nroot = \"/var/lib/buildkit\"\n[worker.oci]\n  enabled = false\n[worker.containerd]\n  enabled = true\n  namespace = \"default\"\nEOF\ncat > /etc/cni/net.d/10-nerdctl.conflist <<'EOF'\n{{\n  \"cniVersion\": \"1.0.0\",\n  \"name\": \"bridge\",\n  \"plugins\": [\n    {{\n      \"type\": \"bridge\",\n      \"bridge\": \"nerdctl0\",\n      \"isGateway\": true,\n      \"ipMasq\": true,\n      \"promiscMode\": true,\n      \"ipam\": {{\n        \"type\": \"host-local\",\n        \"ranges\": [[{{ \"subnet\": \"10.88.0.0/16\" }}]],\n        \"routes\": [{{ \"dst\": \"0.0.0.0/0\" }}]\n      }}\n    }},\n    {{ \"type\": \"portmap\", \"capabilities\": {{ \"portMappings\": true }} }},\n    {{ \"type\": \"firewall\" }},\n    {{ \"type\": \"tuning\" }}\n  ]\n}}\nEOF\nprintf '%s\\n' \"$expected_sha\" > \"$marker\"\nchmod 0644 \"$marker\"\n",
        payload_path = escaped_payload_path,
        expected_sha = escaped_expected_sha,
        marker_path = escaped_marker_path,
    )
}

pub(super) fn shell_escape_single_quotes(value: &str) -> String {
    value.replace('\'', "'\"'\"'")
}

pub(super) fn hash_shared_vm_seed_component(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

pub(super) fn shared_vm_cloud_init_seed_digest(
    meta_data: &str,
    user_data: &str,
    network_config: &str,
) -> String {
    let mut seed_material =
        Vec::with_capacity(meta_data.len() + user_data.len() + network_config.len());
    seed_material.extend_from_slice(meta_data.as_bytes());
    seed_material.extend_from_slice(user_data.as_bytes());
    seed_material.extend_from_slice(network_config.as_bytes());
    hash_shared_vm_seed_component(&seed_material)
}

fn shared_vm_cloud_init_seed_digest_path(data_root: &Path) -> PathBuf {
    shared_vm_cloud_init_root(data_root).join(".seed-digest")
}

pub(super) fn render_shared_vm_cloud_init_meta_data(
    data_root: &Path,
    guest_agent_bytes: &[u8],
    egress_proxy_bytes: Option<&[u8]>,
    container_stack_sha256: &str,
) -> Result<String> {
    let guest_ready_marker_path =
        shared_vm_guest_host_share_path(data_root, &shared_vm_guest_control_ready_path(data_root))
            .context("guest ready-marker path should live under shared data root")?;
    let guest_failure_marker_path =
        shared_vm_guest_host_share_path(data_root, &shared_vm_guest_control_failed_path(data_root))
            .context("guest failure-marker path should live under shared data root")?;
    let guest_agent_log_path =
        shared_vm_guest_host_share_path(data_root, &shared_vm_guest_agent_log_path(data_root))
            .context("guest agent log path should live under shared data root")?;
    let mut seed_material = Vec::with_capacity(guest_agent_bytes.len() + 256);
    seed_material.extend_from_slice(guest_agent_bytes);
    if let Some(egress_proxy_bytes) = egress_proxy_bytes {
        seed_material.extend_from_slice(egress_proxy_bytes);
    }
    seed_material.extend_from_slice(container_stack_sha256.as_bytes());
    seed_material.extend_from_slice(render_shared_vm_data_disk_script().as_bytes());
    seed_material.extend_from_slice(render_shared_vm_guest_policy_script().as_bytes());
    seed_material.extend_from_slice(render_shared_vm_early_bootcmd().as_bytes());
    seed_material.extend_from_slice(render_shared_vm_data_disk_service().as_bytes());
    seed_material.extend_from_slice(
        render_shared_vm_guest_agent_service(
            &guest_ready_marker_path,
            &guest_failure_marker_path,
            &guest_agent_log_path,
        )
        .as_bytes(),
    );
    seed_material.extend_from_slice(render_shared_vm_containerd_service().as_bytes());
    seed_material.extend_from_slice(render_shared_vm_buildkit_service().as_bytes());
    let seed_hash = hash_shared_vm_seed_component(&seed_material);
    Ok(format!(
        "instance-id: ctx-avf-linux-{seed_hash}\nlocal-hostname: ctx-avf-linux\n"
    ))
}

pub(super) fn render_shared_vm_cloud_init_network_config() -> &'static str {
    "version: 2\nethernets:\n  default:\n    match:\n      name: \"en*\"\n    dhcp4: true\n    optional: true\n"
}

fn sha256_hex_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0_u8; 8192];
    loop {
        let read = file
            .read(&mut buf)
            .with_context(|| format!("reading {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn stage_shared_vm_runtime_payload(source_path: &Path, destination_path: &Path) -> Result<()> {
    if let Some(parent) = destination_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let tmp_path = destination_path.with_extension("tmp");
    fs::copy(source_path, &tmp_path).with_context(|| {
        format!(
            "staging shared VM runtime payload {} -> {}",
            source_path.display(),
            tmp_path.display()
        )
    })?;
    fs::rename(&tmp_path, destination_path).with_context(|| {
        format!(
            "finalizing shared VM runtime payload {} -> {}",
            tmp_path.display(),
            destination_path.display()
        )
    })?;
    Ok(())
}

pub(super) fn stage_shared_vm_cloud_init_seed(
    data_root: &Path,
    runtime_root: &Path,
    preserve_existing_image: bool,
) -> Result<Option<PathBuf>> {
    let guest_agent_path = shared_vm_guest_agent_helper_path(runtime_root);
    if !guest_agent_path.is_file() {
        bail!(
            "AVF Linux runtime is missing guest-agent payload at {}",
            guest_agent_path.display()
        );
    }
    let egress_proxy_path = shared_vm_egress_proxy_helper_path(runtime_root);
    let container_stack_runtime_path = shared_vm_container_stack_helper_path(runtime_root);
    if !container_stack_runtime_path.is_file() {
        bail!(
            "AVF Linux runtime is missing guest container-stack payload at {}",
            container_stack_runtime_path.display()
        );
    }
    let container_stack_payload_path = shared_vm_container_stack_payload_path(data_root);
    stage_shared_vm_runtime_payload(&container_stack_runtime_path, &container_stack_payload_path)?;
    let container_stack_sha256 = sha256_hex_file(&container_stack_payload_path)?;
    let image_path = shared_vm_cloud_init_image_path(data_root);
    let seed_root = shared_vm_cloud_init_root(data_root);
    let guest_agent_bytes = fs::read(&guest_agent_path)
        .with_context(|| format!("reading {}", guest_agent_path.display()))?;
    let egress_proxy_bytes = if egress_proxy_path.is_file() {
        Some(
            fs::read(&egress_proxy_path)
                .with_context(|| format!("reading {}", egress_proxy_path.display()))?,
        )
    } else {
        None
    };
    let meta_data = render_shared_vm_cloud_init_meta_data(
        data_root,
        &guest_agent_bytes,
        egress_proxy_bytes.as_deref(),
        &container_stack_sha256,
    )?;
    let user_data = render_shared_vm_cloud_init_user_data(
        data_root,
        &guest_agent_bytes,
        egress_proxy_bytes.as_deref(),
        &container_stack_payload_path,
        &container_stack_sha256,
    )?;
    let network_config = render_shared_vm_cloud_init_network_config();
    let seed_digest = shared_vm_cloud_init_seed_digest(&meta_data, &user_data, &network_config);
    let seed_digest_path = shared_vm_cloud_init_seed_digest_path(data_root);
    if preserve_existing_image
        && image_path.is_file()
        && fs::read_to_string(&seed_digest_path)
            .ok()
            .map(|value| value.trim() == seed_digest)
            .unwrap_or(false)
    {
        return Ok(Some(image_path));
    }

    fs::remove_dir_all(&seed_root).ok();
    fs::create_dir_all(&seed_root).with_context(|| format!("creating {}", seed_root.display()))?;
    fs::write(shared_vm_cloud_init_meta_data_path(data_root), &meta_data).with_context(|| {
        format!(
            "writing {}",
            shared_vm_cloud_init_meta_data_path(data_root).display()
        )
    })?;
    fs::write(shared_vm_cloud_init_user_data_path(data_root), &user_data).with_context(|| {
        format!(
            "writing {}",
            shared_vm_cloud_init_user_data_path(data_root).display()
        )
    })?;
    fs::write(
        shared_vm_cloud_init_network_config_path(data_root),
        &network_config,
    )
    .with_context(|| {
        format!(
            "writing {}",
            shared_vm_cloud_init_network_config_path(data_root).display()
        )
    })?;
    fs::write(&seed_digest_path, format!("{seed_digest}\n"))
        .with_context(|| format!("writing {}", seed_digest_path.display()))?;

    let source_dir = stage_shared_vm_cloud_init_source_dir(data_root)?;
    fs::remove_file(&image_path).ok();
    create_cloud_init_seed_iso(&source_dir, &image_path)?;

    Ok(Some(image_path))
}

fn stage_shared_vm_cloud_init_source_dir(data_root: &Path) -> Result<PathBuf> {
    let source_dir = shared_vm_cloud_init_root(data_root).join(SHARED_VM_CLOUD_INIT_SOURCE_DIR);
    fs::remove_dir_all(&source_dir).ok();
    fs::create_dir_all(&source_dir)
        .with_context(|| format!("creating {}", source_dir.display()))?;
    for (source, name) in [
        (shared_vm_cloud_init_meta_data_path(data_root), "meta-data"),
        (shared_vm_cloud_init_user_data_path(data_root), "user-data"),
        (
            shared_vm_cloud_init_network_config_path(data_root),
            "network-config",
        ),
    ] {
        fs::copy(&source, source_dir.join(name)).with_context(|| {
            format!("staging {} into {}", source.display(), source_dir.display())
        })?;
    }
    Ok(source_dir)
}

fn create_cloud_init_seed_iso(source_dir: &Path, image_path: &Path) -> Result<()> {
    let source_dir_str = source_dir.display().to_string();
    let image_path_str = image_path.display().to_string();
    run_command(
        "hdiutil",
        &[
            "makehybrid",
            "-o",
            &image_path_str,
            "-iso",
            "-joliet",
            "-default-volume-name",
            SHARED_VM_CLOUD_INIT_ISO_LABEL,
            &source_dir_str,
        ],
        "creating cloud-init seed ISO",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_shared_vm_cloud_init_source_dir_copies_only_seed_payloads() {
        let temp = std::env::temp_dir().join(format!(
            "ctx-avf-cloud-init-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(shared_vm_cloud_init_root(&temp)).unwrap();
        fs::write(shared_vm_cloud_init_meta_data_path(&temp), "meta").unwrap();
        fs::write(shared_vm_cloud_init_user_data_path(&temp), "user").unwrap();
        fs::write(shared_vm_cloud_init_network_config_path(&temp), "net").unwrap();
        fs::write(
            shared_vm_cloud_init_root(&temp).join(".seed-digest"),
            "digest",
        )
        .unwrap();
        fs::write(shared_vm_cloud_init_image_path(&temp), "stale-image").unwrap();

        let source_dir = stage_shared_vm_cloud_init_source_dir(&temp).unwrap();
        let mut entries = fs::read_dir(&source_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect::<Vec<_>>();
        entries.sort();
        assert_eq!(entries, vec!["meta-data", "network-config", "user-data"]);
        assert_eq!(
            fs::read_to_string(source_dir.join("meta-data")).unwrap(),
            "meta"
        );
        assert_eq!(
            fs::read_to_string(source_dir.join("user-data")).unwrap(),
            "user"
        );
        assert_eq!(
            fs::read_to_string(source_dir.join("network-config")).unwrap(),
            "net"
        );

        fs::remove_dir_all(&temp).ok();
    }
}
