use std::fs;

use anyhow::Result;
use serde_json::json;
use tempfile::tempdir;

use super::{
    classify_install_marker_at, install_marker_path, is_valid_install_attempt_id,
    ManagedInstallMarker,
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
        }
        other => panic!("expected invalid marker, got {other:?}"),
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
