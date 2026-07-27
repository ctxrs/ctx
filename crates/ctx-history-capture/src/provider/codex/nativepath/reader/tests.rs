#[cfg(unix)]
use super::scanner::open_certified_codex_source_with_hooks;
use super::*;

use std::fs::{self, FileTimes};

use tempfile::tempdir;

fn catalog_observation(path: &Path) -> CodexFileObservation {
    let observation = observe_ordinary_file(path).unwrap();
    CodexFileObservation::from_parts(
        observation.len(),
        observation.modified_at(),
        *observation.token(),
    )
}

fn write_replacement_with_modified_time(
    path: &Path,
    contents: &[u8],
    modified_at: std::time::SystemTime,
) {
    fs::write(path, contents).unwrap();
    let file = File::options().write(true).open(path).unwrap();
    file.set_times(FileTimes::new().set_modified(modified_at))
        .unwrap();
}

#[cfg(any(unix, target_os = "windows"))]
#[test]
fn exact_open_handle_observation_matches_catalog_observation() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("rollout.jsonl");
    fs::write(&path, b"stable source\n").unwrap();
    let expected = catalog_observation(&path);
    let file = open_ordinary_file_without_following(&path).unwrap();

    assert_eq!(opened_file_observation(&path, &file).unwrap(), expected);
    validate_open_file_metadata(&path, &file, &expected).unwrap();
}

#[cfg(any(unix, target_os = "windows"))]
#[test]
fn exact_open_handle_rejects_same_length_and_mtime_replacement() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("rollout.jsonl");
    let original = temp.path().join("original.jsonl");
    let replacement_path = temp.path().join("replacement.jsonl");
    fs::write(&path, b"original\n").unwrap();
    let expected = catalog_observation(&path);
    let modified_at = fs::metadata(&path).unwrap().modified().unwrap();

    write_replacement_with_modified_time(&replacement_path, b"replaced\n", modified_at);
    fs::rename(&path, original).unwrap();
    fs::rename(replacement_path, &path).unwrap();
    let replacement = open_ordinary_file_without_following(&path).unwrap();
    let replacement_metadata = replacement.metadata().unwrap();
    assert_eq!(replacement_metadata.len(), expected.len);
    assert_eq!(
        CodexFileObservation::from_parts(
            replacement_metadata.len(),
            replacement_metadata.modified().unwrap(),
            expected.change_token,
        )
        .modified_at_ms,
        expected.modified_at_ms
    );

    assert!(validate_open_file_metadata(&path, &replacement, &expected).is_err());
}

#[cfg(unix)]
#[test]
fn scanner_open_rejects_hostile_swap_even_after_original_path_is_restored() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("rollout.jsonl");
    let original = temp.path().join("original.jsonl");
    let replacement = temp.path().join("replacement.jsonl");
    fs::write(&path, b"original\n").unwrap();
    let expected = catalog_observation(&path);
    let modified_at = fs::metadata(&path).unwrap().modified().unwrap();

    let result = open_certified_codex_source_with_hooks(
        &path,
        &expected,
        || {
            fs::rename(&path, &original).unwrap();
            write_replacement_with_modified_time(&path, b"replaced\n", modified_at);
        },
        || {
            fs::rename(&path, &replacement).unwrap();
            fs::rename(&original, &path).unwrap();
        },
    );

    assert!(result.is_err());
    assert_eq!(fs::read(&path).unwrap(), b"original\n");
    assert_eq!(fs::read(&replacement).unwrap(), b"replaced\n");
}
