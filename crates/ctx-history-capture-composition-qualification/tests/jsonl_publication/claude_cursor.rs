use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

use ctx_history_core::{CaptureProvider, CoreRecord};
use ctx_history_index::{VerifiedIndex, WriterOptions};
use serde_json::{json, Value};

use super::*;
use crate::{
    provider::source_backed::family::jsonl::set_after_jsonl_semantic_preflight_hook,
    refresh_source_backed_generation, register_landed_source_backed_route, ProviderCatalogSupport,
    ProviderImportSupport, ProviderSource, ProviderSourceKind, ProviderSourceStatus,
    SourceBackedProviderRegistry, SourceBackedRouteSelection, SourceBackedSourceFailureClass,
};

const CURSOR_SOURCE_FORMAT: &str = "cursor_agent_transcript_jsonl_tree";

fn writer_options() -> WriterOptions {
    WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    }
}

fn build_discovered_provider_registry(
    context: &DiscoveryContext,
    data_root: &Path,
    provider: CaptureProvider,
) -> SourceBackedAutomaticRegistryBuild {
    let probes = crate::test_provider_probes();
    let report = ctx_history_source_discovery::discover_provider_sources_for_provider_with_context(
        &probes, context, provider,
    );
    build_automatic_source_backed_registry_from_report_with_probes(
        &probes, context, data_root, report,
    )
}

fn registry(
    provider: CaptureProvider,
    source_format: &'static str,
    root: &Path,
) -> SourceBackedProviderRegistry {
    let mut registry = SourceBackedProviderRegistry::new();
    register_source(&mut registry, provider, source_format, root);
    assert_eq!(registry.routes().len(), 1);
    registry
}

fn register_source(
    registry: &mut SourceBackedProviderRegistry,
    provider: CaptureProvider,
    source_format: &'static str,
    root: &Path,
) {
    register_landed_source_backed_route(
        registry,
        ProviderSource {
            provider,
            path: root.to_path_buf(),
            exists: true,
            source_format,
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
}

fn write_transcript(path: &Path, rows: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = Vec::new();
    for row in rows {
        serde_json::to_writer(&mut bytes, row).unwrap();
        bytes.push(b'\n');
    }
    fs::write(path, bytes).unwrap();
}

fn append_transcript(path: &Path, row: &Value) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    serde_json::to_writer(&mut file, row).unwrap();
    file.write_all(b"\n").unwrap();
    file.sync_all().unwrap();
}

fn indexed_records(
    index: &Path,
    provider: CaptureProvider,
    native_session_id: &str,
) -> Vec<CoreRecord> {
    let verified = VerifiedIndex::open(index).unwrap();
    let mut records = verified
        .manifest()
        .sources
        .iter()
        .filter(|source| source.observation().source().provider() == provider.as_str())
        .flat_map(|source| {
            verified
                .core_source_event_page(source.observation().source(), None, 64)
                .unwrap()
                .items
                .into_iter()
                .map(|item| item.core_record)
        })
        .filter(|record| record.provider_session_id.as_deref() == Some(native_session_id))
        .collect::<Vec<_>>();
    records.sort_by_key(|record| record.event_sequence);
    records
}

fn certified_prefix_bytes(index: &Path, provider: CaptureProvider) -> u64 {
    let verified = VerifiedIndex::open(index).unwrap();
    verified
        .manifest()
        .sources
        .iter()
        .find(|source| source.observation().source().provider() == provider.as_str())
        .unwrap()
        .frontier()
        .expect("JSONL publication must persist a checkpoint frontier")
        .certified_prefix_bytes()
}

fn assert_literal_bodies(records: &[CoreRecord], expected: &[&str]) {
    assert_eq!(
        records
            .iter()
            .map(|record| record.content.normalized_body.as_deref().unwrap())
            .collect::<Vec<_>>(),
        expected
    );
}

#[allow(clippy::too_many_arguments)]
fn exercise_lifecycle(
    provider: CaptureProvider,
    source_format: &'static str,
    source_root: &Path,
    transcript: &Path,
    index: &Path,
    native_session_id: &str,
    first: Value,
    second: Value,
    racing: Value,
) {
    write_transcript(transcript, &[first]);
    let registry = registry(provider, source_format, source_root);
    let records = || indexed_records(index, provider, native_session_id);

    let cold = refresh_source_backed_generation(index, &registry, writer_options()).unwrap();
    assert!(cold.failed_routes.is_empty());
    assert_eq!(cold.successful_route_ids.len(), 1);
    let cold_records = records();
    assert_literal_bodies(&cold_records, &["literal first"]);
    let cold_checkpoint = certified_prefix_bytes(index, provider);
    assert_eq!(cold_checkpoint, fs::metadata(transcript).unwrap().len());

    let noop = refresh_source_backed_generation(index, &registry, writer_options()).unwrap();
    assert!(noop.failed_routes.is_empty());
    assert_eq!(noop.commit.generation_id, cold.commit.generation_id);
    assert_eq!(records(), cold_records);
    assert_eq!(certified_prefix_bytes(index, provider), cold_checkpoint);

    append_transcript(transcript, &second);
    let appended = refresh_source_backed_generation(index, &registry, writer_options()).unwrap();
    assert!(appended.failed_routes.is_empty());
    let appended_records = records();
    assert_literal_bodies(&appended_records, &["literal first", "literal second"]);
    assert_eq!(appended_records[0].event_id, cold_records[0].event_id);
    let appended_checkpoint = certified_prefix_bytes(index, provider);
    assert!(appended_checkpoint > cold_checkpoint);
    assert_eq!(appended_checkpoint, fs::metadata(transcript).unwrap().len());

    append_transcript(transcript, &racing);
    let hook_path = fs::canonicalize(transcript).unwrap();
    set_after_jsonl_semantic_preflight_hook(hook_path.clone(), move || {
        let before = fs::read_to_string(&hook_path).unwrap();
        let after = before.replace("race-before", "race-after!");
        assert_eq!(before.len(), after.len());
        assert_ne!(before, after);
        fs::write(&hook_path, after).unwrap();
    });

    let failed = refresh_source_backed_generation(index, &registry, writer_options()).unwrap();
    assert!(matches!(
        failed.failed_routes.as_slice(),
        [failure]
            if failure.class == SourceBackedSourceFailureClass::SourceChanged
                && failure.carried_forward
    ));
    assert_eq!(certified_prefix_bytes(index, provider), appended_checkpoint);
    assert_eq!(records(), appended_records);

    let recovered = refresh_source_backed_generation(index, &registry, writer_options()).unwrap();
    assert!(recovered.failed_routes.is_empty());
    let recovered_records = records();
    assert_literal_bodies(
        &recovered_records,
        &["literal first", "literal second", "race-after!"],
    );
    assert_eq!(recovered_records[0].event_id, cold_records[0].event_id);
    assert_eq!(
        certified_prefix_bytes(index, provider),
        fs::metadata(transcript).unwrap().len()
    );
}

fn cursor_message(role: &str, timestamp: &str, text: &str) -> Value {
    json!({
        "timestamp": timestamp,
        "role": role,
        "message": {
            "role": role,
            "content": [{"type": "text", "text": text}]
        }
    })
}

#[test]
fn cursor_route_publishes_cold_append_and_recovers_from_carried_checkpoint() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("cursor-data");
    let native_session_id = "neutral-cursor-session";
    let transcript = root
        .join("projects/project/agent-transcripts")
        .join(native_session_id)
        .join(format!("{native_session_id}.jsonl"));
    exercise_lifecycle(
        CaptureProvider::Cursor,
        CURSOR_SOURCE_FORMAT,
        &root,
        &transcript,
        &temp.path().join("cursor-index"),
        native_session_id,
        cursor_message("user", "2026-08-16T00:00:00Z", "literal first"),
        cursor_message("assistant", "2026-08-16T00:00:01Z", "literal second"),
        cursor_message("assistant", "2026-08-16T00:00:02Z", "race-before"),
    );
}

#[cfg(unix)]
#[test]
fn cursor_incomplete_canonical_inventory_retains_last_good_generation() {
    use std::os::unix::fs::symlink;

    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("cursor-data");
    let native_session_id = "retained-cursor-session";
    let transcript = root
        .join("projects/project/agent-transcripts")
        .join(native_session_id)
        .join(format!("{native_session_id}.jsonl"));
    let retained = cursor_message("user", "2026-08-16T00:00:00Z", "retained literal");
    write_transcript(&transcript, &[retained]);
    let index = temp.path().join("cursor-index");
    let registry = registry(CaptureProvider::Cursor, CURSOR_SOURCE_FORMAT, &root);
    let records = || indexed_records(&index, CaptureProvider::Cursor, native_session_id);
    let checkpoint = || certified_prefix_bytes(&index, CaptureProvider::Cursor);
    let cold = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert!(cold.failed_routes.is_empty());
    let cold_records = records();
    let cold_checkpoint = checkpoint();

    let linked_target = temp.path().join("linked-project-target");
    fs::create_dir_all(&linked_target).unwrap();
    symlink(&linked_target, root.join("projects/a-linked-project")).unwrap();

    let failed = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();

    assert!(matches!(
        failed.failed_routes.as_slice(),
        [failure] if failure.carried_forward
    ));
    assert_eq!(failed.commit.generation_id, cold.commit.generation_id);
    assert_eq!(records(), cold_records);
    assert_eq!(checkpoint(), cold_checkpoint);
}

fn claude_message(kind: &str, uuid: &str, session_id: &str, text: &str) -> Value {
    json!({
        "type": kind,
        "uuid": uuid,
        "sessionId": session_id,
        "message": {"role": kind, "content": text}
    })
}

#[test]
fn claude_route_publishes_cold_append_and_recovers_from_carried_checkpoint() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let projects = temp.path().join("projects");
    let native_session_id = "neutral-claude-session";
    let transcript = projects
        .join("project")
        .join(format!("{native_session_id}.jsonl"));
    exercise_lifecycle(
        CaptureProvider::Claude,
        "claude_projects_jsonl_tree",
        &projects,
        &transcript,
        &temp.path().join("claude-index"),
        native_session_id,
        claude_message("user", "literal-first", native_session_id, "literal first"),
        claude_message(
            "assistant",
            "literal-second",
            native_session_id,
            "literal second",
        ),
        claude_message(
            "assistant",
            "literal-racing",
            native_session_id,
            "race-before",
        ),
    );
}

#[test]
fn automatic_claude_root_replacement_retires_sources_absent_from_strict_subset() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let first_home = temp.path().join("first");
    let first_projects = first_home.join("projects");
    let replacement_projects = temp.path().join("replacement/projects");
    let retained_session = "retained-session";
    let retired_session = "retired-session";
    let retired_transcript = first_projects.join(format!("project/{retired_session}.jsonl"));
    write_transcript(
        &first_projects.join(format!("project/{retained_session}.jsonl")),
        &[claude_message(
            "user",
            "first-retained-event",
            retained_session,
            "first root retained content",
        )],
    );
    write_transcript(
        &retired_transcript,
        &[claude_message(
            "user",
            "first-retired-event",
            retired_session,
            "first root retired content",
        )],
    );
    write_transcript(
        &replacement_projects.join(format!("project/{retained_session}.jsonl")),
        &[claude_message(
            "user",
            "replacement-retained-event",
            retained_session,
            "replacement root retained content",
        )],
    );

    let index = temp.path().join("index");
    let initial_context = DiscoveryContext::new(
        temp.path(),
        temp.path(),
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    )
    .with_env("CLAUDE_CONFIG_DIR", &first_home);
    let data_root = temp.path().join("data");
    let initial =
        build_discovered_provider_registry(&initial_context, &data_root, CaptureProvider::Claude);
    assert!(initial.issues.is_empty(), "{:?}", initial.issues);
    refresh_source_backed_generation(&index, &initial.registry, writer_options()).unwrap();

    let replacement_home = temp.path().join("replacement");
    let replacement_context = DiscoveryContext::new(
        temp.path(),
        temp.path(),
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    )
    .with_env("CLAUDE_CONFIG_DIR", &replacement_home);
    let replacement = build_discovered_provider_registry(
        &replacement_context,
        &data_root,
        CaptureProvider::Claude,
    );
    assert!(replacement.issues.is_empty(), "{:?}", replacement.issues);
    let receipt =
        refresh_source_backed_generation(&index, &replacement.registry, writer_options()).unwrap();
    assert!(receipt.failed_routes.is_empty());
    assert_eq!(receipt.complete_inventory_route_ids.len(), 1);
    assert_eq!(receipt.removals.len(), 1);

    let verified = VerifiedIndex::open(&index).unwrap();
    assert_eq!(
        verified
            .manifest()
            .sources
            .iter()
            .filter(|source| source.observation().source().provider() == "claude")
            .count(),
        1
    );
    assert_literal_bodies(
        &indexed_records(&index, CaptureProvider::Claude, retained_session),
        &["replacement root retained content"],
    );
    assert!(indexed_records(&index, CaptureProvider::Claude, retired_session).is_empty());
    assert!(retired_transcript.exists());
    assert!(fs::read_to_string(retired_transcript)
        .unwrap()
        .contains("first root retired content"));
}

#[test]
fn claude_roots_with_the_same_relative_session_path_publish_independent_sources() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let personal = temp.path().join("personal/projects");
    let work = temp.path().join("work/projects");
    let relative = Path::new("project/shared-session.jsonl");
    write_transcript(
        &personal.join(relative),
        &[claude_message(
            "user",
            "personal-event",
            "shared-session",
            "personal pineapple marker",
        )],
    );
    write_transcript(
        &work.join(relative),
        &[claude_message(
            "user",
            "work-event",
            "shared-session",
            "work kumquat marker",
        )],
    );
    let definitions = [
        ("personal", personal.parent().unwrap()),
        ("work", work.parent().unwrap()),
    ]
    .into_iter()
    .map(
        |(id, path)| ctx_history_capture_model::ProviderRootDefinition {
            id: id.to_owned(),
            provider: CaptureProvider::Claude,
            path: path.to_path_buf(),
            group: None,
            kind: None,
        },
    )
    .collect::<Vec<_>>();
    let context = DiscoveryContext::new(
        temp.path(),
        temp.path(),
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    )
    .with_configured_provider_roots(definitions);
    let report = DiscoveryReport {
        sources: [("personal", &personal), ("work", &work)]
            .into_iter()
            .map(|(root_id, root)| ProviderSource {
                provider: CaptureProvider::Claude,
                path: root.to_path_buf(),
                exists: true,
                source_format: "claude_projects_jsonl_tree",
                source_kind: ProviderSourceKind::NativeHistory,
                import_support: ProviderImportSupport::Native,
                catalog_support: ProviderCatalogSupport::None,
                status: ProviderSourceStatus::Available,
                unsupported_reason: None,
                route_provenance:
                    ctx_history_capture_model::ProviderSourceRouteProvenance::ConfiguredRoot {
                        root_id: root_id.to_owned(),
                        root_path: root.parent().unwrap().to_path_buf(),
                        route_role: ctx_history_capture_model::ProviderRouteRole::from_static(
                            "claude-projects",
                        ),
                        automatic_route_role: None,
                    },
            })
            .collect(),
        issues: Vec::new(),
    };
    let build = build_automatic_source_backed_registry_from_report_with_probes_and_retained_roots(
        &crate::test_provider_probes(),
        &context,
        &temp.path().join("data"),
        report,
        &std::collections::BTreeMap::new(),
    );
    assert!(build.issues.is_empty(), "{:?}", build.issues);
    let registry = build.registry;

    let index = temp.path().join("index");
    let result = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert!(
        result.failed_routes.is_empty(),
        "{:?}",
        result.failed_routes
    );
    assert_eq!(result.successful_route_ids.len(), 2);

    let verified = VerifiedIndex::open(&index).unwrap();
    let claude_sources = verified
        .manifest()
        .sources
        .iter()
        .filter(|source| source.observation().source().provider() == "claude")
        .collect::<Vec<_>>();
    assert_eq!(claude_sources.len(), 2);
    let mut bodies = claude_sources
        .into_iter()
        .flat_map(|source| {
            verified
                .core_source_event_page(source.observation().source(), None, 8)
                .unwrap()
                .items
                .into_iter()
                .map(|item| item.core_record.content.normalized_body.unwrap())
        })
        .collect::<Vec<_>>();
    bodies.sort();
    assert_eq!(
        bodies,
        vec![
            "personal pineapple marker".to_owned(),
            "work kumquat marker".to_owned(),
        ]
    );
}

#[test]
fn unavailable_configured_claude_home_carries_only_itself_while_peer_refreshes() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let personal_home = temp.path().join("personal");
    let work_home = temp.path().join("work");
    let personal = personal_home.join("projects");
    let work = work_home.join("projects");
    let personal_transcript = personal.join("project/personal-session.jsonl");
    let work_transcript = work.join("project/work-session.jsonl");
    write_transcript(
        &personal_transcript,
        &[claude_message(
            "user",
            "personal-first",
            "personal-session",
            "personal initial marker",
        )],
    );
    write_transcript(
        &work_transcript,
        &[claude_message(
            "user",
            "work-first",
            "work-session",
            "work retained marker",
        )],
    );
    let definitions = vec![
        ctx_history_capture_model::ProviderRootDefinition {
            id: "personal".to_owned(),
            provider: CaptureProvider::Claude,
            path: personal_home.clone(),
            group: Some("personal".to_owned()),
            kind: None,
        },
        ctx_history_capture_model::ProviderRootDefinition {
            id: "work".to_owned(),
            provider: CaptureProvider::Claude,
            path: work_home.clone(),
            group: Some("work".to_owned()),
            kind: None,
        },
    ];
    let context = DiscoveryContext::new(
        temp.path(),
        temp.path(),
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    )
    .with_configured_provider_roots(definitions);
    let data_root = temp.path().join("data");
    let initial = build_discovered_provider_registry(&context, &data_root, CaptureProvider::Claude);
    assert!(initial.issues.is_empty(), "{:?}", initial.issues);
    let index = temp.path().join("index");
    refresh_source_backed_generation(&index, &initial.registry, writer_options()).unwrap();

    append_transcript(
        &personal_transcript,
        &claude_message(
            "assistant",
            "personal-second",
            "personal-session",
            "personal refreshed marker",
        ),
    );
    let displaced_work_home = temp.path().join("work-displaced");
    fs::rename(&work_home, &displaced_work_home).unwrap();
    let current = build_discovered_provider_registry(&context, &data_root, CaptureProvider::Claude);
    assert!(
        current.issues.iter().any(|issue| matches!(
            issue,
            SourceBackedAutomaticRegistryIssue::Unavailable {
                source,
                reason: SourceBackedAutomaticUnavailableReason::SourceStatus(
                    ProviderSourceStatus::Missing
                ),
            } if source.path == work
        )),
        "{:?}",
        current.issues
    );
    fs::rename(&displaced_work_home, &work_home).unwrap();
    let receipt =
        refresh_source_backed_generation(&index, &current.registry, writer_options()).unwrap();
    assert!(
        matches!(
            receipt.failed_routes.as_slice(),
            [failure]
                if failure.class == SourceBackedSourceFailureClass::Unavailable
                    && failure.carried_forward
        ),
        "{:?}",
        receipt.failed_routes
    );
    let personal_records = indexed_records(&index, CaptureProvider::Claude, "personal-session");
    assert_literal_bodies(
        &personal_records,
        &["personal initial marker", "personal refreshed marker"],
    );
    let work_records = indexed_records(&index, CaptureProvider::Claude, "work-session");
    assert_literal_bodies(&work_records, &["work retained marker"]);
    let published = VerifiedIndex::open(&index).unwrap();
    assert_eq!(published.manifest().provider_roots().len(), 2);
    assert!(published
        .manifest()
        .provider_roots()
        .iter()
        .all(|root| root.routes().len() == 1));
}

#[test]
fn cold_unavailable_configured_claude_home_does_not_block_healthy_peer() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let personal_home = temp.path().join("personal");
    let work_home = temp.path().join("work");
    let personal = personal_home.join("projects");
    write_transcript(
        &personal.join("project/personal-session.jsonl"),
        &[claude_message(
            "user",
            "personal-first",
            "personal-session",
            "personal cold marker",
        )],
    );
    let definitions = vec![
        ctx_history_capture_model::ProviderRootDefinition {
            id: "personal".to_owned(),
            provider: CaptureProvider::Claude,
            path: personal_home.clone(),
            group: Some("personal".to_owned()),
            kind: None,
        },
        ctx_history_capture_model::ProviderRootDefinition {
            id: "work".to_owned(),
            provider: CaptureProvider::Claude,
            path: work_home.clone(),
            group: Some("work".to_owned()),
            kind: None,
        },
    ];
    let context = DiscoveryContext::new(
        temp.path(),
        temp.path(),
        DiscoveryPlatform::Linux,
        crate::DiscoveryPlatformDirs::default(),
    )
    .with_configured_provider_roots(definitions);
    let build = build_discovered_provider_registry(
        &context,
        &temp.path().join("data"),
        CaptureProvider::Claude,
    );
    assert!(
        build.issues.iter().any(|issue| matches!(
            issue,
            SourceBackedAutomaticRegistryIssue::Unavailable {
                source,
                reason: SourceBackedAutomaticUnavailableReason::SourceStatus(
                    ProviderSourceStatus::Missing
                ),
            } if source.path == work_home.join("projects")
        )),
        "{:?}",
        build.issues
    );
    let index = temp.path().join("index");
    let receipt =
        refresh_source_backed_generation(&index, &build.registry, writer_options()).unwrap();
    assert!(
        matches!(
            receipt.failed_routes.as_slice(),
            [failure]
                if failure.class == SourceBackedSourceFailureClass::Unavailable
                    && !failure.carried_forward
        ),
        "{:?}",
        receipt.failed_routes
    );
    assert_eq!(
        receipt.successful_route_ids.len(),
        2,
        "the inferred missing default and healthy named home remain independent"
    );
    assert_literal_bodies(
        &indexed_records(&index, CaptureProvider::Claude, "personal-session"),
        &["personal cold marker"],
    );
    let published = VerifiedIndex::open(&index).unwrap();
    let personal_root = published
        .manifest()
        .provider_roots()
        .iter()
        .find(|root| root.definition().id == "personal")
        .unwrap();
    let work_root = published
        .manifest()
        .provider_roots()
        .iter()
        .find(|root| root.definition().id == "work")
        .unwrap();
    assert_eq!(personal_root.routes().len(), 1);
    assert!(work_root.routes().is_empty());
}

#[test]
fn claude_duplicate_identity_retains_warm_source_atomically_while_sibling_advances_and_repairs() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let projects = temp.path().join("projects");
    let index = temp.path().join("index");
    let retained_session = "retained-claude-session";
    let sibling_session = "advancing-claude-session";
    let retained_transcript = projects
        .join("project")
        .join(format!("{retained_session}.jsonl"));
    let sibling_transcript = projects
        .join("project")
        .join(format!("{sibling_session}.jsonl"));
    write_transcript(
        &retained_transcript,
        &[claude_message(
            "user",
            "retained-event",
            retained_session,
            "retained before failure",
        )],
    );
    write_transcript(
        &sibling_transcript,
        &[claude_message(
            "user",
            "sibling-first",
            sibling_session,
            "sibling first",
        )],
    );
    let registry = registry(
        CaptureProvider::Claude,
        "claude_projects_jsonl_tree",
        &projects,
    );

    let initial = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert!(initial.failed_routes.is_empty());
    let initial_generation = VerifiedIndex::open(&index)
        .unwrap()
        .generation_id()
        .to_owned();
    let retained_before = indexed_records(&index, CaptureProvider::Claude, retained_session);
    assert_literal_bodies(&retained_before, &["retained before failure"]);

    append_transcript(
        &retained_transcript,
        &claude_message(
            "assistant",
            "retained-event",
            retained_session,
            "must never publish partially",
        ),
    );
    append_transcript(
        &sibling_transcript,
        &claude_message(
            "assistant",
            "sibling-second",
            sibling_session,
            "sibling second",
        ),
    );

    let quarantined =
        refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert!(quarantined.failed_routes.is_empty());
    assert_eq!(quarantined.logical_source_failures.total(), 1);
    let [failure] = quarantined.logical_source_failures.failures() else {
        panic!("one Claude source failure expected");
    };
    assert!(failure.carried_forward);
    assert_eq!(failure.source.provider(), CaptureProvider::Claude.as_str());
    assert!(failure.detail.contains("repeats a stable event identity"));
    assert_ne!(
        VerifiedIndex::open(&index).unwrap().generation_id(),
        initial_generation
    );
    assert_eq!(
        indexed_records(&index, CaptureProvider::Claude, retained_session),
        retained_before
    );
    assert_literal_bodies(
        &indexed_records(&index, CaptureProvider::Claude, sibling_session),
        &["sibling first", "sibling second"],
    );

    write_transcript(
        &retained_transcript,
        &[claude_message(
            "assistant",
            "repaired-event",
            retained_session,
            "repaired replacement",
        )],
    );
    let repaired = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert!(repaired.failed_routes.is_empty());
    assert!(repaired.logical_source_failures.is_empty());
    assert_literal_bodies(
        &indexed_records(&index, CaptureProvider::Claude, retained_session),
        &["repaired replacement"],
    );
}

#[test]
fn cold_bad_claude_leaf_does_not_block_sibling_or_cursor_provider() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let projects = temp.path().join("projects");
    let cursor_root = temp.path().join("cursor-data");
    let index = temp.path().join("index");
    let bad_session = "cold-bad-claude-session";
    let sibling_session = "cold-good-claude-session";
    let cursor_session = "cold-good-cursor-session";
    write_transcript(
        &projects
            .join("bad-project")
            .join(format!("{bad_session}.jsonl")),
        &[
            claude_message("user", "duplicate", bad_session, "bad first"),
            claude_message("assistant", "duplicate", bad_session, "bad second"),
        ],
    );
    write_transcript(
        &projects
            .join("good-project")
            .join(format!("{sibling_session}.jsonl")),
        &[claude_message(
            "user",
            "good-claude",
            sibling_session,
            "good Claude sibling",
        )],
    );
    write_transcript(
        &cursor_root
            .join("projects/project/agent-transcripts")
            .join(cursor_session)
            .join(format!("{cursor_session}.jsonl")),
        &[cursor_message(
            "user",
            "2026-08-23T00:00:00Z",
            "good Cursor provider",
        )],
    );
    let mut registry = SourceBackedProviderRegistry::new();
    register_source(
        &mut registry,
        CaptureProvider::Claude,
        "claude_projects_jsonl_tree",
        &projects,
    );
    register_source(
        &mut registry,
        CaptureProvider::Cursor,
        "cursor_agent_transcript_jsonl_tree",
        &cursor_root,
    );

    let receipt = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert!(receipt.failed_routes.is_empty());
    assert_eq!(receipt.successful_route_ids.len(), 2);
    assert_eq!(receipt.logical_source_failures.total(), 1);
    assert!(!receipt.logical_source_failures.failures()[0].carried_forward);
    assert!(indexed_records(&index, CaptureProvider::Claude, bad_session).is_empty());
    assert_literal_bodies(
        &indexed_records(&index, CaptureProvider::Claude, sibling_session),
        &["good Claude sibling"],
    );
    assert_literal_bodies(
        &indexed_records(&index, CaptureProvider::Cursor, cursor_session),
        &["good Cursor provider"],
    );
}
