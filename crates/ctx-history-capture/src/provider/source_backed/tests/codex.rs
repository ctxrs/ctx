use std::{fs::OpenOptions, io::Write};

use super::*;

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
