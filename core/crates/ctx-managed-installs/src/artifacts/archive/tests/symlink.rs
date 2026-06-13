use super::super::{extract_tar_gz_to_dir, extract_zip_to_dir};
use super::helpers::{patch_zip_entry_unix_mode, write_tar_gz};
use std::io::Write;

#[test]
fn archive_extraction_tar_gz_rejects_symlink_escape() {
    let temp = tempfile::tempdir().expect("tempdir");
    let tar_path = temp.path().join("symlink-escape.tar.gz");
    write_tar_gz(&tar_path, |builder| {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_mode(0o777);
        header.set_size(0);
        header.set_path("bin/ctx")?;
        header.set_link_name("../../outside")?;
        header.set_cksum();
        builder.append(&header, std::io::empty())?;
        Ok(())
    })
    .expect("write tar.gz");

    let out_dir = temp.path().join("out");
    let err = extract_tar_gz_to_dir(&tar_path, &out_dir).expect_err("symlink escape should fail");
    assert!(
        err.to_string().contains("escapes extraction root"),
        "unexpected error: {err:#}"
    );
    assert!(!temp.path().join("outside").exists());
}

#[test]
fn archive_extraction_zip_rejects_symlink_escape() {
    let temp = tempfile::tempdir().expect("tempdir");
    let zip_path = temp.path().join("symlink-escape.zip");
    let file = std::fs::File::create(&zip_path).expect("create zip");
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default().unix_permissions(0o120777);
    zip.start_file("bin/ctx", options)
        .expect("start zip symlink");
    zip.write_all(b"../../outside").expect("write zip symlink");
    zip.finish().expect("finish zip");
    patch_zip_entry_unix_mode(&zip_path, "bin/ctx", 0o120777);

    let out_dir = temp.path().join("out");
    let err = extract_zip_to_dir(&zip_path, &out_dir).expect_err("symlink escape should fail");
    assert!(
        err.to_string().contains("escapes extraction root"),
        "unexpected error: {err:#}"
    );
    assert!(!temp.path().join("outside").exists());
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::path::Path;

    #[test]
    fn archive_extraction_tar_gz_allows_in_root_symlink() {
        let temp = tempfile::tempdir().expect("tempdir");
        let tar_path = temp.path().join("safe-symlink.tar.gz");
        write_tar_gz(&tar_path, |builder| {
            let mut dir_header = tar::Header::new_gnu();
            dir_header.set_entry_type(tar::EntryType::Directory);
            dir_header.set_mode(0o755);
            dir_header.set_size(0);
            dir_header.set_cksum();
            builder.append_data(&mut dir_header, "bin", std::io::empty())?;

            let mut symlink_header = tar::Header::new_gnu();
            symlink_header.set_entry_type(tar::EntryType::Symlink);
            symlink_header.set_mode(0o777);
            symlink_header.set_size(0);
            symlink_header.set_path("bin/npm")?;
            symlink_header.set_link_name("../lib/node_modules/npm/bin/npm-cli.js")?;
            symlink_header.set_cksum();
            builder.append(&symlink_header, std::io::empty())?;
            Ok(())
        })
        .expect("write tar.gz");

        let out_dir = temp.path().join("out");
        extract_tar_gz_to_dir(&tar_path, &out_dir).expect("extract safe symlink");
        let target = std::fs::read_link(out_dir.join("bin/npm")).expect("read symlink");
        assert_eq!(target, Path::new("../lib/node_modules/npm/bin/npm-cli.js"));
    }

    #[test]
    fn archive_extraction_tar_gz_allows_safe_symlink_ancestor() {
        let temp = tempfile::tempdir().expect("tempdir");
        let tar_path = temp.path().join("safe-symlink-ancestor.tar.gz");
        write_tar_gz(&tar_path, |builder| {
            let mut target_dir_header = tar::Header::new_gnu();
            target_dir_header.set_entry_type(tar::EntryType::Directory);
            target_dir_header.set_mode(0o755);
            target_dir_header.set_size(0);
            target_dir_header.set_cksum();
            builder.append_data(&mut target_dir_header, "terminfo/32", std::io::empty())?;

            let mut symlink_header = tar::Header::new_gnu();
            symlink_header.set_entry_type(tar::EntryType::Symlink);
            symlink_header.set_mode(0o777);
            symlink_header.set_size(0);
            symlink_header.set_path("terminfo/2")?;
            symlink_header.set_link_name("32")?;
            symlink_header.set_cksum();
            builder.append(&symlink_header, std::io::empty())?;

            let data = b"entry";
            let mut file_header = tar::Header::new_gnu();
            file_header.set_mode(0o644);
            file_header.set_size(data.len() as u64);
            file_header.set_cksum();
            builder.append_data(&mut file_header, "terminfo/2/2621a", &data[..])?;
            Ok(())
        })
        .expect("write tar.gz");

        let out_dir = temp.path().join("out");
        extract_tar_gz_to_dir(&tar_path, &out_dir).expect("extract safe symlink ancestor");
        assert_eq!(
            std::fs::read(out_dir.join("terminfo/32/2621a")).expect("read symlink target file"),
            b"entry"
        );
    }

    #[test]
    fn archive_extraction_tar_gz_allows_file_write_through_safe_symlink() {
        let temp = tempfile::tempdir().expect("tempdir");
        let tar_path = temp.path().join("safe-final-symlink.tar.gz");
        write_tar_gz(&tar_path, |builder| {
            let mut target_dir_header = tar::Header::new_gnu();
            target_dir_header.set_entry_type(tar::EntryType::Directory);
            target_dir_header.set_mode(0o755);
            target_dir_header.set_size(0);
            target_dir_header.set_cksum();
            builder.append_data(&mut target_dir_header, "terminfo/32", std::io::empty())?;

            let mut ancestor_symlink = tar::Header::new_gnu();
            ancestor_symlink.set_entry_type(tar::EntryType::Symlink);
            ancestor_symlink.set_mode(0o777);
            ancestor_symlink.set_size(0);
            ancestor_symlink.set_path("terminfo/2")?;
            ancestor_symlink.set_link_name("32")?;
            ancestor_symlink.set_cksum();
            builder.append(&ancestor_symlink, std::io::empty())?;

            let mut final_symlink = tar::Header::new_gnu();
            final_symlink.set_entry_type(tar::EntryType::Symlink);
            final_symlink.set_mode(0o777);
            final_symlink.set_size(0);
            final_symlink.set_path("terminfo/32/2621a")?;
            final_symlink.set_link_name("target")?;
            final_symlink.set_cksum();
            builder.append(&final_symlink, std::io::empty())?;

            let data = b"entry";
            let mut file_header = tar::Header::new_gnu();
            file_header.set_mode(0o644);
            file_header.set_size(data.len() as u64);
            file_header.set_cksum();
            builder.append_data(&mut file_header, "terminfo/2/2621a", &data[..])?;
            Ok(())
        })
        .expect("write tar.gz");

        let out_dir = temp.path().join("out");
        extract_tar_gz_to_dir(&tar_path, &out_dir).expect("extract safe final symlink");
        assert_eq!(
            std::fs::read(out_dir.join("terminfo/32/target")).expect("read symlink target file"),
            b"entry"
        );
    }

    #[test]
    fn archive_extraction_tar_gz_replaces_self_referential_symlink_with_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let tar_path = temp.path().join("self-symlink-file.tar.gz");
        write_tar_gz(&tar_path, |builder| {
            let mut symlink_header = tar::Header::new_gnu();
            symlink_header.set_entry_type(tar::EntryType::Symlink);
            symlink_header.set_mode(0o777);
            symlink_header.set_size(0);
            symlink_header.set_path("terminfo/n/ncr260vt300wpp")?;
            symlink_header.set_link_name("ncr260vt300wpp")?;
            symlink_header.set_cksum();
            builder.append(&symlink_header, std::io::empty())?;

            let data = b"entry";
            let mut file_header = tar::Header::new_gnu();
            file_header.set_mode(0o644);
            file_header.set_size(data.len() as u64);
            file_header.set_cksum();
            builder.append_data(&mut file_header, "terminfo/n/ncr260vt300wpp", &data[..])?;
            Ok(())
        })
        .expect("write tar.gz");

        let out_dir = temp.path().join("out");
        extract_tar_gz_to_dir(&tar_path, &out_dir).expect("extract self symlink then file");
        assert_eq!(
            std::fs::read(out_dir.join("terminfo/n/ncr260vt300wpp")).expect("read file"),
            b"entry"
        );
    }

    #[test]
    fn archive_extraction_tar_gz_allows_in_root_symlink_loop() {
        let temp = tempfile::tempdir().expect("tempdir");
        let tar_path = temp.path().join("symlink-loop.tar.gz");
        write_tar_gz(&tar_path, |builder| {
            let mut symlink_header = tar::Header::new_gnu();
            symlink_header.set_entry_type(tar::EntryType::Symlink);
            symlink_header.set_mode(0o777);
            symlink_header.set_size(0);
            symlink_header.set_path("terminfo/N/NCR260VT300WPP")?;
            symlink_header.set_link_name("NCR260VT300WPP")?;
            symlink_header.set_cksum();
            builder.append(&symlink_header, std::io::empty())?;
            Ok(())
        })
        .expect("write tar.gz");

        let out_dir = temp.path().join("out");
        extract_tar_gz_to_dir(&tar_path, &out_dir).expect("extract symlink loop");
        assert_eq!(
            std::fs::read_link(out_dir.join("terminfo/N/NCR260VT300WPP")).expect("read symlink"),
            Path::new("NCR260VT300WPP")
        );
    }

    #[test]
    fn archive_extraction_tar_gz_rejects_symlink_escape_under_canonical_parent() {
        let temp = tempfile::tempdir().expect("tempdir");
        let tar_path = temp.path().join("canonical-parent-symlink-escape.tar.gz");
        write_tar_gz(&tar_path, |builder| {
            let mut target_dir_header = tar::Header::new_gnu();
            target_dir_header.set_entry_type(tar::EntryType::Directory);
            target_dir_header.set_mode(0o755);
            target_dir_header.set_size(0);
            target_dir_header.set_cksum();
            builder.append_data(&mut target_dir_header, "d", std::io::empty())?;

            let mut ancestor_symlink = tar::Header::new_gnu();
            ancestor_symlink.set_entry_type(tar::EntryType::Symlink);
            ancestor_symlink.set_mode(0o777);
            ancestor_symlink.set_size(0);
            ancestor_symlink.set_path("a/b/c")?;
            ancestor_symlink.set_link_name("../../d")?;
            ancestor_symlink.set_cksum();
            builder.append(&ancestor_symlink, std::io::empty())?;

            let mut escaping_symlink = tar::Header::new_gnu();
            escaping_symlink.set_entry_type(tar::EntryType::Symlink);
            escaping_symlink.set_mode(0o777);
            escaping_symlink.set_size(0);
            escaping_symlink.set_path("a/b/c/escape")?;
            escaping_symlink.set_link_name("../../outside")?;
            escaping_symlink.set_cksum();
            builder.append(&escaping_symlink, std::io::empty())?;
            Ok(())
        })
        .expect("write tar.gz");

        let out_dir = temp.path().join("out");
        let err =
            extract_tar_gz_to_dir(&tar_path, &out_dir).expect_err("symlink escape should fail");
        assert!(
            err.to_string().contains("escapes extraction root"),
            "unexpected error: {err:#}"
        );
        assert!(!out_dir.join("d/escape").exists());
    }

    #[test]
    fn archive_extraction_tar_gz_rejects_existing_symlink_ancestor_escape() {
        let temp = tempfile::tempdir().expect("tempdir");
        let tar_path = temp.path().join("existing-symlink-ancestor.tar.gz");
        write_tar_gz(&tar_path, |builder| {
            let data = b"escape";
            let mut file_header = tar::Header::new_gnu();
            file_header.set_mode(0o644);
            file_header.set_size(data.len() as u64);
            file_header.set_cksum();
            builder.append_data(&mut file_header, "link/file", &data[..])?;
            Ok(())
        })
        .expect("write tar.gz");

        let out_dir = temp.path().join("out");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&outside).expect("create outside");
        std::fs::create_dir_all(&out_dir).expect("create out");
        std::os::unix::fs::symlink(&outside, out_dir.join("link")).expect("create symlink");

        let err = extract_tar_gz_to_dir(&tar_path, &out_dir)
            .expect_err("symlink ancestor escape should fail");
        assert!(
            err.to_string().contains("escaped extraction root"),
            "unexpected error: {err:#}"
        );
        assert!(!outside.join("file").exists());
    }

    #[test]
    fn archive_extraction_tar_gz_rejects_existing_final_symlink_escape() {
        let temp = tempfile::tempdir().expect("tempdir");
        let tar_path = temp.path().join("existing-final-symlink.tar.gz");
        write_tar_gz(&tar_path, |builder| {
            let data = b"escape";
            let mut file_header = tar::Header::new_gnu();
            file_header.set_mode(0o644);
            file_header.set_size(data.len() as u64);
            file_header.set_cksum();
            builder.append_data(&mut file_header, "link", &data[..])?;
            Ok(())
        })
        .expect("write tar.gz");

        let out_dir = temp.path().join("out");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&outside).expect("create outside");
        std::fs::create_dir_all(&out_dir).expect("create out");
        std::os::unix::fs::symlink(&outside, out_dir.join("link")).expect("create symlink");

        let err = extract_tar_gz_to_dir(&tar_path, &out_dir)
            .expect_err("final symlink escape should fail");
        assert!(
            err.to_string().contains("escaped extraction root")
                || err.to_string().contains("escapes extraction root")
                || err.to_string().contains("must be relative"),
            "unexpected error: {err:#}"
        );
        assert!(!outside.join("link").exists());
    }
}
