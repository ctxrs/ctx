use super::*;
use std::os::unix::fs::PermissionsExt;

pub(super) enum UnrestrictedNetworkPidFile {
    Malformed,
    Stale,
}

pub(super) struct UnrestrictedNetworkHarness {
    temp: tempfile::TempDir,
    fakebin: std::path::PathBuf,
    sandbox_log_path: std::path::PathBuf,
    helper_log_path: std::path::PathBuf,
    pid_file_path: std::path::PathBuf,
    _sandbox_cli: EnvGuard,
    _pid_file: EnvGuard,
    _serial: tokio::sync::MutexGuard<'static, ()>,
}

impl UnrestrictedNetworkHarness {
    pub(super) async fn new() -> Self {
        let serial = env_var_test_lock().lock().await;
        let temp = tempfile::tempdir().expect("tempdir");
        let fakebin = temp.path().join("fakebin");
        std::fs::create_dir_all(&fakebin).expect("create fakebin");
        let sandbox_log_path = temp.path().join("sandbox-cli-invocations.log");
        let helper_log_path = temp.path().join("cleanup-helpers.log");
        let pid_file_path = temp.path().join("ctx-egress-proxy.pid");
        let pid_file = EnvGuard::set(
            "CTX_EGRESS_PROXY_PID_FILE",
            &pid_file_path.to_string_lossy(),
        );

        let sandbox_cli_path = temp.path().join("sandbox-cli.sh");
        std::fs::write(
            &sandbox_cli_path,
            format!(
                "#!/bin/sh\nLOG=\"{log}\"\nFAKEBIN=\"{fakebin}\"\nprintf '%s\\n' \"$*\" >> \"$LOG\"\nif [ \"$1\" = \"exec\" ]; then\n  PATH=\"$FAKEBIN:$PATH\" /bin/sh -c \"$7\"\n  exit $?\nfi\nexit 0\n",
                log = sandbox_log_path.display(),
                fakebin = fakebin.display(),
            ),
        )
        .expect("write sandbox CLI shim");
        chmod_executable(&sandbox_cli_path, "sandbox CLI shim");
        let sandbox_cli = EnvGuard::set(
            "CTX_HARNESS_SANDBOX_CLI_PATH",
            &sandbox_cli_path.to_string_lossy(),
        );

        Self {
            temp,
            fakebin,
            sandbox_log_path,
            helper_log_path,
            pid_file_path,
            _sandbox_cli: sandbox_cli,
            _pid_file: pid_file,
            _serial: serial,
        }
    }

    pub(super) fn root(&self) -> &std::path::Path {
        self.temp.path()
    }

    pub(super) fn pid_file_path(&self) -> &std::path::Path {
        &self.pid_file_path
    }

    pub(super) fn settings(&self) -> ContainerExecutionSettings {
        ContainerExecutionSettings {
            network_mode: ContainerNetworkMode::All,
            runtime: ctx_settings_model::ContainerRuntimeKind::NativeContainer,
            ..Default::default()
        }
    }

    pub(super) fn write_proxy_pid(&self, pid_file: UnrestrictedNetworkPidFile) {
        let contents = match pid_file {
            UnrestrictedNetworkPidFile::Malformed => b"\n".as_slice(),
            UnrestrictedNetworkPidFile::Stale => b"999999\n".as_slice(),
        };
        std::fs::write(&self.pid_file_path, contents).expect("write fake proxy pid file");
    }

    pub(super) fn write_failing_teardown_helpers(&self) {
        self.write_rm_script(
            "#!/bin/sh\nprintf 'rm %s\\n' \"$*\" >> \"{log}\"\necho 'failed to remove proxy pid file' >&2\nexit 23\n",
        );
        self.write_iptables_script(
            "#!/bin/sh\nprintf 'iptables %s\\n' \"$*\" >> \"{log}\"\nif [ \"$1\" = \"-P\" ] && [ \"$2\" = \"OUTPUT\" ] && [ \"$3\" = \"ACCEPT\" ]; then\n  echo 'failed to reset output policy' >&2\n  exit 42\nfi\nexit 0\n",
        );
    }

    pub(super) fn write_successful_teardown_helpers(&self) {
        self.write_rm_script(
            "#!/bin/sh\nprintf 'rm %s\\n' \"$*\" >> \"{log}\"\nexec /bin/rm \"$@\"\n",
        );
        self.write_iptables_script(
            "#!/bin/sh\nprintf 'iptables %s\\n' \"$*\" >> \"{log}\"\nexit 0\n",
        );
    }

    pub(super) fn sandbox_log(&self) -> String {
        std::fs::read_to_string(&self.sandbox_log_path).expect("read invocation log")
    }

    pub(super) fn helper_log(&self) -> String {
        std::fs::read_to_string(&self.helper_log_path).expect("read cleanup helper invocation log")
    }

    fn write_rm_script(&self, body: &str) {
        let rm_path = self.fakebin.join("rm");
        std::fs::write(
            &rm_path,
            body.replace("{log}", &self.helper_log_path.display().to_string()),
        )
        .expect("write fake rm");
        chmod_executable(&rm_path, "fake rm");
    }

    fn write_iptables_script(&self, body: &str) {
        let iptables_path = self.fakebin.join("iptables");
        std::fs::write(
            &iptables_path,
            body.replace("{log}", &self.helper_log_path.display().to_string()),
        )
        .expect("write fake iptables");
        chmod_executable(&iptables_path, "fake iptables");
    }
}

fn chmod_executable(path: &std::path::Path, label: &str) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .unwrap_or_else(|err| panic!("chmod {label}: {err}"));
}
