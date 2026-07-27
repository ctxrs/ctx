use std::{cell::RefCell, fs, path::Path};

use super::{
    preflight::{hard_link_is_unsupported, COLD_PROBE_MARKER},
    ColdStoreBuild, StoreError,
};

#[test]
fn cold_begin_proves_supported_hard_links_and_removes_probe_names() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("work.sqlite");

    let builder = ColdStoreBuild::begin(&target).unwrap().unwrap();

    assert!(!target.exists());
    assert!(probe_artifacts(temp.path()).is_empty());
    drop(builder);
}

#[test]
fn unsupported_hard_link_preflight_returns_no_builder_and_cleans_up() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("work.sqlite");

    let builder = ColdStoreBuild::begin_with_hard_link_probe(&target, |_, _| {
        Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
    })
    .unwrap();

    assert!(builder.is_none());
    assert!(!target.exists());
    assert!(probe_artifacts(temp.path()).is_empty());
}

#[test]
fn hard_link_permission_and_unexpected_errors_remain_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("work.sqlite");
    for kind in [
        std::io::ErrorKind::PermissionDenied,
        std::io::ErrorKind::Other,
    ] {
        let result = ColdStoreBuild::begin_with_hard_link_probe(&target, move |_, _| {
            Err(std::io::Error::from(kind))
        });
        assert!(matches!(
            result,
            Err(StoreError::Io(error)) if error.kind() == kind
        ));
        assert!(!target.exists());
        assert!(probe_artifacts(temp.path()).is_empty());
    }
}

#[test]
fn target_winner_during_preflight_is_preserved_without_building_a_stage() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("work.sqlite");
    let raced_target = target.clone();

    let builder =
        ColdStoreBuild::begin_with_hard_link_probe(&target, move |source, probe_target| {
            fs::hard_link(source, probe_target)?;
            fs::write(&raced_target, b"concurrent-winner")?;
            Ok(())
        })
        .unwrap();

    assert!(builder.is_none());
    assert_eq!(fs::read(&target).unwrap(), b"concurrent-winner");
    assert!(probe_artifacts(temp.path()).is_empty());
}

#[test]
fn probe_rejects_non_link_identity_without_removing_the_impostor() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("work.sqlite");
    let impostor_path = RefCell::new(None);

    let result = ColdStoreBuild::begin_with_hard_link_probe(&target, |_, probe_target: &Path| {
        fs::write(probe_target, b"impostor")?;
        *impostor_path.borrow_mut() = Some(probe_target.to_path_buf());
        Ok(())
    });

    assert!(matches!(result, Err(StoreError::ColdStoreInvalidState)));
    assert!(!target.exists());
    let impostor_path = impostor_path.into_inner().unwrap();
    assert_eq!(fs::read(&impostor_path).unwrap(), b"impostor");
    fs::remove_file(impostor_path).unwrap();
    assert!(probe_artifacts(temp.path()).is_empty());
}

#[cfg(unix)]
#[test]
fn probe_rejects_symlink_identity_without_following_or_removing_it() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("work.sqlite");
    let symlink_path = RefCell::new(None);

    let result =
        ColdStoreBuild::begin_with_hard_link_probe(&target, |source, probe_target: &Path| {
            symlink(source, probe_target)?;
            *symlink_path.borrow_mut() = Some(probe_target.to_path_buf());
            Ok(())
        });

    assert!(matches!(result, Err(StoreError::ColdStoreInvalidState)));
    assert!(!target.exists());
    let symlink_path = symlink_path.into_inner().unwrap();
    assert!(fs::symlink_metadata(&symlink_path)
        .unwrap()
        .file_type()
        .is_symlink());
    fs::remove_file(symlink_path).unwrap();
    assert!(probe_artifacts(temp.path()).is_empty());
}

#[test]
fn platform_hard_link_unsupported_codes_are_normalized() {
    assert!(hard_link_is_unsupported(&std::io::Error::from(
        std::io::ErrorKind::Unsupported
    )));
    assert!(hard_link_is_unsupported(&std::io::Error::from(
        std::io::ErrorKind::CrossesDevices
    )));
    for kind in [
        std::io::ErrorKind::PermissionDenied,
        std::io::ErrorKind::Other,
    ] {
        assert!(!hard_link_is_unsupported(&std::io::Error::from(kind)));
    }
    #[cfg(target_os = "windows")]
    for code in [1, 50] {
        assert!(hard_link_is_unsupported(
            &std::io::Error::from_raw_os_error(code)
        ));
    }
}

fn probe_artifacts(parent: &Path) -> Vec<std::path::PathBuf> {
    fs::read_dir(parent)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains(COLD_PROBE_MARKER))
        })
        .collect()
}
