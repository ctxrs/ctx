use super::*;

pub(crate) fn render_shared_vm_cloud_init_user_data(
    data_root: &Path,
    guest_agent_bytes: &[u8],
    egress_proxy_bytes: Option<&[u8]>,
    container_stack_host_path: &Path,
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
    let guest_container_stack_payload_path =
        shared_vm_guest_host_share_path(data_root, container_stack_host_path)
            .context("guest container-stack payload path should live under shared data root")?;
    let guest_agent_b64 = indent_cloud_init_block(&wrap_cloud_init_base64(guest_agent_bytes), 6);
    let guest_agent_service = indent_cloud_init_block(
        &render_shared_vm_guest_agent_service(
            &guest_ready_marker_path,
            &guest_failure_marker_path,
            &guest_agent_log_path,
        ),
        6,
    );
    let guest_agent_launcher = indent_cloud_init_block(
        &render_shared_vm_guest_agent_launcher_script(
            &guest_ready_marker_path,
            &guest_failure_marker_path,
            &guest_agent_log_path,
        ),
        6,
    );
    let host_data_service = indent_cloud_init_block(&render_shared_vm_host_data_mount_service(), 6);
    let data_disk_script = indent_cloud_init_block(&render_shared_vm_data_disk_script(), 6);
    let data_disk_service = indent_cloud_init_block(&render_shared_vm_data_disk_service(), 6);
    let guest_policy_script = indent_cloud_init_block(&render_shared_vm_guest_policy_script(), 6);
    let containerd_service = indent_cloud_init_block(&render_shared_vm_containerd_service(), 6);
    let buildkit_service = indent_cloud_init_block(&render_shared_vm_buildkit_service(), 6);
    let install_script = indent_cloud_init_block(
        &render_shared_vm_container_stack_install_script(
            &guest_container_stack_payload_path,
            container_stack_sha256,
        ),
        6,
    );
    let egress_proxy_block = egress_proxy_bytes.map(|bytes| {
        let egress_proxy_b64 = indent_cloud_init_block(&wrap_cloud_init_base64(bytes), 6);
        format!(
            "  - path: /usr/local/bin/ctx-egress-proxy\n    permissions: '0755'\n    encoding: b64\n    content: |\n{egress_proxy_b64}\n"
        )
    });
    let escaped_container_stack_host_path =
        shell_escape_single_quotes(&guest_container_stack_payload_path.display().to_string());
    let mut write_files = String::new();
    write_files.push_str(&format!(
        "  - path: /usr/local/bin/ctx-avf-linux-guest-agent\n    permissions: '0755'\n    encoding: b64\n    content: |\n{guest_agent_b64}\n"
    ));
    write_files.push_str(&format!(
        "  - path: {launcher_path}\n    permissions: '0755'\n    content: |\n{guest_agent_launcher}\n",
        launcher_path = SHARED_VM_GUEST_AGENT_LAUNCHER_PATH,
    ));
    write_files.push_str(&egress_proxy_block.unwrap_or_default());
    write_files.push_str(&format!(
        "  - path: {data_disk_install_path}\n    permissions: '0755'\n    content: |\n{data_disk_script}\n",
        data_disk_install_path = SHARED_VM_DATA_DISK_INSTALL_PATH,
        data_disk_script = data_disk_script,
    ));
    write_files.push_str(&format!(
        "  - path: {policy_install_path}\n    permissions: '0755'\n    content: |\n{guest_policy_script}\n",
        policy_install_path = SHARED_VM_GUEST_POLICY_INSTALL_PATH,
        guest_policy_script = guest_policy_script,
    ));
    write_files.push_str(&format!(
        "  - path: {install_path}\n    permissions: '0755'\n    content: |\n{install_script}\n",
        install_path = SHARED_VM_GUEST_CONTAINER_STACK_INSTALL_PATH,
    ));
    write_files.push_str(&format!(
        "  - path: /etc/systemd/system/{data_disk_service_name}\n    permissions: '0644'\n    content: |\n{data_disk_service}\n",
        data_disk_service_name = SHARED_VM_DATA_DISK_SERVICE_NAME,
        data_disk_service = data_disk_service,
    ));
    write_files.push_str(&format!(
        "  - path: /etc/systemd/system/{host_data_service_name}\n    permissions: '0644'\n    content: |\n{host_data_service}\n",
        host_data_service_name = SHARED_VM_HOST_DATA_SERVICE_NAME,
    ));
    write_files.push_str(&format!(
        "  - path: /etc/systemd/system/{containerd_service_name}\n    permissions: '0644'\n    content: |\n{containerd_service}\n",
        containerd_service_name = SHARED_VM_CONTAINERD_SERVICE_NAME,
    ));
    write_files.push_str(&format!(
        "  - path: /etc/systemd/system/{buildkit_service_name}\n    permissions: '0644'\n    content: |\n{buildkit_service}\n",
        buildkit_service_name = SHARED_VM_BUILDKIT_SERVICE_NAME,
    ));
    write_files.push_str(&format!(
        "  - path: /etc/systemd/system/{guest_agent_service_name}\n    permissions: '0644'\n    content: |\n{guest_agent_service}\n",
        guest_agent_service_name = SHARED_VM_GUEST_AGENT_SERVICE_NAME,
    ));
    let prepare_guest_agent_cmd = indent_cloud_init_block(
        &format!(
            "echo \"[ctx-avf-linux] preparing {guest_agent_service_name}\" >/dev/hvc0\nls -l /usr/local/bin/ctx-avf-linux-guest-agent >/dev/hvc0 2>&1\nls -l {launcher_path} >/dev/hvc0 2>&1\nls -l /etc/systemd/system/{guest_agent_service_name} >/dev/hvc0 2>&1\nls -l '{container_stack_host_path}' >/dev/hvc0 2>&1",
            guest_agent_service_name = SHARED_VM_GUEST_AGENT_SERVICE_NAME,
            launcher_path = SHARED_VM_GUEST_AGENT_LAUNCHER_PATH,
            container_stack_host_path = escaped_container_stack_host_path,
        ),
        4,
    );
    let enable_data_disk_cmd = indent_cloud_init_block(
        &format!(
            "systemctl enable --now {data_disk_service_name} >/dev/hvc0 2>&1 || (systemctl status {data_disk_service_name} --no-pager >/dev/hvc0 2>&1; exit 1)",
            data_disk_service_name = SHARED_VM_DATA_DISK_SERVICE_NAME,
        ),
        4,
    );
    let enable_host_data_cmd = indent_cloud_init_block(
        &format!(
            "systemctl enable --now {host_data_service_name} >/dev/hvc0 2>&1 || (systemctl status {host_data_service_name} --no-pager >/dev/hvc0 2>&1; exit 1)",
            host_data_service_name = SHARED_VM_HOST_DATA_SERVICE_NAME,
        ),
        4,
    );
    let early_bootcmd = indent_cloud_init_block(&render_shared_vm_early_bootcmd(), 4);
    let install_container_stack_cmd = indent_cloud_init_block(
        &format!(
            "{install_path} >/dev/hvc0 2>&1",
            install_path = SHARED_VM_GUEST_CONTAINER_STACK_INSTALL_PATH,
        ),
        4,
    );
    let enable_containerd_cmd = indent_cloud_init_block(
        &format!(
            "systemctl enable --now {containerd_service_name} >/dev/hvc0 2>&1 || (systemctl status {containerd_service_name} --no-pager >/dev/hvc0 2>&1; exit 1)",
            containerd_service_name = SHARED_VM_CONTAINERD_SERVICE_NAME,
        ),
        4,
    );
    let enable_buildkit_cmd = indent_cloud_init_block(
        &format!(
            "systemctl enable --now {buildkit_service_name} >/dev/hvc0 2>&1 || (systemctl status {buildkit_service_name} --no-pager >/dev/hvc0 2>&1; exit 1)",
            buildkit_service_name = SHARED_VM_BUILDKIT_SERVICE_NAME,
        ),
        4,
    );
    let enable_guest_agent_cmd = indent_cloud_init_block(
        &format!(
            "systemctl enable --now {guest_agent_service_name} >/dev/hvc0 2>&1 || (systemctl status {guest_agent_service_name} --no-pager >/dev/hvc0 2>&1; exit 1)",
            guest_agent_service_name = SHARED_VM_GUEST_AGENT_SERVICE_NAME,
        ),
        4,
    );
    Ok(format!(
        "#cloud-config\nbootcmd:\n  - |\n{early_bootcmd}\nwrite_files:\n{write_files}runcmd:\n  - [ systemctl, daemon-reload ]\n  - |\n{enable_host_data_cmd}\n  - |\n{enable_data_disk_cmd}\n  - |\n{prepare_guest_agent_cmd}\n  - |\n{install_container_stack_cmd}\n  - |\n{enable_containerd_cmd}\n  - |\n{enable_buildkit_cmd}\n  - |\n{enable_guest_agent_cmd}\n",
        prepare_guest_agent_cmd = prepare_guest_agent_cmd,
        enable_host_data_cmd = enable_host_data_cmd,
        enable_data_disk_cmd = enable_data_disk_cmd,
        early_bootcmd = early_bootcmd,
        install_container_stack_cmd = install_container_stack_cmd,
        enable_containerd_cmd = enable_containerd_cmd,
        enable_buildkit_cmd = enable_buildkit_cmd,
        enable_guest_agent_cmd = enable_guest_agent_cmd,
    ))
}
