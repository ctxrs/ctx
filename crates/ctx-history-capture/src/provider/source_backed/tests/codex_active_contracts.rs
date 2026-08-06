use std::{
    io::{BufWriter, Write},
    process::Command,
};

use ctx_history_core::{RepositoryAbstentionReason, RepositoryVcsObservationKind};

use super::*;

fn write_codex_lineage_session(
    path: &Path,
    native_session_id: &str,
    parent_native_session_id: Option<&str>,
    events: &[serde_json::Value],
) {
    let mut payload = serde_json::json!({
        "id": native_session_id,
        "session_id": native_session_id,
        "timestamp": "2026-08-04T12:00:00Z",
        "cwd": "/tmp/explicit-codex-source",
        "originator": "codex_cli_rs",
        "cli_version": "0.1.0",
        "source": "cli",
        "model_provider": "openai"
    });
    if let Some(parent) = parent_native_session_id {
        payload["forked_from_id"] = serde_json::Value::String(parent.to_owned());
    }
    let mut lines = vec![serde_json::json!({
        "timestamp": "2026-08-04T12:00:00Z",
        "type": "session_meta",
        "payload": payload
    })];
    lines.extend_from_slice(events);
    let mut contents = lines
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    contents.push('\n');
    fs::write(path, contents).unwrap();
}

fn codex_exec_call(call_id: &str, command: &str, repository: &Path) -> serde_json::Value {
    serde_json::json!({
        "timestamp": "2026-08-04T12:00:01Z",
        "type": "response_item",
        "payload": {
            "type": "function_call",
            "name": "exec_command",
            "call_id": call_id,
            "arguments": serde_json::json!({
                "cmd": command,
                "workdir": repository,
                "yield_time_ms": 10000
            }).to_string()
        }
    })
}

fn codex_successful_result(call_id: &str, output: &str) -> serde_json::Value {
    serde_json::json!({
        "timestamp": "2026-08-04T12:00:02Z",
        "type": "response_item",
        "payload": {
            "type": "function_call_output",
            "call_id": call_id,
            "status": "success",
            "output": output
        }
    })
}

fn append_codex_non_display_lineage_results(path: &Path, component: usize, facts: usize) {
    let file = fs::OpenOptions::new().append(true).open(path).unwrap();
    let mut writer = BufWriter::new(file);
    for fact in 0..facts {
        writeln!(
            writer,
            r#"{{"timestamp":"2026-08-04T12:00:01Z","type":"response_item","payload":{{"type":"tool_search_output","call_id":"lineage-capacity-{component:02}-{fact:04}"}}}}"#,
        )
        .unwrap();
    }
    writer.flush().unwrap();
}

#[test]
fn codex_jsonl_cold_route_publishes_beyond_old_aggregate_lineage_fact_limit() {
    const COMPONENTS: usize = 65;
    const FACTS_PER_PARENT: usize = 4_096;
    const OLD_AGGREGATE_LINEAGE_FACT_LIMIT: usize = 262_144;
    const LINEAGE_FACTS: usize = COMPONENTS * FACTS_PER_PARENT;
    const LATE_COMPONENT_MARKER: &str = "latecomponentlineagecapacitymarker";

    assert_eq!(LINEAGE_FACTS, 266_240);
    const {
        assert!(LINEAGE_FACTS > OLD_AGGREGATE_LINEAGE_FACT_LIMIT);
    }

    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    let mut corpus_bytes = 0_u64;
    for component in 0..COMPONENTS {
        let parent_id = format!("019facf0-5555-7000-8000-{component:012x}");
        let child_id = format!("019facf1-5555-7000-8000-{component:012x}");
        let parent_path = sessions.join(format!("rollout-{parent_id}.jsonl"));
        let child_path = sessions.join(format!("rollout-{child_id}.jsonl"));

        let parent_anchor = serde_json::json!({
            "timestamp": "2026-08-04T11:59:59Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{
                    "type": "output_text",
                    "text": format!("lineage parent anchor {component:02}")
                }]
            }
        });
        write_codex_lineage_session(&parent_path, &parent_id, None, &[parent_anchor]);
        append_codex_non_display_lineage_results(&parent_path, component, FACTS_PER_PARENT);
        let child_events = (component + 1 == COMPONENTS)
            .then(|| {
                serde_json::json!({
                    "timestamp": "2026-08-04T12:00:02Z",
                    "type": "response_item",
                    "payload": {
                        "type": "message",
                        "role": "assistant",
                        "content": [{
                            "type": "output_text",
                            "text": LATE_COMPONENT_MARKER
                        }]
                    }
                })
            })
            .into_iter()
            .collect::<Vec<_>>();
        write_codex_lineage_session(&child_path, &child_id, Some(&parent_id), &child_events);
        corpus_bytes = corpus_bytes
            .checked_add(fs::metadata(parent_path).unwrap().len())
            .and_then(|bytes| bytes.checked_add(fs::metadata(child_path).unwrap().len()))
            .unwrap();
    }
    assert!(corpus_bytes > 32 * 1024 * 1024);
    assert!(corpus_bytes < 64 * 1024 * 1024);

    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_session_jsonl_tree",
            ProviderImportSupport::Native,
            &sessions,
        ),
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();
    let observed = Arc::new(Mutex::new(None));
    let observed_from_hook = Arc::clone(&observed);
    super::super::set_after_codex_session_tree_stage_hook(move |counters| {
        *observed_from_hook.lock().unwrap() = Some(counters);
    });

    let refreshed = refresh_source_backed_generation(
        &index,
        &registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();
    let expected_sources = COMPONENTS * 2;
    let expected_indexed_documents = COMPONENTS + 1;
    let expected_complete_records = LINEAGE_FACTS + expected_sources + expected_indexed_documents;
    let counters = observed.lock().unwrap().expect("stage hook must run");
    assert_eq!(counters.cold_sources, expected_sources as u64);
    assert_eq!(counters.scanner_sources_started, expected_sources as u64);
    assert_eq!(counters.scanner_sources_completed, expected_sources as u64);
    assert_eq!(
        counters.complete_records_scanned,
        expected_complete_records as u64
    );
    assert_eq!(counters.staged_documents, expected_indexed_documents as u64);

    assert_eq!(refreshed.successful_route_ids.len(), 1);
    assert!(refreshed.failed_routes.is_empty());
    assert_eq!(
        refreshed.record_completion(),
        SourceBackedRecordCompletion::Completed
    );
    assert_eq!(refreshed.certified_source_count, expected_sources);
    assert_eq!(refreshed.commit.certified_sources, expected_sources);
    assert_eq!(
        refreshed.commit.indexed_documents,
        expected_indexed_documents as u64
    );
    let certified_complete_records = refreshed
        .sources
        .iter()
        .map(|source| source.counts().complete_records)
        .sum::<u64>();
    assert_eq!(certified_complete_records, expected_complete_records as u64);
    assert!(certified_complete_records > OLD_AGGREGATE_LINEAGE_FACT_LIMIT as u64);
    assert_eq!(
        refreshed
            .sources
            .iter()
            .map(|source| source.counts().rejected_records)
            .sum::<u64>(),
        0
    );
    assert_eq!(
        VerifiedIndex::open(&index)
            .unwrap()
            .search_event_candidates(LATE_COMPONENT_MARKER, 8)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn codex_jsonl_configured_lineage_exhaustion_publishes_conservatively_without_retry() {
    const MARKER: &str = "lineagecapacityconservativepublicationmarker";
    const COPIED_OID: &str = "cccccccccccccccccccccccccccccccccccccccc";
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let repository = temp.path().join("repo");
    let index = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    fs::create_dir_all(&repository).unwrap();
    for arguments in [
        vec!["init", "-q"],
        vec!["config", "user.name", "ctx test"],
        vec!["config", "user.email", "ctx@example.invalid"],
    ] {
        assert!(Command::new("git")
            .args(arguments)
            .current_dir(&repository)
            .status()
            .unwrap()
            .success());
    }
    let parent_id = "019facf0-5555-7000-8001-000000000001";
    let child_id = "019facf0-5555-7000-8001-000000000002";
    let parent = sessions.join(format!("rollout-{parent_id}.jsonl"));
    let child = sessions.join(format!("rollout-{child_id}.jsonl"));
    let mut copied_call = codex_exec_call(
        "call-configured-capacity-copied",
        "git commit -m copied && git rev-parse --verify HEAD",
        &repository,
    );
    let mut copied_result = codex_successful_result(
        "call-configured-capacity-copied",
        &format!("[main ccccccc] copied\n{COPIED_OID}\n"),
    );
    // Keep this pair before the child's fork boundary so classification must
    // consult the parent fact set instead of taking PR #290's post-fork fast
    // path. The one-fact test cap makes that parent set conservative.
    copied_call["timestamp"] = serde_json::Value::String("2026-08-04T11:59:58Z".to_owned());
    copied_result["timestamp"] = serde_json::Value::String("2026-08-04T11:59:59Z".to_owned());
    write_codex_lineage_session(
        &parent,
        parent_id,
        None,
        &[copied_call.clone(), copied_result.clone()],
    );
    write_codex_lineage_session(
        &child,
        child_id,
        Some(parent_id),
        &[
            copied_call,
            copied_result,
            serde_json::json!({
                "timestamp": "2026-08-04T12:00:02Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": MARKER}]
                }
            }),
        ],
    );

    let adapter =
        crate::provider::codex::nativepath::CodexSessionTreeJsonlFamilyAdapterV0::new(vec![
            sessions.clone(),
        ])
        .unwrap()
        .with_lineage_budget_limits(4_096, 1);
    let driver =
        super::super::family::jsonl::jsonl_family_driver(Arc::new(adapter), sessions.clone());
    let route = SourceBackedRoute::automatic(
        fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_session_jsonl_tree",
            ProviderImportSupport::Native,
            &sessions,
        ),
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )
    .unwrap();
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(route);

    let refreshed = refresh_source_backed_generation(
        &index,
        &registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();
    assert_eq!(
        refreshed.record_completion(),
        SourceBackedRecordCompletion::Completed
    );
    assert_eq!(refreshed.successful_route_ids.len(), 1);
    assert!(refreshed.failed_routes.is_empty());
    assert_eq!(refreshed.certified_source_count, 2);
    let verified = VerifiedIndex::open(&index).unwrap();
    assert_eq!(
        verified.search_event_candidates(MARKER, 8).unwrap().len(),
        1,
        "configured lineage exhaustion must not discard unrelated cold output"
    );
    let copied_child = verified
        .search_event_candidates(COPIED_OID, 8)
        .unwrap()
        .into_iter()
        .filter_map(|candidate| {
            verified
                .core_record_by_id(candidate.event.event_id.as_uuid())
                .unwrap()
        })
        .find(|record| record.provider_session_id.as_deref() == Some(child_id))
        .expect("configured-capacity child result must publish");
    assert!(copied_child.repository_vcs_observations.is_empty());
    assert!(copied_child
        .repository_abstentions
        .iter()
        .any(|abstention| {
            abstention.reason == RepositoryAbstentionReason::ProviderOutputUnjoined
                && abstention.detail.as_deref()
                    == Some("provider_execution_origin_lineage_unproven")
        }));
}

#[test]
fn codex_jsonl_workers_reuse_repository_certification_across_leaf_stripes() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let repository = temp.path().join("repo");
    let index = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    fs::create_dir_all(&repository).unwrap();
    assert!(Command::new("git")
        .args(["init", "-q"])
        .current_dir(&repository)
        .status()
        .unwrap()
        .success());
    fs::write(
        repository.join("tracked.txt"),
        "repository cache sentinel\n",
    )
    .unwrap();
    assert!(Command::new("git")
        .args(["add", "tracked.txt"])
        .current_dir(&repository)
        .status()
        .unwrap()
        .success());
    assert!(Command::new("git")
        .args([
            "-c",
            "user.name=ctx test",
            "-c",
            "user.email=ctx@example.invalid",
            "commit",
            "-qm",
            "initial",
        ])
        .current_dir(&repository)
        .status()
        .unwrap()
        .success());
    for index in 0_u128..32 {
        let native_session_id = format!("019facf0-3333-7777-8888-{index:012}");
        let rollout = String::from_utf8(codex_rollout_bytes(
            &native_session_id,
            &["worker repository cache sentinel"],
        ))
        .unwrap()
        .replace(
            "/tmp/explicit-codex-source",
            repository.to_string_lossy().as_ref(),
        );
        fs::write(
            sessions.join(format!("rollout-{native_session_id}.jsonl")),
            rollout,
        )
        .unwrap();
    }

    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_session_jsonl_tree",
            ProviderImportSupport::Native,
            &sessions,
        ),
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();
    let observed = Arc::new(Mutex::new(None));
    let observed_from_hook = Arc::clone(&observed);
    super::super::set_after_codex_session_tree_stage_hook(move |counters| {
        *observed_from_hook.lock().unwrap() = Some(counters);
    });
    let refreshed = refresh_source_backed_generation(
        &index,
        &registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();
    assert_eq!(refreshed.commit.indexed_documents, 32);
    let counters = observed.lock().unwrap().expect("stage hook must run");
    assert_eq!(counters.scanner_sources_started, 32);
    assert!(counters.repository_full_git_certification_probes > 0);
    assert!(counters.repository_full_git_certification_probes <= 16);
    assert!(counters.repository_full_git_certification_probes < 32);
}

#[test]
fn codex_jsonl_warm_replay_prepares_parent_lineage_before_changed_child() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let repository = temp.path().join("repo");
    let index = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    fs::create_dir_all(&repository).unwrap();
    for arguments in [
        vec!["init", "-q"],
        vec!["config", "user.name", "ctx test"],
        vec!["config", "user.email", "ctx@example.invalid"],
    ] {
        assert!(Command::new("git")
            .args(arguments)
            .current_dir(&repository)
            .status()
            .unwrap()
            .success());
    }
    fs::write(repository.join("tracked.txt"), "tracked\n").unwrap();
    for arguments in [vec!["add", "tracked.txt"], vec!["commit", "-qm", "seed"]] {
        assert!(Command::new("git")
            .args(arguments)
            .current_dir(&repository)
            .status()
            .unwrap()
            .success());
    }

    // Keep the child lexically ahead of its parent so the shared inventory's
    // canonical path order is the opposite of the required lineage order.
    let parent_id = "f19facf0-4444-7777-8888-000000000001";
    let child_id = "019facf0-4444-7777-8888-000000000002";
    let parent_path = sessions.join(format!("rollout-{parent_id}.jsonl"));
    let child_path = sessions.join(format!("rollout-{child_id}.jsonl"));
    assert!(child_path < parent_path);
    let copied_oid = "518dedb053f04ab0b529c7d2e8dafb322974fbf6";
    let cold_child_oid = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let warm_copied_oid = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let mut copied_call = codex_exec_call(
        "call-copied-parent",
        "git commit -m copied && git rev-parse --verify HEAD",
        &repository,
    );
    let mut copied_result = codex_successful_result(
        "call-copied-parent",
        &format!("[main 518dedb] copied\n{copied_oid}\n"),
    );
    let mut warm_copied_call = codex_exec_call(
        "call-warm-copied-parent",
        "git commit -m warm-copied && git rev-parse --verify HEAD",
        &repository,
    );
    let mut warm_copied_result = codex_successful_result(
        "call-warm-copied-parent",
        &format!("[main bbbbbbb] warm copied\n{warm_copied_oid}\n"),
    );
    // PR #290 treats exact call/result pairs recorded strictly after a child
    // session starts as child-owned. This pair models genuinely copied parent
    // history, so keep its event times before the child's fork boundary.
    copied_call["timestamp"] = serde_json::Value::String("2026-08-04T11:59:58Z".to_owned());
    copied_result["timestamp"] = serde_json::Value::String("2026-08-04T11:59:59Z".to_owned());
    warm_copied_call["timestamp"] = serde_json::Value::String("2026-08-04T11:59:56Z".to_owned());
    warm_copied_result["timestamp"] = serde_json::Value::String("2026-08-04T11:59:57Z".to_owned());
    write_codex_lineage_session(
        &parent_path,
        parent_id,
        None,
        &[
            copied_call.clone(),
            copied_result.clone(),
            warm_copied_call.clone(),
            warm_copied_result.clone(),
        ],
    );
    let cold_child_call = codex_exec_call(
        "call-cold-child",
        "git commit -m cold-child && git rev-parse --verify HEAD",
        &repository,
    );
    let cold_child_result = codex_successful_result(
        "call-cold-child",
        &format!("[main aaaaaaa] cold child\n{cold_child_oid}\n"),
    );
    write_codex_lineage_session(
        &child_path,
        child_id,
        Some(parent_id),
        &[
            copied_call,
            copied_result,
            cold_child_call,
            cold_child_result,
        ],
    );

    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_session_jsonl_tree",
            ProviderImportSupport::Native,
            &sessions,
        ),
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();
    let options = WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    };
    let cold_counters = Arc::new(Mutex::new(None));
    let observed_cold = Arc::clone(&cold_counters);
    super::super::set_after_codex_session_tree_stage_hook(move |counters| {
        *observed_cold.lock().unwrap() = Some(counters);
    });
    refresh_source_backed_generation(&index, &registry, options.clone()).unwrap();
    let cold_counters = cold_counters.lock().unwrap().unwrap();
    assert_eq!(cold_counters.cold_sources, 2);

    let cold_index = VerifiedIndex::open(&index).unwrap();
    let copied_records = cold_index
        .search_event_candidates(copied_oid, 8)
        .unwrap()
        .into_iter()
        .map(|candidate| {
            cold_index
                .core_record_by_id(candidate.event.event_id.as_uuid())
                .unwrap()
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(copied_records.len(), 1);
    assert!(!copied_records[0].repository_vcs_observations.is_empty());
    let child_sessions = cold_index
        .sessions_by_provider_session_id(child_id, Some("codex"))
        .unwrap();
    assert_eq!(child_sessions.len(), 1);
    let child_session_id = child_sessions[0].session_id;
    let cold_copied_child = cold_index
        .core_events_for_session(child_session_id.as_uuid())
        .unwrap()
        .into_iter()
        .map(|record| record.core_record)
        .find(|record| {
            record
                .content
                .normalized_body
                .as_deref()
                .is_some_and(|body| body.contains(copied_oid))
        })
        .expect("cold copied child result must publish");
    assert!(cold_copied_child.repository_vcs_observations.is_empty());
    assert!(cold_copied_child
        .repository_abstentions
        .iter()
        .any(|abstention| {
            abstention.reason == RepositoryAbstentionReason::ProviderOutputUnjoined
                && abstention.detail.as_deref()
                    == Some("copied_provider_history_has_ancestor_execution")
        }));
    let cold_unique = cold_index
        .search_event_candidates(cold_child_oid, 8)
        .unwrap();
    assert_eq!(cold_unique.len(), 1);
    let cold_unique = cold_index
        .core_record_by_id(cold_unique[0].event.event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert!(matches!(
        cold_unique.repository_vcs_observations[0].kind,
        RepositoryVcsObservationKind::Outcome(_)
    ));
    drop(cold_index);

    let mut child = fs::OpenOptions::new()
        .append(true)
        .open(&child_path)
        .unwrap();
    writeln!(child, "{warm_copied_call}").unwrap();
    writeln!(child, "{warm_copied_result}").unwrap();
    child.sync_all().unwrap();

    let warm_counters = Arc::new(Mutex::new(None));
    let observed_warm = Arc::clone(&warm_counters);
    super::super::set_after_codex_session_tree_stage_hook(move |counters| {
        *observed_warm.lock().unwrap() = Some(counters);
    });
    refresh_source_backed_generation(&index, &registry, options).unwrap();
    let warm_counters = warm_counters.lock().unwrap().unwrap();
    assert_eq!(warm_counters.replayed_sources, 1);
    assert_eq!(warm_counters.appended_sources, 1);

    let warm_index = VerifiedIndex::open(&index).unwrap();
    let warm_copied_child = warm_index
        .core_events_for_session(child_session_id.as_uuid())
        .unwrap()
        .into_iter()
        .map(|record| record.core_record)
        .find(|record| {
            record
                .content
                .normalized_body
                .as_deref()
                .is_some_and(|body| body.contains(warm_copied_oid))
        })
        .expect("warm copied child result must publish");
    assert!(warm_copied_child.repository_vcs_observations.is_empty());
    assert!(warm_copied_child
        .repository_abstentions
        .iter()
        .any(|abstention| {
            abstention.reason == RepositoryAbstentionReason::ProviderOutputUnjoined
                && abstention.detail.as_deref()
                    == Some("copied_provider_history_has_ancestor_execution")
        }));
}

fn assert_cold_route_failure(
    error: SourceBackedCoordinatorError,
    class: SourceBackedSourceFailureClass,
) {
    match error {
        SourceBackedCoordinatorError::NoUsableSourceRoutes { failed_routes } => {
            assert_eq!(failed_routes.len(), 1);
            assert_eq!(failed_routes[0].class, class);
            assert!(!failed_routes[0].carried_forward);
        }
        error => panic!("expected one unusable source route, got {error:?}"),
    }
}

#[test]
fn active_source_family_contract_explicit_codex_append_catches_up() {
    let temp = tempdir().unwrap();
    let selected = temp.path().join("selected.jsonl");
    let index = temp.path().join("index");
    let native_session_id = "019facf0-3333-7777-8888-000000000003";
    fs::write(
        &selected,
        codex_rollout_bytes(native_session_id, &["explicitfrozenmarker"]),
    )
    .unwrap();

    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_session_jsonl",
            ProviderImportSupport::Explicit,
            &selected,
        ),
        SourceBackedRouteSelection::ExplicitManual,
    )
    .unwrap();
    let cold = refresh_source_backed_generation(
        &index,
        &registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();
    assert_eq!(cold.commit.indexed_documents, 1);
    let source = cold.sources[0].observation().source().clone();
    let verified = VerifiedIndex::open(&index).unwrap();
    let first = verified
        .source_event_page(&source, None, 8)
        .unwrap()
        .items
        .into_iter()
        .next()
        .unwrap();
    let first_core = verified
        .core_record_by_id(first.event_id.as_uuid())
        .unwrap()
        .unwrap();
    assert_eq!(
        first_core.content.normalized_body.as_deref(),
        Some("explicitfrozenmarker")
    );

    let append = codex_rollout_bytes(native_session_id, &["discarded", "explicitappendmarker"]);
    let second_line = append
        .split(|byte| *byte == b'\n')
        .nth(2)
        .expect("fixture has a second message");
    let mut file = fs::OpenOptions::new().append(true).open(&selected).unwrap();
    file.write_all(second_line).unwrap();
    file.write_all(b"\n").unwrap();
    file.sync_all().unwrap();

    let observed_counters = Arc::new(Mutex::new(None));
    let captured_counters = Arc::clone(&observed_counters);
    super::super::set_after_explicit_codex_stage_hook(move |counters| {
        *captured_counters.lock().unwrap() = Some(counters);
    });
    let appended = refresh_source_backed_generation(
        &index,
        &registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();
    let counters = observed_counters
        .lock()
        .unwrap()
        .take()
        .expect("explicit Codex append must report its selected disposition");
    assert_eq!(counters.appended_sources, 1);
    assert_eq!(counters.replaced_sources, 0);
    assert_eq!(counters.cold_sources, 0);
    assert_eq!(appended.commit.indexed_documents, 2);
    assert_eq!(
        VerifiedIndex::open(&index)
            .unwrap()
            .search_event_candidates("explicitappendmarker", 8)
            .unwrap()
            .len(),
        1
    );
}

#[cfg(unix)]
#[test]
fn active_source_family_contract_explicit_codex_replacement_rejects_inode_swap() {
    let temp = tempdir().unwrap();
    let selected = temp.path().join("selected.jsonl");
    let moved = temp.path().join("selected-scanned.jsonl");
    let index = temp.path().join("index");
    let native_session_id = "019facf0-3333-7777-8888-000000000014";
    fs::write(
        &selected,
        codex_rollout_bytes(native_session_id, &["explicitinodeswapbase"]),
    )
    .unwrap();
    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_session_jsonl",
            ProviderImportSupport::Explicit,
            &selected,
        ),
        SourceBackedRouteSelection::ExplicitManual,
    )
    .unwrap();
    let options = WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    };
    let seeded = refresh_source_backed_generation(&index, &registry, options.clone()).unwrap();
    let seeded_generation = seeded.commit.generation_id.clone();

    let append = codex_rollout_bytes(native_session_id, &["discarded", "inode swap staged"]);
    let line = append.split(|byte| *byte == b'\n').nth(2).unwrap();
    let mut file = fs::OpenOptions::new().append(true).open(&selected).unwrap();
    file.write_all(line).unwrap();
    file.write_all(b"\n").unwrap();
    file.sync_all().unwrap();
    let replacement = fs::read(&selected).unwrap();
    let swap_path = selected.clone();
    super::super::set_after_explicit_codex_stage_hook(move |_| {
        fs::rename(&swap_path, moved).unwrap();
        fs::write(swap_path, replacement).unwrap();
    });

    let failed = refresh_source_backed_generation(&index, &registry, options).unwrap();
    assert_carried_route_failure(
        &failed,
        &seeded_generation,
        SourceBackedSourceFailureClass::SourceChanged,
    );
    let retained = VerifiedIndex::open(&index).unwrap();
    assert_eq!(retained.generation_id(), seeded_generation);
    assert_eq!(
        retained
            .search_event_candidates("explicitinodeswapbase", 8)
            .unwrap()
            .len(),
        1
    );
    assert!(retained
        .search_event_candidates("inode swap staged", 8)
        .unwrap()
        .is_empty());
}

#[test]
fn active_source_family_contract_explicit_codex_defers_append_after_staging() {
    let temp = tempdir().unwrap();
    let selected = temp.path().join("selected.jsonl");
    let index = temp.path().join("index");
    let native_session_id = "019facf0-3333-7777-8888-000000000004";
    fs::write(
        &selected,
        codex_rollout_bytes(native_session_id, &["explicitfrozenmarker"]),
    )
    .unwrap();
    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_session_jsonl",
            ProviderImportSupport::Explicit,
            &selected,
        ),
        SourceBackedRouteSelection::ExplicitManual,
    )
    .unwrap();

    let append = codex_rollout_bytes(native_session_id, &["discarded", "deferredappendmarker"]);
    let second_line = append
        .split(|byte| *byte == b'\n')
        .nth(2)
        .expect("fixture has a second message")
        .to_vec();
    let append_path = selected.clone();
    super::super::set_after_explicit_codex_stage_hook(move |_| {
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(append_path)
            .unwrap();
        file.write_all(&second_line).unwrap();
        file.write_all(b"\n").unwrap();
        file.sync_all().unwrap();
    });
    let frozen = refresh_source_backed_generation(
        &index,
        &registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();
    assert_eq!(frozen.commit.indexed_documents, 1);
    assert!(VerifiedIndex::open(&index)
        .unwrap()
        .search_event_candidates("deferredappendmarker", 8)
        .unwrap()
        .is_empty());

    let caught_up = refresh_source_backed_generation(
        &index,
        &registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();
    assert_eq!(caught_up.commit.indexed_documents, 2);
    assert_eq!(
        VerifiedIndex::open(&index)
            .unwrap()
            .search_event_candidates("deferredappendmarker", 8)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn active_source_family_contract_explicit_codex_deletes_disappeared_file() {
    let temp = tempdir().unwrap();
    let selected = temp.path().join("selected.jsonl");
    let index = temp.path().join("index");
    let native_session_id = "019facf0-3333-7777-8888-000000000012";
    fs::write(
        &selected,
        codex_rollout_bytes(native_session_id, &["explicitdeletionmarker"]),
    )
    .unwrap();
    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_session_jsonl",
            ProviderImportSupport::Explicit,
            &selected,
        ),
        SourceBackedRouteSelection::ExplicitManual,
    )
    .unwrap();
    let options = WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    };
    let cold = refresh_source_backed_generation(&index, &registry, options.clone()).unwrap();
    assert_eq!(cold.commit.indexed_documents, 1);

    fs::remove_file(selected).unwrap();
    let deleted = refresh_source_backed_generation(&index, &registry, options).unwrap();
    assert_eq!(deleted.commit.indexed_documents, 0);
    assert_eq!(VerifiedIndex::open(&index).unwrap().document_count(), 0);
}
#[test]
fn active_source_family_contract_codex_tree_defers_append_after_staging() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019facf0-3333-7777-8888-000000000005";
    let selected = sessions.join(format!("rollout-{native_session_id}.jsonl"));
    fs::write(
        &selected,
        codex_rollout_bytes(native_session_id, &["treefrozenmarker"]),
    )
    .unwrap();

    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_session_jsonl_tree",
            ProviderImportSupport::Native,
            &sessions,
        ),
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();

    let append = codex_rollout_bytes(native_session_id, &["discarded", "treeappendmarker"]);
    let appended_line = append
        .split_inclusive(|byte| *byte == b'\n')
        .nth(2)
        .expect("fixture has a second message")
        .to_vec();
    let append_path = selected.clone();
    super::super::set_after_codex_session_tree_stage_hook(move |_| {
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(append_path)
            .unwrap();
        file.write_all(&appended_line).unwrap();
        file.sync_all().unwrap();
    });

    let frozen = refresh_source_backed_generation(
        &index,
        &registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();
    assert_eq!(frozen.commit.indexed_documents, 1);
    assert!(VerifiedIndex::open(&index)
        .unwrap()
        .search_event_candidates("treeappendmarker", 8)
        .unwrap()
        .is_empty());

    let caught_up = refresh_source_backed_generation(
        &index,
        &registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();
    assert_eq!(caught_up.commit.indexed_documents, 2);
    assert_eq!(
        VerifiedIndex::open(&index)
            .unwrap()
            .search_event_candidates("treeappendmarker", 8)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn active_source_family_contract_codex_tree_admits_append_during_cold_catalog() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let archived_sessions = temp.path().join("archived_sessions");
    let index = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    fs::create_dir_all(&archived_sessions).unwrap();
    let native_session_id = "019facf0-3333-7777-8888-000000000006";
    let selected = sessions.join(format!("rollout-{native_session_id}.jsonl"));
    fs::write(
        &selected,
        codex_rollout_bytes(native_session_id, &["catalogfrozenmarker"]),
    )
    .unwrap();

    let append = codex_rollout_bytes(native_session_id, &["discarded", "catalogappendmarker"]);
    let appended_line = append
        .split_inclusive(|byte| *byte == b'\n')
        .nth(2)
        .expect("fixture has a second message")
        .to_vec();
    let append_path = selected.clone();
    crate::provider::codex::nativepath::install_after_codex_metadata_inventory_hook(move || {
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(append_path)
            .unwrap();
        file.write_all(&appended_line).unwrap();
        file.sync_all().unwrap();
    });

    let mut registry = SourceBackedProviderRegistry::new();
    super::super::register_codex_session_tree_routes(
        &mut registry,
        vec![
            fixture_provider_source_at(
                CaptureProvider::Codex,
                "codex_session_jsonl_tree",
                ProviderImportSupport::Native,
                &sessions,
            ),
            fixture_provider_source_at(
                CaptureProvider::Codex,
                "codex_session_jsonl_tree",
                ProviderImportSupport::Native,
                &archived_sessions,
            ),
        ],
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();

    let cold = refresh_source_backed_generation(
        &index,
        &registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();
    assert_eq!(cold.commit.indexed_documents, 1);
    assert_eq!(
        VerifiedIndex::open(&index)
            .unwrap()
            .search_event_candidates("catalogappendmarker", 8)
            .unwrap()
            .len(),
        0
    );
    let catch_up = refresh_source_backed_generation(
        &index,
        &registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();
    assert_eq!(catch_up.commit.indexed_documents, 2);
    assert_eq!(
        VerifiedIndex::open(&index)
            .unwrap()
            .search_event_candidates("catalogappendmarker", 8)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn active_source_family_contract_codex_tree_defers_new_session_after_staging() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    let first_session_id = "019facf0-3333-7777-8888-000000000007";
    fs::write(
        sessions.join(format!("rollout-{first_session_id}.jsonl")),
        codex_rollout_bytes(first_session_id, &["firsttreesessionmarker"]),
    )
    .unwrap();

    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_session_jsonl_tree",
            ProviderImportSupport::Native,
            &sessions,
        ),
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();

    let second_session_id = "019facf0-3333-7777-8888-000000000008";
    let second_path = sessions.join(format!("rollout-{second_session_id}.jsonl"));
    super::super::set_after_codex_session_tree_stage_hook(move |_| {
        fs::write(
            second_path,
            codex_rollout_bytes(second_session_id, &["deferredtreesessionmarker"]),
        )
        .unwrap();
    });

    let options = WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    };
    let frozen = refresh_source_backed_generation(&index, &registry, options.clone()).unwrap();
    assert_eq!(frozen.commit.indexed_documents, 1);
    assert!(VerifiedIndex::open(&index)
        .unwrap()
        .search_event_candidates("deferredtreesessionmarker", 8)
        .unwrap()
        .is_empty());

    let caught_up = refresh_source_backed_generation(&index, &registry, options).unwrap();
    assert_eq!(caught_up.commit.indexed_documents, 2);
    assert_eq!(
        VerifiedIndex::open(&index)
            .unwrap()
            .search_event_candidates("deferredtreesessionmarker", 8)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn active_source_family_contract_codex_tree_rejects_captured_session_removal() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019facf0-3333-7777-8888-000000000009";
    let selected = sessions.join(format!("rollout-{native_session_id}.jsonl"));
    fs::write(
        &selected,
        codex_rollout_bytes(native_session_id, &["removedtreesessionmarker"]),
    )
    .unwrap();

    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_session_jsonl_tree",
            ProviderImportSupport::Native,
            &sessions,
        ),
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();
    super::super::set_after_codex_session_tree_stage_hook(move |_| {
        fs::remove_file(selected).unwrap();
    });

    let error = refresh_source_backed_generation(
        &index,
        &registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap_err();
    assert_cold_route_failure(error, SourceBackedSourceFailureClass::SourceChanged);
    assert!(VerifiedIndex::open(&index).is_err());
}

#[test]
fn active_source_family_contract_codex_tree_rejects_deleted_source_reappearance() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let nested = sessions.join("nested");
    let index = temp.path().join("index");
    fs::create_dir_all(&nested).unwrap();
    let native_session_id = "019facf0-3333-7777-8888-000000000010";
    let selected = nested.join(format!("rollout-{native_session_id}.jsonl"));
    fs::write(
        &selected,
        codex_rollout_bytes(native_session_id, &["deletionbasemarker"]),
    )
    .unwrap();

    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_session_jsonl_tree",
            ProviderImportSupport::Native,
            &sessions,
        ),
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();
    let options = WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    };
    let seeded = refresh_source_backed_generation(&index, &registry, options.clone()).unwrap();
    let seeded_generation = seeded.commit.generation_id.clone();
    fs::remove_file(&selected).unwrap();

    let recreate_path = selected.clone();
    super::super::set_after_codex_session_tree_stage_hook(move |_| {
        fs::write(
            recreate_path,
            codex_rollout_bytes(native_session_id, &["reappearedsourcemarker"]),
        )
        .unwrap();
    });
    let failed = refresh_source_backed_generation(&index, &registry, options.clone()).unwrap();
    assert_carried_route_failure(
        &failed,
        &seeded_generation,
        SourceBackedSourceFailureClass::SourceChanged,
    );
    let preserved = VerifiedIndex::open(&index).unwrap();
    assert_eq!(preserved.generation_id(), seeded_generation);
    assert_eq!(
        preserved
            .search_event_candidates("deletionbasemarker", 8)
            .unwrap()
            .len(),
        1
    );
    assert!(preserved
        .search_event_candidates("reappearedsourcemarker", 8)
        .unwrap()
        .is_empty());

    let recovered = refresh_source_backed_generation(&index, &registry, options).unwrap();
    assert_ne!(recovered.commit.generation_id, seeded_generation);
    assert_eq!(
        VerifiedIndex::open(&index)
            .unwrap()
            .search_event_candidates("reappearedsourcemarker", 8)
            .unwrap()
            .len(),
        1
    );
}

#[cfg(unix)]
#[test]
fn active_source_family_contract_codex_tree_rejects_root_replacement_with_same_leaf() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let moved_sessions = temp.path().join("moved-sessions");
    let replacement = temp.path().join("replacement");
    let index = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    fs::create_dir_all(&replacement).unwrap();
    let native_session_id = "019facf0-3333-7777-8888-000000000011";
    let file_name = format!("rollout-{native_session_id}.jsonl");
    let selected = sessions.join(&file_name);
    fs::write(
        &selected,
        codex_rollout_bytes(native_session_id, &["retainedrootmarker"]),
    )
    .unwrap();
    fs::hard_link(&selected, replacement.join(&file_name)).unwrap();

    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_session_jsonl_tree",
            ProviderImportSupport::Native,
            &sessions,
        ),
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();

    let replace_sessions = sessions.clone();
    super::super::set_after_codex_session_tree_stage_hook(move |_| {
        fs::rename(&replace_sessions, moved_sessions).unwrap();
        fs::rename(replacement, replace_sessions).unwrap();
    });
    let error = refresh_source_backed_generation(
        &index,
        &registry,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap_err();
    assert_cold_route_failure(error, SourceBackedSourceFailureClass::SourceChanged);
    assert!(VerifiedIndex::open(&index).is_err());
}
