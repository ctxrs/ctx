use std::path::{Path, PathBuf};

pub(super) fn write_lifecycle_sandbox_cli_shim(
    dir: &Path,
    host_os: &str,
    host_arch: &str,
) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let path = dir.join("ctx-avf-linux-sandbox-cli-runtime-manager-test.sh");
    let script = sandbox_cli_script(host_os, host_arch);
    std::fs::write(&path, script).expect("write AVF sandbox helper shim");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod AVF sandbox helper shim");
    path
}

fn sandbox_cli_script(host_os: &str, host_arch: &str) -> String {
    include_str!("sandbox_cli_shim.sh")
        .replace("__HOST_OS__", host_os)
        .replace("__HOST_ARCH__", host_arch)
}
