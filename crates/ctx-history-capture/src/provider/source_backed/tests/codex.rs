use std::{fs::OpenOptions, io::Write};

#[cfg(target_os = "linux")]
use std::{
    process::Command,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    thread,
    time::Duration,
};

use ctx_history_core::{EventOrigin, SessionRelationshipKind};

use super::*;
use crate::provider::codex::nativepath::install_after_codex_lineage_normalization_hook_v0;

#[cfg(target_os = "linux")]
const CODEX_FD_BUDGET_CHILD_ENV: &str = "CTX_TEST_CODEX_FD_BUDGET_CHILD";
#[cfg(target_os = "linux")]
const CODEX_FD_BUDGET_TEST: &str =
    "provider::source_backed::tests::codex::codex_generation_bounds_leaf_fds_under_soft_nofile_1024";

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
    codex_lineage_rollout_with_events(
        native_session_id,
        parent_native_session_id,
        relationship,
        advisory_session_id,
        &[serde_json::json!({
            "timestamp": "2026-08-06T12:00:01Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": marker}]
            }
        })],
    )
}

fn codex_lineage_rollout_with_events(
    native_session_id: &str,
    parent_native_session_id: Option<&str>,
    relationship: SessionRelationshipKind,
    advisory_session_id: Option<&str>,
    events: &[serde_json::Value],
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
    std::iter::once(serde_json::json!({
        "timestamp": "2026-08-06T12:00:00Z",
        "type": "session_meta",
        "payload": payload,
    }))
    .chain(events.iter().cloned())
    .flat_map(|record| {
        let mut line = serde_json::to_vec(&record).unwrap();
        line.push(b'\n');
        line
    })
    .collect()
}

fn codex_lineage_call(call_id: &str, command: &str) -> serde_json::Value {
    serde_json::json!({
        "timestamp": "2026-08-06T11:59:58Z",
        "type": "response_item",
        "payload": {
            "type": "function_call",
            "name": "exec_command",
            "call_id": call_id,
            "arguments": serde_json::json!({
                "cmd": command,
                "workdir": "/tmp/root-normalization",
                "yield_time_ms": 10000
            }).to_string()
        }
    })
}

fn codex_lineage_result(call_id: &str, output: &str) -> serde_json::Value {
    serde_json::json!({
        "timestamp": "2026-08-06T11:59:59Z",
        "type": "response_item",
        "payload": {
            "type": "function_call_output",
            "call_id": call_id,
            "status": "success",
            "output": output
        }
    })
}

fn codex_dense_lineage_events(component: usize, pairs: usize) -> Vec<serde_json::Value> {
    (0..pairs)
        .flat_map(|pair| {
            let call_id = format!("dense-component-{component:02}-call-{pair:03}");
            [
                codex_lineage_call(&call_id, &format!("printf dense-{component:02}-{pair:03}")),
                codex_lineage_result(
                    &call_id,
                    &format!("dense-component-{component:02}-result-{pair:03}"),
                ),
            ]
        })
        .collect()
}

fn register_codex_route(
    registry: &mut SourceBackedProviderRegistry,
    path: &Path,
    source_format: &'static str,
    import_support: ProviderImportSupport,
    selection: SourceBackedRouteSelection,
) {
    register_landed_source_backed_route(
        registry,
        fixture_provider_source_at(CaptureProvider::Codex, source_format, import_support, path),
        selection,
    )
    .unwrap();
}

fn assert_copied_result(
    records: &[CoreRecord],
    native_session_id: &str,
    output_marker: &str,
) -> CoreRecord {
    let record = records
        .iter()
        .find(|record| {
            record.provider_session_id.as_deref() == Some(native_session_id)
                && record
                    .content
                    .normalized_body
                    .as_deref()
                    .is_some_and(|body| body.contains(output_marker))
        })
        .unwrap_or_else(|| {
            panic!("missing copied result {output_marker} in session {native_session_id}")
        });
    assert!(matches!(
        record.event_origin,
        EventOrigin::CopiedFromAncestor { .. }
    ));
    record.clone()
}

fn register_codex_tree(sessions: &Path) -> SourceBackedProviderRegistry {
    register_codex_trees(&[(sessions, ProviderImportSupport::Native)])
}

fn append_codex_lineage_message(path: &Path, native_session_id: &str, marker: &str) {
    let bytes = codex_lineage_rollout(
        native_session_id,
        None,
        SessionRelationshipKind::Root,
        None,
        marker,
    );
    OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(bytes.split_inclusive(|byte| *byte == b'\n').nth(1).unwrap())
        .unwrap();
}

fn route_identity_for_path(
    registry: &SourceBackedProviderRegistry,
    path: &Path,
) -> SourceRouteIdentity {
    registry
        .routes()
        .find(|route| route.source.path == path)
        .and_then(|route| route.route_identity.clone())
        .unwrap()
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

#[cfg(target_os = "linux")]
fn set_soft_nofile_limit(limit: libc::rlim_t) {
    let mut current = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: this runs only in the isolated child process below, before any
    // refresh worker starts. The child exits without restoring its soft limit.
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut current) },
        0
    );
    assert!(
        current.rlim_max >= limit,
        "hard RLIMIT_NOFILE {} is below required test limit {limit}",
        current.rlim_max
    );
    current.rlim_cur = limit;
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &current) }, 0);
    let mut observed = current;
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut observed) },
        0
    );
    assert_eq!(observed.rlim_cur, limit);
}

#[cfg(target_os = "linux")]
fn open_fd_count() -> usize {
    fs::read_dir("/proc/self/fd").unwrap().count()
}

#[cfg(target_os = "linux")]
fn run_codex_fd_budget_child() {
    const TREE_SOURCES: usize = 2_048;
    const EXPLICIT_SOURCES: usize = 16;
    const SOFT_NOFILE: libc::rlim_t = 1_024;
    const MAX_OPEN_FDS: usize = 256;

    set_soft_nofile_limit(SOFT_NOFILE);
    let temp = tempdir().unwrap();
    let tree = temp.path().join("automatic-tree");
    let explicit = temp.path().join("explicit-routes");
    let index = temp.path().join("index");
    fs::create_dir_all(&tree).unwrap();
    fs::create_dir_all(&explicit).unwrap();
    for source in 0..TREE_SOURCES {
        let native_session_id = format!("019fb000-0000-7000-8000-{source:012x}");
        fs::write(
            tree.join(format!("rollout-{native_session_id}.jsonl")),
            codex_lineage_rollout(
                &native_session_id,
                None,
                SessionRelationshipKind::Root,
                None,
                "bounded automatic leaf",
            ),
        )
        .unwrap();
    }
    let mut registry = register_codex_tree(&tree);
    for source in 0..EXPLICIT_SOURCES {
        let native_session_id = format!("019fb001-0000-7000-8000-{source:012x}");
        let path = explicit.join(format!("route-{source:02}.jsonl"));
        fs::write(
            &path,
            codex_lineage_rollout(
                &native_session_id,
                None,
                SessionRelationshipKind::Root,
                None,
                "bounded explicit leaf",
            ),
        )
        .unwrap();
        register_codex_route(
            &mut registry,
            &path,
            "codex_session_jsonl",
            ProviderImportSupport::Explicit,
            SourceBackedRouteSelection::ExplicitManual,
        );
    }

    let baseline = open_fd_count();
    let sampling = Arc::new(AtomicBool::new(true));
    let peak = Arc::new(AtomicUsize::new(baseline));
    let sampling_from_thread = Arc::clone(&sampling);
    let peak_from_thread = Arc::clone(&peak);
    let monitor = thread::spawn(move || {
        while sampling_from_thread.load(Ordering::Acquire) {
            peak_from_thread.fetch_max(open_fd_count(), Ordering::Relaxed);
            thread::sleep(Duration::from_millis(1));
        }
        peak_from_thread.fetch_max(open_fd_count(), Ordering::Relaxed);
    });
    let writer_options = WriterOptions {
        indexer_threads: 1,
        ..WriterOptions::default()
    };
    let refreshed = super::super::family::jsonl::with_family_scanner_workers(16, || {
        refresh_source_backed_generation_with_work_budget_for_test(
            &index,
            &registry,
            writer_options,
            16,
        )
    });
    let scanner_workers = super::super::family::jsonl::jsonl_family_scanner_max_worker_count();
    sampling.store(false, Ordering::Release);
    monitor.join().unwrap();

    let refreshed = refreshed.unwrap();
    let high_water = peak.load(Ordering::Relaxed);
    assert!(refreshed.failed_routes.is_empty());
    assert_eq!(
        scanner_workers, 16,
        "FD regression did not exercise 16 workers"
    );
    assert_eq!(
        refreshed.certified_source_count,
        TREE_SOURCES + EXPLICIT_SOURCES
    );
    assert!(
        high_water <= MAX_OPEN_FDS,
        "Codex refresh FD high-water {high_water} exceeded bound {MAX_OPEN_FDS} (baseline {baseline})"
    );
    eprintln!(
        "CODEX_FD_BUDGET_RECEIPT soft_nofile={SOFT_NOFILE} sources={} routes={} scanner_workers={scanner_workers} baseline_fds={baseline} peak_fds={high_water} bound={MAX_OPEN_FDS}",
        TREE_SOURCES + EXPLICIT_SOURCES,
        EXPLICIT_SOURCES + 1,
    );
}

#[cfg(target_os = "linux")]
#[test]
fn codex_generation_bounds_leaf_fds_under_soft_nofile_1024() {
    if std::env::var_os(CODEX_FD_BUDGET_CHILD_ENV).is_some() {
        run_codex_fd_budget_child();
        return;
    }
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            CODEX_FD_BUDGET_TEST,
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CODEX_FD_BUDGET_CHILD_ENV, "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "soft-NOFILE Codex refresh child failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    eprint!("{}", String::from_utf8_lossy(&output.stdout));
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
}

#[test]
fn codex_distinct_automatic_and_explicit_files_with_one_native_id_are_quarantined() {
    let temp = tempdir().unwrap();
    let automatic = temp.path().join("automatic");
    let explicit = temp.path().join("explicit.jsonl");
    let index = temp.path().join("index");
    fs::create_dir_all(&automatic).unwrap();
    let duplicate = "019fa000-0000-7000-8000-000000003290";
    let valid = "019fa000-0000-7000-8000-000000003291";
    fs::write(
        automatic.join(format!("rollout-{duplicate}.jsonl")),
        codex_lineage_rollout(
            duplicate,
            None,
            SessionRelationshipKind::Root,
            None,
            "automatic duplicate must be quarantined",
        ),
    )
    .unwrap();
    fs::write(
        &explicit,
        codex_lineage_rollout(
            duplicate,
            None,
            SessionRelationshipKind::Root,
            None,
            "explicit duplicate must be quarantined",
        ),
    )
    .unwrap();
    fs::write(
        automatic.join(format!("rollout-{valid}.jsonl")),
        codex_lineage_rollout(
            valid,
            None,
            SessionRelationshipKind::Root,
            None,
            "unrelated valid component",
        ),
    )
    .unwrap();

    let mut registry = SourceBackedProviderRegistry::new();
    register_codex_route(
        &mut registry,
        &automatic,
        "codex_session_jsonl_tree",
        ProviderImportSupport::Native,
        SourceBackedRouteSelection::Automatic,
    );
    register_codex_route(
        &mut registry,
        &explicit,
        "codex_session_jsonl",
        ProviderImportSupport::Explicit,
        SourceBackedRouteSelection::ExplicitManual,
    );
    let observed = Arc::new(Mutex::new(None));
    let observed_from_hook = Arc::clone(&observed);
    install_after_codex_lineage_normalization_hook_v0(move |observation| {
        *observed_from_hook.lock().unwrap() = Some(observation);
    });

    let refreshed =
        refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
    let observation = observed.lock().unwrap().clone().unwrap();
    assert_eq!(observation.valid_sources, 1);
    assert_eq!(observation.rejected_sources, 2);
    assert_eq!(observation.worker_starts_at_normalization, 0);
    assert_eq!(observation.worker_start_latch.starts(), 1);
    assert_eq!(refreshed.successful_route_ids.len(), 1);
    assert_eq!(refreshed.failed_routes.len(), 1);
    assert_eq!(
        refreshed.failed_routes[0].class,
        SourceBackedSourceFailureClass::Unreadable
    );
    let records = core_records(&VerifiedIndex::open(&index).unwrap());
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].content.normalized_body.as_deref(),
        Some("unrelated valid component")
    );
}

#[test]
fn codex_explicit_parent_facts_precede_automatic_child_regardless_of_route_order() {
    let temp = tempdir().unwrap();
    let automatic = temp.path().join("automatic-child");
    let explicit_parent = temp.path().join("explicit-parent.jsonl");
    let index = temp.path().join("index");
    fs::create_dir_all(&automatic).unwrap();
    let parent = "019fa000-0000-7000-8000-000000003292";
    let child = "019fa000-0000-7000-8000-000000003293";
    let call = codex_lineage_call("call-explicit-parent", "git rev-parse --verify HEAD");
    let result = codex_lineage_result(
        "call-explicit-parent",
        "explicit-parent-copy-output aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    );
    fs::write(
        &explicit_parent,
        codex_lineage_rollout_with_events(
            parent,
            None,
            SessionRelationshipKind::Root,
            None,
            &[call.clone(), result.clone()],
        ),
    )
    .unwrap();
    fs::write(
        automatic.join(format!("rollout-{child}.jsonl")),
        codex_lineage_rollout_with_events(
            child,
            Some(parent),
            SessionRelationshipKind::Forked,
            Some(parent),
            &[call, result],
        ),
    )
    .unwrap();

    let mut registry = SourceBackedProviderRegistry::new();
    // Register the automatic child first to exercise the formerly failing
    // production order: its route scans before the explicit parent route.
    register_codex_route(
        &mut registry,
        &automatic,
        "codex_session_jsonl_tree",
        ProviderImportSupport::Native,
        SourceBackedRouteSelection::Automatic,
    );
    register_codex_route(
        &mut registry,
        &explicit_parent,
        "codex_session_jsonl",
        ProviderImportSupport::Explicit,
        SourceBackedRouteSelection::ExplicitManual,
    );
    let refreshed =
        refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
    assert!(refreshed.failed_routes.is_empty());
    let records = core_records(&VerifiedIndex::open(&index).unwrap());
    let copied = assert_copied_result(&records, child, "explicit-parent-copy-output");
    let parent_record = records
        .iter()
        .find(|record| record.provider_session_id.as_deref() == Some(parent))
        .unwrap();
    assert_eq!(copied.parent_session_id, Some(parent_record.session_id));
    assert_eq!(copied.root_session_id, parent_record.session_id);
}

#[test]
fn codex_exact_route_composes_carried_parent_authority_without_reparsing_it() {
    for automatic_parent in [true, false] {
        let temp = tempdir().unwrap();
        let parent_dir = temp.path().join("automatic-parent");
        let child_dir = temp.path().join("automatic-child");
        fs::create_dir_all(&parent_dir).unwrap();
        fs::create_dir_all(&child_dir).unwrap();
        let parent_path = if automatic_parent {
            parent_dir.join("parent.jsonl")
        } else {
            temp.path().join("explicit-parent.jsonl")
        };
        let child_path = if automatic_parent {
            temp.path().join("explicit-child.jsonl")
        } else {
            child_dir.join("child.jsonl")
        };
        let parent = if automatic_parent {
            "019fa000-0000-7000-8000-000000003310"
        } else {
            "019fa000-0000-7000-8000-000000003312"
        };
        let child = if automatic_parent {
            "019fa000-0000-7000-8000-000000003311"
        } else {
            "019fa000-0000-7000-8000-000000003313"
        };
        let call_id = format!("exact-carried-parent-{automatic_parent}");
        let call = codex_lineage_call(&call_id, "git rev-parse --verify HEAD");
        let result = codex_lineage_result(&call_id, "exact carried parent output");
        fs::write(
            &parent_path,
            codex_lineage_rollout_with_events(
                parent,
                None,
                SessionRelationshipKind::Root,
                None,
                &[call.clone(), result.clone()],
            ),
        )
        .unwrap();
        fs::write(
            &child_path,
            codex_lineage_rollout_with_events(
                child,
                Some(parent),
                SessionRelationshipKind::Forked,
                Some(parent),
                &[call, result],
            ),
        )
        .unwrap();

        let mut registry = SourceBackedProviderRegistry::new();
        let register_parent = |registry: &mut SourceBackedProviderRegistry| {
            register_codex_route(
                registry,
                if automatic_parent {
                    &parent_dir
                } else {
                    &parent_path
                },
                if automatic_parent {
                    "codex_session_jsonl_tree"
                } else {
                    "codex_session_jsonl"
                },
                if automatic_parent {
                    ProviderImportSupport::Native
                } else {
                    ProviderImportSupport::Explicit
                },
                if automatic_parent {
                    SourceBackedRouteSelection::Automatic
                } else {
                    SourceBackedRouteSelection::ExplicitManual
                },
            );
        };
        let register_child = |registry: &mut SourceBackedProviderRegistry| {
            register_codex_route(
                registry,
                if automatic_parent {
                    &child_path
                } else {
                    &child_dir
                },
                if automatic_parent {
                    "codex_session_jsonl"
                } else {
                    "codex_session_jsonl_tree"
                },
                if automatic_parent {
                    ProviderImportSupport::Explicit
                } else {
                    ProviderImportSupport::Native
                },
                if automatic_parent {
                    SourceBackedRouteSelection::ExplicitManual
                } else {
                    SourceBackedRouteSelection::Automatic
                },
            );
        };
        if automatic_parent {
            register_parent(&mut registry);
            register_child(&mut registry);
        } else {
            register_child(&mut registry);
            register_parent(&mut registry);
        }
        let index = temp.path().join("index");
        refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
        let child_route = route_identity_for_path(
            &registry,
            if automatic_parent {
                &child_path
            } else {
                &child_dir
            },
        );
        append_codex_lineage_message(&child_path, child, "dirty child suffix");
        let observed = Arc::new(Mutex::new(None));
        let observed_from_hook = Arc::clone(&observed);
        install_after_codex_lineage_normalization_hook_v0(move |observation| {
            *observed_from_hook.lock().unwrap() = Some(observation);
        });
        let refreshed = refresh_source_backed_generation_for_routes(
            &index,
            &registry,
            WriterOptions::default(),
            [child_route],
        )
        .unwrap();
        assert!(refreshed.failed_routes.is_empty());
        assert_eq!(
            observed
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .lineage_fact_source_scans,
            0
        );
        let records = core_records(&VerifiedIndex::open(&index).unwrap());
        assert_copied_result(&records, child, "exact carried parent output");
    }
}

#[cfg(unix)]
#[test]
fn codex_exact_route_ignores_unrelated_carried_replacement_after_preparation() {
    let temp = tempdir().unwrap();
    let parent_path = temp.path().join("parent.jsonl");
    let child_path = temp.path().join("child.jsonl");
    let unrelated_path = temp.path().join("unrelated.jsonl");
    let replacement_path = temp.path().join("unrelated-replacement");
    let moved_path = temp.path().join("unrelated-prepared");
    let index = temp.path().join("index");
    let parent = "019fa000-0000-7000-8000-000000003320";
    let child = "019fa000-0000-7000-8000-000000003321";
    let unrelated = "019fa000-0000-7000-8000-000000003322";
    let call = codex_lineage_call("exact-scope-parent", "git rev-parse --verify HEAD");
    let result = codex_lineage_result("exact-scope-parent", "exact scope parent output");
    fs::write(
        &parent_path,
        codex_lineage_rollout_with_events(
            parent,
            None,
            SessionRelationshipKind::Root,
            None,
            &[call.clone(), result.clone()],
        ),
    )
    .unwrap();
    fs::write(
        &child_path,
        codex_lineage_rollout_with_events(
            child,
            Some(parent),
            SessionRelationshipKind::Forked,
            Some(parent),
            &[call, result],
        ),
    )
    .unwrap();
    let unrelated_old = codex_lineage_rollout(
        unrelated,
        None,
        SessionRelationshipKind::Root,
        None,
        "unrelated carried old aa",
    );
    let unrelated_new = codex_lineage_rollout(
        unrelated,
        None,
        SessionRelationshipKind::Root,
        None,
        "unrelated carried new bb",
    );
    assert_eq!(unrelated_old.len(), unrelated_new.len());
    fs::write(&unrelated_path, unrelated_old).unwrap();
    fs::write(&replacement_path, unrelated_new).unwrap();

    let mut registry = SourceBackedProviderRegistry::new();
    for path in [&unrelated_path, &parent_path, &child_path] {
        register_codex_route(
            &mut registry,
            path,
            "codex_session_jsonl",
            ProviderImportSupport::Explicit,
            SourceBackedRouteSelection::ExplicitManual,
        );
    }
    let cold =
        refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
    assert!(cold.failed_routes.is_empty());
    let child_route = route_identity_for_path(&registry, &child_path);
    append_codex_lineage_message(&child_path, child, "selected child exact suffix");

    let unrelated_from_hook = unrelated_path.clone();
    install_after_codex_lineage_normalization_hook_v0(move |_| {
        fs::rename(&unrelated_from_hook, moved_path).unwrap();
        fs::rename(replacement_path, unrelated_from_hook).unwrap();
    });
    let refreshed = refresh_source_backed_generation_for_routes(
        &index,
        &registry,
        WriterOptions::default(),
        [child_route.clone()],
    )
    .unwrap();
    assert!(refreshed.failed_routes.is_empty());
    assert_eq!(refreshed.successful_route_ids, vec![child_route]);
    assert_ne!(refreshed.commit.generation_id, cold.commit.generation_id);

    let records = core_records(&VerifiedIndex::open(&index).unwrap());
    assert!(records.iter().any(|record| {
        record.provider_session_id.as_deref() == Some(child)
            && record.content.normalized_body.as_deref() == Some("selected child exact suffix")
    }));
    assert!(records.iter().any(|record| {
        record.provider_session_id.as_deref() == Some(unrelated)
            && record.content.normalized_body.as_deref() == Some("unrelated carried old aa")
    }));
    assert!(!records.iter().any(|record| {
        record.provider_session_id.as_deref() == Some(unrelated)
            && record.content.normalized_body.as_deref() == Some("unrelated carried new bb")
    }));
}

#[cfg(unix)]
#[test]
fn codex_exact_route_rejects_participating_replacement_after_preparation() {
    for replace_parent in [true, false] {
        let temp = tempdir().unwrap();
        let parent_path = temp.path().join("parent.jsonl");
        let child_path = temp.path().join("child.jsonl");
        let replacement_path = temp.path().join("replacement");
        let moved_path = temp.path().join("prepared");
        let index = temp.path().join("index");
        let parent = "019fa000-0000-7000-8000-000000003323";
        let child = "019fa000-0000-7000-8000-000000003324";
        let call_id = format!("exact-participant-parent-{replace_parent}");
        let call = codex_lineage_call(&call_id, "git rev-parse --verify HEAD");
        let result = codex_lineage_result(&call_id, "exact participant parent output");
        fs::write(
            &parent_path,
            codex_lineage_rollout_with_events(
                parent,
                None,
                SessionRelationshipKind::Root,
                None,
                &[call.clone(), result.clone()],
            ),
        )
        .unwrap();
        fs::write(
            &child_path,
            codex_lineage_rollout_with_events(
                child,
                Some(parent),
                SessionRelationshipKind::Forked,
                Some(parent),
                &[call, result],
            ),
        )
        .unwrap();

        let mut registry = SourceBackedProviderRegistry::new();
        for path in [&parent_path, &child_path] {
            register_codex_route(
                &mut registry,
                path,
                "codex_session_jsonl",
                ProviderImportSupport::Explicit,
                SourceBackedRouteSelection::ExplicitManual,
            );
        }
        let cold =
            refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
        assert!(cold.failed_routes.is_empty());
        let parent_route = route_identity_for_path(&registry, &parent_path);
        let child_route = route_identity_for_path(&registry, &child_path);
        append_codex_lineage_message(&child_path, child, "dirty selected participant suffix");

        let target = if replace_parent {
            &parent_path
        } else {
            &child_path
        };
        fs::write(&replacement_path, fs::read(target).unwrap()).unwrap();
        let target_from_hook = target.clone();
        install_after_codex_lineage_normalization_hook_v0(move |_| {
            fs::rename(&target_from_hook, moved_path).unwrap();
            fs::rename(replacement_path, target_from_hook).unwrap();
        });
        let rejected = refresh_source_backed_generation_for_routes(
            &index,
            &registry,
            WriterOptions::default(),
            [child_route.clone()],
        )
        .unwrap();
        assert_eq!(rejected.failed_routes.len(), 1);
        assert_eq!(
            rejected.failed_routes[0].class,
            SourceBackedSourceFailureClass::SourceChanged
        );
        assert_eq!(
            VerifiedIndex::open(&index).unwrap().generation_id(),
            cold.commit.generation_id
        );

        if replace_parent {
            let ancestor_retried = refresh_source_backed_generation_for_routes(
                &index,
                &registry,
                WriterOptions::default(),
                [parent_route.clone()],
            )
            .unwrap();
            assert!(ancestor_retried.failed_routes.is_empty());
            assert_eq!(ancestor_retried.successful_route_ids, vec![parent_route]);
            assert!(!core_records(&VerifiedIndex::open(&index).unwrap())
                .iter()
                .any(|record| {
                    record.provider_session_id.as_deref() == Some(child)
                        && record.content.normalized_body.as_deref()
                            == Some("dirty selected participant suffix")
                }));
        }
        let retried = refresh_source_backed_generation_for_routes(
            &index,
            &registry,
            WriterOptions::default(),
            [child_route.clone()],
        )
        .unwrap();
        assert!(retried.failed_routes.is_empty());
        assert_eq!(retried.successful_route_ids, vec![child_route]);
        assert!(core_records(&VerifiedIndex::open(&index).unwrap())
            .iter()
            .any(|record| {
                record.provider_session_id.as_deref() == Some(child)
                    && record.content.normalized_body.as_deref()
                        == Some("dirty selected participant suffix")
            }));
    }
}

fn register_three_level_codex_routes(
    registry: &mut SourceBackedProviderRegistry,
    automatic: &Path,
    middle: &Path,
    reverse: bool,
) {
    let register_automatic = |registry: &mut SourceBackedProviderRegistry| {
        register_codex_route(
            registry,
            automatic,
            "codex_session_jsonl_tree",
            ProviderImportSupport::Native,
            SourceBackedRouteSelection::Automatic,
        );
    };
    let register_middle = |registry: &mut SourceBackedProviderRegistry| {
        register_codex_route(
            registry,
            middle,
            "codex_session_jsonl",
            ProviderImportSupport::Explicit,
            SourceBackedRouteSelection::ExplicitManual,
        );
    };
    if reverse {
        register_middle(registry);
        register_automatic(registry);
    } else {
        register_automatic(registry);
        register_middle(registry);
    }
}

#[test]
fn codex_three_level_cross_route_output_is_registration_order_independent() {
    let temp = tempdir().unwrap();
    let automatic = temp.path().join("automatic-root-and-grandchild");
    let explicit_middle = temp.path().join("explicit-middle.jsonl");
    fs::create_dir_all(&automatic).unwrap();
    let root = "019fa000-0000-7000-8000-000000003294";
    let middle = "019fa000-0000-7000-8000-000000003295";
    let grandchild = "019fa000-0000-7000-8000-000000003296";
    let root_call = codex_lineage_call("call-three-level-root", "git rev-parse --verify HEAD");
    let root_result = codex_lineage_result(
        "call-three-level-root",
        "three-level-root-output bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    );
    let middle_call = codex_lineage_call("call-three-level-middle", "git rev-parse --verify HEAD");
    let middle_result = codex_lineage_result(
        "call-three-level-middle",
        "three-level-middle-output cccccccccccccccccccccccccccccccccccccccc",
    );
    fs::write(
        automatic.join(format!("rollout-{root}.jsonl")),
        codex_lineage_rollout_with_events(
            root,
            None,
            SessionRelationshipKind::Root,
            None,
            &[root_call.clone(), root_result.clone()],
        ),
    )
    .unwrap();
    fs::write(
        &explicit_middle,
        codex_lineage_rollout_with_events(
            middle,
            Some(root),
            SessionRelationshipKind::Forked,
            Some(root),
            &[
                root_call.clone(),
                root_result.clone(),
                middle_call.clone(),
                middle_result.clone(),
            ],
        ),
    )
    .unwrap();
    fs::write(
        automatic.join(format!("rollout-{grandchild}.jsonl")),
        codex_lineage_rollout_with_events(
            grandchild,
            Some(middle),
            SessionRelationshipKind::Forked,
            Some(root),
            &[root_call, root_result, middle_call, middle_result],
        ),
    )
    .unwrap();

    let mut forward = SourceBackedProviderRegistry::new();
    register_three_level_codex_routes(&mut forward, &automatic, &explicit_middle, false);
    let mut reversed = SourceBackedProviderRegistry::new();
    register_three_level_codex_routes(&mut reversed, &automatic, &explicit_middle, true);
    let forward_index = temp.path().join("forward-index");
    let reversed_index = temp.path().join("reversed-index");
    let forward_receipt =
        refresh_source_backed_generation(&forward_index, &forward, WriterOptions::default())
            .unwrap();
    let reversed_receipt =
        refresh_source_backed_generation(&reversed_index, &reversed, WriterOptions::default())
            .unwrap();
    assert!(forward_receipt.failed_routes.is_empty());
    assert!(reversed_receipt.failed_routes.is_empty());

    let mut forward_records = core_records(&VerifiedIndex::open(&forward_index).unwrap());
    let mut reversed_records = core_records(&VerifiedIndex::open(&reversed_index).unwrap());
    assert_copied_result(&forward_records, grandchild, "three-level-middle-output");
    assert_copied_result(&reversed_records, grandchild, "three-level-middle-output");
    forward_records.sort_by_key(|record| record.event_id.to_string());
    reversed_records.sort_by_key(|record| record.event_id.to_string());
    assert_eq!(forward_records, reversed_records);
}

#[test]
fn codex_exact_leaf_uses_three_level_carried_authority_and_missing_parent_fails() {
    let temp = tempdir().unwrap();
    let root_dir = temp.path().join("root-route");
    let leaf_dir = temp.path().join("leaf-route");
    fs::create_dir_all(&root_dir).unwrap();
    fs::create_dir_all(&leaf_dir).unwrap();
    let root_path = root_dir.join("root.jsonl");
    let middle_path = temp.path().join("middle.jsonl");
    let leaf_path = leaf_dir.join("leaf.jsonl");
    let root = "019fa000-0000-7000-8000-000000003314";
    let middle = "019fa000-0000-7000-8000-000000003315";
    let leaf = "019fa000-0000-7000-8000-000000003316";
    let root_call = codex_lineage_call("exact-three-root", "git rev-parse --verify HEAD");
    let root_result = codex_lineage_result("exact-three-root", "exact three root output");
    let middle_call = codex_lineage_call("exact-three-middle", "git rev-parse --verify HEAD");
    let middle_result = codex_lineage_result("exact-three-middle", "exact three middle output");
    fs::write(
        &root_path,
        codex_lineage_rollout_with_events(
            root,
            None,
            SessionRelationshipKind::Root,
            None,
            &[root_call.clone(), root_result.clone()],
        ),
    )
    .unwrap();
    fs::write(
        &middle_path,
        codex_lineage_rollout_with_events(
            middle,
            Some(root),
            SessionRelationshipKind::Forked,
            Some(root),
            &[
                root_call.clone(),
                root_result.clone(),
                middle_call.clone(),
                middle_result.clone(),
            ],
        ),
    )
    .unwrap();
    fs::write(
        &leaf_path,
        codex_lineage_rollout_with_events(
            leaf,
            Some(middle),
            SessionRelationshipKind::Forked,
            Some(root),
            &[root_call, root_result, middle_call, middle_result],
        ),
    )
    .unwrap();

    let mut registry = SourceBackedProviderRegistry::new();
    // Reverse topological registration is intentional.
    register_codex_route(
        &mut registry,
        &leaf_dir,
        "codex_session_jsonl_tree",
        ProviderImportSupport::Native,
        SourceBackedRouteSelection::Automatic,
    );
    register_codex_route(
        &mut registry,
        &middle_path,
        "codex_session_jsonl",
        ProviderImportSupport::Explicit,
        SourceBackedRouteSelection::ExplicitManual,
    );
    register_codex_route(
        &mut registry,
        &root_dir,
        "codex_session_jsonl_tree",
        ProviderImportSupport::Explicit,
        SourceBackedRouteSelection::ExplicitManual,
    );
    let leaf_route = route_identity_for_path(&registry, &leaf_dir);
    let index = temp.path().join("index");
    let cold =
        refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
    append_codex_lineage_message(&leaf_path, leaf, "dirty three-level leaf");
    let observed = Arc::new(Mutex::new(None));
    let observed_from_hook = Arc::clone(&observed);
    install_after_codex_lineage_normalization_hook_v0(move |observation| {
        *observed_from_hook.lock().unwrap() = Some(observation);
    });
    let refreshed = refresh_source_backed_generation_for_routes(
        &index,
        &registry,
        WriterOptions::default(),
        [leaf_route.clone()],
    )
    .unwrap();
    assert!(refreshed.failed_routes.is_empty());
    assert_eq!(
        observed
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .lineage_fact_source_scans,
        0
    );
    assert_copied_result(
        &core_records(&VerifiedIndex::open(&index).unwrap()),
        leaf,
        "exact three middle output",
    );

    fs::remove_file(&root_path).unwrap();
    append_codex_lineage_message(&leaf_path, leaf, "dirty leaf after parent deletion");
    let failed = refresh_source_backed_generation_for_routes(
        &index,
        &registry,
        WriterOptions::default(),
        [leaf_route],
    )
    .unwrap();
    assert_eq!(failed.failed_routes.len(), 1);
    assert_eq!(
        VerifiedIndex::open(&index).unwrap().generation_id(),
        refreshed.commit.generation_id
    );
    assert_ne!(cold.commit.generation_id, refreshed.commit.generation_id);
}

#[test]
fn codex_generation_spills_more_than_sixteen_near_budget_components_four_at_a_time() {
    const COMPONENTS: usize = 17;
    const FACT_PAIRS: usize = 20;
    const BYTE_LIMIT: usize = 2_300;
    const FACT_LIMIT: usize = 44;

    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    for component in 0..COMPONENTS {
        let root = format!("019fa100-0000-7000-8000-{component:012x}");
        let child = format!("019fa101-0000-7000-8000-{component:012x}");
        let events = codex_dense_lineage_events(component, FACT_PAIRS);
        fs::write(
            sessions.join(format!("rollout-{root}.jsonl")),
            codex_lineage_rollout_with_events(
                &root,
                None,
                SessionRelationshipKind::Root,
                None,
                &events,
            ),
        )
        .unwrap();
        fs::write(
            sessions.join(format!("rollout-{child}.jsonl")),
            codex_lineage_rollout_with_events(
                &child,
                Some(&root),
                SessionRelationshipKind::Forked,
                Some(&root),
                &events,
            ),
        )
        .unwrap();
    }

    let registry = register_codex_tree(&sessions);
    registry
        .codex_generation
        .as_ref()
        .unwrap()
        .set_generation_lineage_budget_limits(BYTE_LIMIT, FACT_LIMIT);
    let observed = Arc::new(Mutex::new(None));
    let observed_from_hook = Arc::clone(&observed);
    install_after_codex_lineage_normalization_hook_v0(move |observation| {
        *observed_from_hook.lock().unwrap() = Some(observation);
    });

    let refreshed =
        refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
    assert!(refreshed.failed_routes.is_empty());
    let observation = observed.lock().unwrap().clone().unwrap();
    assert_eq!(observation.lineage_fact_source_scans, COMPONENTS as u64);
    assert_eq!(observation.worker_starts_at_normalization, 0);
    assert_eq!(
        observation.worker_start_latch.starts(),
        (COMPONENTS * 2) as u64
    );
    let (active, peak, current_bytes, peak_bytes, component_loads) = registry
        .codex_generation
        .as_ref()
        .unwrap()
        .generation_lineage_metrics()
        .unwrap();
    assert_eq!(active, 0);
    assert_eq!(peak, 4);
    assert_eq!(current_bytes, 0);
    assert!(
        (2_200..=BYTE_LIMIT).contains(&peak_bytes),
        "lineage component peak was {peak_bytes} bytes"
    );
    assert_eq!(component_loads, COMPONENTS);

    let records = core_records(&VerifiedIndex::open(&index).unwrap());
    for component in 0..COMPONENTS {
        let child = format!("019fa101-0000-7000-8000-{component:012x}");
        assert_copied_result(
            &records,
            &child,
            &format!("dense-component-{component:02}-result-019"),
        );
    }
}

#[test]
fn codex_many_explicit_routes_share_one_linear_generation_fact_pass_and_lease() {
    const ROUTES: usize = 24;

    let temp = tempdir().unwrap();
    let sessions = temp.path().join("explicit-chain");
    let index = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    let mut registry = SourceBackedProviderRegistry::new();
    for depth in 0..ROUTES {
        let native_session_id = format!("019fa200-0000-7000-8000-{depth:012x}");
        let parent = depth
            .checked_sub(1)
            .map(|parent| format!("019fa200-0000-7000-8000-{parent:012x}"));
        let path = sessions.join(format!("route-{depth:02}.jsonl"));
        fs::write(
            &path,
            codex_lineage_rollout(
                &native_session_id,
                parent.as_deref(),
                if parent.is_some() {
                    SessionRelationshipKind::Forked
                } else {
                    SessionRelationshipKind::Root
                },
                parent.as_deref(),
                &format!("linear explicit route {depth:02}"),
            ),
        )
        .unwrap();
        register_codex_route(
            &mut registry,
            &path,
            "codex_session_jsonl",
            ProviderImportSupport::Explicit,
            SourceBackedRouteSelection::ExplicitManual,
        );
    }
    let observed = Arc::new(Mutex::new(None));
    let observed_from_hook = Arc::clone(&observed);
    install_after_codex_lineage_normalization_hook_v0(move |observation| {
        *observed_from_hook.lock().unwrap() = Some(observation);
    });

    let refreshed =
        refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
    assert!(refreshed.failed_routes.is_empty());
    assert_eq!(refreshed.certified_source_count, ROUTES);
    let observation = observed.lock().unwrap().clone().unwrap();
    assert_eq!(observation.valid_sources, ROUTES);
    assert_eq!(
        observation.lineage_fact_source_scans,
        ROUTES.saturating_sub(1) as u64
    );
    assert_eq!(observation.worker_start_latch.starts(), ROUTES as u64);
    let (active, peak, current_bytes, _peak_bytes, component_loads) = registry
        .codex_generation
        .as_ref()
        .unwrap()
        .generation_lineage_metrics()
        .unwrap();
    assert_eq!(active, 0);
    assert_eq!(peak, 1);
    assert_eq!(current_bytes, 0);
    assert_eq!(component_loads, 1);
    assert_eq!(
        core_records(&VerifiedIndex::open(&index).unwrap()).len(),
        ROUTES
    );
}

#[cfg(unix)]
fn assert_codex_generation_rejects_parent_replacement_after_preparation(longer: bool) {
    let temp = tempdir().unwrap();
    let automatic = temp.path().join("automatic-child");
    let explicit_parent = temp.path().join("explicit-parent.jsonl");
    let replacement = temp.path().join("replacement-parent.jsonl");
    let moved = temp.path().join("prepared-parent.jsonl");
    let index = temp.path().join("index");
    fs::create_dir_all(&automatic).unwrap();
    let parent = "019fa300-0000-7000-8000-000000000001";
    let child = "019fa300-0000-7000-8000-000000000002";
    let call = codex_lineage_call("prepared-parent-call", "git rev-parse --verify HEAD");
    let old_result = codex_lineage_result(
        "prepared-parent-call",
        "prepared parent old output dddddddddddddddddddddddddddddddd",
    );
    let new_result = codex_lineage_result(
        "prepared-parent-call",
        "prepared parent new output eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
    );
    let old_parent = codex_lineage_rollout_with_events(
        parent,
        None,
        SessionRelationshipKind::Root,
        None,
        &[call.clone(), old_result.clone()],
    );
    let mut replacement_events = vec![call.clone(), new_result];
    if longer {
        replacement_events.push(serde_json::json!({
            "timestamp": "2026-08-06T12:00:02Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "long replacement tail"}]
            }
        }));
    }
    let replacement_parent = codex_lineage_rollout_with_events(
        parent,
        None,
        SessionRelationshipKind::Root,
        None,
        &replacement_events,
    );
    if longer {
        assert!(replacement_parent.len() > old_parent.len());
    } else {
        assert_eq!(replacement_parent.len(), old_parent.len());
    }
    fs::write(&explicit_parent, &old_parent).unwrap();
    fs::write(&replacement, &replacement_parent).unwrap();
    fs::write(
        automatic.join(format!("rollout-{child}.jsonl")),
        codex_lineage_rollout_with_events(
            child,
            Some(parent),
            SessionRelationshipKind::Forked,
            Some(parent),
            &[call, old_result],
        ),
    )
    .unwrap();

    let mut registry = SourceBackedProviderRegistry::new();
    register_codex_route(
        &mut registry,
        &automatic,
        "codex_session_jsonl_tree",
        ProviderImportSupport::Native,
        SourceBackedRouteSelection::Automatic,
    );
    register_codex_route(
        &mut registry,
        &explicit_parent,
        "codex_session_jsonl",
        ProviderImportSupport::Explicit,
        SourceBackedRouteSelection::ExplicitManual,
    );
    let seeded =
        refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
    assert!(seeded.failed_routes.is_empty());
    let seeded_index = VerifiedIndex::open(&index).unwrap();
    let seeded_generation = seeded_index.generation_id().to_owned();
    let seeded_records = core_records(&seeded_index);

    let observed = Arc::new(Mutex::new(None));
    let observed_from_hook = Arc::clone(&observed);
    let explicit_parent_from_hook = explicit_parent.clone();
    install_after_codex_lineage_normalization_hook_v0(move |observation| {
        *observed_from_hook.lock().unwrap() = Some(observation);
        fs::rename(&explicit_parent_from_hook, moved).unwrap();
        fs::rename(replacement, explicit_parent_from_hook).unwrap();
    });
    let rejected =
        refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
    let observation = observed.lock().unwrap().clone().unwrap();
    assert_eq!(observation.lineage_fact_source_scans, 1);
    assert_eq!(observation.worker_starts_at_normalization, 0);
    assert_eq!(observation.worker_start_latch.starts(), 0);
    assert_eq!(rejected.failed_routes.len(), 2);
    assert!(rejected.failed_routes.iter().all(|failure| {
        failure.class == SourceBackedSourceFailureClass::SourceChanged && failure.carried_forward
    }));
    let retained = VerifiedIndex::open(&index).unwrap();
    assert_eq!(retained.generation_id(), seeded_generation);
    assert_eq!(core_records(&retained), seeded_records);

    let retried =
        refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
    assert!(retried.failed_routes.is_empty());
    let retried_records = core_records(&VerifiedIndex::open(&index).unwrap());
    assert!(retried_records.iter().any(|record| {
        record.provider_session_id.as_deref() == Some(parent)
            && record
                .content
                .normalized_body
                .as_deref()
                .is_some_and(|body| body.contains("prepared parent new output"))
    }));
    assert!(!retried_records.iter().any(|record| {
        record.provider_session_id.as_deref() == Some(parent)
            && record
                .content
                .normalized_body
                .as_deref()
                .is_some_and(|body| body.contains("prepared parent old output"))
    }));
    if longer {
        assert!(retried_records.iter().any(|record| {
            record.content.normalized_body.as_deref() == Some("long replacement tail")
        }));
    }
}

#[cfg(unix)]
#[test]
fn codex_generation_rejects_same_length_parent_replacement_after_preparation() {
    assert_codex_generation_rejects_parent_replacement_after_preparation(false);
}

#[cfg(unix)]
#[test]
fn codex_generation_rejects_longer_parent_replacement_after_preparation() {
    assert_codex_generation_rejects_parent_replacement_after_preparation(true);
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
    let observation = observed.lock().unwrap().clone().unwrap();
    assert_eq!(observation.valid_sources, 4);
    assert_eq!(observation.rejected_sources, 2);
    assert_eq!(observation.worker_starts_at_normalization, 0);
    assert_eq!(observation.worker_start_latch.starts(), 4);
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
fn codex_root_conflict_projects_typed_source_failures_while_valid_peer_publishes() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("private-codex-sessions");
    let index = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    let root_a = "019fa000-0000-7000-8000-0000000032a0";
    let child_a = "019fa000-0000-7000-8000-0000000032a1";
    let root_b = "019fa000-0000-7000-8000-0000000032b0";
    let invalid_root_marker = "privaterootacanary328";
    let invalid_child_marker = "privatechildacanary328";
    let valid_marker = "validrootbcanary328";
    for (id, parent, relationship, advisory, marker) in [
        (
            root_a,
            None,
            SessionRelationshipKind::Root,
            None,
            invalid_root_marker,
        ),
        (
            child_a,
            Some(root_a),
            SessionRelationshipKind::Delegated,
            Some(root_b),
            invalid_child_marker,
        ),
        (
            root_b,
            None,
            SessionRelationshipKind::Root,
            None,
            valid_marker,
        ),
    ] {
        fs::write(
            sessions.join(format!("rollout-{id}.jsonl")),
            codex_lineage_rollout(id, parent, relationship, advisory, marker),
        )
        .unwrap();
    }
    let registry = register_codex_tree(&sessions);

    let refreshed =
        refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
    assert_eq!(refreshed.certified_source_count, 1);
    assert_eq!(refreshed.commit.indexed_documents, 1);
    assert_eq!(refreshed.logical_source_failures.total(), 2);
    assert_eq!(refreshed.successful_route_outcomes.len(), 1);
    assert_eq!(
        refreshed.successful_route_outcomes[0].logical_source_failure_total,
        2
    );
    assert_eq!(refreshed.record_rejections.total(), 0);
    assert_eq!(
        refreshed.record_completion(),
        SourceBackedRecordCompletion::Completed
    );
    for failure in refreshed.logical_source_failures.failures() {
        assert_eq!(failure.class, SourceBackedSourceFailureClass::Unreadable);
        assert!(!failure.carried_forward);
        for expected in [
            format!("computed_root_native_session_id={root_a}"),
            format!("conflicting_advisory_session_id={root_b}"),
            format!("evidence_source_record=session_meta:{child_a}"),
            format!("computed_root_source_record=session_meta:{root_a}"),
            format!("advisory_source_record=session_meta:{root_b}"),
        ] {
            assert!(failure.detail.contains(&expected), "{}", failure.detail);
        }
        assert!(!failure.detail.contains(sessions.to_str().unwrap()));
        assert!(!failure.detail.contains(invalid_root_marker));
        assert!(!failure.detail.contains(invalid_child_marker));
    }
    let verified = VerifiedIndex::open(&index).unwrap();
    assert_eq!(
        verified
            .search_event_candidates(valid_marker, 8)
            .unwrap()
            .len(),
        1
    );
    assert!(verified
        .search_event_candidates(invalid_root_marker, 8)
        .unwrap()
        .is_empty());
    assert!(verified
        .search_event_candidates(invalid_child_marker, 8)
        .unwrap()
        .is_empty());
    let generation = verified.generation_id().to_owned();

    let replay =
        refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
    assert_eq!(replay.commit.generation_id, generation);
    assert_eq!(replay.logical_source_failures.total(), 2);
    assert_eq!(replay.record_rejections.total(), 0);
}

#[test]
fn codex_warm_root_conflict_quarantines_owned_component_and_publishes_peer_update() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("codex-sessions");
    let index = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    let root_a = "019fa000-0000-7000-8000-0000000033a0";
    let child_a = "019fa000-0000-7000-8000-0000000033a1";
    let root_b = "019fa000-0000-7000-8000-0000000033b0";
    let root_a_marker = "warmrootacanary328";
    let child_a_marker = "warmchildacanary328";
    let old_peer_marker = "warmoldpeercanary328";
    let new_peer_marker = "warmnewpeercanary328";
    let root_a_path = sessions.join(format!("rollout-{root_a}.jsonl"));
    let child_a_path = sessions.join(format!("rollout-{child_a}.jsonl"));
    let root_b_path = sessions.join(format!("rollout-{root_b}.jsonl"));
    fs::write(
        &root_a_path,
        codex_lineage_rollout(
            root_a,
            None,
            SessionRelationshipKind::Root,
            None,
            root_a_marker,
        ),
    )
    .unwrap();
    fs::write(
        &child_a_path,
        codex_lineage_rollout(
            child_a,
            Some(root_a),
            SessionRelationshipKind::Delegated,
            Some(root_a),
            child_a_marker,
        ),
    )
    .unwrap();
    fs::write(
        &root_b_path,
        codex_lineage_rollout(
            root_b,
            None,
            SessionRelationshipKind::Root,
            None,
            old_peer_marker,
        ),
    )
    .unwrap();
    let registry = register_codex_tree(&sessions);

    let initial =
        refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
    assert_eq!(initial.certified_source_count, 3);
    assert!(initial.logical_source_failures.is_empty());
    let initial_generation = initial.commit.generation_id;

    fs::write(
        &child_a_path,
        codex_lineage_rollout(
            child_a,
            Some(root_a),
            SessionRelationshipKind::Delegated,
            Some(root_b),
            child_a_marker,
        ),
    )
    .unwrap();
    fs::write(
        &root_b_path,
        codex_lineage_rollout(
            root_b,
            None,
            SessionRelationshipKind::Root,
            None,
            new_peer_marker,
        ),
    )
    .unwrap();

    let refreshed =
        refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
    assert_ne!(refreshed.commit.generation_id, initial_generation);
    assert_eq!(refreshed.certified_source_count, 1);
    assert_eq!(refreshed.logical_source_failures.total(), 2);
    assert_eq!(refreshed.logical_source_failures.failures().len(), 2);
    assert_eq!(refreshed.logical_source_failures.omitted(), 0);
    assert_eq!(refreshed.record_rejections.total(), 0);
    assert_eq!(
        refreshed.record_completion(),
        SourceBackedRecordCompletion::Completed
    );
    assert_eq!(refreshed.successful_route_outcomes.len(), 1);
    assert_eq!(
        refreshed.successful_route_outcomes[0].logical_source_failure_total,
        2
    );
    for failure in refreshed.logical_source_failures.failures() {
        assert!(!failure.carried_forward);
        for expected in [
            format!("computed_root_native_session_id={root_a}"),
            format!("conflicting_advisory_session_id={root_b}"),
            format!("evidence_source_record=session_meta:{child_a}"),
            format!("computed_root_source_record=session_meta:{root_a}"),
            format!("advisory_source_record=session_meta:{root_b}"),
        ] {
            assert!(failure.detail.contains(&expected), "{}", failure.detail);
        }
        assert!(!failure.detail.contains(sessions.to_str().unwrap()));
        assert!(!failure.detail.contains(root_a_marker));
        assert!(!failure.detail.contains(child_a_marker));
    }
    let verified = VerifiedIndex::open(&index).unwrap();
    assert_eq!(
        verified
            .search_event_candidates(new_peer_marker, 8)
            .unwrap()
            .len(),
        1
    );
    for removed_marker in [root_a_marker, child_a_marker, old_peer_marker] {
        assert!(
            verified
                .search_event_candidates(removed_marker, 8)
                .unwrap()
                .is_empty(),
            "stale marker remained published: {removed_marker}"
        );
    }
}

#[test]
fn codex_many_root_conflicts_keep_exact_total_and_bounded_capture_diagnostics() {
    const CONFLICTING_CHILDREN: usize = 65;
    const REJECTED_SOURCES: usize = CONFLICTING_CHILDREN + 1;

    let temp = tempdir().unwrap();
    let sessions = temp.path().join("private-bounded-codex-conflicts");
    let index = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    let root_a = "019fa000-0000-7000-8000-000000003500";
    let root_b = "019fa000-0000-7000-8000-0000000035b0";
    let evidence_child = "019fa000-0000-7000-8001-000000000000";
    let private_marker = "private bounded capture root conflict 328";
    let valid_marker = "boundedcapturerootconflictvalidpeer328";
    fs::write(
        sessions.join(format!("rollout-{root_a}.jsonl")),
        codex_lineage_rollout(
            root_a,
            None,
            SessionRelationshipKind::Root,
            None,
            private_marker,
        ),
    )
    .unwrap();
    for index in 0..CONFLICTING_CHILDREN {
        let child = format!("019fa000-0000-7000-8001-{index:012x}");
        fs::write(
            sessions.join(format!("rollout-{child}.jsonl")),
            codex_lineage_rollout(
                &child,
                Some(root_a),
                SessionRelationshipKind::Delegated,
                Some(if index == 0 { root_b } else { root_a }),
                private_marker,
            ),
        )
        .unwrap();
    }
    fs::write(
        sessions.join(format!("rollout-{root_b}.jsonl")),
        codex_lineage_rollout(
            root_b,
            None,
            SessionRelationshipKind::Root,
            None,
            valid_marker,
        ),
    )
    .unwrap();
    let registry = register_codex_tree(&sessions);

    let refreshed =
        refresh_source_backed_generation(&index, &registry, WriterOptions::default()).unwrap();
    assert_eq!(refreshed.certified_source_count, 1);
    assert_eq!(refreshed.logical_source_failures.total(), REJECTED_SOURCES);
    assert_eq!(refreshed.logical_source_failures.failures().len(), 64);
    assert_eq!(refreshed.logical_source_failures.omitted(), 2);
    assert_eq!(refreshed.successful_route_outcomes.len(), 1);
    assert_eq!(
        refreshed.successful_route_outcomes[0].logical_source_failure_total,
        REJECTED_SOURCES
    );
    assert_eq!(refreshed.record_rejections.total(), 0);
    assert_eq!(
        refreshed.record_completion(),
        SourceBackedRecordCompletion::Completed
    );
    for failure in refreshed.logical_source_failures.failures() {
        for expected in [
            format!("computed_root_native_session_id={root_a}"),
            format!("conflicting_advisory_session_id={root_b}"),
            format!("evidence_source_record=session_meta:{evidence_child}"),
            format!("computed_root_source_record=session_meta:{root_a}"),
            format!("advisory_source_record=session_meta:{root_b}"),
        ] {
            assert!(failure.detail.contains(&expected), "{}", failure.detail);
        }
        assert!(!failure.detail.contains(sessions.to_str().unwrap()));
        assert!(!failure.detail.contains(private_marker));
    }
    assert_eq!(
        VerifiedIndex::open(&index)
            .unwrap()
            .search_event_candidates(valid_marker, 8)
            .unwrap()
            .len(),
        1
    );
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
    let observation = observed.lock().unwrap().clone().unwrap();
    assert_eq!(observation.valid_sources, 0);
    assert_eq!(observation.rejected_sources, 2);
    assert_eq!(observation.worker_starts_at_normalization, 0);
    assert_eq!(observation.worker_start_latch.starts(), 0);
    assert!(VerifiedIndex::open(&index).is_err());
}

#[test]
fn codex_all_invalid_root_conflict_failure_exposes_path_safe_proof() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("private-codex-sessions");
    let index = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    let root = "019fa000-0000-7000-8000-0000000032c0";
    let child = "019fa000-0000-7000-8000-0000000032c1";
    let missing_advisory = "019fa000-0000-7000-8000-0000000032ff";
    let private_marker = "private all-invalid root-conflict message content";
    fs::write(
        sessions.join(format!("rollout-{root}.jsonl")),
        codex_lineage_rollout(
            root,
            None,
            SessionRelationshipKind::Root,
            None,
            private_marker,
        ),
    )
    .unwrap();
    fs::write(
        sessions.join(format!("rollout-{child}.jsonl")),
        codex_lineage_rollout(
            child,
            Some(root),
            SessionRelationshipKind::Delegated,
            Some(missing_advisory),
            private_marker,
        ),
    )
    .unwrap();

    let error = refresh_source_backed_generation(
        &index,
        &register_codex_tree(&sessions),
        WriterOptions::default(),
    )
    .unwrap_err();
    let SourceBackedCoordinatorError::NoUsableSourceRoutes { failed_routes } = error else {
        panic!("expected an unusable Codex route, got {error:?}");
    };
    assert_eq!(failed_routes.len(), 1);
    let detail = &failed_routes[0].detail;
    for expected in [
        format!("computed_root_native_session_id={root}"),
        format!("conflicting_advisory_session_id={missing_advisory}"),
        format!("evidence_source_record=session_meta:{child}"),
        format!("computed_root_source_record=session_meta:{root}"),
        "advisory_source_record=unavailable".to_owned(),
    ] {
        assert!(detail.contains(&expected), "{detail}");
    }
    assert!(!detail.contains(sessions.to_str().unwrap()));
    assert!(!detail.contains(private_marker));
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
