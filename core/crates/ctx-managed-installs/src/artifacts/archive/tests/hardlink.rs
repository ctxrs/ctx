use super::super::extract_tar_gz_to_dir;
use super::helpers::write_tar_gz;

#[test]
fn archive_extraction_tar_gz_allows_in_root_hardlinks() {
    let temp = tempfile::tempdir().expect("tempdir");
    let tar_path = temp.path().join("hardlink.tar.gz");
    write_tar_gz(&tar_path, |builder| {
        let data = b"linked";
        let mut file_header = tar::Header::new_gnu();
        file_header.set_mode(0o755);
        file_header.set_size(data.len() as u64);
        file_header.set_cksum();
        builder.append_data(&mut file_header, "bin/source", &data[..])?;

        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Link);
        header.set_mode(0o777);
        header.set_size(0);
        header.set_path("bin/ctx")?;
        header.set_link_name("bin/source")?;
        header.set_cksum();
        builder.append(&header, std::io::empty())?;
        Ok(())
    })
    .expect("write tar.gz");

    let out_dir = temp.path().join("out");
    extract_tar_gz_to_dir(&tar_path, &out_dir).expect("extract hardlink");
    assert_eq!(
        std::fs::read(out_dir.join("bin/ctx")).expect("read hardlink"),
        b"linked"
    );
}

#[test]
fn archive_extraction_tar_gz_rejects_escaping_hardlink_target() {
    let temp = tempfile::tempdir().expect("tempdir");
    let tar_path = temp.path().join("hardlink-escape.tar.gz");
    write_tar_gz(&tar_path, |builder| {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Link);
        header.set_mode(0o777);
        header.set_size(0);
        header.set_path("bin/ctx")?;
        header.set_link_name("../outside")?;
        header.set_cksum();
        builder.append(&header, std::io::empty())?;
        Ok(())
    })
    .expect("write tar.gz");

    let out_dir = temp.path().join("out");
    let err = extract_tar_gz_to_dir(&tar_path, &out_dir).expect_err("hardlink escape should fail");
    assert!(
        err.to_string().contains("parent directory"),
        "unexpected error: {err:#}"
    );
}
