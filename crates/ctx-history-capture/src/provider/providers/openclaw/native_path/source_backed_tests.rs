use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use ctx_history_core::{
    BatchHydrationRequest, CaptureProvider, ContentSourceResolver, EventHydrationRequest,
    LocatorRevisionPolicy,
};
use ctx_history_index::{VerifiedIndex, WriterOptions};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{
    provider::source_backed::{
        family::jsonl::{jsonl_family_work, reset_jsonl_family_work, JsonlFamilyWork},
        refresh_source_backed_generation, register_landed_source_backed_route,
        SourceBackedProviderRegistry, SourceBackedRouteSelection,
    },
    ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceStatus,
};

fn registry(root: &Path) -> SourceBackedProviderRegistry {
    let source = ProviderSource {
        provider: CaptureProvider::OpenClaw,
        path: root.to_path_buf(),
        exists: true,
        source_format: "openclaw_session_jsonl_tree",
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

fn writer_options() -> WriterOptions {
    WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    }
}

#[test]
fn shared_family_openclaw_noop_replacement_binding_and_hydration_oracle() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("openclaw");
    let transcript = transcript_path(&root);
    write_fixture(
        &transcript,
        &[
            header("session-1"),
            message("message-1", "user", "openclaw exact"),
            message("message-2", "assistant", "openclaw response"),
        ],
    );
    let registry = registry(&root);
    let index_root = temp.path().join("index");

    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(cold.sources.len(), 1);
    assert_eq!(cold.sources[0].counts().complete_records, 3);
    assert_eq!(cold.sources[0].counts().indexed_documents, 2);

    reset_jsonl_family_work();
    let unchanged =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(jsonl_family_work().provider_projections, 0);
    assert_eq!(unchanged.sources, cold.sources);
    assert_eq!(unchanged.commit.generation_id, cold.commit.generation_id);
    assert_eq!(unchanged.commit.opstamp, cold.commit.opstamp);

    let source = cold.sources[0].observation().source();
    let index = VerifiedIndex::open(&index_root).unwrap();
    let mut events = index.source_event_page(source, None, 10).unwrap().items;
    events.sort_by_key(|event| event.event_sequence);
    assert_eq!(
        events
            .iter()
            .map(|event| event.role.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("user"), Some("assistant")]
    );
    assert!(events.iter().all(|event| {
        event.locator.revision_policy() == LocatorRevisionPolicy::ExactSourceRevision
            && event.locator.certified_source_revision_digest().is_some()
    }));
    let requests = events
        .iter()
        .rev()
        .map(|event| EventHydrationRequest::new(event.event_id, event.locator.clone()).unwrap())
        .collect::<Vec<_>>();
    reset_jsonl_family_work();
    let hydrated = registry
        .resolver_registry()
        .hydrate_batch(&BatchHydrationRequest::new(requests.clone()).unwrap())
        .unwrap()
        .into_records();
    assert_eq!(
        hydrated
            .iter()
            .map(|record| record.provider_bytes.as_slice())
            .collect::<Vec<_>>(),
        vec![
            b"openclaw response".as_slice(),
            b"openclaw exact".as_slice()
        ]
    );
    assert_eq!(
        jsonl_family_work(),
        JsonlFamilyWork {
            discoveries: 0,
            leaf_opens: 1,
            provider_projections: 0,
        }
    );
    let mut digest = Sha256::new();
    for (request, record) in requests.iter().zip(hydrated) {
        digest.update(request.event_id().digest());
        digest.update((record.provider_bytes.len() as u64).to_be_bytes());
        digest.update(record.provider_bytes);
    }
    assert_eq!(
        format!("{:x}", digest.finalize()),
        "83768212e691fd4ae31e6013a10d04d6f4ca26bac352920f88aaa5415e5604a4"
    );

    let before = fs::read_to_string(&transcript).unwrap();
    let rewritten = before.replace("openclaw exact", "openclaw other");
    assert_eq!(rewritten.len(), before.len());
    fs::write(&transcript, rewritten).unwrap();
    reset_jsonl_family_work();
    let rewrite =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(
        jsonl_family_work().provider_projections,
        3,
        "same-length replacement must project each physical record once"
    );
    assert_ne!(rewrite.commit.generation_id, cold.commit.generation_id);

    append_record(
        &transcript,
        &message("message-3", "assistant", "growth replacement"),
    );
    reset_jsonl_family_work();
    let growth =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(
        jsonl_family_work().provider_projections,
        4,
        "growth remains one complete replacement scan"
    );
    assert_eq!(growth.sources[0].counts().indexed_documents, 3);

    let sessions = transcript.parent().unwrap().join("sessions.json");
    let index_before = fs::read_to_string(&sessions).unwrap();
    fs::write(
        &sessions,
        index_before.replace("feature/openclaw", "feature/changed!"),
    )
    .unwrap();
    reset_jsonl_family_work();
    let binding_change =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(
        jsonl_family_work().provider_projections,
        4,
        "auxiliary binding changes must replace instead of certifying a no-op"
    );
    assert_ne!(
        binding_change.commit.generation_id,
        growth.commit.generation_id
    );
}

#[test]
fn shared_family_openclaw_complete_deletion_and_missing_root_are_distinct() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("openclaw");
    let transcript = transcript_path(&root);
    write_fixture(
        &transcript,
        &[header("session-1"), message("message-1", "user", "hello")],
    );
    let registry = registry(&root);
    let index_root = temp.path().join("index");
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(cold.sources.len(), 1);

    fs::remove_file(&transcript).unwrap();
    let deleted =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(deleted.sources.is_empty());
    assert_ne!(deleted.commit.generation_id, cold.commit.generation_id);

    fs::remove_dir_all(&root).unwrap();
    assert!(
        refresh_source_backed_generation(&index_root, &registry, writer_options()).is_err(),
        "a missing authority root is unavailable, not another deletion certificate"
    );
    let index = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(index.generation_id(), deleted.commit.generation_id);
}

fn transcript_path(root: &Path) -> PathBuf {
    root.join("agents/personal-agent/sessions/session-1.jsonl")
}

fn header(id: &str) -> Value {
    json!({
        "type": "session",
        "id": id,
        "timestamp": "2026-07-28T12:00:00Z",
        "cwd": "/workspace/openclaw",
    })
}

fn message(id: &str, role: &str, content: &str) -> Value {
    json!({
        "type": "message",
        "id": id,
        "timestamp": "2026-07-28T12:00:01Z",
        "message": {
            "role": role,
            "content": content,
        }
    })
}

fn append_record(path: &Path, record: &Value) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    serde_json::to_writer(&mut file, record).unwrap();
    file.write_all(b"\n").unwrap();
}

fn write_fixture(path: &Path, records: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = Vec::new();
    for record in records {
        serde_json::to_writer(&mut bytes, record).unwrap();
        bytes.push(b'\n');
    }
    fs::write(path, bytes).unwrap();
    fs::write(
        path.parent().unwrap().join("sessions.json"),
        json!({
            "session-1": {
                "sessionId": "session-1",
                "label": "source-backed fixture",
                "parentSessionId": "parent-1",
                "rootSessionId": "root-1",
                "branch": "feature/openclaw",
            }
        })
        .to_string(),
    )
    .unwrap();
}
