use flate2::write::GzEncoder;
use flate2::Compression;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Mutex as StdMutex;
use tar::{Archive, Builder};

use tempfile::tempdir;

use super::archive::{normalize_oci_archive_to_docker_archive, write_directory_to_tar};
use super::*;
use crate::CTX_HARNESS_SANDBOX_CLI_PATH_ENV;

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

fn env_var_test_lock() -> &'static tokio::sync::Mutex<()> {
    crate::sandbox_cli_env_test_lock()
}

fn write_test_oci_image_archive(tar_path: &Path, repo_tag: &str) {
    let build_root = tempdir().expect("build root tempdir");
    let blobs_root = build_root.path().join("blobs/sha256");
    std::fs::create_dir_all(&blobs_root).expect("create oci blobs root");

    let mut layer_tar_builder = Builder::new(Vec::new());
    let layer_bytes = b"env-from-test\n";
    let mut header = tar::Header::new_gnu();
    header.set_mode(0o755);
    header.set_size(layer_bytes.len() as u64);
    header.set_cksum();
    layer_tar_builder
        .append_data(&mut header, "usr/bin/env", &layer_bytes[..])
        .expect("append test layer payload");
    let layer_tar = layer_tar_builder
        .into_inner()
        .expect("finalize test layer tar");
    let diff_id = hex::encode(Sha256::digest(&layer_tar));

    let mut layer_encoder = GzEncoder::new(Vec::new(), Compression::default());
    layer_encoder
        .write_all(&layer_tar)
        .expect("write gzipped test layer tar");
    let layer_blob = layer_encoder
        .finish()
        .expect("finish gzipped test layer tar");
    let layer_digest = hex::encode(Sha256::digest(&layer_blob));
    std::fs::write(blobs_root.join(&layer_digest), &layer_blob).expect("write layer blob");

    let config = json!({
        "architecture": "arm64",
        "os": "linux",
        "rootfs": {
            "type": "layers",
            "diff_ids": [format!("sha256:{diff_id}")],
        },
        "config": {
            "Cmd": ["/usr/bin/env"],
        },
    });
    let config_bytes = serde_json::to_vec(&config).expect("serialize test image config");
    let config_digest = hex::encode(Sha256::digest(&config_bytes));
    std::fs::write(blobs_root.join(&config_digest), &config_bytes).expect("write config blob");

    let manifest_blob = json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
        "config": {
            "mediaType": "application/vnd.docker.container.image.v1+json",
            "digest": format!("sha256:{config_digest}"),
            "size": config_bytes.len(),
        },
        "layers": [{
            "mediaType": "application/vnd.docker.image.rootfs.diff.tar.gzip",
            "digest": format!("sha256:{layer_digest}"),
            "size": layer_blob.len(),
        }],
    });
    let manifest_blob_bytes =
        serde_json::to_vec(&manifest_blob).expect("serialize test manifest blob");
    let manifest_digest = hex::encode(Sha256::digest(&manifest_blob_bytes));
    std::fs::write(blobs_root.join(&manifest_digest), &manifest_blob_bytes)
        .expect("write manifest blob");

    std::fs::write(
        build_root.path().join("manifest.json"),
        serde_json::to_vec(&vec![json!({
            "Config": format!("blobs/sha256/{config_digest}"),
            "RepoTags": [repo_tag],
            "Layers": [format!("blobs/sha256/{layer_digest}")],
        })])
        .expect("serialize legacy manifest"),
    )
    .expect("write legacy manifest");
    std::fs::write(
        build_root.path().join("index.json"),
        serde_json::to_vec(&json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.index.v1+json",
            "manifests": [{
                "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
                "digest": format!("sha256:{manifest_digest}"),
                "size": manifest_blob_bytes.len(),
                "platform": {
                    "architecture": "arm64",
                    "os": "linux",
                },
            }],
        }))
        .expect("serialize test index"),
    )
    .expect("write index.json");
    std::fs::write(
        build_root.path().join("oci-layout"),
        serde_json::to_vec(&json!({ "imageLayoutVersion": "1.0.0" }))
            .expect("serialize oci-layout"),
    )
    .expect("write oci-layout");

    write_directory_to_tar(build_root.path(), tar_path).expect("write test oci archive");
}

fn write_test_plain_archive(tar_path: &Path, file_name: &str, payload: &[u8]) -> Vec<u8> {
    let mut builder = Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_mode(0o644);
    header.set_size(payload.len() as u64);
    header.set_cksum();
    builder
        .append_data(&mut header, file_name, payload)
        .expect("append plain archive payload");
    let archive_bytes = builder.into_inner().expect("finalize plain archive");
    std::fs::write(tar_path, &archive_bytes).expect("write plain archive");
    archive_bytes
}

#[derive(Default)]
struct RecordingObserver {
    logs: StdMutex<Vec<(HarnessSetupPhase, HarnessSetupLogLevel, String)>>,
    progress: StdMutex<Vec<HarnessSetupProgressUpdate>>,
}

impl HarnessSetupObserver for RecordingObserver {
    fn on_phase(&self, _phase: HarnessSetupPhase, _message: &str) {}

    fn on_log(&self, phase: HarnessSetupPhase, level: HarnessSetupLogLevel, message: &str) {
        self.logs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push((phase, level, message.to_string()));
    }

    fn on_progress(&self, progress: HarnessSetupProgressUpdate) {
        self.progress
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(progress);
    }
}

#[test]
fn normalize_oci_archive_to_docker_archive_rewrites_archive_layout() {
    let temp = tempdir().expect("tempdir");
    let source_tar = temp.path().join("source-oci.tar");
    let normalized_tar = temp.path().join("normalized-docker.tar");
    write_test_oci_image_archive(&source_tar, "ghcr.io/ctxrs/ctx-harness:test-oci");

    let normalized = normalize_oci_archive_to_docker_archive(&source_tar, &normalized_tar)
        .expect("normalize oci archive");
    assert!(normalized, "test archive should normalize as OCI");
    assert!(
        normalized_tar.exists(),
        "normalized docker archive should exist"
    );

    let inspect_root = temp.path().join("inspect");
    std::fs::create_dir_all(&inspect_root).expect("create inspect root");
    let file = std::fs::File::open(&normalized_tar).expect("open normalized archive");
    let mut archive = Archive::new(file);
    archive
        .unpack(&inspect_root)
        .expect("unpack normalized archive");

    assert!(
        inspect_root.join("manifest.json").is_file(),
        "normalized archive should contain manifest.json"
    );
    assert!(
        inspect_root.join("repositories").is_file(),
        "normalized archive should contain repositories"
    );
    assert!(
        !inspect_root.join("index.json").exists(),
        "normalized archive must not preserve OCI index.json"
    );
    assert!(
        !inspect_root.join("oci-layout").exists(),
        "normalized archive must not preserve OCI layout markers"
    );
    assert!(
        !inspect_root.join("blobs").exists(),
        "normalized archive must not preserve OCI blob layout"
    );

    let manifest: Value = serde_json::from_slice(
        &std::fs::read(inspect_root.join("manifest.json")).expect("read normalized manifest"),
    )
    .expect("parse normalized manifest");
    let entry = manifest
        .as_array()
        .and_then(|entries| entries.first())
        .expect("normalized manifest entry");
    let layers = entry
        .get("Layers")
        .and_then(Value::as_array)
        .expect("normalized manifest layers");
    let layer_path = inspect_root.join(
        layers
            .first()
            .and_then(Value::as_str)
            .expect("first normalized layer path"),
    );
    let layer_file = std::fs::File::open(&layer_path).expect("open normalized layer tar");
    let mut layer_archive = Archive::new(layer_file);
    let mut found = false;
    for entry in layer_archive.entries().expect("layer tar entries") {
        let mut entry = entry.expect("layer tar entry");
        if entry.path().expect("entry path") == Path::new("usr/bin/env") {
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut bytes).expect("read test layer file");
            assert_eq!(bytes, b"env-from-test\n");
            found = true;
        }
    }
    assert!(found, "normalized layer tar should contain usr/bin/env");
}

#[tokio::test]
async fn load_container_image_emits_heartbeat_logs_and_progress_while_waiting() {
    let _serial = env_var_test_lock().lock().await;
    let temp = tempdir().expect("tempdir");
    let sandbox_cli_path = temp.path().join("sandbox-cli.sh");
    let marker_path = temp.path().join("image-present");
    let tar_path = temp.path().join("ctx-harness.tar");
    std::fs::write(&tar_path, b"fake-image-tar").expect("write image tar");
    std::fs::write(
            &sandbox_cli_path,
            format!(
                "#!/bin/sh\nset -eu\nmarker='{}'\nif [ \"$1\" = \"load\" ] && [ \"$2\" = \"-i\" ]; then\n  sleep 0.25\n  : > \"$marker\"\n  exit 0\nfi\nif [ \"$1\" = \"image\" ] && [ \"$2\" = \"inspect\" ]; then\n  if [ -f \"$marker\" ]; then\n    exit 0\n  fi\n  exit 1\nfi\nprintf 'unexpected sandbox CLI invocation: %s\\n' \"$*\" >&2\nexit 1\n",
                marker_path.display()
            ),
        )
        .expect("write sandbox CLI shim");
    std::fs::set_permissions(&sandbox_cli_path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod sandbox CLI shim");
    let _guard = EnvGuard::set(
        CTX_HARNESS_SANDBOX_CLI_PATH_ENV,
        &sandbox_cli_path.to_string_lossy(),
    );
    let observer = RecordingObserver::default();

    load_container_image_tar(
        temp.path(),
        &SandboxCommandMode::NativeContainer,
        &tar_path,
        "ghcr.io/ctxrs/ctx-harness:test",
        Some(&observer),
    )
    .await
    .expect("image load should succeed");

    let logs = observer
        .logs
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    assert!(logs.iter().any(|(phase, level, message)| {
        *phase == HarnessSetupPhase::ImageLoad
            && *level == HarnessSetupLogLevel::Info
            && message.contains("still loading harness image into local sandbox runtime")
    }));

    let progress = observer
        .progress
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    assert!(progress.iter().any(|update| {
        update.phase == HarnessSetupPhase::ImageLoad && update.active_download.is_none()
    }));
}

#[tokio::test]
async fn load_container_image_waits_for_post_load_visibility() {
    let _serial = env_var_test_lock().lock().await;
    let temp = tempdir().expect("tempdir");
    let sandbox_cli_path = temp.path().join("sandbox-cli.sh");
    let marker_path = temp.path().join("image-present");
    let tar_path = temp.path().join("ctx-harness.tar");
    std::fs::write(&tar_path, b"fake-image-tar").expect("write image tar");
    std::fs::write(
            &sandbox_cli_path,
            format!(
                "#!/bin/sh\nset -eu\nmarker='{}'\nif [ \"$1\" = \"load\" ] && [ \"$2\" = \"-i\" ]; then\n  (sleep 0.1; : > \"$marker\") &\n  printf 'Loaded image: ghcr.io/ctxrs/ctx-harness:test\\n'\n  exit 0\nfi\nif [ \"$1\" = \"image\" ] && [ \"$2\" = \"inspect\" ]; then\n  if [ -f \"$marker\" ]; then\n    exit 0\n  fi\n  exit 1\nfi\nprintf 'unexpected sandbox CLI invocation: %s\\n' \"$*\" >&2\nexit 1\n",
                marker_path.display()
            ),
        )
        .expect("write sandbox CLI shim");
    std::fs::set_permissions(&sandbox_cli_path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod sandbox CLI shim");
    let _guard = EnvGuard::set(
        CTX_HARNESS_SANDBOX_CLI_PATH_ENV,
        &sandbox_cli_path.to_string_lossy(),
    );

    load_container_image_tar(
        temp.path(),
        &SandboxCommandMode::NativeContainer,
        &tar_path,
        "ghcr.io/ctxrs/ctx-harness:test",
        None,
    )
    .await
    .expect("image visibility should settle after load success");
}

#[tokio::test]
async fn load_container_image_shared_vm_normalizes_oci_archive_before_guest_load() {
    let _serial = env_var_test_lock().lock().await;
    let temp = tempdir().expect("tempdir");
    let helper_path = temp.path().join("ctx-avf-linux-helper.sh");
    let marker_path = temp.path().join("image-present");
    let invocation_log_path = temp.path().join("helper-invocations.log");
    let tar_path = temp.path().join("ctx-harness-oci.tar");
    write_test_oci_image_archive(&tar_path, "ghcr.io/ctxrs/ctx-harness:test");

    std::fs::write(
            &helper_path,
            format!(
                "#!/bin/sh\nset -eu\nmarker='{}'\nlog='{}'\ndata_root=''\nif [ \"$1\" != \"shared-vm-exec\" ]; then\n  printf 'unexpected helper invocation: %s\\n' \"$*\" >&2\n  exit 1\nfi\nshift\nwhile [ \"$#\" -gt 0 ]; do\n  case \"$1\" in\n    --data-root)\n      data_root=\"$2\"\n      shift 2\n      ;;\n    --cwd|--command|--user)\n      shift 2\n      ;;\n    --env)\n      shift 2\n      ;;\n    --)\n      shift\n      break\n      ;;\n    *)\n      printf 'unexpected shared-vm-exec arg: %s\\n' \"$1\" >&2\n      exit 1\n      ;;\n  esac\ndone\nprintf '%s\\n' \"$*\" >> \"$log\"\nif [ \"$1\" = \"load\" ]; then\n  if [ \"${{2:-}}\" != \"-i\" ]; then\n    printf 'shared VM image load should use a guest-visible tar path when normalization succeeds\\n' >&2\n    exit 1\n  fi\n  case \"${{3:-}}\" in\n    /mnt/ctx-host/managed/images/docker-archive/*) ;;\n    *)\n      printf 'expected normalized docker archive path, got %s\\n' \"${{3:-}}\" >&2\n      exit 1\n      ;;\n  esac\n  host_tar=\"$data_root/${{3#/mnt/ctx-host/}}\"\n  tar -tf \"$host_tar\" | grep -q '^repositories$'\n  if tar -tf \"$host_tar\" | grep -q '^index.json$'; then\n    printf 'normalized archive unexpectedly retained OCI index.json\\n' >&2\n    exit 1\n  fi\n  : > \"$marker\"\n  exit 0\nfi\nif [ \"$1\" = \"image\" ] && [ \"${{2:-}}\" = \"inspect\" ]; then\n  if [ -f \"$marker\" ]; then\n    exit 0\n  fi\n  exit 1\nfi\nprintf 'unexpected shared-vm sandbox CLI invocation: %s\\n' \"$*\" >&2\nexit 1\n",
                marker_path.display(),
                invocation_log_path.display(),
            ),
        )
        .expect("write helper shim");
    std::fs::set_permissions(&helper_path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod helper shim");

    load_container_image_tar(
        temp.path(),
        &SandboxCommandMode::SharedVm {
            helper_path: helper_path.clone(),
        },
        &tar_path,
        "ghcr.io/ctxrs/ctx-harness:test",
        None,
    )
    .await
    .expect("shared VM image load should normalize OCI archives");

    let invocation_log =
        std::fs::read_to_string(&invocation_log_path).expect("read helper invocation log");
    assert!(
        invocation_log.contains("/mnt/ctx-host/managed/images/docker-archive/"),
        "expected normalized docker archive guest path:\n{invocation_log}"
    );
}

#[tokio::test]
async fn load_container_image_shared_vm_uses_guest_shared_path_for_data_root_tar() {
    let _serial = env_var_test_lock().lock().await;
    let temp = tempdir().expect("tempdir");
    let helper_path = temp.path().join("ctx-avf-linux-helper.sh");
    let marker_path = temp.path().join("image-present");
    let invocation_log_path = temp.path().join("helper-invocations.log");
    let tar_path = temp.path().join("ctx-harness.tar");
    write_test_plain_archive(&tar_path, "payload.txt", b"fake-image-tar-from-host");
    std::fs::write(
            &helper_path,
            format!(
                "#!/bin/sh\nset -eu\nmarker='{}'\nlog='{}'\nif [ \"$1\" != \"shared-vm-exec\" ]; then\n  printf 'unexpected helper invocation: %s\\n' \"$*\" >&2\n  exit 1\nfi\nshift\nwhile [ \"$#\" -gt 0 ]; do\n  case \"$1\" in\n    --data-root|--cwd|--command|--user)\n      shift 2\n      ;;\n    --env)\n      shift 2\n      ;;\n    --)\n      shift\n      break\n      ;;\n    *)\n      printf 'unexpected shared-vm-exec arg: %s\\n' \"$1\" >&2\n      exit 1\n      ;;\n  esac\ndone\nprintf '%s\\n' \"$*\" >> \"$log\"\nif [ \"$1\" = \"load\" ]; then\n  if [ \"${{2:-}}\" != \"-i\" ]; then\n    printf 'shared VM image load should use a guest-visible tar path when available\\n' >&2\n    exit 1\n  fi\n  case \"${{3:-}}\" in\n    /mnt/ctx-host/*) ;;\n    *)\n      printf 'expected guest shared tar path, got %s\\n' \"${{3:-}}\" >&2\n      exit 1\n      ;;\n  esac\n  : > \"$marker\"\n  exit 0\nfi\nif [ \"$1\" = \"image\" ] && [ \"${{2:-}}\" = \"inspect\" ]; then\n  if [ -f \"$marker\" ]; then\n    exit 0\n  fi\n  exit 1\nfi\nprintf 'unexpected shared-vm sandbox CLI invocation: %s\\n' \"$*\" >&2\nexit 1\n",
                marker_path.display(),
                invocation_log_path.display()
            ),
        )
        .expect("write helper shim");
    std::fs::set_permissions(&helper_path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod helper shim");

    load_container_image_tar(
        temp.path(),
        &SandboxCommandMode::SharedVm {
            helper_path: helper_path.clone(),
        },
        &tar_path,
        "ghcr.io/ctxrs/ctx-harness:test",
        None,
    )
    .await
    .expect("shared VM image load should succeed");

    let invocation_log =
        std::fs::read_to_string(&invocation_log_path).expect("read helper invocation log");
    assert!(
        invocation_log.contains("load"),
        "expected shared VM helper to invoke image load:\n{invocation_log}"
    );
    assert!(
        invocation_log.contains("/mnt/ctx-host/ctx-harness.tar"),
        "shared VM image load must use the guest-visible host share path:\n{invocation_log}"
    );
}

#[tokio::test]
async fn load_container_image_shared_vm_streams_tar_outside_data_root_over_stdin() {
    let _serial = env_var_test_lock().lock().await;
    let data_root = tempdir().expect("data root tempdir");
    let tar_root = tempdir().expect("tar root tempdir");
    let helper_path = data_root.path().join("ctx-avf-linux-helper.sh");
    let marker_path = data_root.path().join("image-present");
    let stdin_capture_path = data_root.path().join("load-stdin.tar");
    let invocation_log_path = data_root.path().join("helper-invocations.log");
    let tar_path = tar_root.path().join("ctx-harness.tar");
    let tar_bytes = write_test_plain_archive(&tar_path, "payload.txt", b"fake-image-tar-from-host");
    std::fs::write(
            &helper_path,
            format!(
                "#!/bin/sh\nset -eu\nmarker='{}'\nstdin_capture='{}'\nlog='{}'\nif [ \"$1\" != \"shared-vm-exec\" ]; then\n  printf 'unexpected helper invocation: %s\\n' \"$*\" >&2\n  exit 1\nfi\nshift\nwhile [ \"$#\" -gt 0 ]; do\n  case \"$1\" in\n    --data-root|--cwd|--command|--user)\n      shift 2\n      ;;\n    --env)\n      shift 2\n      ;;\n    --)\n      shift\n      break\n      ;;\n    *)\n      printf 'unexpected shared-vm-exec arg: %s\\n' \"$1\" >&2\n      exit 1\n      ;;\n  esac\ndone\nprintf '%s\\n' \"$*\" >> \"$log\"\nif [ \"$1\" = \"load\" ]; then\n  if [ \"${{2:-}}\" = \"-i\" ]; then\n    printf 'shared VM image load must not receive host path arguments when the tar is outside the shared root\\n' >&2\n    exit 1\n  fi\n  cat > \"$stdin_capture\"\n  : > \"$marker\"\n  exit 0\nfi\nif [ \"$1\" = \"image\" ] && [ \"${{2:-}}\" = \"inspect\" ]; then\n  if [ -f \"$marker\" ]; then\n    exit 0\n  fi\n  exit 1\nfi\nprintf 'unexpected shared-vm sandbox CLI invocation: %s\\n' \"$*\" >&2\nexit 1\n",
                marker_path.display(),
                stdin_capture_path.display(),
                invocation_log_path.display()
            ),
        )
        .expect("write helper shim");
    std::fs::set_permissions(&helper_path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod helper shim");

    load_container_image_tar(
        data_root.path(),
        &SandboxCommandMode::SharedVm {
            helper_path: helper_path.clone(),
        },
        &tar_path,
        "ghcr.io/ctxrs/ctx-harness:test",
        None,
    )
    .await
    .expect("shared VM image load should succeed");

    let invocation_log =
        std::fs::read_to_string(&invocation_log_path).expect("read helper invocation log");
    assert!(
        invocation_log.contains("load"),
        "expected shared VM helper to invoke image load:\n{invocation_log}"
    );
    assert!(
            !invocation_log.contains("-i"),
            "shared VM image load must stream tar bytes when the tar is outside the shared root:\n{invocation_log}"
        );

    let streamed =
        std::fs::read(&stdin_capture_path).expect("read streamed shared VM image tar bytes");
    assert_eq!(streamed, tar_bytes);
}
