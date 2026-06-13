use std::path::{Path, PathBuf};

pub(in crate::daemon::workspace_runtime::tests) fn write_ready_runtime_sandbox_cli_shim(
    dir: &Path,
) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let path = dir.join("sandbox-cli-ready-runtime-test.sh");
    let inner_path = crate::test_support::avf_linux_runtime_manager_test_sandbox_cli_path(dir);
    let script = format!(
        "#!/bin/sh\nexec \"{inner}\" \"{data_root}\" \"$@\"\n",
        inner = inner_path.display(),
        data_root = dir.display()
    );
    std::fs::write(&path, script).expect("write ready runtime sandbox CLI shim");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod ready runtime sandbox CLI shim");
    path
}
