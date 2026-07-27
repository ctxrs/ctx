#![cfg(unix)]

mod support;

use std::path::{Path, PathBuf};
use support::select_python3_interpreter;

#[test]
fn python_helper_interpreter_selection_is_platform_bounded() {
    assert_eq!(
        select_python3_interpreter("linux", |path| Some(path.to_owned()), |_| true),
        Some(PathBuf::from("/usr/bin/python3"))
    );
    assert_eq!(
        select_python3_interpreter("freebsd", |path| Some(path.to_owned()), |_| true),
        Some(PathBuf::from("/usr/local/bin/python3"))
    );
}

#[test]
fn python_helper_interpreter_selection_reports_missing_candidates() {
    for target_os in ["linux", "freebsd"] {
        assert_eq!(
            select_python3_interpreter(target_os, |_| None, |_| true),
            None
        );
        assert_eq!(
            select_python3_interpreter(target_os, |path| Some(path.to_owned()), |_| false),
            None
        );
    }
}

#[test]
fn python_helper_interpreter_uses_a_validated_canonical_symlink_target() {
    let canonical = Path::new("/usr/local/bin/python3.11");
    let selected = select_python3_interpreter(
        "freebsd",
        |candidate| {
            (candidate == Path::new("/usr/local/bin/python3")).then(|| canonical.to_owned())
        },
        |candidate| candidate == canonical,
    );
    assert_eq!(selected.as_deref(), Some(canonical));
}

#[test]
fn freebsd_selects_a_bounded_versioned_interpreter_without_an_alias() {
    let installed = Path::new("/usr/local/bin/python3.12");
    let selected = select_python3_interpreter(
        "freebsd",
        |candidate| (candidate == installed).then(|| candidate.to_owned()),
        |candidate| candidate == installed,
    );
    assert_eq!(selected.as_deref(), Some(installed));
}
