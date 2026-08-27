use assert_cmd::Command;
use base64::{prelude::BASE64_STANDARD as BASE64, Engine as _};
use ring::{
    rand::SystemRandom,
    signature::{RsaKeyPair, RSA_PKCS1_SHA256},
};
use serde_json::json;
use std::{
    env,
    ffi::OsString,
    fs,
    io::Cursor,
    path::{Path, PathBuf},
};
use tempfile::TempDir;

#[cfg(unix)]
use flate2::{write::GzEncoder, Compression};
#[cfg(unix)]
use tar::{Builder as TarBuilder, EntryType, Header};

use super::{copied_binary, copied_ctx_binary, data_root, file_url};

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

pub(crate) const TEST_RELEASE_PRIVATE_KEY_PEM: &str = r#"-----BEGIN PRIVATE KEY-----
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC4czAqM5XMipjl
QxTatkq8VmeS13e2aEpqT1v/XGL17o43i624H80xEbvB5tV/YzpO5N8sb4wEUj9h
yNzB5/U4S6SM/QadcA9fk/V7KeBOcz15PvZaU0UNp/dKVvzEFtxv/rjQCfA80C2N
30lTwti8pts4IulxVeB7BkIvqs3XADV5zBVwRACHWt5MKcMrXfBcmKRy8TLdNeml
lPgU3V2pj4c54KQ0aoy3/970+ry3P+eT8BlatU4k8R+pS0Oy4s3Ezczj9UrPCREd
1m2tAqaw8B0wRoei+nHEPWqbbzgx8fepv38U9LXmzYpCjSWSZ+zcZ4YBsXlyab3a
2PjyZ42HAgMBAAECggEAHQvis1qhRe8zibMJJzIazdLrh5fP3dVJlrk9mxag7Oqu
0bd42WyEoywQPcZMq71kEsV/EZ/VVF7hZVQ803pkRwO+e4djEcryWNJTj5w2GxSR
wzSzleDUGITxb+8H6hdRin95+iT+hI0iB1v4z6x49ihukEYLLhJgge8n4BrNRISa
P+SInTo/UzO5NIzh8HdQBJqkammS4c/Eij0jVw9onMpOFWKAxcs0hmk1SSy6KouD
yDBqp6m6ILlAuggZutkn+7X4QUzvgBQePYy6BNX57dmFpBWt/8DVc5m4Ciwd+s1L
CLRL86X6YLtc5wTQvdX/xHbW9m/FUXk5EvK2eQ+IyQKBgQD7B4aFQFwHiRjO323d
I7FUcSgsBEz/pYiucEF5c+GQUpSq/ORgFg7sYLAv3312nbu/TdIw2O0KxhhfUX6j
iRGe5NzSogUpRHk3Rq/tbQKULezDi9Lc7ROUuMYRpsHSjiVLB+zYdRDZULBqAdSo
3A0c0/xfCKB0efIJt4SfTVtcvwKBgQC8Git0ry8csFgmwmuxHL1nBmxXBLyZ04Ko
PQ+WyLPgL8cVP3Bf19zXDtmeoPSD8bZODys4UKit3zpZDEKN9S8JeN2E1h5MTgKN
wmOxdimAo0xKHJ/EnvxzfR5UzbrGiuajCFvIDPjItl3gSJ2av1cwQ8ljZBtOoqdX
KiTNCw7ZOQKBgQCTEuSom32P2K4VPmiC4M+blrSfnWFzgoujEBf8TX2BbjC2QXaY
KTRTH476bWl3npCKU9DrV50B6/AJoJievcb6HkKWkeCOPhT64speQ7j4EjQemYRQ
dgI750n8u4PhlfCZlioY4/WcLR8+7JWo3Uw9cKHzF/3SYEQDl2b3Yn49xwKBgFda
g+HNVUCqeFWPpnl60k6dAgUrUvbQ7fV5Xdr1W+t55KdubZ5k3c8Vu2RadRMtVi9M
BhNCCgOtDii6c9H/EhgBBEajNTDUbYUtyCRqrn1p2Iz2XA/wkWaErWhOnjWD3fXK
dO0jcQms/02gC2kJANGOOWEp5TCQgswM60g5oWypAoGADlZTP+97w9NcOJoQdZVi
+I5NLRKHUjAvax4BALtH5uuVIwj6cSwheRkBzd7rU1aQ65yuUYwIznDsC2rir26x
ehIUvhTehZf04otZbIo7UUvFhohRmX5k4/Idf/njMa/dA5afBMM1xE7IkoeHQyLc
3I9zapKTmyq90XvKHvA9eyA=
-----END PRIVATE KEY-----"#;

pub(crate) const TEST_RELEASE_PUBLIC_KEY_PEM: &str = r#"-----BEGIN RSA PUBLIC KEY-----
MIIBCgKCAQEAuHMwKjOVzIqY5UMU2rZKvFZnktd3tmhKak9b/1xi9e6ON4utuB/N
MRG7webVf2M6TuTfLG+MBFI/Ycjcwef1OEukjP0GnXAPX5P1eyngTnM9eT72WlNF
Daf3Slb8xBbcb/640AnwPNAtjd9JU8LYvKbbOCLpcVXgewZCL6rN1wA1ecwVcEQA
h1reTCnDK13wXJikcvEy3TXppZT4FN1dqY+HOeCkNGqMt//e9Pq8tz/nk/AZWrVO
JPEfqUtDsuLNxM3M4/VKzwkRHdZtrQKmsPAdMEaHovpxxD1qm284MfH3qb9/FPS1
5s2KQo0lkmfs3GeGAbF5cmm92tj48meNhwIDAQAB
-----END RSA PUBLIC KEY-----"#;

pub(crate) fn pem_der(pem: &str) -> Vec<u8> {
    let body: String = pem
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .map(str::trim)
        .collect();
    BASE64.decode(body).unwrap()
}

pub(crate) fn sign_test_release_metadata(bytes: &[u8]) -> String {
    let key_pair = RsaKeyPair::from_pkcs8(&pem_der(TEST_RELEASE_PRIVATE_KEY_PEM)).unwrap();
    let rng = SystemRandom::new();
    let mut signature = vec![0; key_pair.public().modulus_len()];
    key_pair
        .sign(&RSA_PKCS1_SHA256, &rng, bytes, &mut signature)
        .unwrap();
    BASE64.encode(signature)
}

#[cfg(windows)]
#[derive(Debug)]
pub(crate) struct WindowsRuntimeRepairRelease {
    pub(crate) target: PathBuf,
    pub(crate) metadata: PathBuf,
    pub(crate) signature: PathBuf,
    pub(crate) runtime_target: PathBuf,
    pub(crate) runtime_artifact_sha: String,
    pub(crate) runtime_version: String,
}

#[cfg(windows)]
fn windows_file_url(path: &Path) -> String {
    let rendered = path.display().to_string();
    let local = rendered.strip_prefix("\\\\?\\").unwrap_or(&rendered);
    assert!(
        !local.chars().any(|value| matches!(value, '%' | '?' | '#')),
        "Windows runtime repair fixture path requires URL escaping: {local}"
    );
    format!("file:///{}", local.replace('\\', "/"))
}

/// Builds the exact Windows ONNX Runtime ZIP shape consumed by the production
/// PowerShell extractor without adding a second archive implementation as a
/// test dependency. The entries are stored (rather than compressed), but carry
/// the Unix type bits which the extractor validates before publication.
#[cfg(windows)]
fn write_windows_runtime_zip(path: &Path, version: &str) {
    const DIRECTORY_MODE: u32 = 0o040755;
    const FILE_MODE: u32 = 0o100644;

    let entries = vec![
        ("LICENSE".to_owned(), b"test license\n".to_vec(), FILE_MODE),
        (
            "MICROSOFT_VC_RUNTIME_LICENSE.rtf".to_owned(),
            b"test Microsoft VC runtime license\n".to_vec(),
            FILE_MODE,
        ),
        (
            "ThirdPartyNotices.txt".to_owned(),
            b"test notices\n".to_vec(),
            FILE_MODE,
        ),
        (
            "VERSION_NUMBER".to_owned(),
            format!("{version}\n").into_bytes(),
            FILE_MODE,
        ),
        (
            "GIT_COMMIT_ID".to_owned(),
            b"test-runtime-commit\n".to_vec(),
            FILE_MODE,
        ),
        ("lib/".to_owned(), Vec::new(), DIRECTORY_MODE),
        (
            "lib/onnxruntime.dll".to_owned(),
            b"fake onnxruntime DLL\n".to_vec(),
            FILE_MODE,
        ),
        (
            "lib/msvcp140.dll".to_owned(),
            b"fake msvcp140 DLL\n".to_vec(),
            FILE_MODE,
        ),
        (
            "lib/msvcp140_1.dll".to_owned(),
            b"fake msvcp140_1 DLL\n".to_vec(),
            FILE_MODE,
        ),
        (
            "lib/vcruntime140.dll".to_owned(),
            b"fake vcruntime140 DLL\n".to_vec(),
            FILE_MODE,
        ),
        (
            "lib/vcruntime140_1.dll".to_owned(),
            b"fake vcruntime140_1 DLL\n".to_vec(),
            FILE_MODE,
        ),
    ];

    let mut archive = Vec::new();
    let mut central_directory = Vec::new();
    let entry_count = u16::try_from(entries.len()).unwrap();
    for (name, contents, mode) in entries {
        let local_offset = u32::try_from(archive.len()).unwrap();
        let name = name.as_bytes();
        let crc32 = zip_crc32(&contents);
        let size = u32::try_from(contents.len()).unwrap();
        let name_length = u16::try_from(name.len()).unwrap();

        zip_u32(&mut archive, 0x0403_4b50);
        zip_u16(&mut archive, 20);
        zip_u16(&mut archive, 0);
        zip_u16(&mut archive, 0);
        zip_u16(&mut archive, 0);
        zip_u16(&mut archive, 0);
        zip_u32(&mut archive, crc32);
        zip_u32(&mut archive, size);
        zip_u32(&mut archive, size);
        zip_u16(&mut archive, name_length);
        zip_u16(&mut archive, 0);
        archive.extend_from_slice(name);
        archive.extend_from_slice(&contents);

        zip_u32(&mut central_directory, 0x0201_4b50);
        zip_u16(&mut central_directory, 0x0314);
        zip_u16(&mut central_directory, 20);
        zip_u16(&mut central_directory, 0);
        zip_u16(&mut central_directory, 0);
        zip_u16(&mut central_directory, 0);
        zip_u16(&mut central_directory, 0);
        zip_u32(&mut central_directory, crc32);
        zip_u32(&mut central_directory, size);
        zip_u32(&mut central_directory, size);
        zip_u16(&mut central_directory, name_length);
        zip_u16(&mut central_directory, 0);
        zip_u16(&mut central_directory, 0);
        zip_u16(&mut central_directory, 0);
        zip_u16(&mut central_directory, 0);
        zip_u32(&mut central_directory, mode << 16);
        zip_u32(&mut central_directory, local_offset);
        central_directory.extend_from_slice(name);
    }

    let central_offset = u32::try_from(archive.len()).unwrap();
    let central_size = u32::try_from(central_directory.len()).unwrap();
    archive.extend_from_slice(&central_directory);
    zip_u32(&mut archive, 0x0605_4b50);
    zip_u16(&mut archive, 0);
    zip_u16(&mut archive, 0);
    zip_u16(&mut archive, entry_count);
    zip_u16(&mut archive, entry_count);
    zip_u32(&mut archive, central_size);
    zip_u32(&mut archive, central_offset);
    zip_u16(&mut archive, 0);

    fs::write(path, archive).unwrap();
}

#[cfg(windows)]
fn zip_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

#[cfg(windows)]
fn zip_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

#[cfg(windows)]
fn zip_crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 0 {
                crc >> 1
            } else {
                (crc >> 1) ^ 0xedb8_8320
            };
        }
    }
    !crc
}

#[cfg(windows)]
pub(crate) fn windows_runtime_repair_release(
    temp: &TempDir,
    target: &Path,
) -> WindowsRuntimeRepairRelease {
    let target = fs::canonicalize(target).unwrap();
    let release_dir = temp.path().join("windows-runtime-repair-release");
    fs::create_dir(&release_dir).unwrap();

    let release_binary = release_dir.join("ctx.exe");
    fs::copy(&target, &release_binary).unwrap();
    let release_binary_sha = sha256_hex(&fs::read(&release_binary).unwrap());
    let runtime_version = "1.27.0";
    let runtime_artifact = release_dir.join("ctx-onnxruntime-windows-x64.zip");
    write_windows_runtime_zip(&runtime_artifact, runtime_version);
    let runtime_artifact_sha = sha256_hex(&fs::read(&runtime_artifact).unwrap());

    let mut metadata_body = format!(
        "CTX_RELEASE_SCHEMA_VERSION=1\n\
CTX_RELEASE_CHANNEL=stable\n\
CTX_RELEASE_VERSION={}\n\
CTX_RELEASE_BASE_URL={}\n\
CTX_RELEASE_ARTIFACT_windows_x64=ctx.exe\n\
CTX_RELEASE_SHA256_windows_x64={release_binary_sha}\n\
CTX_RELEASE_SELF_UPGRADE_ALLOWED=true\n\
CTX_RELEASE_AUTO_UPGRADE_ALLOWED=true\n\
CTX_RELEASE_ONNXRUNTIME_VERSION={runtime_version}\n",
        env!("CARGO_PKG_VERSION"),
        windows_file_url(&release_dir),
    );
    for key in [
        "linux_x64",
        "linux_aarch64",
        "macos_arm64",
        "macos_x64",
        "windows_x64",
        "freebsd_x64",
    ] {
        let platform = key.replace('_', "-");
        let extension = if key == "windows_x64" {
            "zip"
        } else {
            "tar.gz"
        };
        metadata_body.push_str(&format!(
            "CTX_RELEASE_ONNXRUNTIME_ARTIFACT_{key}=ctx-onnxruntime-{platform}.{extension}\n\
CTX_RELEASE_ONNXRUNTIME_SHA256_{key}={runtime_artifact_sha}\n"
        ));
    }
    let metadata = release_dir.join("ctx-release-metadata.env");
    let signature = release_dir.join("ctx-release-metadata.env.sig");
    fs::write(&metadata, &metadata_body).unwrap();
    fs::write(
        &signature,
        format!("{}\n", sign_test_release_metadata(metadata_body.as_bytes())),
    )
    .unwrap();

    WindowsRuntimeRepairRelease {
        target,
        metadata,
        signature,
        runtime_target: data_root(temp)
            .join("runtime")
            .join("onnxruntime")
            .join(runtime_version)
            .join("windows-x64"),
        runtime_artifact_sha,
        runtime_version: runtime_version.to_owned(),
    }
}

#[cfg(windows)]
pub(crate) fn windows_runtime_repair_release_env<'a>(
    command: &'a mut Command,
    release: &WindowsRuntimeRepairRelease,
) -> &'a mut Command {
    command
        .env("CTX_UPGRADE_TEST_TARGET", &release.target)
        .env(
            "CTX_RELEASE_METADATA_URL",
            windows_file_url(&release.metadata),
        )
        .env(
            "CTX_RELEASE_METADATA_SIGNATURE_URL",
            windows_file_url(&release.signature),
        )
        .env(
            "CTX_RELEASE_METADATA_PUBLIC_KEY_PEM",
            TEST_RELEASE_PUBLIC_KEY_PEM,
        )
}

#[cfg(windows)]
pub(crate) fn windows_install_transaction_path(target: &Path) -> PathBuf {
    let file_name = target.file_name().unwrap().to_string_lossy();
    target.with_file_name(format!(".{file_name}.upgrade-install-transaction.json"))
}

#[cfg(unix)]
#[derive(Debug)]
pub(crate) struct FakeRelease {
    pub(crate) target: PathBuf,
    pub(crate) metadata: PathBuf,
    pub(crate) signature: PathBuf,
    pub(crate) artifact_sha: String,
    pub(crate) installed_at: String,
}

#[cfg(unix)]
#[derive(Debug)]
pub(crate) struct FakeRuntime {
    pub(crate) target: PathBuf,
    pub(crate) artifact: PathBuf,
    pub(crate) artifact_sha: String,
    pub(crate) version: String,
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
pub(crate) fn test_platform_key() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "linux_x64",
        ("linux", "aarch64") => "linux_aarch64",
        ("macos", "aarch64") => "macos_arm64",
        ("macos", "x86_64") => "macos_x64",
        ("windows", "x86_64") => "windows_x64",
        ("freebsd", "x86_64") => "freebsd_x64",
        (os, arch) => panic!("unsupported test platform {os}-{arch}"),
    }
}

#[cfg(unix)]
pub(crate) fn install_marker_path(target: &Path) -> PathBuf {
    let file_name = target.file_name().unwrap().to_str().unwrap();
    target.with_file_name(format!("{file_name}.install.json"))
}

#[cfg(unix)]
pub(crate) fn fake_release(temp: &TempDir, latest_version: &str) -> FakeRelease {
    let release = fake_legacy_release(temp, latest_version);
    let _ = add_fake_release_runtime(temp, &release);
    release
}

#[cfg(unix)]
pub(crate) fn fake_legacy_release(temp: &TempDir, latest_version: &str) -> FakeRelease {
    let bin_dir = temp.path().join("bin");
    let release_dir = temp.path().join("release");
    fs::create_dir_all(&bin_dir).unwrap();
    fs::create_dir_all(&release_dir).unwrap();

    let target = bin_dir.join("ctx");
    let current_bytes = write_fake_ctx_binary(&target, env!("CARGO_PKG_VERSION"));
    let current_sha = sha256_hex(&current_bytes);
    let installed_at =
        ctx_history_core::utc_now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

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
        "installed_at": installed_at,
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
        installed_at,
    }
}

#[cfg(unix)]
pub(crate) fn add_fake_release_runtime(temp: &TempDir, release: &FakeRelease) -> FakeRuntime {
    add_fake_release_runtime_version(temp, release, "1.27.0")
}

#[cfg(unix)]
pub(crate) fn add_fake_release_runtime_version(
    temp: &TempDir,
    release: &FakeRelease,
    version: &str,
) -> FakeRuntime {
    let release_dir = release.metadata.parent().unwrap();
    let platform = test_platform_key().replace('_', "-");
    let artifact_name = format!("ctx-onnxruntime-{platform}.tar.gz");
    let artifact = release_dir.join(&artifact_name);
    let library = if platform.starts_with("macos-") {
        "libonnxruntime.dylib"
    } else {
        "libonnxruntime.so"
    };
    write_fake_runtime_archive(&artifact, library, version, "valid");
    let artifact_sha = sha256_hex(&fs::read(&artifact).unwrap());
    rewrite_fake_release_metadata(release, |metadata| {
        let mut metadata = metadata
            .lines()
            .filter(|line| !line.starts_with("CTX_RELEASE_ONNXRUNTIME_"))
            .map(|line| format!("{line}\n"))
            .collect::<String>();
        metadata.push_str(&format!("CTX_RELEASE_ONNXRUNTIME_VERSION={version}\n"));
        for key in [
            "linux_x64",
            "linux_aarch64",
            "macos_arm64",
            "macos_x64",
            "windows_x64",
            "freebsd_x64",
        ] {
            let platform = key.replace('_', "-");
            let extension = if key == "windows_x64" {
                "zip"
            } else {
                "tar.gz"
            };
            metadata.push_str(&format!(
                "CTX_RELEASE_ONNXRUNTIME_ARTIFACT_{key}=ctx-onnxruntime-{platform}.{extension}\n\
CTX_RELEASE_ONNXRUNTIME_SHA256_{key}={artifact_sha}\n"
            ));
        }
        metadata
    });
    let target = data_root(temp)
        .join("runtime")
        .join("onnxruntime")
        .join(version)
        .join(platform);
    FakeRuntime {
        target,
        artifact,
        artifact_sha,
        version: version.to_owned(),
    }
}

#[cfg(unix)]
fn write_fake_runtime_archive(artifact: &Path, library: &str, version: &str, mode: &str) {
    let archive = fs::File::create(artifact).unwrap();
    let encoder = GzEncoder::new(archive, Compression::default());
    let mut bundle = TarBuilder::new(encoder);
    append_tar_entry(&mut bundle, "lib/", b"", 0o755, EntryType::Directory);
    let files = [
        ("LICENSE".to_owned(), b"test license\n".to_vec()),
        (
            "ThirdPartyNotices.txt".to_owned(),
            b"test notices\n".to_vec(),
        ),
        (
            "VERSION_NUMBER".to_owned(),
            format!("{version}\n").into_bytes(),
        ),
        (
            "GIT_COMMIT_ID".to_owned(),
            b"test-runtime-commit\n".to_vec(),
        ),
        (
            format!("lib/{library}"),
            b"fake onnxruntime shared library\n".to_vec(),
        ),
    ];
    for (name, contents) in files {
        if name.starts_with("lib/") && mode == "symlink" {
            let mut header = raw_tar_header(&name, 0, 0o755, EntryType::Symlink);
            header.set_link_name("../LICENSE").unwrap();
            header.set_cksum();
            bundle
                .append(&header, Cursor::new(Vec::<u8>::new()))
                .unwrap();
        } else if name.starts_with("lib/") && mode == "special" {
            append_tar_entry(&mut bundle, &name, b"", 0o755, EntryType::Fifo);
        } else {
            let entry_mode = if mode == "unsafe_mode" && name == "LICENSE" {
                0o4644
            } else if name.starts_with("lib/") {
                0o755
            } else {
                0o644
            };
            append_tar_entry(
                &mut bundle,
                &name,
                &contents,
                entry_mode,
                EntryType::Regular,
            );
        }
    }
    if mode == "traversal" {
        append_tar_entry(
            &mut bundle,
            "../escape",
            b"escape\n",
            0o644,
            EntryType::Regular,
        );
    }
    if mode == "unexpected" {
        append_tar_entry(&mut bundle, "EXTRA", b"extra\n", 0o644, EntryType::Regular);
    }
    if mode == "duplicate" {
        append_tar_entry(
            &mut bundle,
            "LICENSE",
            b"duplicate\n",
            0o644,
            EntryType::Regular,
        );
    }
    bundle.finish().unwrap();
    bundle.into_inner().unwrap().finish().unwrap();
}

#[cfg(unix)]
fn append_tar_entry(
    bundle: &mut TarBuilder<GzEncoder<fs::File>>,
    name: &str,
    contents: &[u8],
    mode: u32,
    entry_type: EntryType,
) {
    let header = raw_tar_header(name, contents.len() as u64, mode, entry_type);
    bundle.append(&header, Cursor::new(contents)).unwrap();
}

#[cfg(unix)]
fn raw_tar_header(name: &str, size: u64, mode: u32, entry_type: EntryType) -> Header {
    assert!(name.len() < 100);
    let mut header = Header::new_gnu();
    header.set_size(size);
    header.set_mode(mode);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_entry_type(entry_type);
    header.as_mut_bytes()[..name.len()].copy_from_slice(name.as_bytes());
    header.set_cksum();
    header
}

#[cfg(unix)]
pub(crate) fn rewrite_fake_runtime_archive(
    release: &FakeRelease,
    runtime: &mut FakeRuntime,
    mode: &str,
) {
    let platform = test_platform_key().replace('_', "-");
    let library = if platform.starts_with("macos-") {
        "libonnxruntime.dylib"
    } else {
        "libonnxruntime.so"
    };
    write_fake_runtime_archive(&runtime.artifact, library, &runtime.version, mode);
    let next_sha = sha256_hex(&fs::read(&runtime.artifact).unwrap());
    let previous_sha = runtime.artifact_sha.clone();
    rewrite_fake_release_metadata(release, |metadata| {
        metadata.replace(&previous_sha, &next_sha)
    });
    runtime.artifact_sha = next_sha;
}

#[cfg(unix)]
pub(crate) fn rewrite_fake_release_metadata(
    release: &FakeRelease,
    rewrite: impl FnOnce(String) -> String,
) {
    let next = rewrite(fs::read_to_string(&release.metadata).unwrap());
    fs::write(&release.metadata, &next).unwrap();
    fs::write(
        &release.signature,
        format!("{}\n", sign_test_release_metadata(next.as_bytes())),
    )
    .unwrap();
}

#[cfg(unix)]
pub(crate) fn fake_release_env<'a>(
    command: &'a mut Command,
    release: &FakeRelease,
) -> &'a mut Command {
    command
        .env("CTX_UPGRADE_TEST_TARGET", &release.target)
        .env("CTX_RELEASE_METADATA_URL", file_url(&release.metadata))
        .env(
            "CTX_RELEASE_METADATA_SIGNATURE_URL",
            file_url(&release.signature),
        )
        .env(
            "CTX_RELEASE_METADATA_PUBLIC_KEY_PEM",
            TEST_RELEASE_PUBLIC_KEY_PEM,
        )
}

#[cfg(unix)]
pub(crate) fn managed_candidate(temp: &TempDir, install_attempt_id: &str) -> PathBuf {
    managed_candidate_in(temp, "candidate-bin", install_attempt_id)
}

#[cfg(unix)]
pub(crate) fn managed_candidate_from_binary(
    temp: &TempDir,
    source: &Path,
    install_attempt_id: &str,
) -> PathBuf {
    let copied = copied_binary(temp, source);
    finish_managed_candidate(temp, copied, "candidate-bin", install_attempt_id)
}

#[cfg(unix)]
pub(crate) fn managed_bound_candidate_from_binary(
    temp: &TempDir,
    source: &Path,
    install_attempt_id: &str,
) -> PathBuf {
    let bound = copied_ctx_binary(temp);
    fs::remove_file(&bound).unwrap();
    let target = copied_binary(temp, source);
    finish_managed_candidate_at(target, install_attempt_id)
}

#[cfg(unix)]
pub(crate) fn managed_candidate_in(
    temp: &TempDir,
    directory: &str,
    install_attempt_id: &str,
) -> PathBuf {
    let copied = copied_ctx_binary(temp);
    finish_managed_candidate(temp, copied, directory, install_attempt_id)
}

#[cfg(unix)]
pub(crate) fn ensure_managed_test_binary_is_bounded(target: &Path) {
    const MAX_MANAGED_BINARY_BYTES: u64 = 128 * 1024 * 1024;

    if fs::metadata(target).unwrap().len() > MAX_MANAGED_BINARY_BYTES {
        let stripped = std::process::Command::new("strip")
            .arg(target)
            .status()
            .expect("run strip for the temporary managed debug candidate");
        assert!(
            stripped.success(),
            "fully strip temporary managed debug candidate"
        );
    }
    let size = fs::metadata(target).unwrap().len();
    assert!(
        size <= MAX_MANAGED_BINARY_BYTES,
        "temporary managed debug candidate remains above the production binary bound after full strip: {size} > {MAX_MANAGED_BINARY_BYTES}"
    );
}

#[cfg(unix)]
fn finish_managed_candidate(
    temp: &TempDir,
    copied: PathBuf,
    directory: &str,
    install_attempt_id: &str,
) -> PathBuf {
    let bin_dir = temp.path().join(directory);
    let target = bin_dir.join("ctx");
    fs::create_dir(&bin_dir).unwrap();
    fs::rename(copied, &target).unwrap();
    finish_managed_candidate_at(target, install_attempt_id)
}

#[cfg(unix)]
fn finish_managed_candidate_at(target: PathBuf, install_attempt_id: &str) -> PathBuf {
    ensure_managed_test_binary_is_bounded(&target);
    let current = fs::read(&target).unwrap();
    fs::write(
        install_marker_path(&target),
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "manager": "ctx-hosted-installer",
            "install_attempt_id": install_attempt_id,
            "install_path": target,
            "platform": test_platform_key().replace('_', "-"),
            "channel": "stable",
            "version": env!("CARGO_PKG_VERSION"),
            "sha256": sha256_hex(&current),
            "metadata_url": null,
            "artifact_url": null,
            "installed_at": ctx_history_core::utc_now()
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        }))
        .unwrap(),
    )
    .unwrap();
    target
}

#[cfg(unix)]
pub(crate) fn managed_release_env<'a>(
    command: &'a mut Command,
    release: &FakeRelease,
    binary: &Path,
) -> &'a mut Command {
    let inherited_path = env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![binary.parent().unwrap().to_path_buf()];
    paths.extend(env::split_paths(&inherited_path));
    let path = env::join_paths(paths).unwrap_or_else(|_| OsString::from("/usr/bin:/bin"));
    command
        .env_remove("CTX_UPGRADE_AUTO")
        .env("PATH", path)
        .env("CTX_UPGRADE_INTERVAL_SECONDS", "0")
        .env("CTX_RELEASE_METADATA_URL", file_url(&release.metadata))
        .env(
            "CTX_RELEASE_METADATA_SIGNATURE_URL",
            file_url(&release.signature),
        )
        .env(
            "CTX_RELEASE_METADATA_PUBLIC_KEY_PEM",
            TEST_RELEASE_PUBLIC_KEY_PEM,
        )
}
