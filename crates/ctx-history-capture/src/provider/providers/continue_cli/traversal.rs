use std::{ffi::OsStr, fs, path::Path};

use crate::common::io::{
    ensure_provider_path_parents_are_not_symlinks, ensure_regular_provider_transcript_file,
};
use crate::{CaptureError, Result};

use super::continue_session_json_path;

const CONTINUE_MAX_DIRECTORY_DEPTH: usize = 128;
// Match the direct provider-directory discovery ceiling. Together with the
// depth limit, this bounds entries retained by recursive deterministic sorting.
const CONTINUE_MAX_DIRECTORY_ENTRIES: usize = 1_024;
// Match the structured-content file ceiling and count every scanned entry so
// irrelevant names cannot make recursive traversal work or retention unbounded.
const CONTINUE_MAX_TRAVERSAL_ENTRIES: usize = 4_096;

pub(super) fn visit_continue_session_files(
    root: &Path,
    visit: &mut dyn FnMut(&Path) -> Result<()>,
) -> Result<usize> {
    let mut scanned_entries = 0_usize;
    visit_continue_session_files_at_depth(root, visit, 0, &mut scanned_entries)
}

fn visit_continue_session_files_at_depth(
    root: &Path,
    visit: &mut dyn FnMut(&Path) -> Result<()>,
    depth: usize,
    scanned_entries: &mut usize,
) -> Result<usize> {
    if depth > CONTINUE_MAX_DIRECTORY_DEPTH {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: root.to_path_buf(),
            reason: "Continue CLI session directory nesting exceeds the supported limit",
        });
    }
    let metadata = fs::symlink_metadata(root)?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: root.to_path_buf(),
            reason: "symlinked provider transcript roots are rejected",
        });
    }
    ensure_provider_path_parents_are_not_symlinks(root)?;
    if file_type.is_file() {
        if continue_session_json_path(root) {
            ensure_regular_provider_transcript_file(root)?;
            visit(root)?;
            return Ok(1);
        }
        return Ok(0);
    }
    if !file_type.is_dir() {
        return Ok(0);
    }

    let mut entries = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if *scanned_entries >= CONTINUE_MAX_TRAVERSAL_ENTRIES {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: root.to_path_buf(),
                reason: "Continue CLI session tree exceeds the supported entry limit",
            });
        }
        *scanned_entries += 1;
        if entries.len() >= CONTINUE_MAX_DIRECTORY_ENTRIES {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: root.to_path_buf(),
                reason: "Continue CLI session directory exceeds the supported entry limit",
            });
        }
        entries.push(entry);
    }
    entries.sort_by_cached_key(|entry| continue_filename_order_key(&entry.file_name()));

    let mut visited = 0_usize;
    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            visited = visited.saturating_add(visit_continue_session_files_at_depth(
                &path,
                visit,
                depth.saturating_add(1),
                scanned_entries,
            )?);
        } else if file_type.is_file() && continue_session_json_path(&path) {
            ensure_regular_provider_transcript_file(&path)?;
            visit(&path)?;
            visited = visited.saturating_add(1);
        }
    }
    Ok(visited)
}

fn continue_filename_order_key(name: &OsStr) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        name.as_bytes().to_vec()
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        name.encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>()
    }
    #[cfg(not(any(unix, windows)))]
    {
        name.as_encoded_bytes().to_vec()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    use serde_json::json;

    use crate::test_support_paths::tempdir;

    use super::*;

    fn write_session(path: &Path, session_id: &str, text: &str) {
        fs::write(
            path,
            serde_json::to_vec(&json!({
                "sessionId": session_id,
                "history": [{
                    "message": {
                        "role": "user",
                        "content": text,
                    }
                }]
            }))
            .unwrap(),
        )
        .unwrap();
    }

    fn create_nested_directories(root: &Path, depth: usize) -> PathBuf {
        let mut nested = root.to_path_buf();
        for _ in 0..depth {
            nested = nested.join("d");
            fs::create_dir(&nested).unwrap();
        }
        nested
    }

    #[test]
    fn traversal_visits_session_files_in_deterministic_depth_first_order() {
        let temp = tempdir().unwrap();
        let first_nested = temp.path().join("a-nested");
        let last_nested = temp.path().join("z-nested");
        fs::create_dir(&last_nested).unwrap();
        fs::create_dir(&first_nested).unwrap();
        write_session(&last_nested.join("a.json"), "last", "last");
        write_session(&temp.path().join("m.json"), "middle", "middle");
        write_session(&first_nested.join("z.json"), "first", "first");
        fs::write(temp.path().join("sessions.json"), b"[]").unwrap();

        let mut visited = Vec::new();
        let count = visit_continue_session_files(temp.path(), &mut |path| {
            visited.push(path.to_path_buf());
            Ok(())
        })
        .unwrap();

        assert_eq!(count, 3);
        assert_eq!(
            visited,
            vec![
                first_nested.join("z.json"),
                temp.path().join("m.json"),
                last_nested.join("a.json"),
            ]
        );
    }

    #[test]
    fn traversal_accepts_session_at_maximum_directory_depth() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("sessions");
        fs::create_dir(&root).unwrap();
        let deepest = create_nested_directories(&root, CONTINUE_MAX_DIRECTORY_DEPTH);
        let session = deepest.join("boundary.json");
        write_session(&session, "boundary", "accepted");

        let mut visited = Vec::new();
        let count = visit_continue_session_files(&root, &mut |path| {
            visited.push(path.to_path_buf());
            Ok(())
        })
        .unwrap();

        assert_eq!(count, 1);
        assert_eq!(visited, vec![session]);
    }

    #[test]
    fn traversal_rejects_directory_beyond_maximum_depth() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("sessions");
        fs::create_dir(&root).unwrap();
        let over_limit =
            create_nested_directories(&root, CONTINUE_MAX_DIRECTORY_DEPTH.saturating_add(1));
        write_session(&over_limit.join("too-deep.json"), "too-deep", "rejected");

        let mut visited = Vec::new();
        let error = visit_continue_session_files(&root, &mut |path| {
            visited.push(path.to_path_buf());
            Ok(())
        })
        .unwrap_err();

        assert!(matches!(
            error,
            CaptureError::InvalidProviderTranscriptPath { path, reason }
                if path == over_limit
                    && reason
                        == "Continue CLI session directory nesting exceeds the supported limit"
        ));
        assert!(visited.is_empty());
    }

    #[test]
    fn traversal_accepts_directory_at_entry_limit() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("sessions");
        fs::create_dir(&root).unwrap();
        let mut expected = Vec::with_capacity(CONTINUE_MAX_DIRECTORY_ENTRIES);
        for index in (0..CONTINUE_MAX_DIRECTORY_ENTRIES).rev() {
            let path = root.join(format!("session-{index:04}.json"));
            write_session(&path, &format!("session-{index}"), "accepted");
            expected.push(path);
        }
        expected.sort();

        let mut visited = Vec::new();
        let count = visit_continue_session_files(&root, &mut |path| {
            visited.push(path.to_path_buf());
            Ok(())
        })
        .unwrap();

        assert_eq!(count, CONTINUE_MAX_DIRECTORY_ENTRIES);
        assert_eq!(visited, expected);
    }

    #[test]
    fn traversal_rejects_directory_over_entry_limit_before_visiting() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("sessions");
        fs::create_dir(&root).unwrap();
        for index in 0..=CONTINUE_MAX_DIRECTORY_ENTRIES {
            fs::write(root.join(format!("entry-{index:04}")), b"").unwrap();
        }

        let mut visited = Vec::new();
        let error = visit_continue_session_files(&root, &mut |path| {
            visited.push(path.to_path_buf());
            Ok(())
        })
        .unwrap_err();

        assert!(matches!(
            error,
            CaptureError::InvalidProviderTranscriptPath { path, reason }
                if path == root
                    && reason
                        == "Continue CLI session directory exceeds the supported entry limit"
        ));
        assert!(visited.is_empty());
    }

    #[test]
    fn global_entry_budget_accepts_boundary_and_rejects_broad_tree_before_callback() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("sessions");
        fs::create_dir(&root).unwrap();
        let mut directories = Vec::new();
        for name in ["a", "b", "c", "z"] {
            let directory = root.join(name);
            fs::create_dir(&directory).unwrap();
            directories.push(directory);
        }
        for directory in &directories[..3] {
            for index in 0..1_023 {
                fs::write(directory.join(format!("clutter-{index:04}")), b"").unwrap();
            }
        }
        for index in 0..1_022 {
            fs::write(directories[3].join(format!("clutter-{index:04}")), b"").unwrap();
        }
        let session = directories[3].join("z-session.json");
        write_session(&session, "global-boundary", "accepted");

        let mut visited = Vec::new();
        let count = visit_continue_session_files(&root, &mut |path| {
            visited.push(path.to_path_buf());
            Ok(())
        })
        .unwrap();
        assert_eq!(count, 1);
        assert_eq!(visited, vec![session]);

        fs::write(directories[0].join("extra-clutter"), b"").unwrap();
        visited.clear();
        let error = visit_continue_session_files(&root, &mut |path| {
            visited.push(path.to_path_buf());
            Ok(())
        })
        .unwrap_err();

        assert!(matches!(
            error,
            CaptureError::InvalidProviderTranscriptPath { path, reason }
                if path == directories[3]
                    && reason == "Continue CLI session tree exceeds the supported entry limit"
        ));
        assert!(visited.is_empty());
    }

    #[test]
    fn over_limit_error_is_not_replaced_by_an_earlier_positive_count() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("sessions");
        fs::create_dir(&root).unwrap();
        let accepted = root.join("a.json");
        write_session(&accepted, "accepted", "visited first");
        let over_limit_root = root.join("z-over-limit");
        fs::create_dir(&over_limit_root).unwrap();
        create_nested_directories(&over_limit_root, CONTINUE_MAX_DIRECTORY_DEPTH);

        let mut visited = Vec::new();
        let result = visit_continue_session_files(&root, &mut |path| {
            visited.push(path.to_path_buf());
            Ok(())
        });

        assert!(matches!(
            result,
            Err(CaptureError::InvalidProviderTranscriptPath {
                reason: "Continue CLI session directory nesting exceeds the supported limit",
                ..
            })
        ));
        assert_eq!(visited, vec![accepted]);
    }

    #[cfg(unix)]
    #[test]
    fn traversal_does_not_follow_adversarial_symlink_entries() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().unwrap();
        let root = temp.path().join("sessions");
        let outside = temp.path().join("outside");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&outside).unwrap();
        let accepted = root.join("accepted.json");
        let outside_session = outside.join("outside.json");
        write_session(&accepted, "accepted", "inside");
        write_session(&outside_session, "outside", "must not visit");
        symlink(&outside, root.join("linked-directory")).unwrap();
        symlink(&outside_session, root.join("linked-session.json")).unwrap();

        let mut visited = Vec::new();
        let count = visit_continue_session_files(&root, &mut |path| {
            visited.push(path.to_path_buf());
            Ok(())
        })
        .unwrap();

        assert_eq!(count, 1);
        assert_eq!(visited, vec![accepted]);
    }
}
