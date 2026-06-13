use super::*;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub(super) struct HelperProcessFixture {
    temp: tempfile::TempDir,
    machine_name: String,
    fakebin: PathBuf,
    helper_dir: PathBuf,
    ps_count_path: PathBuf,
    pkill_log_path: PathBuf,
}

impl HelperProcessFixture {
    pub(super) fn new() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let machine_name = sandbox_machine_name(temp.path());
        let fakebin = temp.path().join("fakebin");
        std::fs::create_dir_all(&fakebin).expect("create fakebin");
        let helper_dir = temp
            .path()
            .join("managed")
            .join("runtimes")
            .join("sandbox-cli")
            .join("macos")
            .join("aarch64")
            .join("sandbox-cli-5.8.0")
            .join("usr")
            .join("libexec")
            .join("sandbox-cli");
        let ps_count_path = temp.path().join("ps-count");
        let pkill_log_path = temp.path().join("pkill-invocations.log");

        Self {
            temp,
            machine_name,
            fakebin,
            helper_dir,
            ps_count_path,
            pkill_log_path,
        }
    }

    pub(super) fn root_path(&self) -> &Path {
        self.temp.path()
    }

    pub(super) fn machine_name(&self) -> &str {
        &self.machine_name
    }

    pub(super) fn pkill_log_path(&self) -> &Path {
        &self.pkill_log_path
    }

    pub(super) fn gvproxy_command(&self) -> String {
        format!(
            "{} -forward-sock {} {}",
            self.helper_dir.join("gvproxy").display(),
            sandbox_machine_temp_root(self.temp.path())
                .join("sandbox-cli")
                .join(format!("{}-api.sock", self.machine_name))
                .display(),
            self.machine_name,
        )
    }

    pub(super) fn vfkit_command(&self) -> String {
        format!(
            "/opt/homebrew/bin/vfkit --device virtio-blk,path={} --device virtio-net,unixSocketPath={}",
            self.temp
                .path()
                .join("sandbox-cli")
                .join("xdg")
                .join("data")
                .join("containers")
                .join("sandbox-cli")
                .join("machine")
                .join("applehv")
                .join(format!("{}-arm64.raw", self.machine_name))
                .display(),
            sandbox_machine_temp_root(self.temp.path())
                .join("sandbox-cli")
                .join(format!("{}-gvproxy.sock", self.machine_name))
                .display(),
        )
    }

    pub(super) fn write_fake_ps(&self, first_output: &str, second_output: &str) {
        let ps_path = self.fakebin.join("ps");
        std::fs::write(
            &ps_path,
            format!(
                "#!/bin/sh\ncount=0\nif [ -f \"{count_path}\" ]; then\n  count=$(cat \"{count_path}\")\nfi\ncount=$((count + 1))\nprintf '%s' \"$count\" > \"{count_path}\"\nif [ \"$1\" = \"-axo\" ] && [ \"$count\" -eq 1 ]; then\n  printf '{first_output}'\n  exit 0\nfi\nif [ \"$1\" = \"-axo\" ] && [ \"$count\" -eq 2 ]; then\n  printf '{second_output}'\n  exit 0\nfi\nexit 1\n",
                count_path = self.ps_count_path.display(),
            ),
        )
        .expect("write fake ps");
        std::fs::set_permissions(&ps_path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake ps");
    }

    pub(super) fn write_fake_pkill(&self, body: &str) {
        let kill_path = self.fakebin.join("pkill");
        std::fs::write(&kill_path, format!("#!/bin/sh\n{body}")).expect("write fake pkill");
        std::fs::set_permissions(&kill_path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake pkill");
    }

    pub(super) fn install_path_guard(&self) -> EnvGuard {
        let prior_path = std::env::var("PATH").unwrap_or_default();
        let path_value = format!("{}:{prior_path}", self.fakebin.display());
        EnvGuard::set("PATH", &path_value)
    }
}
