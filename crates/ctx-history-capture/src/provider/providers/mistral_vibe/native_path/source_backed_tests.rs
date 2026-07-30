use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

use ctx_history_core::{
    BatchHydrationRequest, CaptureProvider, ContentSourceResolver, EventHydrationRequest,
};
use ctx_history_index::{VerifiedIndex, WriterOptions};
use serde_json::json;
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

fn fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session = root.join("session-alpha");
    fs::create_dir_all(&session).unwrap();
    fs::write(
        session.join("meta.json"),
        serde_json::to_vec(&json!({
            "session_id": "session-alpha",
            "start_time": "2026-07-28T12:00:00Z",
            "git_branch": "main",
            "environment": {"working_directory": "/workspace/project"}
        }))
        .unwrap(),
    )
    .unwrap();
    let messages = session.join("messages.jsonl");
    fs::write(
        &messages,
        [
            json!({
                "role": "user",
                "message_id": "message-user",
                "timestamp": "2026-07-28T12:00:01Z",
                "content": "mistral exact"
            })
            .to_string(),
            json!({
                "role": "assistant",
                "message_id": "message-assistant",
                "timestamp": "2026-07-28T12:00:02Z",
                "content": "mistral response"
            })
            .to_string(),
        ]
        .join("\n")
            + "\n",
    )
    .unwrap();
    (temp, root, messages)
}

fn registry(root: &Path) -> SourceBackedProviderRegistry {
    let source = ProviderSource {
        provider: CaptureProvider::MistralVibe,
        path: root.to_path_buf(),
        exists: true,
        source_format: "mistral_vibe_session_jsonl_tree",
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
fn shared_family_mistral_noop_metadata_churn_and_grouped_hydration_oracle() {
    let (temp, root, messages) = fixture();
    let registry = registry(&root);
    let index_root = temp.path().join("index");
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(cold.sources.len(), 1);
    assert_eq!(cold.sources[0].counts().complete_records, 2);
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
        vec![b"mistral response".as_slice(), b"mistral exact".as_slice()]
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
        "cbefc9da9231f61f466de93d72da7ff202d1706f39ff545341dec1e82687b213"
    );

    let before = fs::read_to_string(&messages).unwrap();
    let rewritten = before.replace("mistral exact", "mistral other");
    assert_eq!(rewritten.len(), before.len());
    fs::write(&messages, rewritten).unwrap();
    reset_jsonl_family_work();
    let rewrite =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(jsonl_family_work().provider_projections, 2);
    assert_ne!(rewrite.commit.generation_id, cold.commit.generation_id);

    writeln!(
        OpenOptions::new().append(true).open(&messages).unwrap(),
        "{}",
        json!({
            "role": "user",
            "message_id": "message-third",
            "timestamp": "2026-07-28T12:00:03Z",
            "content": "growth replacement"
        })
    )
    .unwrap();
    reset_jsonl_family_work();
    let growth =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(
        jsonl_family_work().provider_projections,
        3,
        "replacement-only growth must project the complete file once"
    );
    assert_eq!(growth.sources[0].counts().indexed_documents, 3);
}
