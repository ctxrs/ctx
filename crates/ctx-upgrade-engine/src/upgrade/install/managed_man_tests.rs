use super::*;
use std::os::unix::fs::{symlink, PermissionsExt};

const OLD_BINARY_SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn page(name: &str, bytes: &[u8]) -> ManagedManPage {
    ManagedManPage {
        name: name.to_owned(),
        bytes: bytes.to_vec(),
    }
}

fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let bin = temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o700)).unwrap();
    let executable = bin.join("ctx");
    fs::write(&executable, b"ctx binary").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let man = temp.path().join("man/man1");
    fs::create_dir_all(&man).unwrap();
    (temp, executable, man)
}

fn install_marker(
    executable: &Path,
    man: &Path,
    files: &[(&str, &[u8])],
    receipt_binary_sha: &str,
) {
    for (name, bytes) in files {
        fs::write(man.join(name), bytes).unwrap();
    }
    let marker = json!({
        "schema_version": 1,
        "manager": "ctx-hosted-installer",
        "install_path": executable,
        "platform": platform_key().unwrap(),
        "channel": "stable",
        "version": "2.0.0",
        "sha256": sha256_hex(&fs::read(executable).unwrap()),
        "man_pages": {
            "schema_version": 1,
            "status": "installed",
            "directory": man,
            "files": files.iter().map(|(name, bytes)| json!({"name": name, "sha256": sha256_hex(bytes)})).collect::<Vec<_>>(),
            "binary_sha256": receipt_binary_sha,
        }
    });
    atomic_write_json(&marker::install_marker_path(executable), &marker).unwrap();
}

fn run(executable: &Path, pages: Vec<ManagedManPage>) -> bool {
    reconcile_at(executable, platform_key().unwrap(), || {
        Ok(ManagedManBundle { pages })
    })
    .unwrap()
}

fn marker_value(executable: &Path) -> Value {
    serde_json::from_slice(&fs::read(marker::install_marker_path(executable)).unwrap()).unwrap()
}

#[test]
fn explicit_opt_out_replaces_only_the_receipt() {
    let (_temp, executable, man) = fixture();
    let binary_sha = sha256_hex(&fs::read(&executable).unwrap());
    install_marker(&executable, &man, &[("ctx.1", b"old")], &binary_sha);

    disable_at(&executable, platform_key().unwrap()).unwrap();

    let marker = marker_value(&executable);
    assert_eq!(
        marker["man_pages"],
        json!({"schema_version": 1, "status": "disabled"})
    );
    assert_eq!(marker["version"], "2.0.0");
    assert_eq!(fs::read(man.join("ctx.1")).unwrap(), b"old");
}

#[test]
fn current_and_disabled_receipts_skip_rendering() {
    let (_temp, executable, man) = fixture();
    let current_sha = sha256_hex(&fs::read(&executable).unwrap());
    install_marker(&executable, &man, &[("ctx.1", b"old")], &current_sha);
    assert!(!reconcile_at(&executable, platform_key().unwrap(), || {
        bail!("current receipt must not render")
    })
    .unwrap());

    let marker_path = marker::install_marker_path(&executable);
    let mut marker = marker_value(&executable);
    marker[RECEIPT_KEY] = json!({"schema_version": 1, "status": "disabled"});
    atomic_write_json(&marker_path, &marker).unwrap();
    assert!(!reconcile_at(&executable, platform_key().unwrap(), || {
        bail!("disabled receipt must not render")
    })
    .unwrap());
}

#[test]
fn missing_and_invalid_receipts_are_ignored() {
    let (_temp, executable, man) = fixture();
    install_marker(&executable, &man, &[("ctx.1", b"old")], OLD_BINARY_SHA);
    let marker_path = marker::install_marker_path(&executable);
    let mut marker = marker_value(&executable);
    marker.as_object_mut().unwrap().remove(RECEIPT_KEY);
    atomic_write_json(&marker_path, &marker).unwrap();
    let before = fs::read(&marker_path).unwrap();
    assert!(!reconcile_at(&executable, platform_key().unwrap(), || {
        bail!("missing receipt must not render")
    })
    .unwrap());
    assert_eq!(fs::read(&marker_path).unwrap(), before);

    marker[RECEIPT_KEY] = json!({"schema_version": 99, "status": "installed"});
    atomic_write_json(&marker_path, &marker).unwrap();
    let before = fs::read(&marker_path).unwrap();
    assert!(!reconcile_at(&executable, platform_key().unwrap(), || {
        bail!("invalid receipt must not render")
    })
    .unwrap());
    assert_eq!(fs::read(&marker_path).unwrap(), before);
}

#[test]
fn held_install_lock_defers_without_rendering() {
    let (_temp, executable, man) = fixture();
    install_marker(&executable, &man, &[("ctx.1", b"old")], OLD_BINARY_SHA);
    let _lock = InstallationLock::try_acquire(&executable).unwrap().unwrap();
    assert!(!reconcile_at(&executable, platform_key().unwrap(), || {
        bail!("held lock must not render")
    })
    .unwrap());
}

#[test]
fn normal_refresh_replaces_adds_removes_and_records_minimal_receipt() {
    let (_temp, executable, man) = fixture();
    install_marker(
        &executable,
        &man,
        &[("ctx.1", b"old"), ("ctx-old.1", b"retired")],
        OLD_BINARY_SHA,
    );
    fs::write(man.join("ctx-user.1"), b"user").unwrap();
    assert!(run(
        &executable,
        vec![page("ctx.1", b"new"), page("ctx-new.1", b"new2")]
    ));
    assert_eq!(fs::read(man.join("ctx.1")).unwrap(), b"new");
    assert_eq!(fs::read(man.join("ctx-new.1")).unwrap(), b"new2");
    assert!(!man.join("ctx-old.1").exists());
    assert_eq!(fs::read(man.join("ctx-user.1")).unwrap(), b"user");

    let receipt = marker_value(&executable)[RECEIPT_KEY].clone();
    assert_eq!(receipt.as_object().unwrap().len(), 5);
    assert_eq!(receipt["status"], "installed");
    assert_eq!(
        receipt["binary_sha256"],
        sha256_hex(&fs::read(&executable).unwrap())
    );
}

#[test]
fn modified_or_missing_recorded_page_is_left_stale() {
    for replacement in [Some(b"user changes".as_slice()), None] {
        let (_temp, executable, man) = fixture();
        install_marker(&executable, &man, &[("ctx.1", b"old")], OLD_BINARY_SHA);
        match replacement {
            Some(bytes) => fs::write(man.join("ctx.1"), bytes).unwrap(),
            None => fs::remove_file(man.join("ctx.1")).unwrap(),
        }
        assert!(!run(&executable, vec![page("ctx.1", b"new")]));
        assert_eq!(
            marker_value(&executable)[RECEIPT_KEY]["binary_sha256"],
            sha256_hex(&fs::read(&executable).unwrap())
        );
        assert!(!reconcile_at(&executable, platform_key().unwrap(), || {
            bail!("failed attempt must not repeat for this binary")
        })
        .unwrap());
    }
}

#[test]
fn symlink_nonregular_and_unsafe_directory_are_left_stale() {
    let (_temp, executable, man) = fixture();
    install_marker(&executable, &man, &[("ctx.1", b"old")], OLD_BINARY_SHA);
    fs::remove_file(man.join("ctx.1")).unwrap();
    symlink("elsewhere", man.join("ctx.1")).unwrap();
    assert!(!run(&executable, vec![page("ctx.1", b"new")]));

    let (_temp, executable, man) = fixture();
    install_marker(&executable, &man, &[("ctx.1", b"old")], OLD_BINARY_SHA);
    fs::remove_file(man.join("ctx.1")).unwrap();
    fs::create_dir(man.join("ctx.1")).unwrap();
    assert!(!run(&executable, vec![page("ctx.1", b"new")]));

    let (_temp, executable, man) = fixture();
    install_marker(&executable, &man, &[("ctx.1", b"old")], OLD_BINARY_SHA);
    fs::set_permissions(&man, fs::Permissions::from_mode(0o770)).unwrap();
    assert!(!run(&executable, vec![page("ctx.1", b"new")]));
    assert_eq!(fs::read(man.join("ctx.1")).unwrap(), b"old");
}

#[test]
fn unrecorded_destination_is_never_adopted() {
    let (_temp, executable, man) = fixture();
    install_marker(&executable, &man, &[("ctx.1", b"old")], OLD_BINARY_SHA);
    fs::write(man.join("ctx-new.1"), b"new2").unwrap();
    assert!(!run(
        &executable,
        vec![page("ctx.1", b"new"), page("ctx-new.1", b"new2")]
    ));
    assert_eq!(fs::read(man.join("ctx.1")).unwrap(), b"old");
    assert_eq!(fs::read(man.join("ctx-new.1")).unwrap(), b"new2");
}

#[test]
fn interrupted_mixed_pages_are_not_recovered() {
    let (_temp, executable, man) = fixture();
    install_marker(
        &executable,
        &man,
        &[("ctx.1", b"old"), ("ctx-new.1", b"old2")],
        OLD_BINARY_SHA,
    );
    fs::write(man.join("ctx.1"), b"new").unwrap();
    assert!(!run(
        &executable,
        vec![page("ctx.1", b"new"), page("ctx-new.1", b"new2")]
    ));
    assert_eq!(fs::read(man.join("ctx-new.1")).unwrap(), b"old2");
    assert_eq!(
        marker_value(&executable)[RECEIPT_KEY]["binary_sha256"],
        sha256_hex(&fs::read(&executable).unwrap())
    );
}

#[test]
fn invalid_or_unmanaged_marker_never_writes() {
    let (_temp, executable, man) = fixture();
    install_marker(&executable, &man, &[("ctx.1", b"old")], OLD_BINARY_SHA);
    let marker_path = marker::install_marker_path(&executable);
    let mut invalid = marker_value(&executable);
    invalid["sha256"] = json!("0".repeat(64));
    atomic_write_json(&marker_path, &invalid).unwrap();
    let before = fs::read(&marker_path).unwrap();
    assert!(!run(&executable, vec![page("ctx.1", b"new")]));
    assert_eq!(fs::read(&marker_path).unwrap(), before);
    assert_eq!(fs::read(man.join("ctx.1")).unwrap(), b"old");
}
