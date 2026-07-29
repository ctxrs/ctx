use std::fs;

use super::*;

fn write_files(root: &Path, names: &[&str]) {
    fs::create_dir_all(root).unwrap();
    for name in names {
        fs::write(root.join(name), b"retirement-test").unwrap();
    }
}

#[test]
fn no_op_and_repeat_are_idempotent() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("data");
    write_files(&root, &["config.toml"]);

    retire(&root).unwrap();
    retire(&root).unwrap();

    assert_eq!(
        fs::read(root.join("config.toml")).unwrap(),
        b"retirement-test"
    );
}

#[test]
fn exact_store_vector_stage_probe_and_worker_leaves_are_retired() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("data");
    let uuid = "00000000-0000-4000-8000-000000000001";
    let names = [
        "work.sqlite",
        "work.sqlite-wal",
        "work.sqlite-shm",
        "work.sqlite-journal",
        "work.sqlite.event-search-bulk.lock.sqlite",
        "work.sqlite.event-search-bulk.lock.sqlite-wal",
        "work.sqlite.source-inventory.lock.sqlite-shm",
        "work.sqlite.migration.lock.sqlite-journal",
        "work.sqlite.ctx-native-cold.lock",
        "work.sqlite.ctx-native-cold-00000000-0000-4000-8000-000000000001.sqlite",
        "work.sqlite.ctx-native-cold-00000000-0000-4000-8000-000000000001.sqlite-wal",
        "work.sqlite.ctx-native-cold-00000000-0000-4000-8000-000000000001.sqlite.source-inventory.lock.sqlite-journal",
        "work.sqlite.ctx-native-cold-probe-00000000-0000-4000-8000-000000000001.source",
        "work.sqlite.ctx-native-cold-probe-00000000-0000-4000-8000-000000000001.target",
        "vectors.sqlite",
        "vectors.sqlite-wal",
        "vectors.sqlite-shm",
        "vectors.sqlite-journal",
        "semantic-worker.lock",
        "semantic-worker.guard",
        "semantic-worker.json",
        "semantic-worker.json.42.tmp",
    ];
    write_files(&root, &names);
    assert!(canonical_uuid_v4(uuid));

    retire(&root).unwrap();

    for name in names {
        assert!(!root.join(name).exists(), "retained exact leaf {name}");
    }
}

#[test]
fn unproven_names_nonempty_directories_and_current_state_are_preserved() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("data");
    let names = [
        "work.sqlite-lock",
        "work.sqlite.lock",
        "work.sqlite.ctx-native-cold-00000000-0000-7000-8000-000000000001.sqlite",
        "work.sqlite.ctx-native-cold-00000000-0000-4000-8000-000000000001.sqlite-lock",
        "semantic-worker.json.pid.tmp",
        "config.toml",
        "usage.sqlite",
        "install.json",
    ];
    write_files(&root, &names);
    for directory in [
        "spool",
        "objects",
        "semantic-vectors",
        "work-record",
        "unknown",
        "search/semantic",
        "relational",
        "logs",
    ] {
        let directory = root.join(directory);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("current"), b"current").unwrap();
    }

    retire(&root).unwrap();

    for name in names {
        assert_eq!(fs::read(root.join(name)).unwrap(), b"retirement-test");
    }
    for directory in [
        "spool",
        "objects",
        "semantic-vectors",
        "work-record",
        "unknown",
        "search/semantic",
        "relational",
        "logs",
    ] {
        assert_eq!(
            fs::read(root.join(directory).join("current")).unwrap(),
            b"current"
        );
    }
}

#[cfg(unix)]
#[test]
fn unaccounted_hardlink_fails_before_any_leaf_is_deleted() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("data");
    write_files(&root, &["work.sqlite", "vectors.sqlite"]);
    fs::hard_link(root.join("vectors.sqlite"), root.join("unknown-link")).unwrap();

    let error = retire(&root).expect_err("hard-linked exact leaf must fail closed");

    assert!(format!("{error:#}").contains("hard-linked old Store file"));
    assert!(root.join("work.sqlite").is_file());
    assert!(root.join("vectors.sqlite").is_file());
    assert!(root.join("unknown-link").is_file());
}

#[cfg(unix)]
#[test]
fn intentional_probe_hardlinks_are_preserved_without_blocking_store_retirement() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("data");
    let source =
        root.join("work.sqlite.ctx-native-cold-probe-00000000-0000-4000-8000-000000000001.source");
    let target =
        root.join("work.sqlite.ctx-native-cold-probe-00000000-0000-4000-8000-000000000001.target");
    write_files(&root, &["work.sqlite"]);
    fs::write(&source, b"probe").unwrap();
    fs::hard_link(&source, &target).unwrap();

    retire(&root).unwrap();

    assert!(!root.join("work.sqlite").exists());
    assert_eq!(fs::read(source).unwrap(), b"probe");
    assert_eq!(fs::read(target).unwrap(), b"probe");
}

#[cfg(unix)]
#[test]
fn replacement_after_preflight_fails_before_partial_deletion() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("data");
    write_files(&root, &["work.sqlite", "vectors.sqlite"]);
    let moved = root.join("moved-vector");

    let error = retire_with(&root, || {
        fs::rename(root.join("vectors.sqlite"), &moved).unwrap();
        symlink(&moved, root.join("vectors.sqlite")).unwrap();
    })
    .expect_err("replacement must fail the complete revalidation");

    assert!(format!("{error:#}").contains("non-regular old Store file"));
    assert!(root.join("work.sqlite").is_file());
    assert_eq!(fs::read(moved).unwrap(), b"retirement-test");
}
