use super::*;

fn custom_catalog_entry(path: PathBuf) -> CatalogEntry {
    CatalogEntry {
        provider: CaptureProvider::Custom.as_str().to_owned(),
        source_format: CUSTOM_SOURCE_FORMAT.to_owned(),
        path,
        catalog_lineage: encode_hex(&[0x11; 32]),
        route_identity: None,
        relocate_from: None,
        enabled: true,
    }
}

#[test]
fn exact_source_missing_path_returns_the_neutral_typed_marker() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("missing-history.jsonl");

    let error = explicit_source_for_path(
        &temp.path().join("data"),
        &path,
        Some(CaptureProvider::Codex),
        false,
    )
    .unwrap_err();
    let missing = error
        .downcast_ref::<crate::ExplicitSourcePathMissing>()
        .unwrap();

    assert_eq!(missing.path(), path);
    assert_eq!(missing.source_error().kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn catalog_revalidation_missing_path_retains_the_no_follow_operation() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("missing-history.jsonl");
    let entry = custom_catalog_entry(path.clone());

    let error = source_from_catalog_entry(&temp.path().join("data"), &entry).unwrap_err();
    let missing = error
        .downcast_ref::<crate::ExplicitSourcePathMissing>()
        .unwrap();

    assert_eq!(
        error.to_string(),
        format!("check catalog source path {}", path.display())
    );
    assert_eq!(missing.path(), path);
    assert_eq!(missing.source_error().kind(), std::io::ErrorKind::NotFound);
}

fn assert_catalog_revalidation_rejects_links_or_reparse_points(paths: &[&Path]) {
    let data_root = paths[0].parent().unwrap().join("data");
    for path in paths {
        let entry = custom_catalog_entry((*path).to_path_buf());
        let error = source_from_catalog_entry(&data_root, &entry).unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("symlinked explicit provider source roots are rejected"),
            "{error:#}"
        );
        assert!(!error.is::<crate::ExplicitSourcePathMissing>(), "{error:#}");
    }
}

#[cfg(unix)]
#[test]
fn catalog_revalidation_rejects_live_and_dangling_file_and_directory_symlinks_on_unix() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let live_file = temp.path().join("history.jsonl");
    let live_dir = temp.path().join("history");
    let missing_file = temp.path().join("missing.jsonl");
    let missing_dir = temp.path().join("missing");
    let live_file_link = temp.path().join("live-file-link");
    let live_dir_link = temp.path().join("live-dir-link");
    let dangling_file_link = temp.path().join("dangling-file-link");
    let dangling_dir_link = temp.path().join("dangling-dir-link");
    fs::write(&live_file, b"\n").unwrap();
    fs::create_dir(&live_dir).unwrap();
    symlink(&live_file, &live_file_link).unwrap();
    symlink(&live_dir, &live_dir_link).unwrap();
    symlink(&missing_file, &dangling_file_link).unwrap();
    symlink(&missing_dir, &dangling_dir_link).unwrap();

    assert_catalog_revalidation_rejects_links_or_reparse_points(&[
        &live_file_link,
        &live_dir_link,
        &dangling_file_link,
        &dangling_dir_link,
    ]);
}

#[cfg(target_os = "windows")]
#[test]
fn catalog_revalidation_rejects_live_and_dangling_file_and_directory_symlinks_on_windows() {
    use std::{
        io::ErrorKind,
        os::windows::fs::{symlink_dir, symlink_file},
    };

    fn symlink_unavailable(error: &std::io::Error) -> bool {
        error.kind() == ErrorKind::PermissionDenied || error.raw_os_error() == Some(1314)
    }

    let temp = tempfile::tempdir().unwrap();
    let live_file = temp.path().join("history.jsonl");
    let live_dir = temp.path().join("history");
    let missing_file = temp.path().join("missing.jsonl");
    let missing_dir = temp.path().join("missing");
    let live_file_link = temp.path().join("live-file-link");
    let live_dir_link = temp.path().join("live-dir-link");
    let dangling_file_link = temp.path().join("dangling-file-link");
    let dangling_dir_link = temp.path().join("dangling-dir-link");
    fs::write(&live_file, b"\n").unwrap();
    fs::create_dir(&live_dir).unwrap();
    for result in [
        symlink_file(&live_file, &live_file_link),
        symlink_dir(&live_dir, &live_dir_link),
        symlink_file(&missing_file, &dangling_file_link),
        symlink_dir(&missing_dir, &dangling_dir_link),
    ] {
        if let Err(error) = result {
            if symlink_unavailable(&error) {
                return;
            }
            panic!("failed to create Windows catalog symlink: {error}");
        }
    }

    assert_catalog_revalidation_rejects_links_or_reparse_points(&[
        &live_file_link,
        &live_dir_link,
        &dangling_file_link,
        &dangling_dir_link,
    ]);
}

fn assert_exact_source_rejects_links_or_reparse_points(paths: &[&Path]) {
    let data_root = paths[0].parent().unwrap().join("data");
    for path in paths {
        let error = explicit_source_for_path(&data_root, path, Some(CaptureProvider::Codex), false)
            .unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("symlinked explicit provider source roots are rejected"),
            "{error:#}"
        );
        assert!(!error.to_string().contains("does not exist"), "{error:#}");
        assert!(!error.is::<crate::ExplicitSourcePathMissing>(), "{error:#}");
    }
}

#[cfg(unix)]
#[test]
fn exact_source_rejects_live_and_dangling_final_symlinks_on_unix() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let live_target = temp.path().join("history.jsonl");
    let missing_target = temp.path().join("missing.jsonl");
    let live_link = temp.path().join("live-link.jsonl");
    let dangling_link = temp.path().join("dangling-link.jsonl");
    fs::write(&live_target, b"\n").unwrap();
    symlink(&live_target, &live_link).unwrap();
    symlink(&missing_target, &dangling_link).unwrap();

    assert_exact_source_rejects_links_or_reparse_points(&[&live_link, &dangling_link]);
}

#[cfg(target_os = "windows")]
#[test]
fn exact_source_rejects_live_and_dangling_final_symlinks_on_windows() {
    use std::{io::ErrorKind, os::windows::fs::symlink_file};

    fn symlink_unavailable(error: &std::io::Error) -> bool {
        error.kind() == ErrorKind::PermissionDenied || error.raw_os_error() == Some(1314)
    }

    let temp = tempfile::tempdir().unwrap();
    let live_target = temp.path().join("history.jsonl");
    let missing_target = temp.path().join("missing.jsonl");
    let live_link = temp.path().join("live-link.jsonl");
    let dangling_link = temp.path().join("dangling-link.jsonl");
    fs::write(&live_target, b"\n").unwrap();
    for (target, link) in [
        (&live_target, &live_link),
        (&missing_target, &dangling_link),
    ] {
        if let Err(error) = symlink_file(target, link) {
            if symlink_unavailable(&error) {
                return;
            }
            panic!("failed to create Windows file symlink: {error}");
        }
    }

    assert_exact_source_rejects_links_or_reparse_points(&[&live_link, &dangling_link]);
}

#[cfg(target_os = "windows")]
#[test]
fn exact_source_and_catalog_revalidation_reject_live_and_dangling_directory_junctions() {
    use std::process::Command;

    fn create_directory_junction(junction: &Path, target: &Path) {
        let status = Command::new("cmd.exe")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(junction)
            .arg(target)
            .status()
            .expect("create Windows directory junction");
        assert!(
            status.success(),
            "failed to create Windows directory junction"
        );
    }

    let temp = tempfile::tempdir().unwrap();
    let live_target = temp.path().join("live-history");
    let live_junction = temp.path().join("live-history-junction");
    let dangling_target = temp.path().join("dangling-history");
    let dangling_junction = temp.path().join("dangling-history-junction");
    fs::create_dir(&live_target).unwrap();
    fs::create_dir(&dangling_target).unwrap();
    create_directory_junction(&live_junction, &live_target);
    create_directory_junction(&dangling_junction, &dangling_target);
    fs::remove_dir(&dangling_target).unwrap();

    assert_catalog_revalidation_rejects_links_or_reparse_points(&[
        &live_junction,
        &dangling_junction,
    ]);
    assert_exact_source_rejects_links_or_reparse_points(&[&live_junction, &dangling_junction]);
}
