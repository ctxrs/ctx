#[allow(unused_imports)]
use super::*;

pub(crate) fn ctx_from_binary(temp: &TempDir, binary: &Path) -> Command {
    let mut command = Command::new(binary);
    apply_hermetic_env(&mut command, temp);
    command
}

pub(crate) fn copied_ctx_binary(temp: &TempDir) -> PathBuf {
    let source = PathBuf::from(Command::cargo_bin("ctx").unwrap().get_program().to_owned());
    let target = temp.path().join(if cfg!(windows) {
        "ctx-test-copy.exe"
    } else {
        "ctx-test-copy"
    });
    if fs::hard_link(&source, &target).is_err() {
        fs::copy(&source, &target).unwrap();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(&target).unwrap().permissions();
        permissions.set_mode(permissions.mode() | 0o700);
        fs::set_permissions(&target, permissions).unwrap();
    }
    target
}

pub(crate) fn hosted_install_marker_path(binary: &Path) -> PathBuf {
    let mut marker = binary.as_os_str().to_owned();
    marker.push(".install.json");
    PathBuf::from(marker)
}

pub(crate) fn copy_dir_all(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let entry_path = entry.path();
        let target = to.join(entry.file_name());
        if entry_path.is_dir() {
            copy_dir_all(&entry_path, &target);
        } else {
            fs::copy(entry_path, target).unwrap();
        }
    }
}

pub(crate) fn file_url(path: &Path) -> String {
    format!("file://{}", path.display())
}

#[cfg(unix)]
pub(crate) fn write_fake_ctx_binary(path: &Path, version: &str) -> Vec<u8> {
    let bytes = format!("#!/bin/sh\nprintf 'ctx {version}\\n'\n").into_bytes();
    fs::write(path, &bytes).unwrap();
    make_file_executable(path);
    bytes
}

#[cfg(unix)]
pub(crate) fn write_hanging_ctx_binary(path: &Path) {
    fs::write(
        path,
        "#!/bin/sh\n\
if [ -n \"${CTX_SHADOW_MARKER:-}\" ]; then\n\
  touch \"$CTX_SHADOW_MARKER\"\n\
fi\n\
sleep 5\n\
printf 'ctx 0.1.0\\n'\n",
    )
    .unwrap();
    make_file_executable(path);
}

#[cfg(unix)]
pub(crate) fn make_file_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

#[cfg(unix)]
pub(crate) fn install_marker_path(target: &Path) -> PathBuf {
    let file_name = target.file_name().unwrap().to_str().unwrap();
    target.with_file_name(format!("{file_name}.install.json"))
}

#[cfg(unix)]
pub(crate) fn fake_release(temp: &TempDir, latest_version: &str) -> FakeRelease {
    let bin_dir = temp.path().join("bin");
    let release_dir = temp.path().join("release");
    fs::create_dir_all(&bin_dir).unwrap();
    fs::create_dir_all(&release_dir).unwrap();

    let target = bin_dir.join("ctx");
    let current_bytes = write_fake_ctx_binary(&target, env!("CARGO_PKG_VERSION"));
    let current_sha = sha256_hex(&current_bytes);

    let marker = json!({
        "schema_version": 1,
        "manager": "ctx-hosted-installer",
        "install_attempt_id": "ia_test_upgrade_attempt",
        "install_path": target,
        "platform": test_platform_key().replace('_', "-"),
        "channel": "stable",
        "version": env!("CARGO_PKG_VERSION"),
        "sha256": current_sha,
        "metadata_url": null,
        "artifact_url": null,
    });
    fs::write(
        install_marker_path(&target),
        serde_json::to_vec_pretty(&marker).unwrap(),
    )
    .unwrap();

    let artifact = release_dir.join("ctx");
    let artifact_bytes = write_fake_ctx_binary(&artifact, latest_version);
    let artifact_sha = sha256_hex(&artifact_bytes);
    let platform = test_platform_key();
    let metadata = release_dir.join("ctx-release-metadata.env");
    let metadata_body = format!(
        "CTX_RELEASE_SCHEMA_VERSION=1\n\
CTX_RELEASE_CHANNEL=stable\n\
CTX_RELEASE_VERSION={latest_version}\n\
CTX_RELEASE_BASE_URL={}\n\
CTX_RELEASE_ARTIFACT_{platform}=ctx\n\
CTX_RELEASE_SHA256_{platform}={artifact_sha}\n\
CTX_RELEASE_SELF_UPGRADE_ALLOWED=true\n\
CTX_RELEASE_AUTO_UPGRADE_ALLOWED=true\n",
        file_url(&release_dir)
    );
    fs::write(&metadata, &metadata_body).unwrap();
    let signature = release_dir.join("ctx-release-metadata.env.sig");
    fs::write(
        &signature,
        format!("{}\n", sign_test_release_metadata(metadata_body.as_bytes())),
    )
    .unwrap();

    FakeRelease {
        target,
        metadata,
        signature,
        artifact_sha,
    }
}

pub(crate) fn expected_device_path(_home: &Path, state: &Path) -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        state.join("ctx").join("device.json")
    }
    #[cfg(target_os = "macos")]
    {
        _home
            .join("Library")
            .join("Application Support")
            .join("ctx")
            .join("device.json")
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        state.join("ctx").join("device.json")
    }
}
