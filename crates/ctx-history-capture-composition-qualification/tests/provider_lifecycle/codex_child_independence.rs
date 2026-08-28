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

const CURRENT_PARSER_REVISION: &str = "codex-nativepath-core-activity-v11-jsonl-page-omission";

#[path = "codex_child_independence/quarantine.rs"]
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

fn custom_tool_result(call_id: &str, output: String) -> serde_json::Value {
    custom_tool_result_value(call_id, serde_json::Value::String(output))
}

fn custom_tool_call(call_id: &str) -> serde_json::Value {
    serde_json::json!({
        "timestamp": "2026-08-09T12:00:03Z",
        "type": "response_item",
        "payload": {
            "type": "custom_tool_call",
            "status": "completed",
            "call_id": call_id,
            "name": "image",
            "input": "{}"
        }
    })
}

fn custom_tool_result_value(call_id: &str, output: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "timestamp": "2026-08-09T12:00:04Z",
        "type": "response_item",
        "payload": {
            "type": "custom_tool_call_output",
            "call_id": call_id,
            "output": output
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
    for root in roots {
        register_landed_source_backed_route(
            &mut registry,
            fixture_provider_source_at(
                CaptureProvider::Codex,
                "codex_session_jsonl_tree",
                ProviderImportSupport::Native,
                *root,
            ),
            SourceBackedRouteSelection::Automatic,
        )
        .unwrap();
    }
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

#[path = "codex_child_independence/configured_roots.rs"]
mod configured_roots;
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
    let mut search_event_ids = search_event_candidates(index, search_marker, 32)
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

#[path = "codex_child_independence/item_completed.rs"]
mod item_completed;
#[path = "codex_child_independence/projection_behaviors.rs"]
mod projection_behaviors;
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
