use std::fs;

use anyhow::Result;
use serde_json::json;
use tempfile::tempdir;

#[cfg(windows)]
use crate::upgrade::install::path_identity::windows_disk_path_identity;
#[cfg(windows)]
use std::{ffi::OsString, os::windows::ffi::OsStringExt as _, path::PathBuf, process::Command};

use super::{
    absent_install_marker_error, classify_install_marker_at, install_fingerprint,
    install_marker_path, installation_is_unmanaged_at, invalid_install_marker_recovery_guidance,
    is_valid_install_attempt_id, unmanaged_install_conversion_guidance, ManagedInstallMarker,
};
use crate::upgrade::sha256_hex;

fn executable_copy(root: &std::path::Path, bytes: &[u8]) -> Result<std::path::PathBuf> {
    let executable = root.join(if cfg!(windows) { "ctx.exe" } else { "ctx" });
    fs::write(&executable, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))?;
    }
    Ok(fs::canonicalize(executable)?)
}

#[test]
fn install_attempt_id_matches_hosted_canonical_form() {
    assert!(is_valid_install_attempt_id("ia_12345678"));
    assert!(is_valid_install_attempt_id(&format!(
        "ia_{}",
        "a".repeat(128)
    )));
    for invalid in [
        "12345678",
        "ia_1234567",
        "ia_contains space",
        " ia_12345678",
        "ia_12345678\n",
    ] {
        assert!(
            !is_valid_install_attempt_id(invalid),
            "accepted {invalid:?}"
        );
    }
    assert!(!is_valid_install_attempt_id(&format!(
        "ia_{}",
        "a".repeat(129)
    )));
}

#[test]
fn managed_reinstall_guidance_requires_safe_handoff_and_the_platform_installer() {
    let absent = absent_install_marker_error().to_string();
    for guidance in [
        absent.as_str(),
        unmanaged_install_conversion_guidance(),
        invalid_install_marker_recovery_guidance(),
    ] {
        assert!(guidance.contains("ctx daemon disable --prepare-uninstall --format=json"));
        assert!(guidance.contains("after a successful receipt"));
        assert!(guidance.contains("ctx docs show unmanaged-installs"));
        #[cfg(windows)]
        {
            assert!(guidance.contains("irm https://ctx.rs/install.ps1 | iex"));
            assert!(guidance.contains("BinDir"));
            assert!(!guidance.contains("curl -fsSL"));
        }
        #[cfg(not(windows))]
        {
            assert!(guidance.contains("curl -fsSL https://ctx.rs/install | sh"));
            assert!(!guidance.contains("install.ps1"));
        }
    }
}

#[test]
fn absent_and_corrupt_managed_markers_have_distinct_classifications() -> Result<()> {
    let fixture = tempdir()?;
    let executable = executable_copy(fixture.path(), b"temporary ctx executable copy")?;

    assert!(matches!(
        classify_install_marker_at(&executable, "test-platform"),
        ManagedInstallMarker::Absent
    ));

    fs::write(install_marker_path(&executable), b"{not-json")?;
    match classify_install_marker_at(&executable, "test-platform") {
        ManagedInstallMarker::Invalid { reason } => {
            assert!(reason.contains("parse ctx install marker"), "{reason}");
            assert!(
                reason.contains(invalid_install_marker_recovery_guidance()),
                "{reason}"
            );
        }
        other => panic!("expected invalid marker, got {other:?}"),
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn unmanaged_classification_requires_a_plainly_absent_marker() -> Result<()> {
    let fixture = tempdir()?;
    let executable = executable_copy(fixture.path(), b"temporary ctx executable copy")?;

    assert!(installation_is_unmanaged_at(&executable));
    fs::write(install_marker_path(&executable), b"{not-json")?;
    assert!(
        !installation_is_unmanaged_at(&executable),
        "a present but corrupt marker is not an unmanaged installation"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn unmanaged_installation_fingerprint_digests_an_absent_marker() -> Result<()> {
    let fixture = tempdir()?;
    let executable = executable_copy(fixture.path(), b"temporary ctx executable copy")?;

    let fingerprint = install_fingerprint(&executable)?;

    assert_eq!(
        fingerprint.binary_sha256,
        sha256_hex(b"temporary ctx executable copy")
    );
    assert_eq!(fingerprint.marker_sha256, sha256_hex(b""));
    Ok(())
}

#[test]
fn every_invalid_marker_classification_preserves_its_reason_and_adds_safe_recovery() -> Result<()> {
    let fixture = tempdir()?;
    let bytes = b"temporary ctx executable copy";
    let executable = executable_copy(fixture.path(), bytes)?;
    let other_root = fixture.path().join("other");
    fs::create_dir(&other_root)?;
    let other = executable_copy(&other_root, b"other executable")?;
    let valid = json!({
        "manager": "ctx-hosted-installer",
        "install_path": executable.display().to_string(),
        "platform": "test-platform",
        "channel": "stable",
        "version": "0.26.0",
        "sha256": sha256_hex(bytes),
    });

    let mut cases = Vec::new();
    let mut unsupported = valid.clone();
    unsupported["manager"] = json!("another-manager");
    cases.push(("unsupported manager", unsupported));
    let mut path_mismatch = valid.clone();
    path_mismatch["install_path"] = json!(other.display().to_string());
    cases.push(("path mismatch", path_mismatch));
    let mut platform_mismatch = valid.clone();
    platform_mismatch["platform"] = json!("another-platform");
    cases.push(("platform mismatch", platform_mismatch));
    let mut hash_mismatch = valid;
    hash_mismatch["sha256"] = json!("0".repeat(64));
    cases.push(("hash mismatch", hash_mismatch));

    for (expected_reason, marker) in cases {
        fs::write(
            install_marker_path(&executable),
            serde_json::to_vec(&marker)?,
        )?;
        let ManagedInstallMarker::Invalid { reason } =
            classify_install_marker_at(&executable, "test-platform")
        else {
            panic!("expected {expected_reason} marker to be invalid");
        };
        assert!(reason.contains(expected_reason), "{reason}");
        assert!(
            reason.contains(invalid_install_marker_recovery_guidance()),
            "{reason}"
        );
    }
    Ok(())
}

#[test]
fn valid_marker_uses_the_canonical_executable_path() -> Result<()> {
    let fixture = tempdir()?;
    let bytes = b"temporary ctx executable copy";
    let executable = executable_copy(fixture.path(), bytes)?;
    let marker = json!({
        "manager": "ctx-hosted-installer",
        "install_path": executable.display().to_string(),
        "platform": "test-platform",
        "channel": "stable",
        "version": "0.26.0",
        "sha256": sha256_hex(bytes),
    });
    fs::write(
        install_marker_path(&executable),
        serde_json::to_vec(&marker)?,
    )?;

    match classify_install_marker_at(&executable, "test-platform") {
        ManagedInstallMarker::Valid(marker) => {
            assert_eq!(marker.install_path, executable);
        }
        other => panic!("expected valid marker, got {other:?}"),
    }
    Ok(())
}

#[cfg(windows)]
#[test]
fn windows_runtime_marker_accepts_ordinary_path_for_verbatim_executable() -> Result<()> {
    let fixture = tempdir()?;
    let bytes = b"temporary ctx executable copy";
    let executable = executable_copy(fixture.path(), bytes)?;
    let ordinary = PathBuf::from(OsString::from_wide(
        &windows_disk_path_identity(&executable)
            .ok_or_else(|| anyhow::anyhow!("test executable is not a local Windows disk path"))?,
    ));
    let marker = json!({
        "manager": "ctx-hosted-installer",
        "install_path": ordinary,
        "platform": "windows-x64",
        "channel": "stable",
        "version": "1.0.0",
        "sha256": sha256_hex(bytes),
    });
    fs::write(
        install_marker_path(&executable),
        serde_json::to_vec(&marker)?,
    )?;

    match classify_install_marker_at(&executable, "windows-x64") {
        ManagedInstallMarker::Valid(marker) => assert_eq!(marker.install_path, executable),
        other => panic!("expected ordinary Windows marker path to be valid, got {other:?}"),
    }
    Ok(())
}

#[cfg(windows)]
#[test]
fn windows_runtime_marker_rejects_junction_alias_claim() -> Result<()> {
    let fixture = tempdir()?;
    let target = fixture.path().join("target");
    fs::create_dir(&target)?;
    let bytes = b"temporary ctx executable copy";
    let executable = executable_copy(&target, bytes)?;
    let junction = fixture.path().join("junction");
    let output = Command::new("cmd.exe")
        .args(["/D", "/C", "mklink", "/J"])
        .arg(&junction)
        .arg(&target)
        .output()?;
    assert!(
        output.status.success(),
        "failed to create runtime-marker junction fixture: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let alias = junction.join("ctx.exe");
    assert_eq!(fs::canonicalize(&alias)?, executable);
    let marker = json!({
        "manager": "ctx-hosted-installer",
        "install_path": alias,
        "platform": "windows-x64",
        "channel": "stable",
        "version": "1.0.0",
        "sha256": sha256_hex(bytes),
    });
    fs::write(
        install_marker_path(&executable),
        serde_json::to_vec(&marker)?,
    )?;

    let ManagedInstallMarker::Invalid { reason } =
        classify_install_marker_at(&executable, "windows-x64")
    else {
        panic!("expected aliased Windows marker path to be invalid");
    };
    assert!(reason.contains("path mismatch"), "{reason}");
    Ok(())
}

#[cfg(unix)]
#[test]
fn symlinked_managed_marker_is_invalid() -> Result<()> {
    use std::os::unix::fs::symlink;

    let fixture = tempdir()?;
    let executable = executable_copy(fixture.path(), b"temporary ctx executable copy")?;
    let other = fixture.path().join("other-marker");
    fs::write(&other, b"{}")?;
    symlink(&other, install_marker_path(&executable))?;

    assert!(matches!(
        classify_install_marker_at(&executable, "test-platform"),
        ManagedInstallMarker::Invalid { .. }
    ));
    Ok(())
}
