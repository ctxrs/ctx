use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QualifiedClonePath {
    DescriptorHardLink,
    PortableCopy,
}

fn qualified_clone_path(target_os: &str) -> Option<QualifiedClonePath> {
    match target_os {
        "linux" | "macos" => Some(QualifiedClonePath::DescriptorHardLink),
        "windows" | "freebsd" => Some(QualifiedClonePath::PortableCopy),
        _ => None,
    }
}

#[test]
fn every_release_os_selects_a_qualified_clone_path() {
    assert_eq!(
        qualified_clone_path("linux"),
        Some(QualifiedClonePath::DescriptorHardLink)
    );
    assert_eq!(
        qualified_clone_path("macos"),
        Some(QualifiedClonePath::DescriptorHardLink)
    );
    assert_eq!(
        qualified_clone_path("windows"),
        Some(QualifiedClonePath::PortableCopy)
    );
    assert_eq!(
        qualified_clone_path("freebsd"),
        Some(QualifiedClonePath::PortableCopy)
    );
    assert_eq!(qualified_clone_path("unsupported"), None);
}

#[test]
fn clone_resource_bounds_fail_before_copying() {
    let mut files = 0;
    let mut bytes = 0;
    admit_clone_resource(&mut files, &mut bytes, 4, 1, 4).unwrap();
    assert!(matches!(
        admit_clone_resource(&mut files, &mut bytes, 0, 1, 4),
        Err(IndexError::CurrentRepublishFileLimit {
            actual: 2,
            maximum: 1
        })
    ));

    let mut files = 0;
    let mut bytes = 0;
    assert!(matches!(
        admit_clone_resource(&mut files, &mut bytes, 5, 1, 4),
        Err(IndexError::CurrentRepublishByteLimit {
            actual: 5,
            maximum: 4
        })
    ));
}

#[test]
fn managed_paths_must_be_single_relative_components() {
    assert!(validate_single_component(Path::new("meta.json")).is_ok());
    for path in ["../meta.json", "nested/meta.json", "/meta.json"] {
        assert!(matches!(
            validate_single_component(Path::new(path)),
            Err(IndexError::CurrentRepublishSourceTopology(_))
        ));
    }
}
