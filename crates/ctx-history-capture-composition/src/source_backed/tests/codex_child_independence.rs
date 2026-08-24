use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    fs::OpenOptions,
    io::Write,
    path::Path,
    rc::Rc,
};

use super::*;
use crate::provider::source_backed::family::jsonl::{
    set_after_standard_zstd_snapshot_hook, set_before_jsonl_terminal_physical_revalidation_hook,
};
use ctx_history_core::{
    CertifiedSource, CoreDiscoveryExclusion, ProviderNativeSessionRelationship, SourceFrontier,
    TypedKey,
};
use ctx_history_index::{GenerationWriter, RevalidationTarget, WriterOptions};

const CURRENT_PARSER_REVISION: &str = "codex-nativepath-core-activity-v8-inherited-session-lineage";

mod quarantine;

fn writer_options() -> WriterOptions {
    WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    }
}

fn incremental_refresh(
    index_root: &Path,
    registry: &SourceBackedProviderRegistry,
    base: &SourceBackedRefreshReceipt,
) -> (SourceBackedRefreshReceipt, u64) {
    let mut completed_records = 0;
    let receipt = SourceBackedRefreshExecutor::new(registry.clone(), writer_options())
        .with_base_route_controls(base.route_controls.clone())
        .refresh_scope_with_detailed_progress_and_reconciliation(
            index_root,
            SourceBackedRefreshScope::All,
            SourceBackedReconciliationDemand::Incremental,
            |update| {
                completed_records =
                    completed_records.max(update.progress.completed_records.unwrap_or(0));
                Ok(())
            },
        )
        .unwrap();
    (receipt, completed_records)
}

fn incremental_refresh_member(
    index_root: &Path,
    registry: &SourceBackedProviderRegistry,
    base: &SourceBackedRefreshReceipt,
    root: &Path,
    member: PathBuf,
) -> SourceBackedRefreshReceipt {
    SourceBackedRefreshExecutor::new(registry.clone(), writer_options())
        .with_base_route_controls(base.route_controls.clone())
        .refresh_scope_with_detailed_progress_publication_metadata_reconciliation_and_worksets(
            index_root,
            SourceBackedRefreshScope::All,
            SourceBackedReconciliationDemand::Incremental,
            BTreeMap::from([(route_identity(registry, root), BTreeSet::from([member]))]),
            |_| Ok(()),
            |_| Ok(Vec::new()),
        )
        .unwrap()
}

fn session_path(root: &Path, native_session_id: &str) -> PathBuf {
    root.join(format!("rollout-{native_session_id}.jsonl"))
}

fn jsonl_bytes(records: impl IntoIterator<Item = serde_json::Value>) -> Vec<u8> {
    records
        .into_iter()
        .flat_map(|record| {
            let mut line = serde_json::to_vec(&record).unwrap();
            line.push(b'\n');
            line
        })
        .collect()
}

fn session_meta(
    native_session_id: &str,
    relationship: ProviderNativeSessionRelationship,
    parent_native_session_id: Option<&str>,
) -> serde_json::Value {
    let source = match (relationship, parent_native_session_id) {
        (ProviderNativeSessionRelationship::Delegated, Some(parent)) => serde_json::json!({
            "subagent": {"thread_spawn": {"parent_thread_id": parent}}
        }),
        _ => serde_json::json!("cli"),
    };
    let mut payload = serde_json::json!({
        "id": native_session_id,
        "session_id": native_session_id,
        "timestamp": "2026-08-09T12:00:00Z",
        "cwd": "/tmp/codex-child-independence",
        "originator": "codex_cli_rs",
        "cli_version": "0.1.0",
        "source": source,
        "model_provider": "openai"
    });
    if let Some(parent) = parent_native_session_id {
        match relationship {
            ProviderNativeSessionRelationship::Delegated => {
                payload["parent_thread_id"] = serde_json::json!(parent);
            }
            ProviderNativeSessionRelationship::Forked => {
                payload["forked_from_id"] = serde_json::json!(parent);
            }
            ProviderNativeSessionRelationship::ResumedFrom => {
                payload["history_base"] = serde_json::json!({
                    "thread_id": parent,
                    "end_ordinal_exclusive": 3,
                    "end_byte_offset": 512
                });
            }
            relationship => panic!("unsupported fixture relationship {relationship:?}"),
        }
    }
    serde_json::json!({
        "timestamp": "2026-08-09T12:00:00Z",
        "type": "session_meta",
        "payload": payload
    })
}

fn message(marker: &str) -> serde_json::Value {
    serde_json::json!({
        "timestamp": "2026-08-09T12:00:01Z",
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": marker}]
        }
    })
}

fn turn_context() -> serde_json::Value {
    turn_context_with_id("019fb100-0000-7000-8000-000000000001")
}

fn turn_context_with_id(turn_id: &str) -> serde_json::Value {
    serde_json::json!({
        "timestamp": "2026-08-09T12:00:02Z",
        "type": "turn_context",
        "payload": {
            "turn_id": turn_id,
            "cwd": "/tmp/codex-child-independence"
        }
    })
}

fn exec_call(call_id: &str) -> serde_json::Value {
    exec_call_with_command(call_id, "git rev-parse HEAD")
}

fn exec_call_with_command(call_id: &str, command: &str) -> serde_json::Value {
    serde_json::json!({
        "timestamp": "2026-08-09T12:00:03Z",
        "type": "response_item",
        "payload": {
            "type": "function_call",
            "name": "exec_command",
            "call_id": call_id,
            "arguments": serde_json::json!({
                "cmd": command,
                "workdir": "/tmp/codex-child-independence",
                "yield_time_ms": 10000
            }).to_string()
        }
    })
}

fn exact_exec_result(call_id: &str, output: &str) -> serde_json::Value {
    serde_json::json!({
        "timestamp": "2026-08-09T12:00:04Z",
        "type": "response_item",
        "payload": {
            "type": "function_call_output",
            "call_id": call_id,
            "status": "success",
            "output": format!(
                "Script completed\nProcess exited with code 0\nFinal output:\n{output}"
            )
        }
    })
}

fn exact_mcp_result(call_id: &str, output: &str) -> serde_json::Value {
    serde_json::json!({
        "timestamp": "2026-08-09T12:00:05Z",
        "type": "event_msg",
        "payload": {
            "type": "mcp_tool_call_end",
            "call_id": call_id,
            "invocation": {
                "server": "ctx",
                "tool": "search",
                "arguments": {"query": "terminal uniqueness"}
            },
            "duration": {"secs": 0, "nanos": 42},
            "result": {
                "Ok": {
                    "content": [{"type": "text", "text": output}],
                    "isError": false
                }
            }
        }
    })
}

fn exec_result(call_id: &str, marker: &str) -> serde_json::Value {
    successful_result(
        call_id,
        format!("{marker}\n0123456789abcdef0123456789abcdef01234567\n"),
    )
}

fn successful_result(call_id: &str, output: String) -> serde_json::Value {
    serde_json::json!({
        "timestamp": "2026-08-09T12:00:04Z",
        "type": "response_item",
        "payload": {
            "type": "function_call_output",
            "call_id": call_id,
            "status": "success",
            "output": format!(
                "Chunk ID: abc123\nWall time: 0.125 seconds\nProcess exited with code 0\nFinal output:\n{output}"
            )
        }
    })
}

fn write_session(
    root: &Path,
    native_session_id: &str,
    relationship: ProviderNativeSessionRelationship,
    parent_native_session_id: Option<&str>,
    events: impl IntoIterator<Item = serde_json::Value>,
) {
    let records = std::iter::once(session_meta(
        native_session_id,
        relationship,
        parent_native_session_id,
    ))
    .chain(events);
    fs::write(session_path(root, native_session_id), jsonl_bytes(records)).unwrap();
}

fn append_event(path: &Path, event: serde_json::Value) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(&jsonl_bytes([event])).unwrap();
    file.sync_all().unwrap();
}

fn destructively_mutate_session(path: &Path, replacement: &Path, mutation: &str) {
    match mutation {
        "truncate" => {
            let file = OpenOptions::new().write(true).open(path).unwrap();
            file.set_len(fs::metadata(path).unwrap().len() / 2).unwrap();
            file.sync_all().unwrap();
        }
        "replacement" => {
            fs::remove_file(path).unwrap();
            fs::rename(replacement, path).unwrap();
        }
        _ => unreachable!(),
    }
}

fn register_tree(roots: &[&Path]) -> SourceBackedProviderRegistry {
    let mut registry = SourceBackedProviderRegistry::new();
    super::super::register_codex_session_tree_routes(
        &mut registry,
        roots
            .iter()
            .map(|root| {
                fixture_provider_source_at(
                    CaptureProvider::Codex,
                    "codex_session_jsonl_tree",
                    ProviderImportSupport::Native,
                    *root,
                )
            })
            .collect(),
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();
    registry
}

fn build_discovered_codex_registry(
    context: &DiscoveryContext,
    data_root: &Path,
) -> SourceBackedAutomaticRegistryBuild {
    let probes = crate::test_provider_probes();
    let report = ctx_history_source_discovery::discover_provider_sources_for_provider_with_context(
        &probes,
        context,
        CaptureProvider::Codex,
    );
    build_automatic_source_backed_registry_from_report_with_probes(
        &probes, context, data_root, report,
    )
}

fn add_explicit_route(registry: &mut SourceBackedProviderRegistry, path: &Path) {
    register_landed_source_backed_route(
        registry,
        fixture_provider_source_at(
            CaptureProvider::Codex,
            "codex_session_jsonl",
            ProviderImportSupport::Explicit,
            path,
        ),
        SourceBackedRouteSelection::ExplicitManual,
    )
    .unwrap();
}

#[test]
fn configured_codex_homes_with_the_same_native_session_publish_independent_sources() {
    let temp = tempdir().unwrap();
    let fixture = fs::canonicalize(temp.path()).unwrap();
    let personal = fixture.join("personal/sessions");
    let personal_archive = fixture.join("personal/archived_sessions");
    let work = fixture.join("work/sessions");
    fs::create_dir_all(&personal).unwrap();
    fs::create_dir_all(&personal_archive).unwrap();
    fs::create_dir_all(&work).unwrap();
    let native_session_id = "019fb000-0000-7000-8000-000000000099";
    write_session(
        &personal,
        native_session_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [message("personal pineapple marker")],
    );
    write_session(
        &work,
        native_session_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [message("work kumquat marker")],
    );
    write_session(
        &personal_archive,
        native_session_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [message("personal archived duplicate should coalesce")],
    );
    let mut registry = SourceBackedProviderRegistry::new();
    for (root, lineage) in [
        (&personal, [8; 32]),
        (&personal_archive, [8; 32]),
        (&work, [9; 32]),
    ] {
        super::super::register_configured_codex_session_tree_route(
            &mut registry,
            fixture_provider_source_at(
                CaptureProvider::Codex,
                "codex_session_jsonl_tree",
                ProviderImportSupport::Native,
                root,
            ),
            SourceBackedRouteSelection::ExplicitManual,
            Some(lineage),
        )
        .unwrap();
    }

    let index_root = fixture.join("index");
    let receipt =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(
        receipt.failed_routes.is_empty(),
        "{:?}",
        receipt.failed_routes
    );
    assert_eq!(receipt.successful_route_ids.len(), 3);

    let archive_refresh = refresh_source_backed_generation_for_routes(
        &index_root,
        &registry,
        writer_options(),
        [route_identity(&registry, &personal_archive)],
    )
    .unwrap();
    assert!(
        archive_refresh.failed_routes.is_empty(),
        "{:?}",
        archive_refresh.failed_routes
    );

    let index = VerifiedIndex::open(&index_root).unwrap();
    let codex_sources = index
        .manifest()
        .sources
        .iter()
        .filter(|source| source.observation().source().provider() == "codex")
        .collect::<Vec<_>>();
    assert_eq!(codex_sources.len(), 2);
    let mut bodies = codex_sources
        .into_iter()
        .flat_map(|source| {
            index
                .core_source_event_page(source.observation().source(), None, 16)
                .unwrap()
                .items
                .into_iter()
                .filter_map(|item| item.core_record.content.normalized_body)
        })
        .collect::<Vec<_>>();
    bodies.sort();
    assert!(bodies
        .iter()
        .any(|body| body == "personal pineapple marker"));
    assert!(bodies.iter().any(|body| body == "work kumquat marker"));
    assert!(!bodies
        .iter()
        .any(|body| body == "personal archived duplicate should coalesce"));
}

#[test]
fn unavailable_configured_codex_home_carries_only_itself_while_peer_refreshes() {
    let temp = tempdir().unwrap();
    let fixture = fs::canonicalize(temp.path()).unwrap();
    let personal_home = fixture.join("personal-codex-home");
    let work_home = fixture.join("work-codex-home");
    let personal_sessions = personal_home.join("sessions");
    let work_sessions = work_home.join("sessions");
    fs::create_dir_all(&personal_sessions).unwrap();
    fs::create_dir_all(&work_sessions).unwrap();
    let personal_session_id = "019fb000-0000-7000-8000-000000000081";
    let work_session_id = "019fb000-0000-7000-8000-000000000082";
    write_session(
        &personal_sessions,
        personal_session_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [message("configured Codex personal initial")],
    );
    write_session(
        &work_sessions,
        work_session_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [message("configured Codex work retained")],
    );
    let context = DiscoveryContext::new(
        &fixture,
        &fixture,
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    )
    .with_configured_provider_roots(vec![
        ctx_history_capture_model::ProviderRootDefinition {
            id: "personal".to_owned(),
            provider: CaptureProvider::Codex,
            path: personal_home.clone(),
            group: Some("personal".to_owned()),
        },
        ctx_history_capture_model::ProviderRootDefinition {
            id: "work".to_owned(),
            provider: CaptureProvider::Codex,
            path: work_home.clone(),
            group: Some("work".to_owned()),
        },
    ]);
    let data_root = fixture.join("data");
    let initial = build_discovered_codex_registry(&context, &data_root);
    assert!(initial.issues.is_empty(), "{:?}", initial.issues);
    let index_root = fixture.join("index");
    let initial_receipt =
        refresh_source_backed_generation(&index_root, &initial.registry, writer_options()).unwrap();
    assert!(initial_receipt.failed_routes.is_empty());

    append_event(
        &session_path(&personal_sessions, personal_session_id),
        message("configured Codex personal refreshed"),
    );
    let displaced_work_home = fixture.join("work-codex-displaced");
    fs::rename(&work_home, &displaced_work_home).unwrap();
    fs::write(&work_home, b"temporarily not a directory").unwrap();
    let current = build_discovered_codex_registry(&context, &data_root);
    assert_eq!(
        current
            .issues
            .iter()
            .filter(|issue| matches!(issue, SourceBackedAutomaticRegistryIssue::Discovery(_)))
            .count(),
        1,
        "equivalent physical selector issues are deduplicated while all routes are retained"
    );
    fs::remove_file(&work_home).unwrap();
    fs::rename(&displaced_work_home, &work_home).unwrap();

    let receipt =
        refresh_source_backed_generation(&index_root, &current.registry, writer_options()).unwrap();
    assert_eq!(receipt.failed_routes.len(), 3);
    assert!(receipt.failed_routes.iter().all(|failure| {
        failure.class == SourceBackedSourceFailureClass::Unavailable && failure.carried_forward
    }));
    let index = VerifiedIndex::open(&index_root).unwrap();
    assert!(source_records_contain(
        &index,
        personal_session_id,
        "configured Codex personal refreshed"
    ));
    assert!(source_records_contain(
        &index,
        work_session_id,
        "configured Codex work retained"
    ));
    let work_root = index
        .manifest()
        .provider_roots()
        .iter()
        .find(|root| root.definition().id == "work")
        .unwrap();
    assert_eq!(work_root.routes().len(), 3);
}

#[test]
fn cold_unavailable_configured_codex_home_does_not_block_healthy_peer() {
    let temp = tempdir().unwrap();
    let fixture = fs::canonicalize(temp.path()).unwrap();
    let personal_home = fixture.join("personal-codex-cold");
    let work_home = fixture.join("work-codex-cold");
    let personal_sessions = personal_home.join("sessions");
    fs::create_dir_all(&personal_sessions).unwrap();
    fs::write(&work_home, b"temporarily not a directory").unwrap();
    let personal_session_id = "019fb000-0000-7000-8000-000000000083";
    write_session(
        &personal_sessions,
        personal_session_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [message("configured Codex personal cold")],
    );
    let context = DiscoveryContext::new(
        &fixture,
        &fixture,
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    )
    .with_configured_provider_roots(vec![
        ctx_history_capture_model::ProviderRootDefinition {
            id: "personal".to_owned(),
            provider: CaptureProvider::Codex,
            path: personal_home,
            group: Some("personal".to_owned()),
        },
        ctx_history_capture_model::ProviderRootDefinition {
            id: "work".to_owned(),
            provider: CaptureProvider::Codex,
            path: work_home,
            group: Some("work".to_owned()),
        },
    ]);
    let build = build_discovered_codex_registry(&context, &fixture.join("data"));
    let index_root = fixture.join("index");
    let receipt =
        refresh_source_backed_generation(&index_root, &build.registry, writer_options()).unwrap();
    assert_eq!(receipt.failed_routes.len(), 3);
    assert!(receipt.failed_routes.iter().all(|failure| {
        failure.class == SourceBackedSourceFailureClass::Unavailable && !failure.carried_forward
    }));
    let index = VerifiedIndex::open(&index_root).unwrap();
    assert!(source_records_contain(
        &index,
        personal_session_id,
        "configured Codex personal cold"
    ));
    let work_root = index
        .manifest()
        .provider_roots()
        .iter()
        .find(|root| root.definition().id == "work")
        .unwrap();
    assert!(work_root.routes().is_empty());
}

#[test]
fn codex_subagent_preserves_provider_root_session_in_core_records() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index_root = temp.path().join("index");
    fs::create_dir_all(&sessions).unwrap();
    let root_native_session_id = "019fb000-0000-7000-8000-000000000081";
    let child_native_session_id = "019fb000-0000-7000-8000-000000000082";
    let metadata = serde_json::json!({
        "timestamp": "2026-08-09T12:00:00Z",
        "type": "session_meta",
        "payload": {
            "id": child_native_session_id,
            "session_id": root_native_session_id,
            "parent_thread_id": root_native_session_id,
            "timestamp": "2026-08-09T12:00:00Z",
            "cwd": "/tmp/codex-child-independence",
            "source": {
                "subagent": {
                    "thread_spawn": {
                        "depth": 1,
                        "parent_thread_id": root_native_session_id
                    }
                }
            }
        }
    });
    fs::write(
        session_path(&sessions, child_native_session_id),
        jsonl_bytes([metadata, message("providerrootsessionmarker")]),
    )
    .unwrap();

    let registry = register_tree(&[&sessions]);
    let receipt =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(receipt.failed_routes.is_empty());
    assert!(receipt.logical_source_failures.is_empty());
    let index = VerifiedIndex::open(&index_root).unwrap();
    let records = records_for(&index, child_native_session_id);
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(
        record.session_relationship,
        Some(ProviderNativeSessionRelationship::Delegated)
    );
    assert_eq!(
        record.agent_scope,
        Some(ctx_history_core::AgentScope::Subagent)
    );
    assert!(record.parent_session_id.is_some());
    assert_eq!(record.root_session_id, record.parent_session_id);
    assert_eq!(record.parser_revision, CURRENT_PARSER_REVISION);
}

fn route_identity(registry: &SourceBackedProviderRegistry, root: &Path) -> SourceRouteIdentity {
    registry
        .routes()
        .find(|route| route.source.path == root)
        .and_then(|route| route.route_identity.clone())
        .expect("registered Codex route has an identity")
}

fn certificate_for(index: &VerifiedIndex, native_session_id: &str) -> CertifiedSource {
    index
        .manifest()
        .sources
        .iter()
        .find(|certificate| {
            source_native_session_id(certificate.observation().source()) == Some(native_session_id)
        })
        .cloned()
        .unwrap_or_else(|| panic!("missing certificate for {native_session_id}"))
}

fn source_native_session_id(source: &SourceKey) -> Option<&str> {
    let SourceAnchor::ProviderNative { key, .. } = source.anchor() else {
        return None;
    };
    match key {
        TypedKey::Utf8(value) => Some(value),
        TypedKey::Composite(parts) => parts.last().and_then(|part| match part {
            TypedKey::Utf8(value) => Some(value.as_str()),
            _ => None,
        }),
        _ => None,
    }
}

fn provider_checkpoint_envelope(
    index: &VerifiedIndex,
    native_session_id: &str,
) -> (usize, usize, usize, serde_json::Value) {
    let certificate = certificate_for(index, native_session_id);
    let frontier = certificate.frontier().unwrap();
    frontier.validate_contract().unwrap();
    let TypedKey::Utf8(family_json) = frontier.checkpoint() else {
        panic!("new family checkpoint was not compact UTF-8");
    };
    let family = serde_json::from_str::<serde_json::Value>(family_json).unwrap();
    let provider = family
        .get("provider_checkpoint")
        .expect("Codex family checkpoint omitted provider state")
        .clone();
    let provider_bytes = provider
        .get("Utf8")
        .and_then(|value| value.as_str())
        .map_or(0, str::len);
    (
        provider_bytes,
        family_json.len(),
        serde_json::to_vec(frontier).unwrap().len(),
        provider,
    )
}

fn certificate_with_provider_checkpoint(
    index: &VerifiedIndex,
    native_session_id: &str,
    provider_checkpoint: TypedKey,
) -> CertifiedSource {
    let current = certificate_for(index, native_session_id);
    let frontier = current.frontier().unwrap();
    let TypedKey::Utf8(family_json) = frontier.checkpoint() else {
        panic!("Codex family checkpoint was not compact UTF-8");
    };
    let mut family = serde_json::from_str::<serde_json::Value>(family_json).unwrap();
    family["provider_checkpoint"] = serde_json::to_value(provider_checkpoint).unwrap();
    let checkpoint = TypedKey::Utf8(serde_json::to_string(&family).unwrap());
    let modified_frontier = SourceFrontier::new(
        frontier.checkpoint_kind(),
        checkpoint,
        frontier.certified_prefix_bytes(),
        *frontier.certified_prefix_digest(),
    )
    .unwrap();
    CertifiedSource::certify_with_frontier(
        current.observation().clone(),
        current.observation().clone(),
        current.parser_revision(),
        *current.content_digest(),
        current.counts(),
        Some(modified_frontier),
    )
    .unwrap()
}

fn install_single_source_certificate(
    index_root: &Path,
    native_session_id: &str,
    provider_checkpoint: TypedKey,
) -> String {
    let current = VerifiedIndex::open(index_root).unwrap();
    let routes = current.manifest().source_routes().to_vec();
    let replacement =
        certificate_with_provider_checkpoint(&current, native_session_id, provider_checkpoint);
    let records = records_for(&current, native_session_id);
    assert_eq!(
        routes
            .iter()
            .flat_map(|route| route.sources())
            .filter(|source| source.exact_descriptor_eq(replacement.observation().source()))
            .count(),
        1
    );
    drop(current);

    let mut writer = GenerationWriter::open(index_root, writer_options())
        .unwrap()
        .into_writer()
        .unwrap();
    writer
        .set_source_route_plan(
            routes
                .iter()
                .map(|route| route.route_identity().clone())
                .collect::<BTreeSet<_>>(),
            BTreeSet::new(),
        )
        .unwrap();
    for route in &routes {
        writer
            .begin_source_route_stage(route.route_identity().clone())
            .unwrap();
        for source in route.sources() {
            assert!(source.exact_descriptor_eq(replacement.observation().source()));
            writer.begin_source(source.clone()).unwrap();
            for record in &records {
                writer.add_core_record(record.clone()).unwrap();
            }
            writer.certify_source(replacement.clone()).unwrap();
        }
        writer
            .finish_source_route_stage(route.route_identity())
            .unwrap();
    }
    writer.set_present_source_routes(routes).unwrap();
    writer
        .commit(|target| match target {
            RevalidationTarget::Source(actual) => actual == &replacement,
            RevalidationTarget::Deletion(_) => false,
        })
        .unwrap()
        .generation_id
}

fn retired_semantic_v2_checkpoint(native_session_id: &str) -> TypedKey {
    TypedKey::Utf8(
        serde_json::to_string(&serde_json::json!({
            "version": 2,
            "pending_tool_authorities": [{
                "call_id_sha256": "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=",
                "record_start": 1,
                "record_end": 2,
                "raw_ordinal": 1,
                "continuation_cell_id": null,
                "continuation_conflicted": false,
                "continuation_call_id_sha256": "",
                "continuation_capacity_exceeded": false,
                "correlation_ambiguous": false,
                "invocation_origin": {"kind": "unique_to_session"}
            }],
            "owner": {
                "native_session_id": native_session_id,
                "parent_native_session_id": null,
                "advisory_session_id": native_session_id,
                "root_native_session_id": native_session_id,
                "session_relationship": "root",
                "started_at": "2026-08-09T12:00:00Z",
                "cwd": "/tmp/codex-child-independence",
                "originator": "codex_cli_rs",
                "cli_version": "0.1.0",
                "source_kind": "cli",
                "external_agent_id": null,
                "role_hint": null,
                "model_provider": "openai",
                "git": null
            },
            "local_turn_started": false
        }))
        .unwrap(),
    )
}

fn retired_semantic_v6_checkpoint(native_session_id: &str) -> TypedKey {
    TypedKey::Utf8(format!(
        "codex.projector-checkpoint.v6:{}",
        serde_json::to_string(&serde_json::json!({
            "version": 6,
            "owner": {
                "native_session_id": native_session_id,
                "parent_native_session_id": null,
                "root_native_session_id": null,
                "session_relationship": "root",
                "started_at": "2026-08-09T12:00:00Z",
                "cwd": "/tmp/codex-child-independence",
                "originator": "codex_cli_rs",
                "cli_version": "0.1.0",
                "source_kind": "cli",
                "external_agent_id": null,
                "role_hint": null,
                "model_provider": "openai",
                "git": null
            },
            "local_turn_started": false,
            "pending_calls": {}
        }))
        .unwrap()
    ))
}

fn assert_legacy_provider_checkpoint_is_inert(
    case: &str,
    provider_checkpoint: impl FnOnce(&str) -> TypedKey,
) {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join(format!("sessions-{case}"));
    let index_root = temp.path().join(format!("index-{case}"));
    let cold_root = temp.path().join(format!("cold-{case}"));
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fb000-0000-7000-8000-00000000005a";
    let call_id = format!("{case}-pending-call");
    let marker = format!("{case}semanticcheckpointreplacementtoken");
    let path = session_path(&sessions, native_session_id);
    write_session(
        &sessions,
        native_session_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [turn_context(), exec_call(&call_id)],
    );
    let registry = register_tree(&[&sessions]);
    refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();

    let injected_generation = install_single_source_certificate(
        &index_root,
        native_session_id,
        provider_checkpoint(native_session_id),
    );
    let injected = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(injected.generation_id(), injected_generation);
    assert_eq!(
        certificate_for(&injected, native_session_id).parser_revision(),
        CURRENT_PARSER_REVISION
    );
    let injected_certificate_bytes =
        serde_json::to_vec(&certificate_for(&injected, native_session_id)).unwrap();
    drop(injected);

    let unchanged =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(unchanged.commit.generation_id, injected_generation);
    let unchanged_index = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(
        serde_json::to_vec(&certificate_for(&unchanged_index, native_session_id)).unwrap(),
        injected_certificate_bytes
    );
    drop(unchanged_index);

    append_event(&path, exec_result(&call_id, &marker));
    let (appended, _) = incremental_refresh(&index_root, &registry, &unchanged);
    assert!(appended.failed_routes.is_empty());
    assert!(appended.logical_source_failures.is_empty());

    let rebuilt = VerifiedIndex::open(&index_root).unwrap();
    let rebuilt_snapshot = source_snapshot(&rebuilt, native_session_id, &marker);
    let (_, _, _, rebuilt_checkpoint) = provider_checkpoint_envelope(&rebuilt, native_session_id);
    assert_current_provider_checkpoint(&rebuilt_checkpoint);
    assert_eq!(
        certificate_for(&rebuilt, native_session_id)
            .frontier()
            .unwrap()
            .certified_prefix_bytes(),
        fs::metadata(&path).unwrap().len()
    );
    drop(rebuilt);

    let cold = refresh_source_backed_generation(&cold_root, &registry, writer_options()).unwrap();
    assert!(cold.failed_routes.is_empty());
    assert_eq!(
        cold.commit.certified_source_bytes,
        appended.commit.certified_source_bytes
    );
    let cold = VerifiedIndex::open(&cold_root).unwrap();
    assert_eq!(
        source_snapshot(&cold, native_session_id, &marker),
        rebuilt_snapshot
    );
}

fn assert_current_provider_checkpoint(checkpoint: &serde_json::Value) {
    const MAX_PROVIDER_CHECKPOINT_BYTES: usize = 64 * 1024 - 5;
    let encoded = checkpoint
        .get("Utf8")
        .and_then(serde_json::Value::as_str)
        .expect("Codex provider checkpoint must be compact UTF-8");
    assert!(encoded.starts_with("codex.projector-checkpoint.v8:"));
    assert!(encoded.len() <= MAX_PROVIDER_CHECKPOINT_BYTES);
}

fn records_for(index: &VerifiedIndex, native_session_id: &str) -> Vec<CoreRecord> {
    let certificate = certificate_for(index, native_session_id);
    let mut cursor = None;
    let mut records = Vec::new();
    loop {
        let page = index
            .source_event_page(certificate.observation().source(), cursor.as_ref(), 256)
            .unwrap();
        records.extend(page.items.into_iter().map(|item| {
            index
                .core_record_by_id(item.event_id.as_uuid())
                .unwrap()
                .unwrap()
        }));
        let Some(next_cursor) = page.next_cursor else {
            break;
        };
        cursor = Some(next_cursor);
    }
    records.sort_by_key(|record| record.event_sequence);
    records
}

fn source_records_contain(index: &VerifiedIndex, native_session_id: &str, marker: &str) -> bool {
    records_for(index, native_session_id).iter().any(|record| {
        record
            .content
            .normalized_body
            .as_deref()
            .is_some_and(|body| body.contains(marker))
    })
}

fn result_record_for_call<'a>(records: &'a [CoreRecord], call_id: &str) -> &'a CoreRecord {
    records
        .iter()
        .find(|record| {
            record.content.activity.as_ref().is_some_and(|activity| {
                activity.provider_call_id == Some(TypedKey::Utf8(call_id.to_owned()))
                    && activity.result.is_some()
            })
        })
        .unwrap_or_else(|| panic!("missing result for {call_id}"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SourceSnapshot {
    certificate: Vec<u8>,
    records: Vec<Vec<u8>>,
    search_event_ids: Vec<String>,
}

fn source_snapshot(
    index: &VerifiedIndex,
    native_session_id: &str,
    search_marker: &str,
) -> SourceSnapshot {
    let mut search_event_ids = index
        .search_event_candidates(search_marker, 32)
        .unwrap()
        .into_iter()
        .filter(|candidate| {
            candidate.event.provider_session_id.as_deref() == Some(native_session_id)
        })
        .map(|candidate| candidate.event.event_id.to_string())
        .collect::<Vec<_>>();
    search_event_ids.sort();
    SourceSnapshot {
        certificate: serde_json::to_vec(&certificate_for(index, native_session_id)).unwrap(),
        records: records_for(index, native_session_id)
            .into_iter()
            .map(|record| serde_json::to_vec(&record).unwrap())
            .collect(),
        search_event_ids,
    }
}

#[test]
fn inherited_codex_session_metadata_is_admitted_in_both_provider_orders() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions-inherited-metadata-orders");
    let index_root = temp.path().join("index-inherited-metadata-orders");
    fs::create_dir_all(&sessions).unwrap();
    let owner_first_id = "019fb000-0000-7000-8000-000000000050";
    let owner_first_parent = "019fb000-0000-7000-8000-000000000051";
    let ancestor_first_id = "019fb000-0000-7000-8000-000000000052";
    let ancestor_first_parent = "019fb000-0000-7000-8000-000000000053";
    let neighbor_id = "019fb000-0000-7000-8000-000000000054";

    fs::write(
        session_path(&sessions, owner_first_id),
        jsonl_bytes([
            session_meta(
                owner_first_id,
                ProviderNativeSessionRelationship::Forked,
                Some(owner_first_parent),
            ),
            message("ownerfirstinheritedmetadatamarker"),
            session_meta(
                owner_first_parent,
                ProviderNativeSessionRelationship::Root,
                None,
            ),
            session_meta(
                owner_first_id,
                ProviderNativeSessionRelationship::Forked,
                Some(owner_first_parent),
            ),
        ]),
    )
    .unwrap();
    fs::write(
        session_path(&sessions, ancestor_first_id),
        jsonl_bytes([
            session_meta(
                ancestor_first_parent,
                ProviderNativeSessionRelationship::Root,
                None,
            ),
            session_meta(
                ancestor_first_id,
                ProviderNativeSessionRelationship::Forked,
                Some(ancestor_first_parent),
            ),
            message("ancestorfirstinheritedmetadatamarker"),
            session_meta(
                ancestor_first_parent,
                ProviderNativeSessionRelationship::Root,
                None,
            ),
        ]),
    )
    .unwrap();
    write_session(
        &sessions,
        neighbor_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [message("inheritedmetadataneighbormarker")],
    );

    let registry = register_tree(&[&sessions]);
    let receipt =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(receipt.failed_routes.is_empty());
    assert!(receipt.logical_source_failures.is_empty());
    assert_eq!(receipt.sources.len(), 3);

    let index = VerifiedIndex::open(&index_root).unwrap();
    for (native_session_id, marker) in [
        (owner_first_id, "ownerfirstinheritedmetadatamarker"),
        (ancestor_first_id, "ancestorfirstinheritedmetadatamarker"),
        (neighbor_id, "inheritedmetadataneighbormarker"),
    ] {
        assert_eq!(records_for(&index, native_session_id).len(), 1);
        assert_eq!(index.search_event_candidates(marker, 8).unwrap().len(), 1);
    }
    for native_session_id in [owner_first_id, ancestor_first_id] {
        let records = records_for(&index, native_session_id);
        let [record] = records.as_slice() else {
            panic!("one inherited-metadata owner record expected");
        };
        assert_eq!(
            record.provider_session_id.as_deref(),
            Some(native_session_id)
        );
        assert_eq!(
            record.session_relationship,
            Some(ProviderNativeSessionRelationship::Forked)
        );
        assert!(record.parent_session_id.is_some());
        assert!(record.root_session_id.is_none());
    }
}

#[test]
fn codex_rollout_ownership_quarantine_retries_after_file_repair() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions-malformed-owner-neighbor");
    let index_root = temp.path().join("index-malformed-owner-neighbor");
    fs::create_dir_all(&sessions).unwrap();
    let valid_session_id = "019fb000-0000-7000-8000-000000000060";
    let repairable_session_id = "019fb000-0000-7000-8000-000000000061";
    let conflicting_session_id = "019fb000-0000-7000-8000-000000000062";
    let neighbor_marker = "validneighborretainedmarker";
    let previously_valid_marker = "repairablepreviouslyvalidmarker";
    let late_bad_marker = "latequarantinedprefixmarker";
    let repaired_marker = "repairablecorrectedownermarker";
    write_session(
        &sessions,
        valid_session_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [message(neighbor_marker)],
    );
    let repairable_path = session_path(&sessions, repairable_session_id);
    write_session(
        &sessions,
        repairable_session_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [message(previously_valid_marker)],
    );
    let registry = register_tree(&[&sessions]);
    let initial =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(initial.failed_routes.is_empty());
    assert!(initial.logical_source_failures.is_empty());
    let initial_index = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(records_for(&initial_index, repairable_session_id).len(), 1);
    drop(initial_index);

    // The ownership ambiguity is deliberately beyond catalog's bounded
    // physical prefix. Each metadata record is individually valid; only the
    // complete set proves the later branch is disconnected from its owner.
    for ordinal in 0..33 {
        append_event(
            &repairable_path,
            message(&format!("lateownershipprefix{ordinal}")),
        );
    }
    append_event(&repairable_path, message(late_bad_marker));
    append_event(
        &repairable_path,
        session_meta(
            conflicting_session_id,
            ProviderNativeSessionRelationship::Root,
            None,
        ),
    );

    // The member workset first exercises append checkpoint restoration in the
    // bounded partial path. Quarantine then falls through to exhaustive
    // discovery, which retains the exact prior source until this file repairs.
    let quarantined = incremental_refresh_member(
        &index_root,
        &registry,
        &initial,
        &sessions,
        repairable_path.clone(),
    );
    assert!(
        quarantined.failed_routes.is_empty(),
        "unexpected route failures: {:?}",
        quarantined.failed_routes
    );
    assert_eq!(quarantined.logical_source_failures.total(), 1);
    assert!(quarantined.record_rejections.is_empty());
    let [failure] = quarantined.logical_source_failures.failures() else {
        panic!("one quarantined Codex rollout failure expected");
    };
    assert_eq!(
        failure.class,
        ctx_history_capture_runtime::SourceBackedSourceFailureClass::Unreadable
    );
    assert_eq!(
        failure.detail,
        format!(
            "Codex session ownership is ambiguous or conflicting; quarantined rollout file {}",
            repairable_path.display()
        )
    );
    assert_eq!(failure.source.provider(), CaptureProvider::Codex.as_str());

    let index = VerifiedIndex::open(&index_root).unwrap();
    assert!(index
        .search_event_candidates(neighbor_marker, 32)
        .unwrap()
        .into_iter()
        .any(|candidate| candidate.event.provider_session_id.as_deref() == Some(valid_session_id)));
    assert!(index.manifest().sources.iter().any(|certificate| {
        source_native_session_id(certificate.observation().source()) == Some(repairable_session_id)
    }));
    assert!(index.manifest().sources.iter().all(|certificate| {
        source_native_session_id(certificate.observation().source()) != Some(conflicting_session_id)
    }));
    assert!(source_records_contain(
        &index,
        repairable_session_id,
        previously_valid_marker
    ));
    assert!(index
        .search_event_candidates(late_bad_marker, 8)
        .unwrap()
        .is_empty());
    drop(index);

    fs::write(
        &repairable_path,
        jsonl_bytes([
            session_meta(
                repairable_session_id,
                ProviderNativeSessionRelationship::Root,
                None,
            ),
            message(repaired_marker),
        ]),
    )
    .unwrap();

    let repaired = incremental_refresh_member(
        &index_root,
        &registry,
        &quarantined,
        &sessions,
        repairable_path.clone(),
    );
    assert!(repaired.failed_routes.is_empty());
    assert!(repaired.logical_source_failures.is_empty());
    let repaired_index = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(records_for(&repaired_index, repairable_session_id).len(), 1);
    assert!(!source_records_contain(
        &repaired_index,
        repairable_session_id,
        previously_valid_marker
    ));
    assert_eq!(
        repaired_index
            .search_event_candidates(repaired_marker, 8)
            .unwrap()
            .len(),
        1
    );
    assert!(repaired_index
        .search_event_candidates(neighbor_marker, 8)
        .unwrap()
        .into_iter()
        .any(|candidate| candidate.event.provider_session_id.as_deref() == Some(valid_session_id)));
}

#[test]
fn codex_retrieval_exclusion_survives_raw_append_hydration_and_keeps_ids_stable() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions-retrieval-exclusion");
    let index_root = temp.path().join("index-retrieval-exclusion");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fb000-0000-7000-8000-00000000005b";
    let retrieval_call_id = "retrieval-call";
    let ordinary_call_id = "ordinary-call";
    let path = session_path(&sessions, native_session_id);
    write_session(
        &sessions,
        native_session_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [
            turn_context(),
            exec_call_with_command(retrieval_call_id, "ctx search retrievaldiscoverymarker"),
        ],
    );
    let registry = register_tree(&[&sessions]);

    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(cold.failed_routes.is_empty());
    let cold_index = VerifiedIndex::open(&index_root).unwrap();
    let cold_records = records_for(&cold_index, native_session_id);
    assert_eq!(cold_records.len(), 1);
    assert_eq!(
        cold_records[0].content.discovery_exclusion,
        Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
    );
    assert!(cold_records[0].content.activity.is_some());
    let retrieval_invocation_id = cold_records[0].event_id;
    assert_eq!(
        certificate_for(&cold_index, native_session_id).parser_revision(),
        CURRENT_PARSER_REVISION
    );
    let (_, _, _, checkpoint) = provider_checkpoint_envelope(&cold_index, native_session_id);
    assert_current_provider_checkpoint(&checkpoint);
    drop(cold_index);

    append_event(
        &path,
        exact_exec_result(retrieval_call_id, "retrievaldiscoverymarker result"),
    );
    let (appended, _) = incremental_refresh(&index_root, &registry, &cold);
    assert!(appended.failed_routes.is_empty());
    let appended_index = VerifiedIndex::open(&index_root).unwrap();
    let appended_records = records_for(&appended_index, native_session_id);
    assert_eq!(appended_records.len(), 2);
    assert_eq!(appended_records[0].event_id, retrieval_invocation_id);
    assert!(appended_records.iter().all(|record| {
        record.content.discovery_exclusion == Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
            && record.content.activity.is_some()
    }));
    assert!(
        appended_index
            .search_event_candidates("retrievaldiscoverymarker", 32)
            .unwrap()
            .into_iter()
            .all(|candidate| candidate.event.provider_session_id.as_deref()
                != Some(native_session_id))
    );
    drop(appended_index);

    append_event(
        &path,
        exec_call_with_command(ordinary_call_id, "ctx status"),
    );
    append_event(
        &path,
        exact_exec_result(ordinary_call_id, "ordinarycontrolmarker result"),
    );
    let controlled =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(controlled.failed_routes.is_empty());
    let controlled_index = VerifiedIndex::open(&index_root).unwrap();
    let controlled_records = records_for(&controlled_index, native_session_id);
    assert_eq!(controlled_records.len(), 4);
    assert_eq!(controlled_records[0].event_id, retrieval_invocation_id);
    let ordinary = controlled_records
        .iter()
        .filter(|record| {
            record
                .content
                .activity
                .as_ref()
                .and_then(|activity| activity.provider_call_id.as_ref())
                == Some(&TypedKey::Utf8(ordinary_call_id.to_owned()))
        })
        .collect::<Vec<_>>();
    assert_eq!(ordinary.len(), 2);
    assert!(ordinary
        .iter()
        .all(|record| record.content.discovery_exclusion.is_none()));
    assert!(
        controlled_index
            .search_event_candidates("ordinarycontrolmarker", 32)
            .unwrap()
            .into_iter()
            .any(|candidate| candidate.event.provider_session_id.as_deref()
                == Some(native_session_id))
    );
}

#[path = "codex_child_independence/terminal_results.rs"]
mod terminal_results;

#[path = "codex_child_independence/compressed.rs"]
mod compressed;
#[path = "codex_child_independence/continuous_append.rs"]
mod continuous_append;
#[path = "codex_child_independence/lifecycle.rs"]
mod lifecycle;
#[path = "codex_child_independence/repository.rs"]
mod repository;
