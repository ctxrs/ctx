use super::*;
use std::io::Write;

#[test]
fn managed_data_root_copy_preserves_opaque_data_and_excludes_live_coordination() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let destination = temp.path().join("disk with spaces");
    platform_security::create_private_directory_all(&source).unwrap();
    let identity = crate::identity::installation_id(&source).unwrap();
    for (name, content) in [
        ("config.toml", "[search]\nsemantic = false\n"),
        ("credentials.json", "opaque-private-state"),
        ("opaque.lock", "not-a-ctx-coordination-file"),
        ("runtime/library", "runtime-bytes"),
        (
            "search/lexical/generations/one/data",
            "immutable-generation",
        ),
        ("daemon/query.sock", "stale"),
        ("daemon-installations/test/lease", "stale"),
        (".config.mutation.lock", "stale"),
    ] {
        let path = source.join(name);
        platform_security::create_private_directory_all(path.parent().unwrap()).unwrap();
        platform_security::create_private_file_new(&path)
            .unwrap()
            .write_all(content.as_bytes())
            .unwrap();
    }
    copy::copy_root(&source, &destination).unwrap();
    assert_eq!(
        crate::identity::existing_installation_id(&destination)
            .unwrap()
            .unwrap(),
        identity
    );
    assert_eq!(
        fs::read(source.join("install.json")).unwrap(),
        fs::read(destination.join("install.json")).unwrap()
    );
    assert_eq!(
        fs::read(destination.join("credentials.json")).unwrap(),
        b"opaque-private-state"
    );
    assert_eq!(
        fs::read(destination.join("opaque.lock")).unwrap(),
        b"not-a-ctx-coordination-file"
    );
    assert!(destination.join("runtime/library").is_file());
    assert!(destination
        .join("search/lexical/generations/one/data")
        .is_file());
    assert!(!destination.join("daemon").exists());
    assert!(!destination.join("daemon-installations").exists());
    assert!(!destination.join(".config.mutation.lock").exists());
    assert!(source.join("daemon/query.sock").exists());
    platform_security::verify_private_directory(&destination).unwrap();
    platform_security::verify_private_file(&destination.join("credentials.json")).unwrap();
}

#[test]
fn managed_data_root_copy_refuses_nonempty_overlapping_and_unavailable_destinations() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    platform_security::create_private_directory_all(&source).unwrap();
    for destination in [
        &source,
        &source.join("nested"),
        &temp.path().to_path_buf(),
        &temp.path().join("unmounted/data"),
    ] {
        assert!(copy::validate_destination(&source, destination).is_err());
    }
    let destination = temp.path().join("nonempty");
    platform_security::create_private_directory_all(&destination).unwrap();
    fs::write(destination.join("keep"), "keep").unwrap();
    assert!(copy::copy_root(&source, &destination).is_err());
    assert_eq!(fs::read(destination.join("keep")).unwrap(), b"keep");
}

#[cfg(unix)]
#[test]
fn managed_data_root_copy_refuses_links_without_changing_source_or_link_target() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    platform_security::create_private_directory_all(&source).unwrap();
    let outside = temp.path().join("provider");
    fs::write(&outside, "provider-data").unwrap();
    std::os::unix::fs::symlink(&outside, source.join("unsafe")).unwrap();
    assert!(copy::copy_root(&source, &temp.path().join("destination")).is_err());
    assert!(fs::symlink_metadata(source.join("unsafe"))
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(fs::read(&outside).unwrap(), b"provider-data");
}
