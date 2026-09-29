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

#[test]
fn rejected_document_replaces_stale_facts_and_retries_only_when_needed() {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join(".graf/index.db");
    let yaml = root.path().join("manifest.yaml");
    fs::write(&yaml, "kind: Original\n").unwrap();
    fs::write(root.path().join("good.txt"), "Good document").unwrap();
    let initial = run(root.path(), &db).unwrap();
    assert_eq!(initial.parsed_files, 2);
    fs::write(&yaml, "kind: Partial\n---\nkind: [broken\n").unwrap();
    fs::write(root.path().join("good.txt"), "Updated document").unwrap();
    let rejected = run(root.path(), &db).unwrap();
    assert_eq!(rejected.parsed_files, 1);
    assert_eq!(rejected.rejected_files, 1);
    assert_eq!(rejected.diagnostics.len(), 1);
    assert_eq!(rejected.diagnostics[0].file, "manifest.yaml");
    assert!(rejected.diagnostics[0].message.contains("document 2"));
    let snapshot = Store::open_read_only(&db).unwrap().snapshot().unwrap();
    assert!(
        snapshot
            .nodes
            .iter()
            .all(|node| node.file != "manifest.yaml")
    );
    assert!(snapshot.nodes.iter().any(|node| node.file == "good.txt"));
    for _ in 0..2 {
        let noop = run(root.path(), &db).unwrap();
        assert_eq!(noop.generation, rejected.generation);
        assert_eq!(
            (noop.parsed_files, noop.rejected_files, noop.unchanged_files),
            (0, 0, 2)
        );
        assert_eq!(noop.diagnostics[0].message, rejected.diagnostics[0].message);
    }
    let forced = run_with_options(
        root.path(),
        &db,
        &IndexOptions {
            force: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!((forced.parsed_files, forced.rejected_files), (1, 1));
    fs::write(&yaml, "kind: Repaired\n---\nkind: Second\n").unwrap();
    let repaired = run(root.path(), &db).unwrap();
    assert_eq!((repaired.parsed_files, repaired.rejected_files), (1, 0));
    assert!(repaired.diagnostics.is_empty());
    let snapshot = Store::open_read_only(&db).unwrap().snapshot().unwrap();
    assert_eq!(
        snapshot
            .nodes
            .iter()
            .filter(|node| node.kind == "document_field")
            .count(),
        2
    );
}

#[test]
fn invalid_text_encoding_is_a_file_diagnostic_and_explicit_add_keeps_capture() {
    let root = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let path = source.path().join("source.txt");
    let source_name = path.to_str().unwrap();
    let db = root.path().join(".graf/index.db");
    fs::write(&path, "Original capture").unwrap();
    let (_, initial) = crate::sources::add_and_index(
        root.path(),
        &db,
        source_name,
        None,
        &IndexOptions::default(),
        &ingest::CaptureMetadata::default(),
    )
    .unwrap();
    let cache = fs::read_dir(root.path().join(".graf/sources"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let previous = fs::read(&cache).unwrap();
    fs::write(&path, b"\xff").unwrap();
    let error = crate::sources::add_and_index(
        root.path(),
        &db,
        source_name,
        None,
        &IndexOptions::default(),
        &ingest::CaptureMetadata::default(),
    )
    .unwrap_err();
    assert_eq!(
        error.downcast_ref::<ingest::InputRejected>().unwrap().file,
        source_name
    );
    assert_eq!(fs::read(&cache).unwrap(), previous);
    assert_eq!(
        Store::open_read_only(&db)
            .unwrap()
            .stats()
            .unwrap()
            .generation,
        initial.generation
    );
    fs::write(root.path().join("bad.txt"), b"\xff").unwrap();
    fs::write(root.path().join("good.rs"), "fn ordinary() {}\n").unwrap();
    let report = run(root.path(), &db).unwrap();
    assert_eq!(report.rejected_files, 1);
    assert_eq!(report.diagnostics[0].file, "bad.txt");
    assert_eq!(report.parsed_files, 1);
}

#[test]
fn rejected_local_input_does_not_trigger_semantic_shrink_but_successful_loss_does() {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join(".graf/index.db");
    let yaml = root.path().join("manifest.yaml");
    fs::write(&yaml, "kind: Original\n").unwrap();
    run(root.path(), &db).unwrap();
    let mut facts = ingest::extract_text(
        "manifest.yaml",
        "kind: Original\n",
        "old",
        &IngestOptions::default(),
    )
    .unwrap();
    facts.nodes[1].metadata = serde_json::json!({"inferred":true,"provenance":"semantic"});
    let mut store = Store::open(&db).unwrap();
    let initial = store
        .apply_native(
            root.path().to_str().unwrap(),
            vec![facts],
            vec![],
            Coverage {
                supported_files: 1,
                ..Default::default()
            },
        )
        .unwrap();
    drop(store);
    fs::write(&yaml, "kind: Fewer\n").unwrap();
    let error = run(root.path(), &db).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("semantic extraction would remove")
    );
    assert_eq!(
        Store::open_read_only(&db)
            .unwrap()
            .stats()
            .unwrap()
            .generation,
        initial.generation
    );
    fs::write(&yaml, "kind: [broken\n").unwrap();
    let rejected = run(root.path(), &db).unwrap();
    assert_eq!(rejected.rejected_files, 1);
    assert_eq!(rejected.nodes, 0);
    assert_eq!(rejected.diagnostics.len(), 1);
}

#[cfg(unix)]
#[test]
fn converter_and_provider_failures_preserve_the_previous_graph() {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join(".graf/index.db");
    fs::write(root.path().join("source.txt"), "Original").unwrap();
    let initial = run(root.path(), &db).unwrap();
    let before =
        serde_json::to_value(Store::open_read_only(&db).unwrap().snapshot().unwrap()).unwrap();
    fs::write(root.path().join("source.txt"), "Replacement").unwrap();
    // A bad local file may be diagnosed first, but cannot make later operational errors publish.
    fs::write(root.path().join("a.yaml"), "key: [broken\n").unwrap();
    for provider in [false, true] {
        let adapter = ingest::CommandAdapter {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "exit 1".into()],
            output_file: false,
        };
        let mut options = IndexOptions::default();
        if provider {
            options.ingest.semantic = Some(ingest::SemanticOptions {
                provider: ingest::Provider::Cli,
                command: Some(adapter),
                ..Default::default()
            });
        } else {
            options.ingest.converters.insert("txt".into(), adapter);
        }
        let error = run_with_options(root.path(), &db, &options).unwrap_err();
        assert!(!error.is::<ingest::InputRejected>());
        let store = Store::open_read_only(&db).unwrap();
        assert_eq!(store.stats().unwrap().generation, initial.generation);
        assert_eq!(
            serde_json::to_value(store.snapshot().unwrap()).unwrap(),
            before
        );
    }
}

#[test]
fn previous_document_extractor_revision_is_retried_without_file_edits() {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join(".graf/index.db");
    fs::write(root.path().join("manifest.yaml"), "kind: [broken\n").unwrap();
    run(root.path(), &db).unwrap();
    let mut store = Store::open(&db).unwrap();
    let stamp = store.file_stamps().unwrap().pop().unwrap();
    let old_hash = stamp.hash.replace("ingest-v7:", "ingest-v6:");
    assert_ne!(old_hash, stamp.hash);
    store
        .apply_native(
            root.path().to_str().unwrap(),
            vec![diagnostic(
                "manifest.yaml",
                &old_hash,
                "previous extractor diagnostic",
            )],
            vec![],
            Coverage {
                supported_files: 1,
                ..Default::default()
            },
        )
        .unwrap();
    drop(store);
    let report = run(root.path(), &db).unwrap();
    assert_eq!((report.rejected_files, report.unchanged_files), (1, 0));
    assert!(report.diagnostics[0].message.contains("invalid YAML"));
}

#[test]
fn native_yaml_resource_failures_preserve_prior_semantic_graph() {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join(".graf/index.db");
    let yaml = root.path().join("manifest.yaml");
    fs::write(&yaml, "kind: Original\n").unwrap();
    run(root.path(), &db).unwrap();
    let mut facts = ingest::extract_text(
        "manifest.yaml",
        "kind: Original\n",
        "old",
        &IngestOptions::default(),
    )
    .unwrap();
    facts.nodes[1].metadata = serde_json::json!({"inferred":true,"provenance":"semantic"});
    let mut store = Store::open(&db).unwrap();
    store
        .apply_native(
            root.path().to_str().unwrap(),
            vec![facts],
            vec![],
            Coverage {
                supported_files: 1,
                ..Default::default()
            },
        )
        .unwrap();
    let before = serde_json::to_value(store.snapshot().unwrap()).unwrap();
    drop(store);
    let deep = format!("{}value{}", "[".repeat(129), "]".repeat(129));
    let mut aliases = String::from("a: &a [value]\n");
    for (prior, next) in [('a', 'b'), ('b', 'c'), ('c', 'd'), ('d', 'e'), ('e', 'f')] {
        aliases.push_str(&format!(
            "{next}: &{next} [{}]\n",
            vec![format!("*{prior}"); 10].join(",")
        ));
    }
    for (input, native_error) in [(deep, "recursion limit"), (aliases, "repetition limit")] {
        fs::write(&yaml, input).unwrap();
        let error = run(root.path(), &db).unwrap_err();
        assert!(!error.is::<ingest::InputRejected>());
        assert!(format!("{error:#}").contains(native_error), "{error:#}");
        assert_eq!(
            serde_json::to_value(Store::open_read_only(&db).unwrap().snapshot().unwrap()).unwrap(),
            before
        );
    }
}

#[test]
fn malformed_frontmatter_and_pointer_json_remove_only_their_stale_facts() {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join(".graf/index.db");
    fs::write(
        root.path().join("note.md"),
        "---\ntitle: Original\n---\n# Original\n",
    )
    .unwrap();
    fs::write(
        root.path().join("pointer.gdoc"),
        r#"{"doc_id":"example-id"}"#,
    )
    .unwrap();
    fs::write(root.path().join("good.txt"), "Original text").unwrap();
    run(root.path(), &db).unwrap();
    fs::write(
        root.path().join("note.md"),
        "---\ntitle: [broken\n---\n# Partial\n",
    )
    .unwrap();
    fs::write(root.path().join("pointer.gdoc"), r#"{"doc_id": }"#).unwrap();
    fs::write(root.path().join("good.txt"), "Updated text").unwrap();
    let report = run(root.path(), &db).unwrap();
    assert_eq!((report.parsed_files, report.rejected_files), (1, 2));
    assert_eq!(report.diagnostics.len(), 2);
    let snapshot = Store::open_read_only(&db).unwrap().snapshot().unwrap();
    assert!(snapshot.nodes.iter().all(|node| node.file == "good.txt"));
    let noop = run(root.path(), &db).unwrap();
    assert_eq!(noop.generation, report.generation);
    assert_eq!(
        serde_json::to_value(noop.diagnostics).unwrap(),
        serde_json::to_value(report.diagnostics).unwrap()
    );
    fs::write(
        root.path().join("note.md"),
        "---\ntitle: Repaired\n---\n# Repaired\n",
    )
    .unwrap();
    fs::write(
        root.path().join("pointer.gdoc"),
        r#"{"doc_id":"repaired-id"}"#,
    )
    .unwrap();
    let report = run(root.path(), &db).unwrap();
    assert_eq!((report.parsed_files, report.rejected_files), (2, 0));
    assert!(report.diagnostics.is_empty());
}

#[test]
fn yaml_reference_document_identity_survives_storage_and_rebinding() {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join(".graf/index.db");
    fs::write(
        root.path().join("manifest.yaml"),
        "note: '[[one]]'\n---\nnote: '[[two]]'\n",
    )
    .unwrap();
    fs::write(root.path().join("one.md"), "# One\n").unwrap();
    fs::write(root.path().join("two.md"), "# Two\n").unwrap();
    run(root.path(), &db).unwrap();
    let snapshot = Store::open_read_only(&db).unwrap().snapshot().unwrap();
    let check_edges = |snapshot: &GraphSnapshot| {
        let edges: Vec<_> = snapshot
            .edges
            .iter()
            .filter(|edge| {
                edge.relation == "references" && edge.file.as_deref() == Some("manifest.yaml")
            })
            .collect();
        assert_eq!(edges.len(), 2);
        for (document, target_file) in [(1, "one.md"), (2, "two.md")] {
            let edge = edges
                .iter()
                .find(|edge| edge.metadata["yaml_document"] == document)
                .unwrap();
            let source = snapshot
                .nodes
                .iter()
                .find(|node| node.id == edge.source)
                .unwrap();
            let target = snapshot
                .nodes
                .iter()
                .find(|node| node.id == edge.target)
                .unwrap();
            assert_eq!(source.kind, "document_field");
            assert_eq!(source.metadata["yaml_document"], document);
            assert_eq!(target.file, target_file);
            assert!(edge.line.is_none());
        }
    };
    check_edges(&snapshot);
    let second = snapshot
        .nodes
        .iter()
        .find(|node| node.kind == "document_field" && node.metadata["yaml_document"] == 2)
        .unwrap()
        .id
        .clone();
    fs::remove_file(root.path().join("two.md")).unwrap();
    run(root.path(), &db).unwrap();
    let unresolved = Store::open_read_only(&db).unwrap().snapshot().unwrap();
    assert_eq!(
        unresolved.metadata["graf_unresolved_references"][0]["source"],
        second
    );
    fs::write(root.path().join("two.md"), "# Rebound\n").unwrap();
    let report = run(root.path(), &db).unwrap();
    assert_eq!(report.parsed_files, 1);
    let rebound = Store::open_read_only(&db).unwrap().snapshot().unwrap();
    check_edges(&rebound);
}

#[test]
fn docx_generated_frontmatter_failure_keeps_previous_graph() {
    // Authored minimal OOXML packages: a normal paragraph, and valid Word text
    // whose generated Markdown happens to resemble malformed YAML frontmatter.
    let ordinary = include_bytes!("fixtures/ordinary.docx");
    let frontmatter = include_bytes!("fixtures/frontmatter-text.docx");
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join(".graf/index.db");
    let source = root.path().join("document.docx");
    fs::write(&source, ordinary).unwrap();
    let initial = run(root.path(), &db).unwrap();
    assert_eq!(initial.parsed_files, 1);
    let mut facts = ingest::extract_bytes(
        &source,
        "document.docx",
        ordinary,
        "previous",
        &IngestOptions::default(),
    )
    .unwrap();
    assert_eq!(facts.nodes[0].metadata["converter"], "zip/quick-xml");
    assert!(
        facts.nodes[0].metadata["text"]
            .as_str()
            .unwrap()
            .contains("Ordinary document")
    );
    facts.nodes[0].metadata["inferred"] = serde_json::json!(true);
    facts.nodes[0].metadata["provenance"] = serde_json::json!("semantic");
    let mut store = Store::open(&db).unwrap();
    store
        .apply_native(
            root.path().to_str().unwrap(),
            vec![facts],
            vec![],
            Coverage {
                supported_files: 1,
                ..Default::default()
            },
        )
        .unwrap();
    let before = serde_json::to_value(store.snapshot().unwrap()).unwrap();
    drop(store);
    fs::write(&source, frontmatter).unwrap();
    // Also prepare a normal update: conversion failure must prevent all publication.
    fs::write(root.path().join("another.txt"), "New document").unwrap();
    let error = run(root.path(), &db).unwrap_err();
    assert!(!error.is::<ingest::InputRejected>(), "{error:#}");
    assert!(format!("{error:#}").contains("frontmatter"), "{error:#}");
    assert_eq!(
        serde_json::to_value(Store::open_read_only(&db).unwrap().snapshot().unwrap()).unwrap(),
        before
    );
}
