use super::super::{extract_tar_gz_to_dir, extract_zip_to_dir};
use super::helpers::{set_raw_tar_path, write_tar_gz};
use std::io::Write;

#[test]
fn archive_extraction_zip_rejects_parent_traversal() {
    let temp = tempfile::tempdir().expect("tempdir");
    let zip_path = temp.path().join("traversal.zip");
    let file = std::fs::File::create(&zip_path).expect("create zip");
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default().unix_permissions(0o644);
    zip.start_file("../escape.txt", options)
        .expect("start zip entry");
    zip.write_all(b"escape").expect("write zip entry");
    zip.finish().expect("finish zip");

    let out_dir = temp.path().join("out");
    let err = extract_zip_to_dir(&zip_path, &out_dir).expect_err("zip traversal should fail");
    assert!(
        err.to_string().contains("parent directory"),
        "unexpected error: {err:#}"
    );
    assert!(!temp.path().join("escape.txt").exists());
}

#[test]
fn archive_extraction_tar_gz_rejects_parent_traversal() {
    let temp = tempfile::tempdir().expect("tempdir");
    let tar_path = temp.path().join("traversal.tar.gz");
    write_tar_gz(&tar_path, |builder| {
        let data = b"escape";
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_size(data.len() as u64);
        set_raw_tar_path(&mut header, b"../escape.txt");
        header.set_cksum();
        builder.append(&header, &data[..])?;
        Ok(())
    })
    .expect("write tar.gz");

    let out_dir = temp.path().join("out");
    let err = extract_tar_gz_to_dir(&tar_path, &out_dir).expect_err("tar traversal should fail");
    assert!(
        err.to_string().contains("parent directory"),
        "unexpected error: {err:#}"
    );
    assert!(!temp.path().join("escape.txt").exists());
}

#[test]
fn archive_extraction_tar_gz_skips_global_pax_header_with_empty_path() {
    let temp = tempfile::tempdir().expect("tempdir");
    let tar_path = temp.path().join("pax-global.tar.gz");
    write_tar_gz(&tar_path, |builder| {
        let pax_data = b"10 comment=x\n";
        let mut pax_header = tar::Header::new_gnu();
        pax_header.set_entry_type(tar::EntryType::XGlobalHeader);
        pax_header.set_mode(0o644);
        pax_header.set_size(pax_data.len() as u64);
        set_raw_tar_path(&mut pax_header, b"");
        pax_header.set_cksum();
        builder.append(&pax_header, &pax_data[..])?;

        let data = b"ok";
        let mut file_header = tar::Header::new_gnu();
        file_header.set_mode(0o644);
        file_header.set_size(data.len() as u64);
        file_header.set_cksum();
        builder.append_data(&mut file_header, "bin/goose", &data[..])?;
        Ok(())
    })
    .expect("write tar.gz");

    let out_dir = temp.path().join("out");
    extract_tar_gz_to_dir(&tar_path, &out_dir).expect("extract pax global header");
    assert_eq!(
        std::fs::read(out_dir.join("bin/goose")).expect("read file"),
        b"ok"
    );
}

#[test]
fn archive_extraction_tar_gz_skips_archive_root_directory_entry() {
    let temp = tempfile::tempdir().expect("tempdir");
    let tar_path = temp.path().join("root-dir.tar.gz");
    write_tar_gz(&tar_path, |builder| {
        let mut root_header = tar::Header::new_gnu();
        root_header.set_entry_type(tar::EntryType::Directory);
        root_header.set_mode(0o755);
        root_header.set_size(0);
        root_header.set_path("./")?;
        root_header.set_cksum();
        builder.append(&root_header, std::io::empty())?;

        let data = b"ok";
        let mut file_header = tar::Header::new_gnu();
        file_header.set_mode(0o755);
        file_header.set_size(data.len() as u64);
        file_header.set_path("./goose")?;
        file_header.set_cksum();
        builder.append(&file_header, &data[..])?;
        Ok(())
    })
    .expect("write tar.gz");

    let out_dir = temp.path().join("out");
    extract_tar_gz_to_dir(&tar_path, &out_dir).expect("extract root directory entry");
    assert_eq!(
        std::fs::read(out_dir.join("goose")).expect("read file"),
        b"ok"
    );
}
