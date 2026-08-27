//! End-to-end qualification for fx sessions-tree publication.

use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{BufReader, Write},
    path::{Path, PathBuf},
};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use ctx_history_capture_composition::*;
use ctx_history_capture_model::{ProviderRootDefinition, ProviderRootSourceIdentity};
use ctx_history_core::{CaptureProvider, CoreRecord, SourceAnchor, SourceKey, TypedKey};
use ctx_history_index::{VerifiedIndex, WriterOptions};
use ctx_history_provider_fx::{
    decode_authority, decode_watermark, replay_committed, replay_legacy_snapshot, BoundaryIntent,
    CanonicalState, ColdReplayDisposition, LegacyDefaults, ProviderId, ReplayLimits,
    SessionPreferences, TempFileScratch,
};
use ctx_history_source_discovery::{
    discover_provider_sources_with_context, CursorProbeFragment, CursorTranscriptProbeOutcome,
};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};

#[path = "support/lexical.rs"]
mod lexical_test_support;
use lexical_test_support::search_event_candidates;

const TOOL_FREE_ID: &str = "1700000000001-1700000000000000001-0000000000000001";
const READ_FILE_ID: &str = "1700000000002-1700000000000000002-0000000000000002";
const SOURCE_FORMAT: &str = "fx_sessions_tree";

fn writer_options() -> WriterOptions {
    WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    }
}

fn registry(sessions: &Path) -> SourceBackedProviderRegistry {
    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        ProviderSource {
            provider: CaptureProvider::Fx,
            path: sessions.to_path_buf(),
            exists: true,
            source_format: SOURCE_FORMAT,
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Native,
            catalog_support: ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Available,
            unsupported_reason: None,
            route_provenance: Default::default(),
        },
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();
    assert_eq!(registry.routes().len(), 1);
    registry
}

fn refresh(index: &Path, registry: &SourceBackedProviderRegistry) -> SourceBackedRefreshReceipt {
    refresh_source_backed_generation(index, registry, writer_options()).unwrap()
}

fn assert_clean_receipt(receipt: &SourceBackedRefreshReceipt, expected_sources: usize) {
    assert_eq!(receipt.scanned_routes, 1);
    assert!(receipt.failed_routes.is_empty());
    assert_eq!(receipt.source_failures.total(), 0);
    assert_eq!(receipt.logical_source_failures.total(), 0);
    assert_eq!(receipt.record_rejections.total(), 0);
    assert_eq!(receipt.sources.len(), expected_sources);
    assert_eq!(receipt.successful_route_ids.len(), 1);
    assert_eq!(receipt.complete_inventory_route_ids.len(), 1);
    assert_eq!(receipt.successful_route_outcomes.len(), 1);
    assert_eq!(
        receipt.successful_route_outcomes[0].logical_source_failure_total,
        0
    );
    assert_eq!(
        receipt.successful_route_outcomes[0].logical_source_retryable_failure_total,
        0
    );
}

fn assert_partial_receipt(receipt: &SourceBackedRefreshReceipt, expected_sources: usize) {
    assert!(
        receipt.failed_routes.is_empty(),
        "unexpected fx route failure: {receipt:#?}"
    );
    assert_eq!(receipt.source_failures.total(), 0);
    assert_eq!(receipt.logical_source_failures.total(), 0);
    assert_eq!(receipt.record_rejections.total(), 0);
    assert_eq!(receipt.sources.len(), expected_sources);
    assert_eq!(receipt.successful_route_ids.len(), 1);
    assert!(receipt.complete_inventory_route_ids.is_empty());
    assert_eq!(receipt.successful_route_outcomes.len(), 1);
    assert!(receipt.successful_route_outcomes[0].changed);
    assert_eq!(
        receipt.successful_route_outcomes[0].logical_source_failure_total,
        0
    );
    assert_eq!(
        receipt.successful_route_outcomes[0].logical_source_retryable_failure_total,
        0
    );
}

fn records(index: &Path) -> Vec<CoreRecord> {
    let verified = VerifiedIndex::open(index).unwrap();
    let mut records = verified
        .manifest()
        .sources
        .iter()
        .filter(|source| source.observation().source().provider() == CaptureProvider::Fx.as_str())
        .flat_map(|source| {
            verified
                .core_source_event_page(source.observation().source(), None, 64)
                .unwrap()
                .items
                .into_iter()
                .map(|item| item.core_record)
        })
        .collect::<Vec<_>>();
    records.sort_by(|left, right| {
        left.provider_session_id
            .cmp(&right.provider_session_id)
            .then_with(|| left.event_sequence.cmp(&right.event_sequence))
    });
    records
}

fn records_for(index: &Path, native_session_id: &str) -> Vec<CoreRecord> {
    records(index)
        .into_iter()
        .filter(|record| record.provider_session_id.as_deref() == Some(native_session_id))
        .collect()
}

fn source_for(index: &Path, native_session_id: &str) -> SourceKey {
    VerifiedIndex::open(index)
        .unwrap()
        .manifest()
        .sources
        .iter()
        .find(|source| {
            source_native_session_id(source.observation().source()) == Some(native_session_id)
        })
        .unwrap_or_else(|| panic!("missing fx source for {native_session_id}"))
        .observation()
        .source()
        .clone()
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

fn assert_search_hit(index: &Path, query: &str, native_session_id: &str) {
    let verified = VerifiedIndex::open(index).unwrap();
    assert!(
        search_event_candidates(&verified, query, 16)
            .iter()
            .any(|candidate| candidate.event.provider_session_id.as_deref()
                == Some(native_session_id)),
        "query {query:?} did not find fx session {native_session_id}"
    );
}

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if manifest.is_absolute() {
        return repo_root_from_manifest(manifest);
    }
    if let Ok(current) = std::env::current_dir() {
        let candidate = current.join(&manifest);
        if candidate.join("Cargo.toml").is_file() {
            return repo_root_from_manifest(candidate);
        }
    }
    if let Ok(executable) = std::env::current_exe() {
        for ancestor in executable.ancestors() {
            let candidate = ancestor.join(&manifest);
            if candidate.join("Cargo.toml").is_file() {
                return repo_root_from_manifest(candidate);
            }
        }
    }
    repo_root_from_manifest(manifest)
}

fn repo_root_from_manifest(manifest: PathBuf) -> PathBuf {
    manifest
        .ancestors()
        .find(|candidate| {
            candidate.join("Cargo.toml").is_file()
                && candidate
                    .join("tests/fixtures/provider-history/fx")
                    .is_dir()
        })
        .unwrap_or_else(|| panic!("locate ctx repository above {}", manifest.display()))
        .to_path_buf()
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    let mut entries = fs::read_dir(source)
        .unwrap()
        .map(|entry| entry.unwrap())
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let destination = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &destination);
        } else {
            fs::copy(entry.path(), destination).unwrap();
        }
    }
}

fn fixture_path(relative: &str) -> PathBuf {
    repo_root()
        .join("tests/fixtures/provider-history/fx")
        .join(relative)
}

fn copy_authentic_sessions(sessions: &Path) {
    fs::create_dir_all(sessions).unwrap();
    for (fixture, session_id) in [
        ("v0.0.6/native-v3-tool-free", TOOL_FREE_ID),
        ("v0.0.6/native-v3-read-file", READ_FILE_ID),
    ] {
        copy_tree(
            &fixture_path(&format!("{fixture}/.fx/sessions/{session_id}")),
            &sessions.join(session_id),
        );
    }
}

fn move_commit_boundary_inside_final_record(session: &Path) {
    let events = fs::read(session.join("events.jsonl")).unwrap();
    let final_record_start = events[..events.len() - 1]
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    let invalid_boundary = final_record_start + 5;
    assert!(invalid_boundary < events.len());
    let path = commit_path(session);
    let mut commit: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    commit["through_event_log_bytes"] = json!(invalid_boundary);
    fs::write(path, serde_json::to_vec(&commit).unwrap()).unwrap();
}

fn session_generation(session: &Path) -> String {
    let first = fs::read_to_string(session.join("events.jsonl"))
        .unwrap()
        .lines()
        .next()
        .map(str::to_owned)
        .unwrap();
    serde_json::from_str::<Value>(&first).unwrap()["log_generation"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn commit_path(session: &Path) -> PathBuf {
    session.join(format!("commit.{}.json", session_generation(session)))
}

fn event_id(sequence: u64) -> String {
    format!("{sequence:032x}")
}

fn append_envelope(
    session: &Path,
    sequence: u64,
    timestamp_ms: i64,
    kind: &str,
    payload: Value,
) -> String {
    let event_id = event_id(sequence);
    let envelope = json!({
        "schema_version": 1,
        "log_generation": session_generation(session),
        "seq": sequence,
        "event_id": event_id,
        "timestamp_ms": timestamp_ms,
        "kind": kind,
        "payload": payload,
    });
    let mut file = OpenOptions::new()
        .append(true)
        .open(session.join("events.jsonl"))
        .unwrap();
    serde_json::to_writer(&mut file, &envelope).unwrap();
    file.write_all(b"\n").unwrap();
    file.sync_all().unwrap();
    event_id
}

fn commit_through(session: &Path, sequence: u64, through_event_id: &str) {
    let commit = json!({
        "schema_version": 1,
        "session_id": session.file_name().unwrap().to_str().unwrap(),
        "log_generation": session_generation(session),
        "through_seq": sequence,
        "through_event_id": through_event_id,
        "through_event_log_bytes": fs::metadata(session.join("events.jsonl")).unwrap().len(),
    });
    fs::write(commit_path(session), serde_json::to_vec(&commit).unwrap()).unwrap();
}

fn assistant_turn(user: &str, assistant: &str) -> Value {
    json!({
        "conversation_language": "und-Latn",
        "total_input_tokens": 2_000,
        "total_output_tokens": 20,
        "turn": {
            "kind": "assistant",
            "user": {"text": user, "images": []},
            "assistant": assistant,
            "execution": {"schema_version": 3, "tool_steps": [], "files": []},
        },
    })
}

fn append_committed_turn(session: &Path, user: &str, assistant: &str) {
    let commit: Value = serde_json::from_slice(&fs::read(commit_path(session)).unwrap()).unwrap();
    let sequence = commit["through_seq"].as_u64().unwrap() + 1;
    let id = append_envelope(
        session,
        sequence,
        1_700_001_000_000 + sequence as i64,
        "history_turn_committed",
        assistant_turn(user, assistant),
    );
    commit_through(session, sequence, &id);
}

#[test]
fn authentic_v006_sessions_publish_searchable_content_and_noop_with_stable_identity() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("index");
    copy_authentic_sessions(&sessions);
    let registry = registry(&sessions);

    let cold = refresh(&index, &registry);
    assert_clean_receipt(&cold, 2);
    assert_eq!(VerifiedIndex::open(&index).unwrap().document_count(), 4);
    let cold_records = records(&index);
    assert_eq!(cold_records.len(), 4);
    let tool_free = records_for(&index, TOOL_FREE_ID);
    let read_file = records_for(&index, READ_FILE_ID);
    assert_eq!(tool_free.len(), 2);
    assert_eq!(read_file.len(), 2);
    assert_eq!(tool_free[0].role.as_deref(), Some("user"));
    assert_eq!(tool_free[1].role.as_deref(), Some("assistant"));
    assert_eq!(read_file[0].role.as_deref(), Some("user"));
    assert_eq!(read_file[1].role.as_deref(), Some("assistant"));
    assert!(tool_free[0]
        .content
        .meaningful_text()
        .contains("Return the fixture reply"));
    assert!(tool_free[1]
        .content
        .meaningful_text()
        .contains("SANITIZED_REPLY______"));
    assert!(read_file[0]
        .content
        .meaningful_text()
        .contains("Use only read_file"));
    assert!(read_file[1]
        .content
        .meaningful_text()
        .contains("SANITIZED_READ_OK______"));
    assert_ne!(tool_free[0].source, read_file[0].source);
    assert_ne!(tool_free[0].session_id, read_file[0].session_id);
    assert_search_hit(&index, "fixture reply", TOOL_FREE_ID);
    assert_search_hit(&index, "SANITIZED REPLY", TOOL_FREE_ID);
    assert_search_hit(&index, "only read file", READ_FILE_ID);
    assert_search_hit(&index, "SANITIZED READ OK", READ_FILE_ID);

    let noop = refresh(&index, &registry);
    assert_clean_receipt(&noop, 2);
    assert_eq!(noop.commit.generation_id, cold.commit.generation_id);
    assert_eq!(noop.sources, cold.sources);
    assert_eq!(records(&index), cold_records);
}

fn fx_probes() -> StaticProviderProbeCatalog {
    fn cursor(_: &Path) -> CursorTranscriptProbeOutcome {
        CursorTranscriptProbeOutcome::NotFound
    }
    StaticProviderProbeCatalog::new(CursorProbeFragment::new(cursor))
}

fn fx_report(probes: &StaticProviderProbeCatalog, context: &DiscoveryContext) -> DiscoveryReport {
    let mut report = discover_provider_sources_with_context(probes, context);
    report
        .sources
        .retain(|source| source.provider == CaptureProvider::Fx);
    report.issues.clear();
    report
}

fn install_tool_free_session(root: &Path) {
    copy_tree(
        &fixture_path(&format!(
            "v0.0.6/native-v3-tool-free/.fx/sessions/{TOOL_FREE_ID}"
        )),
        &root.join(TOOL_FREE_ID),
    );
}

fn published_sources(index: &Path, registry: &SourceBackedProviderRegistry) -> Vec<SourceKey> {
    refresh(index, registry)
        .commit
        .manifest()
        .sources
        .iter()
        .map(|source| source.observation().source().clone())
        .collect()
}

#[test]
fn automatic_and_canonical_configured_roots_converge_on_source_identity() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    let sessions = home.join(".fx/sessions");
    fs::create_dir_all(&cwd).unwrap();
    install_tool_free_session(&sessions);
    let probes = fx_probes();
    let automatic_context = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    );
    let automatic = build_automatic_source_backed_registry_from_report_with_probes(
        &probes,
        &automatic_context,
        &temp.path().join("automatic-data"),
        fx_report(&probes, &automatic_context),
    );
    let configured_context = automatic_context
        .clone()
        .with_configured_provider_roots(vec![ProviderRootDefinition {
            id: "canonical".to_owned(),
            provider: CaptureProvider::Fx,
            path: sessions.clone(),
            group: None,
            kind: None,
        }]);
    let configured = build_automatic_source_backed_registry_from_report_with_probes(
        &probes,
        &configured_context,
        &temp.path().join("configured-data"),
        fx_report(&probes, &configured_context),
    );
    assert!(automatic.issues.is_empty(), "{:?}", automatic.issues);
    assert!(configured.issues.is_empty(), "{:?}", configured.issues);
    let (_, _, roots) = configured.registry.applied_provider_roots().unwrap();
    assert_eq!(
        roots[0].source_identity(),
        ProviderRootSourceIdentity::Released
    );
    let automatic_sources =
        published_sources(&temp.path().join("automatic-index"), &automatic.registry);
    let configured_sources =
        published_sources(&temp.path().join("configured-index"), &configured.registry);
    assert_eq!(automatic_sources, configured_sources);

    let retained = BTreeMap::from([(
        "canonical".to_owned(),
        roots[0].retained_authority().unwrap(),
    )]);
    fs::remove_dir_all(&sessions).unwrap();
    let moved_sessions = temp.path().join("moved-sessions");
    install_tool_free_session(&moved_sessions);
    let moved_context =
        automatic_context.with_configured_provider_roots(vec![ProviderRootDefinition {
            id: "canonical".to_owned(),
            provider: CaptureProvider::Fx,
            path: moved_sessions,
            group: None,
            kind: None,
        }]);
    let moved = build_automatic_source_backed_registry_from_report_with_probes_and_retained_roots(
        &probes,
        &moved_context,
        &temp.path().join("moved-data"),
        fx_report(&probes, &moved_context),
        &retained,
    );
    assert!(moved.issues.is_empty(), "{:?}", moved.issues);
    assert_eq!(
        configured_sources,
        published_sources(&temp.path().join("moved-index"), &moved.registry)
    );
}

#[test]
fn distinct_configured_roots_publish_distinct_source_identities() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = temp.path().join("cwd");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cwd).unwrap();
    let roots = ["first", "second"]
        .map(|id| {
            let path = temp.path().join(format!("{id}-sessions"));
            install_tool_free_session(&path);
            ProviderRootDefinition {
                id: id.to_owned(),
                provider: CaptureProvider::Fx,
                path,
                group: None,
                kind: None,
            }
        })
        .to_vec();
    let context = DiscoveryContext::new(
        &home,
        &cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    )
    .with_automatic_provider_discovery(false)
    .with_configured_provider_roots(roots);
    let probes = fx_probes();
    let build = build_automatic_source_backed_registry_from_report_with_probes(
        &probes,
        &context,
        &temp.path().join("data"),
        fx_report(&probes, &context),
    );
    assert!(build.issues.is_empty(), "{:?}", build.issues);
    let (_, _, roots) = build.registry.applied_provider_roots().unwrap();
    assert!(roots
        .iter()
        .all(|root| root.source_identity() == ProviderRootSourceIdentity::NamedV1));
    let sources = published_sources(&temp.path().join("index"), &build.registry);
    assert_eq!(sources.len(), 2);
    assert_ne!(sources[0], sources[1]);
}

#[test]
fn uncommitted_tail_is_excluded_then_committed_append_preserves_old_ids() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    copy_tree(
        &fixture_path(&format!(
            "v0.0.6/native-v3-tool-free/.fx/sessions/{TOOL_FREE_ID}"
        )),
        &sessions.join(TOOL_FREE_ID),
    );
    let index = temp.path().join("index");
    let registry = registry(&sessions);
    let cold = refresh(&index, &registry);
    assert_clean_receipt(&cold, 1);
    let cold_records = records_for(&index, TOOL_FREE_ID);
    let cold_source = cold_records[0].source.clone();
    let cold_session = cold_records[0].session_id;

    let session = sessions.join(TOOL_FREE_ID);
    let dirty_event_id = append_envelope(
        &session,
        4,
        1_700_000_003_000,
        "history_turn_committed",
        assistant_turn(
            "zzfxuncommittedusertoken7q9",
            "zzfxuncommittedassistanttoken7q9",
        ),
    );
    let dirty = refresh(&index, &registry);
    assert_clean_receipt(&dirty, 1);
    assert_eq!(records_for(&index, TOOL_FREE_ID), cold_records);
    assert!(search_event_candidates(
        &VerifiedIndex::open(&index).unwrap(),
        "zzfxuncommittedassistanttoken7q9",
        16
    )
    .is_empty());

    commit_through(&session, 4, &dirty_event_id);
    let appended = refresh(&index, &registry);
    assert_clean_receipt(&appended, 1);
    assert_ne!(appended.commit.generation_id, dirty.commit.generation_id);
    let appended_records = records_for(&index, TOOL_FREE_ID);
    assert_eq!(appended_records.len(), 4);
    assert_eq!(&appended_records[..cold_records.len()], cold_records);
    assert_eq!(appended_records[0].source, cold_source);
    assert_eq!(appended_records[0].session_id, cold_session);
    assert!(appended_records[2..]
        .iter()
        .all(|record| record.source == cold_source && record.session_id == cold_session));
    assert_search_hit(&index, "zzfxuncommittedusertoken7q9", TOOL_FREE_ID);
    assert_search_hit(&index, "zzfxuncommittedassistanttoken7q9", TOOL_FREE_ID);
}

#[test]
fn committed_boundary_inside_a_physical_record_fails_closed_cold_and_warm() {
    let temp = tempfile::tempdir().unwrap();

    let cold_sessions = temp.path().join("cold-sessions");
    install_tool_free_session(&cold_sessions);
    move_commit_boundary_inside_final_record(&cold_sessions.join(TOOL_FREE_ID));
    let cold_index = temp.path().join("cold-index");
    let cold_error =
        refresh_source_backed_generation(&cold_index, &registry(&cold_sessions), writer_options())
            .expect_err("a cold mid-record committed boundary must not publish");
    assert!(
        cold_error
            .to_string()
            .contains("fx committed frame is incomplete or oversized"),
        "{cold_error}"
    );

    let warm_sessions = temp.path().join("warm-sessions");
    install_tool_free_session(&warm_sessions);
    let warm_index = temp.path().join("warm-index");
    let warm_registry = registry(&warm_sessions);
    let imported = refresh(&warm_index, &warm_registry);
    assert_clean_receipt(&imported, 1);
    let records_before = records_for(&warm_index, TOOL_FREE_ID);
    move_commit_boundary_inside_final_record(&warm_sessions.join(TOOL_FREE_ID));
    let rejected = refresh(&warm_index, &warm_registry);
    assert_eq!(records_for(&warm_index, TOOL_FREE_ID), records_before);
    assert_eq!(
        rejected.commit.generation_id, imported.commit.generation_id,
        "failed warm refresh must retain the last durable generation"
    );
}

#[test]
fn warm_same_eof_commit_sequence_and_event_id_rewrites_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    install_tool_free_session(&sessions);
    let index = temp.path().join("index");
    let registry = registry(&sessions);
    let imported = refresh(&index, &registry);
    assert_clean_receipt(&imported, 1);
    let records_before = records_for(&index, TOOL_FREE_ID);
    let mut durable_generation = imported.commit.generation_id.clone();
    let session = sessions.join(TOOL_FREE_ID);
    let path = commit_path(&session);
    let original = fs::read(&path).unwrap();
    let original_commit: Value = serde_json::from_slice(&original).unwrap();

    for (label, corrupted) in [
        ("through_seq", {
            let mut commit = original_commit.clone();
            commit["through_seq"] = json!(commit["through_seq"]
                .as_u64()
                .unwrap()
                .checked_sub(1)
                .unwrap());
            commit
        }),
        ("through_event_id", {
            let mut commit = original_commit.clone();
            commit["through_event_id"] = json!("ffffffffffffffffffffffffffffffff");
            commit
        }),
    ] {
        fs::write(&path, serde_json::to_vec(&corrupted).unwrap()).unwrap();
        let rejected = refresh(&index, &registry);
        let [failure] = rejected.failed_routes.as_slice() else {
            panic!(
                "same-EOF {label} corruption must produce one carried-forward route failure: {rejected:#?}"
            );
        };
        assert_eq!(failure.class, SourceBackedSourceFailureClass::Unreadable);
        assert!(failure.carried_forward);
        assert!(rejected.logical_source_failures.is_empty());
        assert_eq!(
            records_for(&index, TOOL_FREE_ID),
            records_before,
            "same-EOF {label} corruption replaced durable records"
        );
        assert_eq!(
            rejected.commit.generation_id, durable_generation,
            "same-EOF {label} corruption published a generation"
        );

        fs::write(&path, &original).unwrap();
        let recovered = refresh(&index, &registry);
        assert_clean_receipt(&recovered, 1);
        assert_eq!(records_for(&index, TOOL_FREE_ID), records_before);
        durable_generation = recovered.commit.generation_id;
    }
}

#[test]
fn warm_valid_authority_rewrite_recertifies_without_changing_logical_records() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    install_tool_free_session(&sessions);
    let index = temp.path().join("index");
    let registry = registry(&sessions);
    let imported = refresh(&index, &registry);
    assert_clean_receipt(&imported, 1);
    let records_before = records_for(&index, TOOL_FREE_ID);

    let path = sessions.join(TOOL_FREE_ID).join("authority.json");
    let mut authority: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    authority["authority_id"] = json!("ffffffffffffffffffffffffffffffff");
    fs::write(&path, serde_json::to_vec(&authority).unwrap()).unwrap();

    let recertified = refresh(&index, &registry);
    assert_clean_receipt(&recertified, 1);
    assert_ne!(
        recertified.commit.generation_id, imported.commit.generation_id,
        "a changed terminal dependency must be bound into a new certificate"
    );
    assert_eq!(records_for(&index, TOOL_FREE_ID), records_before);
    let noop = refresh(&index, &registry);
    assert_clean_receipt(&noop, 1);
    assert_eq!(noop.commit.generation_id, recertified.commit.generation_id);
}

fn assert_broken_session_retained_while_sibling_advances(
    index: &Path,
    registry: &SourceBackedProviderRegistry,
    sessions: &Path,
    label: &str,
) {
    let broken_before = records_for(index, TOOL_FREE_ID);
    let sibling_before = records_for(index, READ_FILE_ID);
    append_committed_turn(
        &sessions.join(READ_FILE_ID),
        &format!("valid sibling user {label}"),
        &format!("valid sibling assistant {label}"),
    );
    let receipt = refresh(index, registry);
    assert_partial_receipt(&receipt, 2);
    assert_eq!(records_for(index, TOOL_FREE_ID), broken_before);
    let sibling_after = records_for(index, READ_FILE_ID);
    assert_eq!(sibling_after.len(), sibling_before.len() + 2);
    assert_eq!(sibling_after[..sibling_before.len()], sibling_before);
    assert!(sibling_after
        .last()
        .unwrap()
        .content
        .meaningful_text()
        .contains(label));
}

#[test]
fn pending_malformed_and_markerless_members_retain_prior_state_and_publish_valid_sibling() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("index");
    copy_authentic_sessions(&sessions);
    let registry = registry(&sessions);
    let cold = refresh(&index, &registry);
    assert_clean_receipt(&cold, 2);
    let broken = sessions.join(TOOL_FREE_ID);
    let authority_path = broken.join("authority.json");
    let authority = fs::read(&authority_path).unwrap();
    let retained_source = source_for(&index, TOOL_FREE_ID);

    fs::write(broken.join("commit.pending.json"), b"{}\n").unwrap();
    assert_broken_session_retained_while_sibling_advances(
        &index,
        &registry,
        &sessions,
        "pending-marker-sibling",
    );
    fs::remove_file(broken.join("commit.pending.json")).unwrap();
    let recovered = refresh(&index, &registry);
    assert_clean_receipt(&recovered, 2);
    assert_eq!(source_for(&index, TOOL_FREE_ID), retained_source);

    fs::write(&authority_path, b"{\n").unwrap();
    assert_broken_session_retained_while_sibling_advances(
        &index,
        &registry,
        &sessions,
        "malformed-marker-sibling",
    );
    fs::write(&authority_path, &authority).unwrap();
    let recovered = refresh(&index, &registry);
    assert_clean_receipt(&recovered, 2);
    assert_eq!(source_for(&index, TOOL_FREE_ID), retained_source);

    fs::remove_file(&authority_path).unwrap();
    assert_broken_session_retained_while_sibling_advances(
        &index,
        &registry,
        &sessions,
        "markerless-v3-sibling",
    );
    fs::write(&authority_path, authority).unwrap();
    let recovered = refresh(&index, &registry);
    assert_clean_receipt(&recovered, 2);
    assert_eq!(source_for(&index, TOOL_FREE_ID), retained_source);
}

fn legacy_defaults(sessions: &Path) -> LegacyDefaults {
    LegacyDefaults {
        source_root: sessions.display().to_string(),
        preferences: SessionPreferences {
            provider: ProviderId::Gateway,
            model: "fx-legacy".to_owned(),
            effort: "auto".to_owned(),
            fast_mode: false,
        },
    }
}

fn write_v3_migration(session: &Path, state: &CanonicalState, generation: &str) {
    fs::write(
        session.join("authority.json"),
        serde_json::to_vec(&json!({
            "schema_version": 1,
            "session_id": state.id,
            "authority_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "storage_format": "event_log_v1",
            "source": "legacy_migration",
        }))
        .unwrap(),
    )
    .unwrap();
    let mut bytes = Vec::new();
    let mut last_event_id = event_id(1);
    let start = json!({
        "schema_version": 1,
        "log_generation": generation,
        "seq": 1,
        "event_id": event_id(1),
        "timestamp_ms": state.created_at_ms,
        "kind": "session_started",
        "payload": {
            "id": state.id,
            "created_at_ms": state.created_at_ms,
            "origin_workspace_root": state.origin_workspace_root,
            "workspace_root": state.workspace_root,
            "conversation_language": state.conversation_language,
            "preferences": state.preferences,
            "usage": state.usage,
        },
    });
    serde_json::to_writer(&mut bytes, &start).unwrap();
    bytes.push(b'\n');
    for (index, turn) in state.history.iter().enumerate() {
        let sequence = index as u64 + 2;
        last_event_id = event_id(sequence);
        let event = json!({
            "schema_version": 1,
            "log_generation": generation,
            "seq": sequence,
            "event_id": last_event_id,
            "timestamp_ms": state.updated_at_ms,
            "kind": "history_turn_committed",
            "payload": {
                "conversation_language": state.conversation_language,
                "total_input_tokens": state.total_input_tokens,
                "total_output_tokens": state.total_output_tokens,
                "turn": turn.structured_value().unwrap(),
            },
        });
        serde_json::to_writer(&mut bytes, &event).unwrap();
        bytes.push(b'\n');
    }
    fs::write(session.join("events.jsonl"), &bytes).unwrap();
    let through_seq = state.history.len() as u64 + 1;
    fs::write(
        session.join(format!("commit.{generation}.json")),
        serde_json::to_vec(&json!({
            "schema_version": 1,
            "session_id": state.id,
            "log_generation": generation,
            "through_seq": through_seq,
            "through_event_id": last_event_id,
            "through_event_log_bytes": bytes.len(),
        }))
        .unwrap(),
    )
    .unwrap();
    fs::remove_file(session.join("session.json")).unwrap();
}

#[test]
fn legacy_v1_v2_import_and_v3_migration_preserve_source_and_event_identity() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    fs::create_dir_all(sessions.join("old")).unwrap();
    fs::create_dir_all(sessions.join("legacy-v2")).unwrap();
    let v1 = fs::read(fixture_path(
        "upstream-v0.3.73-test-source/schema-v1/.fx/sessions/legacy-v1/session.json",
    ))
    .unwrap();
    let v2 = fs::read(fixture_path(
        "upstream-v0.3.73-test-source/schema-v2/.fx/sessions/legacy-v2/session.json",
    ))
    .unwrap();
    fs::write(sessions.join("old/session.json"), &v1).unwrap();
    fs::write(sessions.join("legacy-v2/session.json"), &v2).unwrap();
    let index = temp.path().join("index");
    let registry = registry(&sessions);

    let imported = refresh(&index, &registry);
    assert_clean_receipt(&imported, 2);
    assert_eq!(records_for(&index, "old").len(), 0);
    let v2_records = records_for(&index, "legacy-v2");
    assert_eq!(v2_records.len(), 3);
    assert_search_hit(&index, "background command turn", "legacy-v2");
    let v1_source = source_for(&index, "old");
    let v2_source = source_for(&index, "legacy-v2");

    let defaults = legacy_defaults(&sessions);
    let v1_state = replay_legacy_snapshot(&v1, &defaults, ReplayLimits::default())
        .unwrap()
        .state;
    let v2_state = replay_legacy_snapshot(&v2, &defaults, ReplayLimits::default())
        .unwrap()
        .state;
    write_v3_migration(
        &sessions.join("old"),
        &v1_state,
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    );
    write_v3_migration(
        &sessions.join("legacy-v2"),
        &v2_state,
        "cccccccccccccccccccccccccccccccc",
    );

    let migrated = refresh(&index, &registry);
    assert_clean_receipt(&migrated, 2);
    assert_ne!(migrated.commit.generation_id, imported.commit.generation_id);
    assert_eq!(source_for(&index, "old"), v1_source);
    assert_eq!(source_for(&index, "legacy-v2"), v2_source);
    assert_eq!(records_for(&index, "old").len(), 0);
    assert_eq!(records_for(&index, "legacy-v2"), v2_records);
}

fn canonical_state(session: &Path) -> CanonicalState {
    let authority = decode_authority(
        &fs::read(session.join("authority.json")).unwrap(),
        ReplayLimits::default(),
    )
    .unwrap();
    let watermark = decode_watermark(
        &fs::read(commit_path(session)).unwrap(),
        ReplayLimits::default(),
    )
    .unwrap();
    let mut events = BufReader::new(fs::File::open(session.join("events.jsonl")).unwrap());
    match replay_committed(
        &authority,
        &watermark,
        &mut events,
        BoundaryIntent::Stable,
        &TempFileScratch,
        ReplayLimits::default(),
    )
    .unwrap()
    {
        ColdReplayDisposition::Canonical(replay) => replay.state,
        ColdReplayDisposition::UnsafePending(intent) => {
            panic!("stable fixture unexpectedly remained pending: {intent:?}")
        }
    }
}

#[test]
fn committed_log_compaction_replacement_rebuilds_without_changing_logical_identity() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    copy_tree(
        &fixture_path(&format!(
            "v0.0.6/native-v3-tool-free/.fx/sessions/{TOOL_FREE_ID}"
        )),
        &sessions.join(TOOL_FREE_ID),
    );
    let index = temp.path().join("index");
    let registry = registry(&sessions);
    let cold = refresh(&index, &registry);
    assert_clean_receipt(&cold, 1);
    let cold_records = records_for(&index, TOOL_FREE_ID);
    let cold_source = source_for(&index, TOOL_FREE_ID);
    let session = sessions.join(TOOL_FREE_ID);
    let state = canonical_state(&session);
    let encoded = serde_json::to_vec(&state).unwrap();
    let digest = format!("{:x}", Sha256::digest(&encoded));
    let replacement_id = "dddddddddddddddddddddddddddddddd";
    let timestamp = state.updated_at_ms;
    append_envelope(
        &session,
        4,
        timestamp,
        "state_replacement_started",
        json!({
            "replacement_id": replacement_id,
            "reason": "log_compaction",
            "encoded_bytes": encoded.len(),
            "sha256": digest,
            "chunk_count": 1,
        }),
    );
    append_envelope(
        &session,
        5,
        timestamp,
        "state_replacement_chunk",
        json!({
            "replacement_id": replacement_id,
            "chunk_index": 0,
            "raw_bytes": encoded.len(),
            "chunk_sha256": digest,
            "base64": STANDARD.encode(&encoded),
        }),
    );
    let committed_id = append_envelope(
        &session,
        6,
        timestamp,
        "state_replacement_committed",
        json!({
            "replacement_id": replacement_id,
            "encoded_bytes": encoded.len(),
            "sha256": digest,
            "chunk_count": 1,
        }),
    );
    commit_through(&session, 6, &committed_id);

    let compacted = refresh(&index, &registry);
    assert_clean_receipt(&compacted, 1);
    assert_ne!(compacted.commit.generation_id, cold.commit.generation_id);
    assert_eq!(source_for(&index, TOOL_FREE_ID), cold_source);
    assert_eq!(records_for(&index, TOOL_FREE_ID), cold_records);
}
