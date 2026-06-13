use super::super::extract_tar_gz_to_dir;
use super::helpers::{path_is_ascii_case_insensitive, write_tar_gz};
use std::path::Path;

#[test]
fn archive_extraction_tar_gz_allows_duplicate_symlink_with_same_target() {
    let temp = tempfile::tempdir().expect("tempdir");
    let tar_path = temp.path().join("duplicate-symlink.tar.gz");
    write_tar_gz(&tar_path, |builder| {
        for _ in 0..2 {
            let mut symlink_header = tar::Header::new_gnu();
            symlink_header.set_entry_type(tar::EntryType::Symlink);
            symlink_header.set_mode(0o777);
            symlink_header.set_size(0);
            symlink_header.set_path("terminfo/2/2621a")?;
            symlink_header.set_link_name("../h/hp2621")?;
            symlink_header.set_cksum();
            builder.append(&symlink_header, std::io::empty())?;
        }
        Ok(())
    })
    .expect("write tar.gz");

    let out_dir = temp.path().join("out");
    extract_tar_gz_to_dir(&tar_path, &out_dir).expect("extract duplicate symlink");
    assert_eq!(
        std::fs::read_link(out_dir.join("terminfo/2/2621a")).expect("read symlink"),
        Path::new("../h/hp2621")
    );
}

#[test]
fn archive_extraction_tar_gz_allows_duplicate_symlink_with_same_resolved_target() {
    let temp = tempfile::tempdir().expect("tempdir");
    let tar_path = temp
        .path()
        .join("duplicate-symlink-same-resolved-target.tar.gz");
    write_tar_gz(&tar_path, |builder| {
        let data = b"target";
        let mut file_header = tar::Header::new_gnu();
        file_header.set_mode(0o644);
        file_header.set_size(data.len() as u64);
        file_header.set_cksum();
        builder.append_data(&mut file_header, "terminfo/l/lft", &data[..])?;

        for target in ["lft", "../l/lft"] {
            let mut symlink_header = tar::Header::new_gnu();
            symlink_header.set_entry_type(tar::EntryType::Symlink);
            symlink_header.set_mode(0o777);
            symlink_header.set_size(0);
            symlink_header.set_path("terminfo/l/lft-pc850")?;
            symlink_header.set_link_name(target)?;
            symlink_header.set_cksum();
            builder.append(&symlink_header, std::io::empty())?;
        }
        Ok(())
    })
    .expect("write tar.gz");

    let out_dir = temp.path().join("out");
    extract_tar_gz_to_dir(&tar_path, &out_dir).expect("extract duplicate symlink");
    assert_eq!(
        std::fs::read(out_dir.join("terminfo/l/lft-pc850")).expect("read symlink target"),
        b"target"
    );
}

#[test]
fn archive_extraction_tar_gz_allows_duplicate_symlink_with_same_missing_target() {
    let temp = tempfile::tempdir().expect("tempdir");
    let tar_path = temp
        .path()
        .join("duplicate-symlink-same-missing-target.tar.gz");
    write_tar_gz(&tar_path, |builder| {
        for target in ["prism12", "./prism12"] {
            let mut symlink_header = tar::Header::new_gnu();
            symlink_header.set_entry_type(tar::EntryType::Symlink);
            symlink_header.set_mode(0o777);
            symlink_header.set_size(0);
            symlink_header.set_path("terminfo/p/p12")?;
            symlink_header.set_link_name(target)?;
            symlink_header.set_cksum();
            builder.append(&symlink_header, std::io::empty())?;
        }
        Ok(())
    })
    .expect("write tar.gz");

    let out_dir = temp.path().join("out");
    extract_tar_gz_to_dir(&tar_path, &out_dir).expect("extract duplicate symlink");
    assert_eq!(
        std::fs::read_link(out_dir.join("terminfo/p/p12")).expect("read symlink"),
        Path::new("prism12")
    );
}

#[test]
fn archive_extraction_tar_gz_rejects_duplicate_symlink_with_different_target() {
    let temp = tempfile::tempdir().expect("tempdir");
    let tar_path = temp
        .path()
        .join("duplicate-symlink-different-target.tar.gz");
    write_tar_gz(&tar_path, |builder| {
        for target in ["../h/hp2621", "../h/other"] {
            let mut symlink_header = tar::Header::new_gnu();
            symlink_header.set_entry_type(tar::EntryType::Symlink);
            symlink_header.set_mode(0o777);
            symlink_header.set_size(0);
            symlink_header.set_path("terminfo/2/2621a")?;
            symlink_header.set_link_name(target)?;
            symlink_header.set_cksum();
            builder.append(&symlink_header, std::io::empty())?;
        }
        Ok(())
    })
    .expect("write tar.gz");

    let out_dir = temp.path().join("out");
    let err = extract_tar_gz_to_dir(&tar_path, &out_dir)
        .expect_err("different duplicate target should fail");
    assert!(
        err.to_string()
            .contains("refused to replace existing path with symlink"),
        "unexpected error: {err:#}"
    );
}

#[test]
fn archive_extraction_tar_gz_rejects_case_only_duplicate_symlink_targets_on_case_sensitive_root() {
    let temp = tempfile::tempdir().expect("tempdir");
    if path_is_ascii_case_insensitive(temp.path()) {
        return;
    }
    let tar_path = temp
        .path()
        .join("duplicate-symlink-case-only-target.tar.gz");
    write_tar_gz(&tar_path, |builder| {
        for target in ["A", "a"] {
            let mut symlink_header = tar::Header::new_gnu();
            symlink_header.set_entry_type(tar::EntryType::Symlink);
            symlink_header.set_mode(0o777);
            symlink_header.set_size(0);
            symlink_header.set_path("x")?;
            symlink_header.set_link_name(target)?;
            symlink_header.set_cksum();
            builder.append(&symlink_header, std::io::empty())?;
        }

        let data = b"target";
        let mut file_header = tar::Header::new_gnu();
        file_header.set_mode(0o644);
        file_header.set_size(data.len() as u64);
        file_header.set_cksum();
        builder.append_data(&mut file_header, "a", &data[..])?;
        Ok(())
    })
    .expect("write tar.gz");

    let out_dir = temp.path().join("out");
    let err = extract_tar_gz_to_dir(&tar_path, &out_dir)
        .expect_err("case-only duplicate symlink targets should fail");
    assert!(
        err.to_string()
            .contains("refused to replace existing path with symlink"),
        "unexpected error: {err:#}"
    );
}
