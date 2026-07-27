use super::*;

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn private_tempdir() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    restrict_private_directory(root.path()).unwrap();
    root
}

#[test]
fn verified_download_is_bounded_reusable_and_ephemeral() {
    let root = private_tempdir();
    let source = root.path().join("source.bin");
    let bytes = b"verified release artifact";
    fs::write(&source, bytes).unwrap();
    let mut artifact = DownloadedArtifact::download_verified(
        root.path(),
        &format!("file://{}", source.display()),
        &digest(bytes),
        bytes.len() as u64,
        Duration::from_secs(1),
    )
    .unwrap();
    let temporary_path = artifact.temporary_path().to_path_buf();
    assert_eq!(artifact.byte_len(), bytes.len() as u64);
    assert_eq!(artifact.sha256(), digest(bytes));
    let mut copied = Vec::new();
    artifact.copy_verified_to(&mut copied).unwrap();
    assert_eq!(copied, bytes);
    let stable = artifact.stable_path().unwrap();
    assert_eq!(fs::read(stable).unwrap(), bytes);
    drop(artifact);
    assert!(!temporary_path.exists());
}

#[test]
fn runtime_download_cache_is_rehashed_and_reused_across_releases() {
    let root = private_tempdir();
    let source = root.path().join("runtime.tar.gz");
    let bytes = b"same signed runtime artifact";
    fs::write(&source, bytes).unwrap();
    let endpoint = format!("file://{}", source.display());
    let expected = digest(bytes);

    let first = DownloadedArtifact::download_or_reuse_verified(
        root.path(),
        &endpoint,
        &expected,
        1024,
        Duration::from_secs(1),
    )
    .unwrap();
    drop(first);
    fs::remove_file(&source).unwrap();

    let mut reused = DownloadedArtifact::download_or_reuse_verified(
        root.path(),
        &endpoint,
        &expected,
        1024,
        Duration::from_secs(1),
    )
    .unwrap();
    let mut copied = Vec::new();
    reused.copy_verified_to(&mut copied).unwrap();
    assert_eq!(copied, bytes);
}

#[test]
fn failed_verification_does_not_leave_a_partial_download() {
    let root = private_tempdir();
    let source = root.path().join("source.bin");
    fs::write(&source, b"wrong bytes").unwrap();
    let error = DownloadedArtifact::download_verified(
        root.path(),
        &format!("file://{}", source.display()),
        &digest(b"expected bytes"),
        1024,
        Duration::from_secs(1),
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("checksum mismatch"));
    assert_eq!(
        fs::read_dir(root.path().join(DOWNLOAD_DIRECTORY))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn oversized_download_does_not_leave_a_partial_download() {
    let root = private_tempdir();
    let source = root.path().join("source.bin");
    fs::write(&source, b"12345").unwrap();
    let error = DownloadedArtifact::download_verified(
        root.path(),
        &format!("file://{}", source.display()),
        &digest(b"12345"),
        4,
        Duration::from_secs(1),
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("exceeds max bytes (4)"));
    assert_eq!(
        fs::read_dir(root.path().join(DOWNLOAD_DIRECTORY))
            .unwrap()
            .count(),
        0
    );
}

#[cfg(unix)]
#[test]
fn held_artifact_detects_same_path_substitution() {
    let root = private_tempdir();
    let source = root.path().join("source.bin");
    let bytes = b"original";
    fs::write(&source, bytes).unwrap();
    let mut artifact = DownloadedArtifact::download_verified(
        root.path(),
        &format!("file://{}", source.display()),
        &digest(bytes),
        1024,
        Duration::from_secs(1),
    )
    .unwrap();
    let replacement = artifact.temporary_path().with_extension("replacement");
    fs::write(&replacement, bytes).unwrap();
    restrict_private_file(&replacement).unwrap();
    fs::rename(&replacement, artifact.temporary_path()).unwrap();
    let error = artifact.copy_verified_to(&mut Vec::new()).unwrap_err();
    assert!(error.to_string().contains("path identity changed"));
    let substituted_path = artifact.temporary_path().to_path_buf();
    drop(artifact);
    assert_eq!(fs::read(substituted_path).unwrap(), bytes);
}

#[cfg(unix)]
#[test]
fn symlinked_download_directory_is_rejected_without_touching_target() {
    use std::os::unix::fs::symlink;

    let root = private_tempdir();
    let target = root.path().join("target");
    fs::create_dir(&target).unwrap();
    symlink(&target, root.path().join(DOWNLOAD_DIRECTORY)).unwrap();
    let source = root.path().join("source.bin");
    fs::write(&source, b"bytes").unwrap();
    let error = DownloadedArtifact::download_verified(
        root.path(),
        &format!("file://{}", source.display()),
        &digest(b"bytes"),
        1024,
        Duration::from_secs(1),
    )
    .unwrap_err();
    assert!(error
        .to_string()
        .contains("create private artifact download directory"));
    assert_eq!(fs::read_dir(target).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn insecure_existing_download_directory_is_rejected_without_repair() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = private_tempdir();
    let downloads = root.path().join(DOWNLOAD_DIRECTORY);
    fs::create_dir(&downloads).unwrap();
    fs::set_permissions(&downloads, fs::Permissions::from_mode(0o755)).unwrap();

    assert!(prepare_download_directory(root.path()).is_err());
    assert_eq!(
        fs::metadata(&downloads).unwrap().permissions().mode() & 0o777,
        0o755
    );
}
