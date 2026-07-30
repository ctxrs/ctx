use std::{fs, io::Write, path::Path};

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

fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join(".kimi-code");
    let session = root.join("sessions/work/session-1");
    let agent = session.join("agents/main");
    fs::create_dir_all(&agent).unwrap();
    fs::write(
        root.join("session_index.jsonl"),
        format!(
            "{}\n",
            json!({
                "sessionId": "session-1",
                "sessionDir": session,
                "workDir": "/workspace/kimi"
            })
        ),
    )
    .unwrap();
    fs::write(
        session.join("state.json"),
        json!({
            "createdAt": "2026-07-17T12:00:00Z",
            "title": "shared family",
            "agents": {"main": {"type": "main"}}
        })
        .to_string(),
    )
    .unwrap();
    let mut wire = fs::File::create(agent.join("wire.jsonl")).unwrap();
    for record in [
        json!({"type": "metadata", "created_at": 1_784_289_600_000_i64}),
        json!({"type": "turn.prompt", "time": 1_784_289_600_001_i64, "input": "kimi exact"}),
        json!({
            "type": "context.append_loop_event",
            "time": 1_784_289_600_002_i64,
            "event": {"type": "tool.result", "toolName": "bash", "exit_code": 7, "output": "kimi failure"}
        }),
    ] {
        writeln!(wire, "{record}").unwrap();
    }
    (temp, root)
}

fn registry(root: &Path) -> SourceBackedProviderRegistry {
    let source = ProviderSource {
        provider: CaptureProvider::KimiCodeCli,
        path: root.to_path_buf(),
        exists: true,
        source_format: "kimi_code_cli_wire_jsonl_tree",
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
fn shared_family_kimi_noop_projection_and_grouped_hydration_oracle() {
    let (temp, root) = fixture();
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
        vec![b"kimi failure".as_slice(), b"kimi exact".as_slice()]
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
        "5c30f1ed49d4941877f12f170dcb44e8810c72c301ba610345a016fc8f1ce0de"
    );
}
