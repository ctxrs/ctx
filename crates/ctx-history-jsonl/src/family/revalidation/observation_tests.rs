use std::{
    cell::RefCell,
    fs::{self, File, FileTimes, OpenOptions},
    io::Write,
    path::PathBuf,
    time::{Duration, UNIX_EPOCH},
};

use super::*;
use crate::family::ProviderSourceRoot;
use ctx_history_source_io::SourceIoError;

thread_local! {
    static AFTER_OBSERVATION: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
}

pub(super) fn after_observation() {
    let hook = AFTER_OBSERVATION.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

fn set_after_observation(hook: impl FnOnce() + 'static) {
    AFTER_OBSERVATION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

type Observer = fn(
    &Path,
    &OpenedProviderSourceFile<SourceIoError>,
) -> JsonlResult<JsonlFileObservation, SourceIoError>;

fn observers() -> [Observer; 2] {
    [observe_opened_file, observe_opened_file_leaf]
}

// Authored filesystem cases, independent of any native provider format.
fn fixture() -> (
    tempfile::TempDir,
    PathBuf,
    OpenedProviderSourceFile<SourceIoError>,
) {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    fs::create_dir(&root).unwrap();
    let path = root.join("source.jsonl");
    fs::write(&path, b"original\n").unwrap();
    File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(1_700_000_000)))
        .unwrap();
    let authority = ProviderSourceRoot::open(&root).unwrap();
    let opened = authority.open_file(Path::new("source.jsonl")).unwrap();
    (temp, path, opened)
}

#[test]
fn unchanged_observation_preserves_exact_values_without_reading_source_bytes() {
    for observe in observers() {
        let (_temp, path, opened) = fixture();
        let original = fs::read(&path).unwrap();
        let first = observe(&path, &opened).unwrap();
        reset_jsonl_prefix_hash_bytes();
        let repeated = observe(&path, &opened).unwrap();
        assert_eq!(repeated, first);
        assert_eq!(first.length(), 9);
        assert_eq!(jsonl_prefix_hash_bytes(), 0);
        assert_eq!(fs::read(&path).unwrap(), original);
    }
}

#[cfg(any(unix, target_os = "windows"))]
#[test]
fn closing_observation_fence_rejects_same_size_rewrite_with_restored_mtime() {
    for observe in observers() {
        let (_temp, path, opened) = fixture();
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        let changed_path = path.clone();
        set_after_observation(move || {
            std::thread::sleep(Duration::from_millis(2));
            let mut file = OpenOptions::new().write(true).open(&changed_path).unwrap();
            file.write_all(b"rewritten").unwrap();
            file.set_times(FileTimes::new().set_modified(modified))
                .unwrap();
        });
        assert!(observe(&path, &opened).is_err());
        assert_eq!(fs::metadata(&path).unwrap().len(), 9);
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);
    }
}

#[test]
fn closing_observation_fence_rejects_named_replacement_and_deletion() {
    for observe in observers() {
        for replacement in [false, true] {
            let (_temp, path, opened) = fixture();
            let changed_path = path.clone();
            set_after_observation(move || {
                fs::rename(&changed_path, changed_path.with_extension("old")).unwrap();
                if replacement {
                    fs::write(&changed_path, b"original\n").unwrap();
                }
            });
            assert!(observe(&path, &opened).is_err());
        }
    }
}

#[test]
fn closing_observation_fence_rejects_replaced_root_with_original_leaf_retained() {
    for observe in observers() {
        let (_temp, path, opened) = fixture();
        let root = path.parent().unwrap().to_path_buf();
        set_after_observation(move || {
            fs::rename(&root, root.with_extension("old")).unwrap();
            fs::create_dir(&root).unwrap();
            fs::write(root.join("source.jsonl"), b"original\n").unwrap();
        });
        assert!(observe(&path, &opened).is_err());
        // The retained tree still names its original file. It is the named
        // root check, not leaf metadata equality, that must reject publication.
        opened.revalidate_leaf().unwrap();
    }
}

#[test]
fn leaf_observation_accepts_sibling_addition_but_exact_root_observation_rejects_it() {
    for (observe, allowed) in [
        (observe_opened_file as Observer, false),
        (observe_opened_file_leaf, true),
    ] {
        let (_temp, path, opened) = fixture();
        let sibling = path.with_file_name("sibling.jsonl");
        set_after_observation(move || {
            std::thread::sleep(Duration::from_millis(2));
            fs::write(sibling, b"sibling\n").unwrap();
        });
        assert_eq!(observe(&path, &opened).is_ok(), allowed);
    }
}

#[test]
fn exact_observation_rejects_append_while_append_observation_accepts_growth() {
    for observe in observers() {
        let (_temp, path, opened) = fixture();
        let growing_path = path.clone();
        set_after_observation(move || {
            OpenOptions::new()
                .append(true)
                .open(growing_path)
                .unwrap()
                .write_all(b"appended\n")
                .unwrap();
        });
        assert!(observe(&path, &opened).is_err());
        assert_eq!(
            observe_opened_file_allow_append(&path, &opened)
                .unwrap()
                .length(),
            18
        );
    }
}

#[cfg(unix)]
#[test]
fn closing_observation_fence_rejects_symlink_leaf_and_root_substitution() {
    use std::os::unix::fs::symlink;

    for observe in observers() {
        for swap_root in [false, true] {
            let (_temp, path, opened) = fixture();
            let swapped = if swap_root {
                path.parent().unwrap().to_path_buf()
            } else {
                path.clone()
            };
            set_after_observation(move || {
                let original = swapped.with_extension("old");
                fs::rename(&swapped, &original).unwrap();
                symlink(&original, &swapped).unwrap();
            });
            assert!(observe(&path, &opened).is_err());
        }
    }
}

#[cfg(unix)]
#[test]
fn ordinary_hardlink_is_admitted_and_same_size_alias_write_is_rejected() {
    for observe in observers() {
        let (_temp, path, _opened) = fixture();
        let alias = path.with_file_name("alias.jsonl");
        fs::hard_link(&path, &alias).unwrap();
        // Capture after link creation: hardlink metadata churn is a new
        // observation, not a reason to prohibit an ordinary hardlinked file.
        let authority = ProviderSourceRoot::<SourceIoError>::open(path.parent().unwrap()).unwrap();
        let opened = authority.open_file(Path::new("source.jsonl")).unwrap();
        observe(&path, &opened).unwrap();
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        set_after_observation(move || {
            std::thread::sleep(Duration::from_millis(2));
            let mut file = OpenOptions::new().write(true).open(alias).unwrap();
            file.write_all(b"rewritten").unwrap();
            file.set_times(FileTimes::new().set_modified(modified))
                .unwrap();
        });
        assert!(observe(&path, &opened).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"rewritten");
    }
}
