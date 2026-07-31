use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use ctx_history_core::{
    BatchHydrationRequest, CaptureProvider, ContentSourceResolver, EventHydrationRequest,
    HydrationFailureKind,
};
use ctx_history_index::{VerifiedIndex, WriterOptions};
use serde_json::{json, Value};

use crate::{
    provider::source_backed::{
        family::jsonl::{
            jsonl_family_work, reset_jsonl_family_work, set_after_jsonl_hydration_observation_hook,
        },
        refresh_source_backed_generation, register_landed_source_backed_route,
        SourceBackedProviderRegistry, SourceBackedRouteSelection,
    },
    ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceStatus,
};

const DIRECT_PROVIDERS: [CaptureProvider; 7] = [
    CaptureProvider::Antigravity,
    CaptureProvider::CopilotCli,
    CaptureProvider::FactoryAiDroid,
    CaptureProvider::Qoder,
    CaptureProvider::QwenCode,
    CaptureProvider::Tabnine,
    CaptureProvider::Windsurf,
];

const QWEN_LIFECYCLE_TRANSCRIPT: &str = concat!(
    "{\"uuid\":\"qwen-event\",\"sessionId\":\"qwen-life\",",
    "\"timestamp\":\"2026-07-25T12:00:01Z\",\"type\":\"user\",",
    "\"cwd\":\"/workspace/qwen\",\"message\":{\"role\":\"user\",",
    "\"content\":[{\"type\":\"text\",\"text\":\"lifecycle sentinel\"}]},",
    "\"model\":\"qwen3-coder\"}\n",
    "{\"uuid\":\"qwen-event-2\",\"sessionId\":\"qwen-life\",",
    "\"timestamp\":\"2026-07-25T12:00:02Z\",\"type\":\"assistant\",",
    "\"cwd\":\"/workspace/qwen\",\"message\":{\"role\":\"assistant\",",
    "\"content\":[{\"type\":\"text\",\"text\":\"second sentinel\"}]},",
    "\"model\":\"qwen3-coder\"}\n"
);

#[derive(Debug)]
struct DirectFixture {
    root: PathBuf,
    transcript: PathBuf,
    source_format: &'static str,
}

fn writer_options() -> WriterOptions {
    WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    }
}

fn direct_registry(
    provider: CaptureProvider,
    root: &Path,
    source_format: &'static str,
) -> SourceBackedProviderRegistry {
    let source = ProviderSource {
        provider,
        path: root.to_path_buf(),
        exists: true,
        source_format,
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        status: ProviderSourceStatus::Available,
        unsupported_reason: None,
    };
    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        source,
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();
    registry
}

fn qwen_fixture(parent: &Path) -> DirectFixture {
    let root = parent.join("qwen");
    let transcript = root.join("workspace/chats/qwen-life.jsonl");
    fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    fs::write(&transcript, QWEN_LIFECYCLE_TRANSCRIPT).unwrap();
    DirectFixture {
        root,
        transcript,
        source_format: "qwen_code_chat_jsonl_tree",
    }
}

fn write_records(path: &Path, records: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = Vec::new();
    for record in records {
        serde_json::to_writer(&mut bytes, record).unwrap();
        bytes.push(b'\n');
    }
    fs::write(path, bytes).unwrap();
}

fn direct_fixture(parent: &Path, provider: CaptureProvider, marker: &str) -> DirectFixture {
    let (root, transcript, source_format, records) = match provider {
        CaptureProvider::Antigravity => {
            let root = parent.join("antigravity");
            let transcript = root.join("agy-life/.system_generated/logs/transcript_full.jsonl");
            (
                root,
                transcript,
                crate::ANTIGRAVITY_CLI_SOURCE_FORMAT,
                vec![json!({
                    "step_index": 0,
                    "source": "user",
                    "type": "USER_INPUT",
                    "status": "ok",
                    "created_at": "2026-07-25T12:00:00Z",
                    "content": marker,
                })],
            )
        }
        CaptureProvider::CopilotCli => {
            let root = parent.join("copilot");
            let transcript = root.join("copilot-life/events.jsonl");
            (
                root,
                transcript,
                crate::COPILOT_CLI_SOURCE_FORMAT,
                vec![
                    json!({
                        "id": "copilot-life-start",
                        "timestamp": "2026-07-25T12:00:00Z",
                        "type": "session.start",
                        "data": {
                            "sessionId": "copilot-life",
                            "startTime": "2026-07-25T12:00:00Z",
                            "selectedModel": "gpt-5-mini",
                            "context": {"cwd": "/workspace/copilot"},
                        },
                    }),
                    json!({
                        "id": "copilot-message",
                        "timestamp": "2026-07-25T12:00:01Z",
                        "type": "user.message",
                        "data": {"content": marker},
                    }),
                ],
            )
        }
        CaptureProvider::FactoryAiDroid => {
            let root = parent.join("factory");
            let transcript = root.join("project/droid-life.jsonl");
            (
                root,
                transcript,
                crate::FACTORY_DROID_SOURCE_FORMAT,
                vec![
                    json!({
                        "type": "session_start",
                        "sessionId": "droid-life",
                        "timestamp": "2026-07-25T12:00:00Z",
                        "cwd": "/workspace/factory",
                        "model": "factory/droid",
                    }),
                    json!({
                        "type": "message",
                        "id": "droid-message",
                        "timestamp": "2026-07-25T12:00:01Z",
                        "message": {
                            "role": "user",
                            "content": [{"type": "text", "text": marker}],
                        },
                        "model": "factory/droid",
                    }),
                ],
            )
        }
        CaptureProvider::Qoder => {
            let root = parent.join("qoder");
            let transcript = root.join("sanitized-workspace/transcript/qoder-life.jsonl");
            (
                root,
                transcript,
                "qoder_transcript_jsonl_tree",
                vec![
                    json!({
                        "type": "session_meta",
                        "sessionId": "qoder-life",
                        "uuid": "qoder-life-meta",
                        "timestamp": "2026-07-25T12:00:00Z",
                        "cwd": "/workspace/qoder",
                        "data": {
                            "meta_type": "session_info",
                            "content": {"mode": "agent", "session_type": "assistant"},
                        },
                    }),
                    json!({
                        "type": "user",
                        "sessionId": "qoder-life",
                        "uuid": "qoder-message",
                        "timestamp": "2026-07-25T12:00:01Z",
                        "cwd": "/workspace/qoder",
                        "message": {"role": "user", "content": marker},
                        "model": "qoder-agent",
                    }),
                ],
            )
        }
        CaptureProvider::QwenCode => {
            let root = parent.join("qwen");
            let transcript = root.join("sanitized-workspace/chats/qwen-life.jsonl");
            (
                root,
                transcript,
                "qwen_code_chat_jsonl_tree",
                vec![json!({
                    "uuid": "qwen-message",
                    "sessionId": "qwen-life",
                    "timestamp": "2026-07-25T12:00:01Z",
                    "type": "user",
                    "cwd": "/workspace/qwen",
                    "message": {
                        "role": "user",
                        "content": [{"type": "text", "text": marker}],
                    },
                    "model": "qwen3-coder",
                })],
            )
        }
        CaptureProvider::Tabnine => {
            let root = parent.join("tabnine");
            let transcript = root.join("tmp/project/chats/session-tabnine-life.jsonl");
            (
                root,
                transcript,
                crate::TABNINE_CLI_SOURCE_FORMAT,
                vec![
                    json!({
                        "sessionId": "tabnine-life",
                        "projectHash": "tabnine-nativepath-project",
                        "startTime": "2026-07-25T12:00:00Z",
                        "lastUpdated": "2026-07-25T12:00:59Z",
                        "kind": "main",
                        "directories": ["/workspace/tabnine"],
                    }),
                    json!({
                        "id": "tabnine-message",
                        "timestamp": "2026-07-25T12:00:01Z",
                        "type": "user",
                        "content": marker,
                        "model": "tabnine-agent",
                    }),
                ],
            )
        }
        CaptureProvider::Windsurf => {
            let root = parent.join("windsurf");
            let transcript = root.join("windsurf-life.jsonl");
            (
                root,
                transcript,
                "windsurf_cascade_hook_transcript_jsonl_tree",
                vec![json!({
                    "status": "done",
                    "type": "user_input",
                    "timestamp": "2026-07-25T12:00:00Z",
                    "user_input": {"user_response": marker},
                })],
            )
        }
        _ => panic!("test fixture requested a non-direct JSONL provider"),
    };
    write_records(&transcript, &records);
    DirectFixture {
        root,
        transcript,
        source_format,
    }
}

fn append_record(provider: CaptureProvider, transcript: &Path, marker: &str) {
    let record = match provider {
        CaptureProvider::Antigravity => json!({
            "step_index": 1,
            "source": "assistant",
            "type": "ASSISTANT_RESPONSE",
            "status": "ok",
            "created_at": "2026-07-25T12:00:02Z",
            "content": marker,
        }),
        CaptureProvider::CopilotCli => json!({
            "id": "copilot-appended",
            "timestamp": "2026-07-25T12:00:02Z",
            "type": "assistant.message",
            "data": {"content": marker},
        }),
        CaptureProvider::FactoryAiDroid => json!({
            "type": "message",
            "id": "droid-appended",
            "timestamp": "2026-07-25T12:00:02Z",
            "message": {
                "role": "assistant",
                "content": [{"type": "text", "text": marker}],
            },
            "model": "factory/droid",
        }),
        CaptureProvider::Qoder => json!({
            "type": "assistant",
            "sessionId": "qoder-life",
            "uuid": "qoder-appended",
            "timestamp": "2026-07-25T12:00:02Z",
            "cwd": "/workspace/qoder",
            "message": {"role": "assistant", "content": marker},
            "model": "qoder-agent",
        }),
        CaptureProvider::QwenCode => json!({
            "uuid": "qwen-appended",
            "sessionId": "qwen-life",
            "timestamp": "2026-07-25T12:00:02Z",
            "type": "assistant",
            "cwd": "/workspace/qwen",
            "message": {
                "role": "assistant",
                "content": [{"type": "text", "text": marker}],
            },
            "model": "qwen3-coder",
        }),
        CaptureProvider::Tabnine => json!({
            "id": "tabnine-appended",
            "timestamp": "2026-07-25T12:00:02Z",
            "type": "assistant",
            "content": marker,
            "model": "tabnine-agent",
        }),
        CaptureProvider::Windsurf => json!({
            "status": "done",
            "type": "assistant_response",
            "timestamp": "2026-07-25T12:00:02Z",
            "assistant_response": marker,
        }),
        _ => panic!("test fixture requested a non-direct JSONL provider"),
    };
    let mut file = OpenOptions::new().append(true).open(transcript).unwrap();
    serde_json::to_writer(&mut file, &record).unwrap();
    file.write_all(b"\n").unwrap();
}

#[test]
fn seven_direct_jsonl_providers_share_cold_noop_append_rewrite_delete_and_hydration() {
    const COLD_MARKER: &str = "cold-marker";
    const EDIT_MARKER: &str = "edit-marker";
    const APPEND_MARKER: &str = "append-marker";
    assert_eq!(COLD_MARKER.len(), EDIT_MARKER.len());

    for provider in DIRECT_PROVIDERS {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let fixture = direct_fixture(temp.path(), provider, COLD_MARKER);
        let registry = direct_registry(provider, &fixture.root, fixture.source_format);
        let index_root = temp.path().join("index");

        reset_jsonl_family_work();
        let cold =
            refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
        assert_eq!(cold.sources.len(), 1, "{provider} cold source count");
        assert!(
            cold.sources[0].counts().indexed_documents > 0,
            "{provider} cold projection was empty"
        );
        assert!(
            jsonl_family_work().provider_projections > 0,
            "{provider} bypassed the shared family projector"
        );
        let source = cold.sources[0].observation().source().clone();
        let mut cold_events = VerifiedIndex::open(&index_root)
            .unwrap()
            .source_event_page(&source, None, 32)
            .unwrap()
            .items;
        cold_events.sort_by_key(|event| event.event_sequence);
        let requests = cold_events
            .iter()
            .map(|event| EventHydrationRequest::new(event.event_id, event.locator.clone()).unwrap())
            .collect::<Vec<_>>();
        reset_jsonl_family_work();
        let hydrated = registry
            .resolver_registry()
            .hydrate_batch(&BatchHydrationRequest::new(requests.clone()).unwrap())
            .unwrap()
            .into_records();
        let rewritten_request = requests[hydrated
            .iter()
            .position(|record| {
                String::from_utf8_lossy(&record.provider_bytes).contains(COLD_MARKER)
            })
            .unwrap_or_else(|| panic!("{provider} hydration lost the provider body"))]
        .clone();
        assert_eq!(
            jsonl_family_work().leaf_opens,
            1,
            "{provider} grouped hydration opened its leaf more than once"
        );
        assert_eq!(jsonl_family_work().provider_projections, 0);

        reset_jsonl_family_work();
        let unchanged =
            refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
        assert_eq!(
            jsonl_family_work().provider_projections,
            0,
            "{provider} no-op projected provider records"
        );
        assert_eq!(unchanged.sources, cold.sources);
        assert_eq!(unchanged.commit.generation_id, cold.commit.generation_id);
        assert_eq!(unchanged.commit.opstamp, cold.commit.opstamp);

        append_record(provider, &fixture.transcript, APPEND_MARKER);
        reset_jsonl_family_work();
        let appended =
            refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
        assert_eq!(
            jsonl_family_work().provider_projections,
            1,
            "{provider} did not certify exactly one appended physical record"
        );
        assert!(appended.sources[0]
            .observation()
            .source()
            .exact_descriptor_eq(&source));
        let mut appended_events = VerifiedIndex::open(&index_root)
            .unwrap()
            .source_event_page(&source, None, 32)
            .unwrap()
            .items;
        appended_events.sort_by_key(|event| event.event_sequence);
        assert!(
            appended_events.len() > cold_events.len(),
            "{provider} append did not publish an event"
        );
        assert_eq!(
            appended_events
                .iter()
                .take(cold_events.len())
                .map(|event| (event.event_id, event.event_sequence, event.locator.clone()))
                .collect::<Vec<_>>(),
            cold_events
                .iter()
                .map(|event| (event.event_id, event.event_sequence, event.locator.clone()))
                .collect::<Vec<_>>(),
            "{provider} append changed prior identities or locators"
        );

        let bytes = fs::read(&fixture.transcript).unwrap();
        let cold_occurrences = bytes
            .windows(COLD_MARKER.len())
            .filter(|window| *window == COLD_MARKER.as_bytes())
            .count();
        assert_eq!(cold_occurrences, 1, "{provider} fixture marker drifted");
        let rewritten = String::from_utf8(bytes)
            .unwrap()
            .replacen(COLD_MARKER, EDIT_MARKER, 1);
        fs::write(&fixture.transcript, rewritten).unwrap();
        reset_jsonl_family_work();
        let replaced =
            refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
        assert_eq!(
            jsonl_family_work().provider_projections as u64,
            replaced.sources[0].counts().complete_records,
            "{provider} rewrite was not a complete replacement pass"
        );
        assert!(replaced.sources[0]
            .observation()
            .source()
            .exact_descriptor_eq(&source));
        assert_ne!(
            replaced.commit.generation_id, appended.commit.generation_id,
            "{provider} rewrite did not publish"
        );
        let stale = registry
            .resolver_registry()
            .hydrate_event(&rewritten_request)
            .unwrap_err();
        assert_eq!(
            stale.kind,
            HydrationFailureKind::StaleRecordEvidence,
            "{provider} rewrite did not preserve stale typing"
        );

        fs::remove_file(&fixture.transcript).unwrap();
        let deleted =
            refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
        assert!(
            deleted.sources.is_empty(),
            "{provider} deletion was not exact"
        );
        assert_eq!(deleted.removals.len(), 1, "{provider} deletion proof count");
        let confirmed_deleted = direct_registry(provider, &fixture.root, fixture.source_format)
            .resolver_registry()
            .hydrate_event(&rewritten_request)
            .unwrap_err();
        assert_eq!(
            confirmed_deleted.kind,
            HydrationFailureKind::ConfirmedDeleted,
            "{provider} deletion did not preserve confirmed-deleted typing"
        );
        let deletion_noop =
            refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
        assert_eq!(
            deletion_noop.commit.generation_id,
            deleted.commit.generation_id
        );
        assert_eq!(deletion_noop.commit.opstamp, deleted.commit.opstamp);
    }
}

#[test]
fn cold_identity_rejection_isolated_from_valid_sibling_and_deletion_remains_exact() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let fixture = qwen_fixture(temp.path());
    let chats = fixture.transcript.parent().unwrap();
    let malformed = chats.join("malformed.jsonl");
    fs::write(&malformed, b"{\"sessionId\":\n").unwrap();
    fs::write(
        chats.join("retained.jsonl"),
        QWEN_LIFECYCLE_TRANSCRIPT.replace("qwen-life", "qwen-retained"),
    )
    .unwrap();
    let registry = direct_registry(
        CaptureProvider::QwenCode,
        &fixture.root,
        fixture.source_format,
    );
    let index_root = temp.path().join("index");

    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(cold.sources.len(), 2);
    let unchanged =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(unchanged.sources, cold.sources);

    fs::remove_file(&fixture.transcript).unwrap();
    let deleted =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(deleted.sources.len(), 1);
    assert_eq!(deleted.removals.len(), 1);
    assert!(malformed.exists());
}

#[test]
fn active_source_family_contract_direct_jsonl_hydration_rejects_crossed_event_identity() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let fixture = qwen_fixture(temp.path());
    let registry = direct_registry(
        CaptureProvider::QwenCode,
        &fixture.root,
        fixture.source_format,
    );
    let index_root = temp.path().join("index");
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let source = cold.sources[0].observation().source();
    let mut events = VerifiedIndex::open(&index_root)
        .unwrap()
        .source_event_page(source, None, 10)
        .unwrap()
        .items;
    events.sort_by_key(|event| event.event_sequence);
    assert_eq!(events.len(), 2);

    let crossed =
        EventHydrationRequest::new(events[0].event_id, events[1].locator.clone()).unwrap();
    let error = registry
        .resolver_registry()
        .hydrate_event(&crossed)
        .unwrap_err();
    assert_eq!(error.kind, HydrationFailureKind::InvalidLocator);
}

#[test]
fn active_source_family_contract_direct_jsonl_hydration_rejects_identity_rewrite_with_append() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let fixture = qwen_fixture(temp.path());
    let registry = direct_registry(
        CaptureProvider::QwenCode,
        &fixture.root,
        fixture.source_format,
    );
    let index_root = temp.path().join("index");
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    let source = cold.sources[0].observation().source();
    let event = VerifiedIndex::open(&index_root)
        .unwrap()
        .source_event_page(source, None, 10)
        .unwrap()
        .items
        .into_iter()
        .find(|event| event.event_sequence == 1)
        .unwrap();
    let request = EventHydrationRequest::new(event.event_id, event.locator).unwrap();
    let rewrite_path = fixture.transcript.clone();
    set_after_jsonl_hydration_observation_hook(move || {
        let mut bytes = fs::read(&rewrite_path).unwrap();
        let identity_offset = bytes
            .windows(b"qwen-life".len())
            .position(|window| window == b"qwen-life")
            .unwrap();
        bytes[identity_offset..identity_offset + b"qwen-life".len()].copy_from_slice(b"qwen-evil");
        bytes.extend_from_slice(
            b"{\"uuid\":\"late\",\"sessionId\":\"qwen-evil\",\"type\":\"assistant\"}\n",
        );
        fs::write(&rewrite_path, bytes).unwrap();
    });

    let error = registry
        .resolver_registry()
        .hydrate_event(&request)
        .unwrap_err();
    assert_eq!(error.kind, HydrationFailureKind::StaleRecordEvidence);
}

#[cfg(target_os = "linux")]
#[test]
fn direct_jsonl_inventory_and_hydration_retain_no_leaf_file_descriptors() {
    fn provider_fds(root: &Path) -> usize {
        fs::read_dir("/proc/self/fd")
            .unwrap()
            .filter_map(std::result::Result::ok)
            .filter_map(|entry| fs::read_link(entry.path()).ok())
            .filter(|target| target.starts_with(root))
            .count()
    }

    const SOURCE_COUNT: usize = 128;
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("qwen");
    let chats = root.join("workspace/chats");
    fs::create_dir_all(&chats).unwrap();
    for ordinal in 0..SOURCE_COUNT {
        fs::write(
            chats.join(format!("session-{ordinal:03}.jsonl")),
            QWEN_LIFECYCLE_TRANSCRIPT.replace("qwen-life", &format!("qwen-fd-{ordinal:03}")),
        )
        .unwrap();
    }
    let registry = direct_registry(
        CaptureProvider::QwenCode,
        &root,
        "qwen_code_chat_jsonl_tree",
    );
    let index_root = temp.path().join("index");
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(cold.sources.len(), SOURCE_COUNT);
    assert!(
        provider_fds(&root) <= 2,
        "inventory retained per-leaf provider descriptors"
    );

    let source = cold.sources[0].observation().source();
    let event = VerifiedIndex::open(&index_root)
        .unwrap()
        .source_event_page(source, None, 1)
        .unwrap()
        .items
        .pop()
        .unwrap();
    registry
        .resolver_registry()
        .hydrate_event(&EventHydrationRequest::new(event.event_id, event.locator).unwrap())
        .unwrap();
    assert!(
        provider_fds(&root) <= 2,
        "hydration retained per-leaf provider descriptors"
    );
}
