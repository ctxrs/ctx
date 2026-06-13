use super::*;
use std::path::{Path, PathBuf};

pub(super) struct HelperDetectionFixture {
    temp: tempfile::TempDir,
    machine_name: String,
    helper_dir: PathBuf,
}

impl HelperDetectionFixture {
    pub(super) fn new() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let machine_name = sandbox_machine_name(temp.path());
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
        Self {
            temp,
            machine_name,
            helper_dir,
        }
    }

    pub(super) fn root_path(&self) -> &Path {
        self.temp.path()
    }

    pub(super) fn machine_name(&self) -> &str {
        &self.machine_name
    }

    pub(super) fn collection_gvproxy_command(&self) -> Vec<String> {
        vec![
            self.helper_dir
                .join("gvproxy")
                .to_string_lossy()
                .into_owned(),
            self.machine_name.clone(),
            sandbox_machine_temp_root(self.temp.path())
                .join("sandbox-cli")
                .join(format!("{}-api.sock", self.machine_name))
                .to_string_lossy()
                .into_owned(),
        ]
    }

    pub(super) fn process_shape_gvproxy_command(&self) -> Vec<String> {
        vec![
            self.helper_dir
                .join("gvproxy")
                .to_string_lossy()
                .into_owned(),
            sandbox_machine_temp_root(self.temp.path())
                .join("sandbox-cli")
                .join(format!("{}-api.sock", self.machine_name))
                .to_string_lossy()
                .into_owned(),
            self.machine_name.clone(),
        ]
    }

    pub(super) fn matching_vfkit_command(&self) -> Vec<String> {
        vec![
            "/opt/homebrew/bin/vfkit".to_string(),
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
                .to_string_lossy()
                .into_owned(),
            self.machine_name.clone(),
        ]
    }

    pub(super) fn wrong_machine_vfkit_command(&self) -> Vec<String> {
        vec![
            String::from("/opt/homebrew/bin/vfkit"),
            self.temp
                .path()
                .join("sandbox-cli")
                .join("xdg")
                .join("data")
                .join("containers")
                .join("sandbox-cli")
                .join("machine")
                .join("applehv")
                .join("ctx-someone-else-arm64.raw")
                .to_string_lossy()
                .into_owned(),
        ]
    }

    pub(super) fn host_gvproxy_command(&self) -> Vec<String> {
        vec![
            String::from("/opt/homebrew/libexec/sandbox-cli/gvproxy"),
            String::from("/tmp/sandbox-cli/sandbox-machine-default-api.sock"),
            String::from("sandbox-machine-default"),
        ]
    }

    pub(super) fn macos_ps_output_with_host_helper(&self) -> String {
        let gvproxy_line = format!(
            " 6622 {} -mtu 1500 -listen-vfkit unixgram://{} -forward-sock {} -forward-identity {} -pid-file {}/gvproxy.pid",
            self.helper_dir.join("gvproxy").display(),
            sandbox_machine_temp_root(self.temp.path())
                .join("sandbox-cli")
                .join(format!("{}-gvproxy.sock", self.machine_name))
                .display(),
            sandbox_machine_temp_root(self.temp.path())
                .join("sandbox-cli")
                .join(format!("{}-api.sock", self.machine_name))
                .display(),
            self.temp
                .path()
                .join("sandbox-cli")
                .join("xdg")
                .join("data")
                .join("containers")
                .join("sandbox-cli")
                .join("machine")
                .join("machine")
                .display(),
            sandbox_machine_temp_root(self.temp.path()).join("sandbox-cli").display(),
        );
        let vfkit_line = format!(
            "12484 /home/fixture/Library/Application Support/vfkit --device virtio-blk,path={} --device virtio-vsock,port=1025,socketURL={} --device virtio-net,unixSocketPath={}",
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
                .join(format!("{}.sock", self.machine_name))
                .display(),
            sandbox_machine_temp_root(self.temp.path())
                .join("sandbox-cli")
                .join(format!("{}-gvproxy.sock", self.machine_name))
                .display(),
        );
        let host_line = String::from(
            "88 /opt/homebrew/libexec/sandbox-cli/gvproxy -forward-sock /tmp/sandbox-cli/sandbox-machine-default-api.sock sandbox-machine-default",
        );
        format!("{gvproxy_line}\n{vfkit_line}\n{host_line}\n")
    }
}
