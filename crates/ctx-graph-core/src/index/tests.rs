use super::*;
use std::{cell::RefCell, fs, fs::OpenOptions};

thread_local! {
    static AFTER_PYTHON_INVENTORY: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
    static BEFORE_PUBLISH_VALIDATION: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
}

pub(super) fn after_python_inventory() {
    let hook = AFTER_PYTHON_INVENTORY.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

pub(super) fn before_publish_validation() {
    let hook = BEFORE_PUBLISH_VALIDATION.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

#[cfg(unix)]
#[test]
fn source_identity_reuse_waits_past_the_timestamp_collision_window() {
    let old = 1_000_000_000_000_u128;
    assert!(!source_identity_settled(
        old,
        old,
        old + SOURCE_IDENTITY_SETTLE_NANOS - 1
    ));
    assert!(source_identity_settled(
        old,
        old,
        old + SOURCE_IDENTITY_SETTLE_NANOS
    ));
    assert!(!source_identity_settled(old, old + 1, old));
    assert!(!source_identity_settled(old + 1, old, old));
}

#[test]
fn publish_scan_content_match_ignores_only_promoted_source_identities() {
    let source = |digest: &str, identity| SourceProof {
        path: "app.py".into(),
        digest: digest.into(),
        identity,
    };
    let proof = |source| ScanProof {
        supported_files: 1,
        unsupported_files: 0,
        ignore_fingerprint: "ignore".into(),
        sources: vec![source],
        managed_sources: vec![],
    };
    let initial = proof(source("same", None));
    let promoted = proof(source(
        "same",
        Some(ManifestSourceIdentity {
            length: 1,
            modified: "2".into(),
            file_id: "3".into(),
        }),
    ));
    let changed = proof(source("changed", None));
    assert!(scan_content_matches(&initial, &promoted));
    assert!(!scan_content_matches(&initial, &changed));
}

#[cfg(unix)]
#[test]
fn publish_allows_fresh_source_identity_to_settle_after_bytes_are_read() {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join(".graf/index.db");
    let source = root.path().join("app.py");
    fs::write(&source, "def first():\n    pass\n").unwrap();
    let initial = run(root.path(), &db).unwrap();

    fs::write(&source, "def second():\n    pass\n").unwrap();
    assert!(manifest_source_identity(&fs::metadata(&source).unwrap()).is_none());
    BEFORE_PUBLISH_VALIDATION.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(|| {
            std::thread::sleep(std::time::Duration::from_millis(2_100));
        }));
    });

    let updated = run(root.path(), &db).unwrap();
    assert_eq!(updated.parsed_files, 1);
    assert_eq!(updated.generation, initial.generation + 1);
    let snapshot = Store::open_read_only(&db).unwrap().snapshot().unwrap();
    assert!(snapshot.nodes.iter().any(|node| node.label == "second"));
    assert!(snapshot.nodes.iter().all(|node| node.label != "first"));
}

#[test]
fn python_inventory_restore_before_unchanged_check_preserves_generation() {
    for api_name in ["api.py", "api"] {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join(".graf/index.db");
        let api = root.path().join(api_name);
        let original = "#!/usr/bin/env python3\nfrom impl import first as entry\n";
        let temporary = "#!/usr/bin/env python3\nfrom impl import second as entry\n";
        fs::write(&api, original).unwrap();
        fs::write(
            root.path().join("impl.py"),
            "def first(): return 1\ndef second(): return 2\n",
        )
        .unwrap();
        fs::write(
            root.path().join("consumer.py"),
            "from api import entry\ndef run(): return entry()\n",
        )
        .unwrap();
        let initial = run(root.path(), &db).unwrap();
        let snapshot = || Store::open_read_only(&db).unwrap().snapshot().unwrap();
        let before = serde_json::to_value(snapshot()).unwrap();
        let target = |graph: &GraphSnapshot| {
            let call = graph
                .edges
                .iter()
                .find(|edge| edge.relation == "calls")
                .unwrap();
            graph
                .nodes
                .iter()
                .find(|node| node.id == call.target)
                .unwrap()
                .qualified_name
                .clone()
                .unwrap()
        };
        assert_eq!(target(&snapshot()), "first");

        fs::write(&api, temporary).unwrap();
        let restored = api.clone();
        AFTER_PYTHON_INVENTORY.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || fs::write(restored, original).unwrap()));
        });
        let error = run(root.path(), &db).unwrap_err();
        // Extensionless sources are also probed for a JavaScript shebang.
        // That earlier inventory guard detects this same byte mismatch first.
        let expected_error = if api_name.ends_with(".py") {
            "Python source changed during scan"
        } else {
            "source changed during JavaScript context discovery"
        };
        assert!(
            error.to_string().contains(expected_error),
            "{api_name}: {error}"
        );
        assert_eq!(snapshot().generation, initial.generation);
        assert_eq!(serde_json::to_value(snapshot()).unwrap(), before);
        assert_eq!(
            run(root.path(), &db).unwrap().generation,
            initial.generation
        );

        // The nearest ordinary case still updates the actual target.
        fs::write(&api, temporary).unwrap();
        run(root.path(), &db).unwrap();
        assert_eq!(target(&snapshot()), "second");
        assert_eq!(run(root.path(), &db).unwrap().parsed_files, 0);
    }
}

#[test]
fn cached_document_change_with_restored_metadata_aborts_before_publish() {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join(".graf/index.db");
    let source = root.path().join("notes.md");
    fs::write(&source, "alpha\n").unwrap();
    let initial = run(root.path(), &db).unwrap();
    let before =
        serde_json::to_value(Store::open_read_only(&db).unwrap().snapshot().unwrap()).unwrap();

    fs::write(&source, "bravo\n").unwrap();
    let metadata = fs::metadata(&source).unwrap();
    let times = std::fs::FileTimes::new()
        .set_accessed(metadata.accessed().unwrap())
        .set_modified(metadata.modified().unwrap());
    let changed = source.clone();
    BEFORE_PUBLISH_VALIDATION.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            fs::write(&changed, "cider\n").unwrap();
            OpenOptions::new()
                .write(true)
                .open(&changed)
                .unwrap()
                .set_times(times)
                .unwrap();
        }));
    });

    let error = run(root.path(), &db).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("source tree changed during indexing"),
        "{error:#}"
    );
    let after = Store::open_read_only(&db).unwrap().snapshot().unwrap();
    assert_eq!(after.generation, initial.generation);
    assert_eq!(serde_json::to_value(after).unwrap(), before);
}
