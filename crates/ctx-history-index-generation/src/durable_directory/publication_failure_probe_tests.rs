use std::{cell::RefCell, rc::Rc};

use tempfile::tempdir;

use super::*;

#[test]
fn replacement_failure_preserves_error_reports_cleanup_and_redacts_paths() {
    let temporary_directory = tempdir().unwrap();
    let target_path = temporary_directory.path().join("meta.json");
    fs::write(&target_path, b"previous").unwrap();
    let observed = Rc::new(RefCell::new(Vec::new()));
    let hook_observed = Rc::clone(&observed);
    let hook = PublicationIoProbeGuard::set(move |probe| {
        let diagnostic = matches!(
            probe,
            crate::PublicationIoProbe::AtomicReplacementFailure(_)
        );
        hook_observed.borrow_mut().push(probe);
        if diagnostic {
            Err(io::Error::from_raw_os_error(87))
        } else {
            Ok(())
        }
    });

    let error = atomic_replace_with(
        &target_path,
        b"replacement",
        |temporary_path, target_path| {
            assert_eq!(fs::read(temporary_path).unwrap(), b"replacement");
            assert_eq!(fs::read(target_path).unwrap(), b"previous");
            Err(io::Error::from_raw_os_error(5))
        },
        || panic!("parent sync must not run after replacement failure"),
    )
    .unwrap_err();
    drop(hook);

    assert_eq!(error.raw_os_error(), Some(5));
    assert_eq!(error.kind(), io::Error::from_raw_os_error(5).kind());
    assert!(std::error::Error::source(&error).is_none());
    assert_eq!(fs::read(&target_path).unwrap(), b"previous");
    assert!(fs::read_dir(temporary_directory.path())
        .unwrap()
        .all(|entry| !is_atomic_temporary_file(&entry.unwrap().file_name())));
    let probe = observed
        .borrow()
        .iter()
        .find_map(|probe| match probe {
            crate::PublicationIoProbe::AtomicReplacementFailure(probe) => Some(*probe),
            _ => None,
        })
        .expect("replacement failure diagnostic");
    assert_eq!(probe.move_error, Some(5));
    assert_eq!(probe.source_cleanup, Some(Ok(())));
    #[cfg(not(windows))]
    assert_eq!(
        (
            probe.source_readonly,
            probe.source_delete_open,
            probe.parent_delete_child_open,
            probe.target_delete_open,
        ),
        (None, None, None, None)
    );
    #[cfg(windows)]
    {
        assert_eq!(probe.source_readonly, Some(false));
        assert!(probe.source_delete_open.is_some());
        assert!(probe.parent_delete_child_open.is_some());
        assert!(probe.target_delete_open.is_some());
    }
    assert!(!format!("{probe:?}").contains(&temporary_directory.path().display().to_string()));
}
