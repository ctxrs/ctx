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
        family::jsonl::{
            jsonl_family_projection_bytes, jsonl_family_work, jsonl_prefix_hash_bytes,
            reset_jsonl_family_work, reset_jsonl_prefix_hash_bytes, JsonlFamilyWork,
        },
        refresh_source_backed_generation, register_landed_source_backed_route,
        SourceBackedProviderRegistry, SourceBackedRouteSelection,
    },
    ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceStatus,
};

fn registry(root: &Path) -> SourceBackedProviderRegistry {
    registry_for_roots(&[(root, SourceBackedRouteSelection::Automatic)])
}

fn registry_for_roots(
    roots: &[(&Path, SourceBackedRouteSelection)],
) -> SourceBackedProviderRegistry {
    let mut registry = SourceBackedProviderRegistry::new();
    for (root, selection) in roots {
        let source = ProviderSource {
            provider: CaptureProvider::Claude,
            path: (*root).to_path_buf(),
            exists: true,
            source_format: "claude_projects_jsonl_tree",
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Native,
            catalog_support: ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Available,
            unsupported_reason: None,
        };
        register_landed_source_backed_route(&mut registry, source, *selection).unwrap();
    }
    registry
}

#[test]
fn shared_family_empty_automatic_root_coexists_with_distinct_explicit_replay() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let automatic = temp.path().join(".claude/projects");
    let explicit = temp.path().join("explicit/projects");
    fs::create_dir_all(&automatic).unwrap();
    write_lines(
        &session_path(&explicit, "-project", "session-1"),
        &[message("session-1", "message-1", "explicit body")],
    );
    let registry = registry_for_roots(&[
        (&automatic, SourceBackedRouteSelection::Automatic),
        (&explicit, SourceBackedRouteSelection::ExplicitManual),
    ]);
    let index_root = temp.path().join("index");

    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(cold.sources.len(), 1);
    let replay =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(replay.sources, cold.sources);
    assert_eq!(replay.commit.generation_id, cold.commit.generation_id);
    let source = replay.sources[0].observation().source();
    let event = VerifiedIndex::open(&index_root)
        .unwrap()
        .source_event_page(source, None, 1)
        .unwrap()
        .items
        .remove(0);
    let hydrated = registry
        .resolver_registry()
        .hydrate_event(&EventHydrationRequest::new(event.event_id, event.locator.clone()).unwrap())
        .unwrap();
    assert_eq!(hydrated.provider_bytes, b"explicit body");
}

fn writer_options() -> WriterOptions {
    WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    }
}

#[test]
fn shared_family_claude_noop_replacement_lineage_and_hydration_oracle() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let projects = temp.path().join(".claude/projects");
    let primary = session_path(&projects, "-project", "session-1");
    let subagent = projects.join("-project/session-1/subagents/agent-review.jsonl");
    write_lines(
        &primary,
        &[
            message("session-1", "message-1", "claude exact"),
            message("session-1", "message-2", "claude response"),
        ],
    );
    write_lines(
        &subagent,
        &[message("session-1", "subagent-message", "subagent body")],
    );
    let registry = registry(&projects);
    let index_root = temp.path().join("index");

    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(cold.sources.len(), 2);
    assert_eq!(
        cold.sources
            .iter()
            .map(|source| source.counts().indexed_documents)
            .sum::<u64>(),
        3
    );

    reset_jsonl_family_work();
    let unchanged =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(jsonl_family_work().provider_projections, 0);
    assert_eq!(unchanged.sources, cold.sources);
    assert_eq!(unchanged.commit.generation_id, cold.commit.generation_id);
    assert_eq!(unchanged.commit.opstamp, cold.commit.opstamp);

    let primary_source = cold
        .sources
        .iter()
        .find(|source| source.counts().indexed_documents == 2)
        .unwrap()
        .observation()
        .source();
    let index = VerifiedIndex::open(&index_root).unwrap();
    let mut events = index
        .source_event_page(primary_source, None, 10)
        .unwrap()
        .items;
    events.sort_by_key(|event| event.event_sequence);
    assert!(events.iter().all(|event| {
        event.parent_session_id.is_none()
            && event.root_session_id == event.session_id
            && event.agent_type == "primary"
            && event.is_primary
            && event.branch.as_deref() == Some("main")
            && event.cwd.as_deref() == Some("/workspace/project")
            && event.locator.revision_policy() == LocatorRevisionPolicy::StableRecordEvidence
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
        vec![b"claude response".as_slice(), b"claude exact".as_slice()]
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
        "c454f0ccd49ec9598691b60c969a1c6f77b7aa685de2a40289e5dac8ab32394a"
    );

    let before = fs::read_to_string(&primary).unwrap();
    let rewritten = before.replace("claude exact", "claude other");
    assert_eq!(rewritten.len(), before.len());
    fs::write(&primary, rewritten).unwrap();
    reset_jsonl_family_work();
    let rewrite =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(
        jsonl_family_work().provider_projections,
        2,
        "same-length Claude rewrite is one replacement pass"
    );
    assert_ne!(rewrite.commit.generation_id, cold.commit.generation_id);

    let rewrite_primary = rewrite
        .sources
        .iter()
        .find(|source| source.counts().indexed_documents == 2)
        .unwrap();
    let rewrite_frontier = rewrite_primary.frontier().unwrap();
    let frozen_prefix_digest = *rewrite_frontier.certified_prefix_digest();
    let mut appended = message("session-1", "message-3", "claude growth");
    let appended_object = appended.as_object_mut().unwrap();
    appended_object.remove("cwd");
    appended_object.remove("version");
    appended_object.remove("gitBranch");
    append_record(&primary, &appended);
    let replacement_payload_bytes = fs::read(&primary).unwrap().len() - 3;
    reset_jsonl_family_work();
    reset_jsonl_prefix_hash_bytes();
    let growth =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(
        jsonl_family_work().provider_projections,
        3,
        "Claude growth must replay prior session metadata"
    );
    assert_eq!(
        jsonl_family_projection_bytes(),
        replacement_payload_bytes,
        "replacement-only Claude reparses every payload byte"
    );
    assert_eq!(
        jsonl_prefix_hash_bytes(),
        0,
        "replacement-only Claude does not attempt append certification"
    );
    let growth_primary = growth
        .sources
        .iter()
        .find(|source| source.counts().indexed_documents == 3)
        .unwrap();
    assert_eq!(growth_primary.counts().complete_records, 3);
    assert_eq!(growth_primary.counts().indexed_documents, 3);
    assert_eq!(
        growth_primary.frontier().unwrap().certified_prefix_bytes(),
        fs::metadata(&primary).unwrap().len()
    );
    assert_ne!(
        growth_primary.frontier().unwrap().certified_prefix_digest(),
        &frozen_prefix_digest
    );
    assert_eq!(
        growth
            .sources
            .iter()
            .map(|source| source.counts().indexed_documents)
            .sum::<u64>(),
        4
    );
    let mut primary_events = VerifiedIndex::open(&index_root)
        .unwrap()
        .source_event_page(growth_primary.observation().source(), None, 10)
        .unwrap()
        .items;
    primary_events.sort_by_key(|event| event.event_sequence);
    let appended_event = primary_events.last().unwrap();
    assert_eq!(appended_event.branch.as_deref(), Some("main"));
    assert_eq!(appended_event.cwd.as_deref(), Some("/workspace/project"));

    let subagent_event = VerifiedIndex::open(&index_root)
        .unwrap()
        .source_event_page(
            growth
                .sources
                .iter()
                .find(|source| source.counts().indexed_documents == 1)
                .unwrap()
                .observation()
                .source(),
            None,
            4,
        )
        .unwrap()
        .items
        .remove(0);
    assert_eq!(subagent_event.agent_type, "subagent");
    assert!(!subagent_event.is_primary);
    assert!(subagent_event.parent_session_id.is_some());
    assert_eq!(
        subagent_event.root_session_id,
        subagent_event.parent_session_id.unwrap()
    );
}

#[cfg(unix)]
#[test]
fn shared_family_claude_accepts_hardlinked_leaves_without_resident_file_handles() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let projects = temp.path().join(".claude/projects");
    let first = session_path(&projects, "-project", "session-1");
    write_lines(
        &first,
        &[message("session-1", "message-1", "hardlink body")],
    );
    fs::hard_link(&first, session_path(&projects, "-project", "session-2")).unwrap();
    let result = refresh_source_backed_generation(
        temp.path().join("index"),
        &registry(&projects),
        writer_options(),
    )
    .unwrap();
    assert_eq!(result.sources.len(), 2);
}

#[test]
fn shared_family_claude_complete_deletion_and_missing_root_are_distinct() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let projects = temp.path().join(".claude/projects");
    let path = session_path(&projects, "-project", "session-1");
    write_lines(&path, &[message("session-1", "message-1", "delete body")]);
    let registry = registry(&projects);
    let index_root = temp.path().join("index");
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert_eq!(cold.sources.len(), 1);

    fs::remove_file(&path).unwrap();
    let deleted =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(deleted.sources.is_empty());

    fs::remove_dir_all(&projects).unwrap();
    assert!(refresh_source_backed_generation(&index_root, &registry, writer_options()).is_err());
    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().generation_id(),
        deleted.commit.generation_id
    );
}

fn session_path(projects: &Path, project: &str, session: &str) -> PathBuf {
    projects.join(project).join(format!("{session}.jsonl"))
}

fn message(session: &str, uuid: &str, text: &str) -> Value {
    json!({
        "sessionId": session,
        "type": "user",
        "uuid": uuid,
        "timestamp": "2026-01-01T00:00:00.000Z",
        "cwd": "/workspace/project",
        "version": "2.1.219",
        "gitBranch": "main",
        "message": {
            "role": "user",
            "content": [{"type": "text", "text": text}]
        }
    })
}

fn write_lines(path: &Path, lines: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = Vec::new();
    for line in lines {
        serde_json::to_writer(&mut bytes, line).unwrap();
        bytes.push(b'\n');
    }
    fs::write(path, bytes).unwrap();
}

fn append_record(path: &Path, record: &Value) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    serde_json::to_writer(&mut file, record).unwrap();
    file.write_all(b"\n").unwrap();
}
