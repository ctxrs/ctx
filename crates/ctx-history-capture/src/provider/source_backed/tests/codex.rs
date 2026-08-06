use std::{fs::OpenOptions, io::Write};

use ctx_history_core::SessionRelationshipKind;

use super::*;
use crate::provider::codex::nativepath::install_after_codex_lineage_normalization_hook_v0;

fn prompt_line(session_id: &str, ts: i64, text: &str) -> Vec<u8> {
    let mut line = serde_json::to_vec(&serde_json::json!({
        "session_id": session_id,
        "ts": ts,
        "text": text,
    }))
    .unwrap();
    line.push(b'\n');
    line
}

fn core_records(index: &VerifiedIndex) -> Vec<CoreRecord> {
    let mut records = Vec::new();
    for source in &index.manifest().sources {
        let source_key = source.observation().source();
        let page = index.source_event_page(source_key, None, 256).unwrap();
        assert!(page.next_cursor.is_none());
        for item in page.items {
            records.push(
                index
                    .core_record_by_id(item.event_id.as_uuid())
                    .unwrap()
                    .unwrap(),
            );
        }
    }
    records.sort_by_key(|record| {
        (
            record.source.source_format().to_owned(),
            record.event_sequence,
        )
    });
    records
}

fn codex_lineage_rollout(
    native_session_id: &str,
    parent_native_session_id: Option<&str>,
    relationship: SessionRelationshipKind,
    advisory_session_id: Option<&str>,
    marker: &str,
) -> Vec<u8> {
    let source = match (relationship, parent_native_session_id) {
        (SessionRelationshipKind::Delegated, Some(parent)) => serde_json::json!({
            "subagent": {"thread_spawn": {"parent_thread_id": parent}}
        }),
        _ => serde_json::json!("cli"),
    };
    let mut payload = serde_json::json!({
        "id": native_session_id,
        "timestamp": "2026-08-06T12:00:00Z",
        "cwd": "/tmp/root-normalization",
        "source": source,
        "model_provider": "openai"
    });
    if let Some(parent) = parent_native_session_id {
        match relationship {
            SessionRelationshipKind::Delegated => {
                payload["parent_thread_id"] = serde_json::json!(parent);
            }
            SessionRelationshipKind::Forked => {
                payload["forked_from_id"] = serde_json::json!(parent);
            }
            SessionRelationshipKind::ResumedFrom => {
                payload["history_base"] = serde_json::json!({
                    "thread_id": parent,
                    "end_ordinal_exclusive": 7,
                    "end_byte_offset": 4096
                });
            }
            relationship => panic!("unsupported Codex fixture relationship: {relationship:?}"),
        }
    }
    if let Some(advisory) = advisory_session_id {
        payload["session_id"] = serde_json::json!(advisory);
    }
    [
        serde_json::json!({
            "timestamp": "2026-08-06T12:00:00Z",
            "type": "session_meta",
            "payload": payload,
        }),
        serde_json::json!({
            "timestamp": "2026-08-06T12:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": marker}]
            }
        }),
    ]
    .into_iter()
    .flat_map(|record| {
        let mut line = serde_json::to_vec(&record).unwrap();
        line.push(b'\n');
        line
    })
    .collect()
}

fn register_codex_tree(sessions: &Path) -> SourceBackedProviderRegistry {
    register_codex_trees(&[(sessions, ProviderImportSupport::Native)])
}

fn register_codex_trees(roots: &[(&Path, ProviderImportSupport)]) -> SourceBackedProviderRegistry {
    let mut registry = SourceBackedProviderRegistry::new();
    super::super::register_codex_session_tree_routes(
        &mut registry,
        roots
            .iter()
            .map(|(root, support)| {
                fixture_provider_source_at(
                    CaptureProvider::Codex,
                    "codex_session_jsonl_tree",
                    *support,
                    root,
                )
            })
            .collect(),
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();
    registry
}

#[test]
fn codex_transitive_root_normalization_quarantines_before_workers() {
    let temp = tempdir().unwrap();
    let automatic = temp.path().join("automatic-sessions");
    let explicit = temp.path().join("explicit-sessions");
    fs::create_dir_all(&automatic).unwrap();
    fs::create_dir_all(&explicit).unwrap();
    let root = "019fa000-0000-7000-8000-000000003280";
    let fork = "019fa000-0000-7000-8000-000000003281";
    let delegated = "019fa000-0000-7000-8000-000000003282";
    let resumed = "019fa000-0000-7000-8000-000000003287";
    let invalid = "019fa000-0000-7000-8000-000000003283";
    let invalid_child = "019fa000-0000-7000-8000-000000003284";
    let absent = "019fa000-0000-7000-8000-000000003289";
    for (directory, id, parent, relationship, advisory, marker) in [
        (
            &automatic,
            root,
            None,
            SessionRelationshipKind::Root,
            None,
            "normalized root",
        ),
        (
            &explicit,
            fork,
            Some(root),
            SessionRelationshipKind::Forked,
            Some(fork),
            "normalized fork",
        ),
        (
            &automatic,
            delegated,
            Some(fork),
            SessionRelationshipKind::Delegated,
            Some(fork),
            "normalized delegated",
        ),
        (
            &explicit,
            resumed,
            Some(delegated),
            SessionRelationshipKind::ResumedFrom,
            Some(root),
            "normalized resumed",
        ),
        (
            &explicit,
            invalid,
            Some(absent),
            SessionRelationshipKind::Forked,
            Some(absent),
            "rejected missing",
        ),
        (
            &automatic,
            invalid_child,
            Some(invalid),
            SessionRelationshipKind::Delegated,
            Some(invalid),
            "rejected descendant",
        ),
    ] {
        fs::write(
            directory.join(format!("rollout-{id}.jsonl")),
            codex_lineage_rollout(id, parent, relationship, advisory, marker),
        )
        .unwrap();
    }
    let observed = Arc::new(Mutex::new(None));
    let observed_from_hook = Arc::clone(&observed);
    install_after_codex_lineage_normalization_hook_v0(move |observation| {
        *observed_from_hook.lock().unwrap() = Some(observation);
    });
    let staged = Arc::new(Mutex::new(None));
    let staged_from_hook = Arc::clone(&staged);
    super::super::set_after_codex_session_tree_stage_hook(move |counters| {
        *staged_from_hook.lock().unwrap() = Some(counters);
    });
    let registry = register_codex_trees(&[
        (&automatic, ProviderImportSupport::Native),
        (&explicit, ProviderImportSupport::Explicit),
    ]);
    let index_path = temp.path().join("index");
    let refreshed =
        refresh_source_backed_generation(&index_path, &registry, WriterOptions::default()).unwrap();
    let observation = observed.lock().unwrap().unwrap();
    assert_eq!(observation.valid_sources, 4);
    assert_eq!(observation.rejected_sources, 2);
    assert_eq!(observation.pre_worker_counters.scanner_sources_started, 0);
    assert_eq!(observation.pre_worker_counters.staged_documents, 0);
    let staged = staged.lock().unwrap().unwrap();
    assert_eq!(staged.scanner_sources_started, 4);
    assert_eq!(staged.scanner_sources_completed, 4);
    assert_eq!(staged.staged_documents, 4);
    assert_eq!(refreshed.commit.indexed_documents, 4);
    assert_eq!(refreshed.certified_source_count, 4);
    let records = core_records(&VerifiedIndex::open(&index_path).unwrap());
    assert_eq!(records.len(), 4);
    let canonical_root = records[0].root_session_id;
    assert!(records
        .iter()
        .all(|record| record.root_session_id == canonical_root));
    assert!(records.iter().all(|record| !record
        .content
        .normalized_body
        .as_deref()
        .is_some_and(|body| body.starts_with("rejected"))));

    let cold_ids = records
        .iter()
        .map(|record| {
            (
                record.content.normalized_body.clone().unwrap(),
                record.event_id,
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    refresh_source_backed_generation(&index_path, &registry, WriterOptions::default()).unwrap();
    let warm_records = core_records(&VerifiedIndex::open(&index_path).unwrap());
    assert_eq!(warm_records.len(), 4);
    assert!(warm_records
        .iter()
        .all(|record| record.root_session_id == canonical_root));
    assert!(warm_records.iter().all(|record| {
        cold_ids.get(record.content.normalized_body.as_deref().unwrap()) == Some(&record.event_id)
    }));

    let mut appended = serde_json::to_vec(&serde_json::json!({
        "timestamp": "2026-08-06T12:00:02Z",
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "normalized append"}]
        }
    }))
    .unwrap();
    appended.push(b'\n');
    OpenOptions::new()
        .append(true)
        .open(automatic.join(format!("rollout-{delegated}.jsonl")))
        .unwrap()
        .write_all(&appended)
        .unwrap();
    refresh_source_backed_generation(&index_path, &registry, WriterOptions::default()).unwrap();
    let append_records = core_records(&VerifiedIndex::open(&index_path).unwrap());
    assert_eq!(append_records.len(), 5);
    assert!(append_records
        .iter()
        .all(|record| record.root_session_id == canonical_root));
    assert!(append_records
        .iter()
        .any(|record| { record.content.normalized_body.as_deref() == Some("normalized append") }));
    assert!(append_records
        .iter()
        .filter(|record| { record.content.normalized_body.as_deref() != Some("normalized append") })
        .all(|record| {
            cold_ids.get(record.content.normalized_body.as_deref().unwrap())
                == Some(&record.event_id)
        }));

    let new_root = "019fa000-0000-7000-8000-000000003279";
    fs::write(
        explicit.join(format!("rollout-{new_root}.jsonl")),
        codex_lineage_rollout(
            new_root,
            None,
            SessionRelationshipKind::Root,
            Some(new_root),
            "normalized new root",
        ),
    )
    .unwrap();
    fs::write(
        automatic.join(format!("rollout-{root}.jsonl")),
        codex_lineage_rollout(
            root,
            Some(new_root),
            SessionRelationshipKind::Forked,
            Some(new_root),
            "normalized root",
        ),
    )
    .unwrap();
    refresh_source_backed_generation(&index_path, &registry, WriterOptions::default()).unwrap();
    let reparented_records = core_records(&VerifiedIndex::open(&index_path).unwrap());
    assert_eq!(reparented_records.len(), 6);
    let reparented_root = reparented_records[0].root_session_id;
    assert_ne!(reparented_root, canonical_root);
    assert!(reparented_records
        .iter()
        .all(|record| record.root_session_id == reparented_root));
    assert!(reparented_records
        .iter()
        .filter(|record| cold_ids.contains_key(record.content.normalized_body.as_deref().unwrap()))
        .all(|record| {
            cold_ids.get(record.content.normalized_body.as_deref().unwrap())
                == Some(&record.event_id)
        }));
}

#[test]
fn codex_all_invalid_lineage_fails_without_workers_or_publication() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    let child = "019fa000-0000-7000-8000-000000003285";
    let grandchild = "019fa000-0000-7000-8000-000000003286";
    let absent = "019fa000-0000-7000-8000-000000003299";
    fs::write(
        sessions.join(format!("rollout-{child}.jsonl")),
        codex_lineage_rollout(
            child,
            Some(absent),
            SessionRelationshipKind::Forked,
            Some(absent),
            "all invalid one",
        ),
    )
    .unwrap();
    fs::write(
        sessions.join(format!("rollout-{grandchild}.jsonl")),
        codex_lineage_rollout(
            grandchild,
            Some(child),
            SessionRelationshipKind::Delegated,
            Some(child),
            "all invalid two",
        ),
    )
    .unwrap();
    let observed = Arc::new(Mutex::new(None));
    let observed_from_hook = Arc::clone(&observed);
    install_after_codex_lineage_normalization_hook_v0(move |observation| {
        *observed_from_hook.lock().unwrap() = Some(observation);
    });
    assert!(refresh_source_backed_generation(
        &index,
        &register_codex_tree(&sessions),
        WriterOptions::default(),
    )
    .is_err());
    let observation = observed.lock().unwrap().unwrap();
    assert_eq!(observation.valid_sources, 0);
    assert_eq!(observation.rejected_sources, 2);
    assert_eq!(observation.pre_worker_counters.scanner_sources_started, 0);
    assert_eq!(observation.pre_worker_counters.staged_documents, 0);
    assert!(VerifiedIndex::open(&index).is_err());
}

#[test]
fn registered_codex_parent_and_exact_subdirectory_keep_parent_source_ownership() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let exact_subdirectory = sessions.join("2026/08/02");
    fs::create_dir_all(&exact_subdirectory).unwrap();
    let root_session_id = "019facf0-1111-7777-8888-000000000001";
    let nested_session_id = "019facf0-2222-7777-8888-000000000002";
    fs::write(
        sessions.join(format!("rollout-{root_session_id}.jsonl")),
        codex_rollout_bytes(root_session_id, &["parent root"]),
    )
    .unwrap();
    let nested_path = exact_subdirectory.join(format!("rollout-{nested_session_id}.jsonl"));
    fs::write(
        &nested_path,
        codex_rollout_bytes(nested_session_id, &["nested old"]),
    )
    .unwrap();

    let mut parent_registry = SourceBackedProviderRegistry::new();
    super::super::register_codex_session_tree_routes(
        &mut parent_registry,
        vec![fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_session_jsonl_tree",
            ProviderImportSupport::Native,
            &sessions,
        )],
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();
    let index = temp.path().join("index");
    let cold = refresh_source_backed_generation(&index, &parent_registry, WriterOptions::default())
        .unwrap();
    let parent_route_identity = cold.successful_route_ids[0].clone();
    assert_eq!(
        cold.commit
            .manifest()
            .source_route(&parent_route_identity)
            .unwrap()
            .sources()
            .len(),
        2
    );

    let mut combined_registry = SourceBackedProviderRegistry::new();
    super::super::register_codex_session_tree_routes(
        &mut combined_registry,
        vec![fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_session_jsonl_tree",
            ProviderImportSupport::Native,
            &sessions,
        )],
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();
    super::super::register_codex_session_tree_routes(
        &mut combined_registry,
        vec![fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_session_jsonl_tree",
            ProviderImportSupport::Explicit,
            &exact_subdirectory,
        )],
        SourceBackedRouteSelection::ExplicitManual,
    )
    .unwrap();
    let route_identities = combined_registry
        .routes()
        .map(|route| route.route_identity.clone().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(route_identities.len(), 2);
    assert_eq!(route_identities[0], parent_route_identity);
    let exact_route_identity = route_identities[1].clone();

    let append_bytes = codex_rollout_bytes(nested_session_id, &["discarded", "nested append"]);
    OpenOptions::new()
        .append(true)
        .open(&nested_path)
        .unwrap()
        .write_all(
            append_bytes
                .split_inclusive(|byte| *byte == b'\n')
                .nth(2)
                .unwrap(),
        )
        .unwrap();
    // Registration declares route authority but does not read or freeze the
    // provider tree. The shared JSONL lifecycle freezes its opening inventory
    // only when this refresh is admitted, so this pre-refresh append belongs
    // to the generation while the parent route still owns the nested source.
    let refreshed =
        refresh_source_backed_generation(&index, &combined_registry, WriterOptions::default())
            .unwrap();

    assert!(
        refreshed.failed_routes.is_empty(),
        "unexpected route failures: {:#?}",
        refreshed.source_failures.failures()
    );
    assert_eq!(refreshed.successful_route_ids.len(), 2);
    assert!(refreshed.logical_source_failures.is_empty());
    let parent_snapshot = refreshed
        .commit
        .manifest()
        .source_route(&parent_route_identity)
        .unwrap();
    assert_eq!(parent_snapshot.sources().len(), 2);
    let parent_sources = parent_snapshot.sources().to_vec();
    assert!(refreshed
        .commit
        .manifest()
        .source_route(&exact_route_identity)
        .unwrap()
        .sources()
        .is_empty());
    assert_eq!(refreshed.sources.len(), 2);
    let bodies = core_records(&VerifiedIndex::open(&index).unwrap())
        .into_iter()
        .filter_map(|record| record.content.normalized_body)
        .collect::<Vec<_>>();
    assert_eq!(bodies, vec!["parent root", "nested old", "nested append"]);

    let caught_up =
        refresh_source_backed_generation(&index, &combined_registry, WriterOptions::default())
            .unwrap();
    assert_eq!(
        caught_up.commit.generation_id,
        refreshed.commit.generation_id
    );
    let bodies = core_records(&VerifiedIndex::open(&index).unwrap())
        .into_iter()
        .filter_map(|record| record.content.normalized_body)
        .collect::<Vec<_>>();
    assert_eq!(bodies, vec!["parent root", "nested old", "nested append"]);
    assert_eq!(
        caught_up
            .commit
            .manifest()
            .source_route(&parent_route_identity)
            .unwrap()
            .sources(),
        parent_sources
    );
    assert!(caught_up
        .commit
        .manifest()
        .source_route(&exact_route_identity)
        .unwrap()
        .sources()
        .is_empty());

    let replay =
        refresh_source_backed_generation(&index, &combined_registry, WriterOptions::default())
            .unwrap();
    assert_eq!(replay.commit.generation_id, caught_up.commit.generation_id);
    assert_eq!(
        replay
            .commit
            .manifest()
            .source_route(&parent_route_identity)
            .unwrap()
            .sources(),
        parent_sources
    );
}

#[test]
fn codex_history_and_sessions_publish_self_contained_core_across_lifecycle() {
    let temp = tempdir().unwrap();
    let home = temp.path().join("home");
    let sessions = home.join(".codex/sessions");
    let history = home.join(".codex/history.jsonl");
    fs::create_dir_all(&sessions).unwrap();

    let native_session_id = "019faadb-b9f2-7413-9fab-edf59fd787a6";
    let session_path = sessions.join(format!("rollout-{native_session_id}.jsonl"));
    fs::write(
        &session_path,
        codex_rollout_bytes(native_session_id, &["complete session body"]),
    )
    .unwrap();
    let prompt_tail = "full-body-tail-marker";
    let prompt_body = format!("complete prompt {} {prompt_tail}", "x".repeat(8_192));
    fs::write(
        &history,
        prompt_line(native_session_id, 1_785_139_200, &prompt_body),
    )
    .unwrap();

    let context = DiscoveryContext::new(
        &home,
        temp.path().join("cwd"),
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    );
    let routes = vec![
        fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_session_jsonl_tree",
            ProviderImportSupport::Native,
            &sessions,
        ),
        fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_history_jsonl",
            ProviderImportSupport::Native,
            &history,
        ),
    ];
    let build = build_automatic_source_backed_registry_from_parts(
        &context,
        &temp.path().join("ctx-data"),
        routes,
        Vec::new(),
    );
    assert_eq!(build.executable_route_count(), 2);
    assert!(build.issues.is_empty());

    let index_path = temp.path().join("index");
    let options = WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    };
    let cold =
        refresh_source_backed_generation(&index_path, &build.registry, options.clone()).unwrap();
    assert_eq!(cold.commit.indexed_documents, 2);
    let index = VerifiedIndex::open(&index_path).unwrap();
    let records = core_records(&index);
    assert_eq!(records.len(), 2);
    assert!(records
        .iter()
        .all(|record| record.validate_contract().is_ok()));
    let prompt = records
        .iter()
        .find(|record| record.source.source_format() == "codex_history_jsonl")
        .unwrap();
    assert_eq!(
        prompt.content.normalized_body.as_deref(),
        Some(prompt_body.as_str())
    );
    assert!(prompt
        .content
        .normalized_body
        .as_deref()
        .unwrap()
        .ends_with(prompt_tail));
    assert_eq!(
        prompt.provider_session_id.as_deref(),
        Some(native_session_id)
    );
    let prompt_first_id = prompt.event_id;
    let session = records
        .iter()
        .find(|record| record.source.source_format() == "codex_session_jsonl")
        .unwrap();
    assert_eq!(
        session.content.normalized_body.as_deref(),
        Some("complete session body")
    );
    assert_eq!(session.cwd.as_deref(), Some("/tmp/explicit-codex-source"));

    OpenOptions::new()
        .append(true)
        .open(&history)
        .unwrap()
        .write_all(&prompt_line(
            native_session_id,
            1_785_139_201,
            "appended prompt",
        ))
        .unwrap();
    let append_bytes = codex_rollout_bytes(native_session_id, &["discarded", "appended session"]);
    let appended_session_line = append_bytes
        .split_inclusive(|byte| *byte == b'\n')
        .nth(2)
        .unwrap();
    OpenOptions::new()
        .append(true)
        .open(&session_path)
        .unwrap()
        .write_all(appended_session_line)
        .unwrap();
    let appended =
        refresh_source_backed_generation(&index_path, &build.registry, options.clone()).unwrap();
    assert_eq!(appended.commit.indexed_documents, 4);
    let appended_generation = appended.commit.generation_id.clone();
    let index = VerifiedIndex::open(&index_path).unwrap();
    let appended_records = core_records(&index);
    assert!(appended_records
        .iter()
        .any(|record| { record.content.normalized_body.as_deref() == Some("appended prompt") }));
    assert!(appended_records
        .iter()
        .any(|record| { record.content.normalized_body.as_deref() == Some("appended session") }));

    let unchanged =
        refresh_source_backed_generation(&index_path, &build.registry, options.clone()).unwrap();
    assert_eq!(unchanged.commit.generation_id, appended_generation);

    fs::write(
        &history,
        [
            prompt_line(native_session_id, 1_785_139_200, "rewritten prompt"),
            prompt_line(native_session_id, 1_785_139_201, "appended prompt"),
        ]
        .concat(),
    )
    .unwrap();
    refresh_source_backed_generation(&index_path, &build.registry, options.clone()).unwrap();
    let index = VerifiedIndex::open(&index_path).unwrap();
    let rewritten = core_records(&index)
        .into_iter()
        .find(|record| {
            record.source.source_format() == "codex_history_jsonl" && record.event_sequence == 0
        })
        .unwrap();
    assert_eq!(rewritten.event_id, prompt_first_id);
    assert_eq!(
        rewritten.content.normalized_body.as_deref(),
        Some("rewritten prompt")
    );

    fs::remove_file(&session_path).unwrap();
    refresh_source_backed_generation(&index_path, &build.registry, options).unwrap();
    let index = VerifiedIndex::open(&index_path).unwrap();
    assert_eq!(index.document_count(), 2);
    assert!(index.manifest().sources.iter().all(|source| source
        .observation()
        .source()
        .source_format()
        == "codex_history_jsonl"));
}
