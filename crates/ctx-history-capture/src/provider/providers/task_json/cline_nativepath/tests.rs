use std::{
    fs,
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
};

use ctx_history_core::CaptureProvider;
use serde_json::{json, Value};
use tempfile::TempDir;

use super::*;

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    api: PathBuf,
    ui: PathBuf,
    metadata: PathBuf,
}

impl Fixture {
    fn new(api: Value, ui: Value) -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("cline-data");
        let task = root.join("tasks").join("task-1");
        fs::create_dir_all(&task).expect("task directory");
        fs::create_dir_all(root.join("state")).expect("state directory");
        let api_path = task.join("api_conversation_history.json");
        let ui_path = task.join("ui_messages.json");
        let metadata = task.join("task_metadata.json");
        let root_index = root.join("state").join("taskHistory.json");
        write_json(&api_path, &api);
        write_json(&ui_path, &ui);
        write_json(
            &metadata,
            &json!({
                "taskId": "task-1",
                "createdAt": "2026-07-25T00:00:00Z",
                "modelId": "test-model"
            }),
        );
        write_json(
            &root_index,
            &json!([{
                "id": "task-1",
                "task": "Reader proof",
                "workspaceDirectory": "/tmp/repo"
            }]),
        );
        Self {
            _temp: temp,
            root,
            api: api_path,
            ui: ui_path,
            metadata,
        }
    }
}

struct ReadResult {
    pages: Vec<ClineCertifiedPage>,
    outcomes: Vec<ClineComponentReadOutcome>,
    stats: ClinePublicationStats,
    catalog: ClineCatalogCompletion,
}

fn read_all(
    root: &Path,
    previous: &[ClineTaskCheckpoint],
    profile: ClineNativeProfile,
) -> ReadResult {
    let discovery = discover_cline_root(root).expect("discover Cline root");
    let mut reader = ClineNativeReader::new(discovery, previous, profile);
    let mut pages = Vec::new();
    while let Some(page) = reader.next_page().expect("read certified page") {
        pages.push(page);
    }
    let catalog = reader.finish_catalog().expect("finish Cline catalog");
    ReadResult {
        pages,
        outcomes: reader.outcomes().to_vec(),
        stats: reader.stats(),
        catalog,
    }
}

fn read_all_roo(root: &Path, profile: ClineNativeProfile) -> ReadResult {
    let discovery = discover_roo_root(root).expect("discover Roo root");
    let mut reader = ClineNativeReader::new(discovery, &[], profile);
    let mut pages = Vec::new();
    while let Some(page) = reader.next_page().expect("read Roo certified page") {
        pages.push(page);
    }
    let catalog = reader.finish_catalog().expect("finish Roo catalog");
    ReadResult {
        pages,
        outcomes: reader.outcomes().to_vec(),
        stats: reader.stats(),
        catalog,
    }
}

fn write_json(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec(value).expect("serialize fixture")).expect("write fixture");
}

fn component_pages(
    pages: &[ClineCertifiedPage],
    component: ClineComponent,
) -> Vec<&ClineCertifiedPage> {
    pages
        .iter()
        .filter(|page| page.source.component == component)
        .collect()
}

fn outcome(
    outcomes: &[ClineComponentReadOutcome],
    component: ClineComponent,
) -> &ClineComponentReadOutcome {
    outcomes
        .iter()
        .find(|outcome| outcome.component == component)
        .expect("component outcome")
}

fn api_messages(values: &[&str]) -> Value {
    Value::Array(
        values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                json!({
                    "id": format!("api-{index}"),
                    "role": if index % 2 == 0 { "user" } else { "assistant" },
                    "content": value
                })
            })
            .collect(),
    )
}

#[test]
fn roo_dialect_discovers_supplemental_metadata_and_fallback_history() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("roo-data");
    let task = root.join("tasks").join("roo-task");
    fs::create_dir_all(&task).expect("task directory");
    write_json(
        &task.join("history_item.json"),
        &json!({
            "id": "roo-authoritative-id",
            "task": "Roo fallback task",
            "ts": "2026-07-25T00:00:00Z",
            "tokensIn": 9,
            "tokensOut": 4
        }),
    );
    write_json(
        &task.join("_index.json"),
        &json!({
            "id": "roo-authoritative-id",
            "model": "roo-model"
        }),
    );
    write_json(
        &task.join("claude_messages.json"),
        &json!([
            {"role": "user", "content": "fallback request"},
            {"role": "assistant", "content": "fallback response"}
        ]),
    );

    let result = read_all_roo(&root, ClineNativeProfile::CoreAndPro);
    let metadata = component_pages(&result.pages, ClineComponent::HistoryItem);
    let fallback = component_pages(&result.pages, ClineComponent::FallbackHistory);
    assert_eq!(metadata.len(), 1);
    assert_eq!(fallback.len(), 2);
    assert!(result
        .pages
        .iter()
        .all(|page| page.source.provider == CaptureProvider::RooCode.as_str()));
    assert!(fallback.iter().all(|page| {
        page.source.task.as_str() == "roo-authoritative-id"
            && page.core.events.iter().all(|event| {
                event.identity.task.as_str() == "roo-authoritative-id"
                    && event.identity.component == ClineEventComponent::FallbackHistory
            })
    }));
    assert_eq!(result.catalog.live_checkpoints.len(), 1);
    assert!(matches!(
        result.catalog.root_index,
        ClineCatalogIndex::Missing
    ));
}

#[test]
fn each_changed_component_is_hydrated_and_parsed_once() {
    let fixture = Fixture::new(api_messages(&["hello"]), json!([]));
    let result = read_all(&fixture.root, &[], ClineNativeProfile::CoreOnly);
    // metadata + API + UI + terminal root-index catalog
    assert_eq!(result.stats.component_hydrations, 4);
    assert_eq!(result.stats.component_parse_passes, 4);
    assert_eq!(result.stats.array_item_parse_attempts, 1);
    assert_eq!(result.catalog.live_checkpoints.len(), 1);
    assert!(result
        .pages
        .iter()
        .all(|page| page.accounting.logical_units <= CLINE_NATIVE_PAGE_MAX_UNITS));
    assert!(result.pages.iter().all(|page| {
        page.accounting.conservative_core_bytes
            <= super::normalize::CLINE_NATIVE_CORE_PAGE_MAX_BYTES
            && page.accounting.conservative_serialized_bytes <= CLINE_NATIVE_PAGE_MAX_BYTES
    }));
}

#[test]
fn source_mutation_after_parse_is_rejected_before_any_file_page_is_exposed() {
    let fixture = Fixture::new(api_messages(&["before"]), json!([]));
    let discovery = discover_cline_root(&fixture.root).expect("discover");
    let mutated = Arc::new(AtomicBool::new(false));
    let mutated_flag = Arc::clone(&mutated);
    let mut reader = ClineNativeReader::new(discovery, &[], ClineNativeProfile::CoreOnly);
    reader.set_before_exposure_hook(move |path, component| {
        if component == ClineComponent::ApiHistory && !mutated_flag.swap(true, Ordering::SeqCst) {
            write_json(path, &api_messages(&["after"]));
        }
    });
    let mut pages = Vec::new();
    while let Some(page) = reader.next_page().expect("reader remains available") {
        pages.push(page);
    }
    assert!(mutated.load(Ordering::SeqCst));
    assert!(component_pages(&pages, ClineComponent::ApiHistory).is_empty());
    let api = outcome(reader.outcomes(), ClineComponent::ApiHistory);
    assert_eq!(
        api.failure.as_ref().map(|failure| failure.kind),
        Some(ClineComponentFailureKind::SourceChanged)
    );
    assert_eq!(reader.stats().component_hydrations, 3);
    assert_eq!(reader.stats().component_parse_passes, 3);
}

#[test]
fn fallback_metadata_must_remain_exact_before_every_array_page_boundary() {
    for metadata_missing in [true, false] {
        for mutate_at_boundary in [0_usize, 1] {
            let fixture = Fixture::new(api_messages(&["first", "second"]), json!([]));
            if metadata_missing {
                fs::remove_file(&fixture.metadata).expect("remove metadata fixture");
            } else {
                // The escaped control character is an invalid task identity.
                // Replacing it with "up" preserves the file length, proving
                // that presence/length-only authority is insufficient.
                write_json(&fixture.metadata, &json!({"taskId": "\n"}));
            }
            let original_len = fs::metadata(&fixture.metadata)
                .ok()
                .map(|value| value.len());
            let discovery = discover_cline_root(&fixture.root).expect("discover fallback task");
            let boundary = Arc::new(AtomicUsize::new(0));
            let boundary_hook = Arc::clone(&boundary);
            let metadata_path = fixture.metadata.clone();
            let mut reader = ClineNativeReader::new(discovery, &[], ClineNativeProfile::CoreOnly);
            reader.set_before_exposure_hook(move |_path, component| {
                if component != ClineComponent::ApiHistory {
                    return;
                }
                let current = boundary_hook.fetch_add(1, Ordering::SeqCst);
                if current == mutate_at_boundary {
                    write_json(&metadata_path, &json!({"taskId": "up"}));
                }
            });

            let mut pages = Vec::new();
            while let Some(page) = reader.next_page().expect("metadata mutation is local") {
                pages.push(page);
            }
            let api_pages = component_pages(&pages, ClineComponent::ApiHistory);
            assert_eq!(
                api_pages.len(),
                mutate_at_boundary,
                "fallback page escaped after metadata mutation: missing={metadata_missing}, boundary={mutate_at_boundary}"
            );
            assert!(api_pages
                .iter()
                .all(|page| page.source.task.as_str() == "task-1"));
            assert!(api_pages.iter().all(|page| !page.terminal));
            let api = outcome(reader.outcomes(), ClineComponent::ApiHistory);
            assert_eq!(api.pages, mutate_at_boundary);
            assert_eq!(
                api.failure.as_ref().map(|failure| failure.kind),
                Some(ClineComponentFailureKind::SourceChanged)
            );
            if !metadata_missing {
                assert_eq!(
                    original_len,
                    fs::metadata(&fixture.metadata)
                        .ok()
                        .map(|value| value.len()),
                    "invalid-to-valid test must preserve metadata length"
                );
            }
        }
    }
}

#[test]
fn core_and_pro_fanout_all_outcomes_without_changing_core_or_page_identity() {
    let fixture = Fixture::new(
        json!([]),
        json!([
            {
                "id": "success",
                "type": "say",
                "say": "command_output",
                "text": "SUCCESS_ONLY",
                "exitCode": 0
            },
            {
                "id": "failure",
                "text": "FAILURE_DIAGNOSTIC",
                "say": "command_output",
                "type": "say",
                "exitCode": 7
            },
            {
                "id": "timeout",
                "type": "say",
                "say": "command_output",
                "text": "TIMEOUT_DIAGNOSTIC",
                "timedOut": true
            },
            {
                "id": "unknown",
                "type": "say",
                "say": "command_output",
                "text": ""
            }
        ]),
    );
    let core = read_all(&fixture.root, &[], ClineNativeProfile::CoreOnly);
    let fanout = read_all(&fixture.root, &[], ClineNativeProfile::CoreAndPro);
    let core_pages = component_pages(&core.pages, ClineComponent::UiMessages);
    let fanout_pages = component_pages(&fanout.pages, ClineComponent::UiMessages);
    assert_eq!(
        core.catalog.live_checkpoints,
        fanout.catalog.live_checkpoints
    );
    assert_eq!(core_pages.len(), 4);
    assert_eq!(fanout_pages.len(), 4);
    assert_eq!(
        core_pages
            .iter()
            .map(|page| page.identity)
            .collect::<Vec<_>>(),
        fanout_pages
            .iter()
            .map(|page| page.identity)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        core_pages
            .iter()
            .map(|page| (&page.expected_frontier, &page.next_safe_frontier))
            .collect::<Vec<_>>(),
        fanout_pages
            .iter()
            .map(|page| (&page.expected_frontier, &page.next_safe_frontier))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        core_pages
            .iter()
            .map(|page| page.accounting.conservative_core_bytes)
            .collect::<Vec<_>>(),
        fanout_pages
            .iter()
            .map(|page| page.accounting.conservative_core_bytes)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        core_pages
            .iter()
            .flat_map(|page| page.core.events.iter())
            .cloned()
            .collect::<Vec<_>>(),
        fanout_pages
            .iter()
            .flat_map(|page| page.core.events.iter())
            .cloned()
            .collect::<Vec<_>>()
    );
    let core_events = core_pages
        .iter()
        .flat_map(|page| page.core.events.iter())
        .collect::<Vec<_>>();
    assert_eq!(core_events.len(), 2);
    assert!(core_events.iter().all(|event| {
        event.body.is_none()
            && event.preview.is_none()
            && event.sparse_output.as_ref().is_some_and(|output| {
                matches!(
                    output.outcome,
                    crate::OutputOutcome::Failure | crate::OutputOutcome::Timeout
                )
            })
    }));
    assert!(core_pages.iter().all(|page| page.transient.is_none()));
    assert_eq!(core.stats.output_bodies_hydrated, 0);
    assert_eq!(core.stats.success_unknown_core_rows, 0);
    assert_eq!(core.stats.success_unknown_hashes, 0);
    assert_eq!(core.stats.success_unknown_previews, 0);
    assert_eq!(core.stats.success_unknown_touches, 0);
    assert_eq!(core.stats.success_unknown_blobs, 0);
    assert_eq!(core.stats.success_unknown_fts_documents, 0);

    let outputs = fanout_pages
        .iter()
        .flat_map(|page| {
            page.transient
                .as_ref()
                .expect("Core+Pro transient lane")
                .observations
                .iter()
        })
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), 4);
    assert_eq!(
        outputs
            .iter()
            .map(|output| output.outcome.outcome)
            .collect::<Vec<_>>(),
        vec![
            crate::OutputOutcome::Success,
            crate::OutputOutcome::Failure,
            crate::OutputOutcome::Timeout,
            crate::OutputOutcome::Unknown,
        ]
    );
    assert_eq!(outputs[0].content, b"SUCCESS_ONLY");
    assert_eq!(outputs[1].content, b"FAILURE_DIAGNOSTIC");
    assert_eq!(outputs[2].content, b"TIMEOUT_DIAGNOSTIC");
    assert!(outputs[3].content.is_empty());
    assert_eq!(fanout.stats.output_bodies_hydrated, 4);
}

#[test]
fn transient_output_pressure_is_local_and_activation_invariant() {
    let oversized = "x".repeat(4 * 1024 * 1024 + 32);
    let fixture = Fixture::new(
        json!([]),
        json!([{
            "id": "large-success",
            "type": "say",
            "say": "command_output",
            "text": oversized,
            "exitCode": 0
        }]),
    );
    let core = read_all(&fixture.root, &[], ClineNativeProfile::CoreOnly);
    let fanout = read_all(&fixture.root, &[], ClineNativeProfile::CoreAndPro);
    let core_page = component_pages(&core.pages, ClineComponent::UiMessages)
        .pop()
        .expect("Core page");
    let fanout_page = component_pages(&fanout.pages, ClineComponent::UiMessages)
        .pop()
        .expect("fanout page");
    assert_eq!(core_page.identity, fanout_page.identity);
    assert_eq!(core_page.expected_frontier, fanout_page.expected_frontier);
    assert_eq!(core_page.next_safe_frontier, fanout_page.next_safe_frontier);
    assert!(core_page.core.events.is_empty());
    assert!(fanout_page.core.events.is_empty());
    let transient = fanout_page.transient.as_ref().expect("transient lane");
    assert!(transient.observations.is_empty());
    assert_eq!(transient.rejected_outputs.len(), 1);
    assert_eq!(
        transient.rejected_outputs[0].kind,
        ClineItemRejectionKind::OversizedTransientOutput
    );
    assert_eq!(fanout.stats.output_bodies_hydrated, 0);
}

#[test]
fn output_only_rewrite_uses_core_pages_in_both_profiles_and_fans_out_once() {
    let fixture = Fixture::new(
        json!([]),
        json!([{
            "id": "output-only",
            "type": "say",
            "say": "command_output",
            "text": "old",
            "exitCode": 0
        }]),
    );
    let baseline = read_all(&fixture.root, &[], ClineNativeProfile::CoreOnly);
    write_json(
        &fixture.ui,
        &json!([{
            "id": "output-only",
            "type": "say",
            "say": "command_output",
            "text": "new-output-longer",
            "exitCode": 0
        }]),
    );

    let core = read_all(
        &fixture.root,
        &baseline.catalog.live_checkpoints,
        ClineNativeProfile::CoreOnly,
    );
    let fanout = read_all(
        &fixture.root,
        &baseline.catalog.live_checkpoints,
        ClineNativeProfile::CoreAndPro,
    );
    let core_pages = component_pages(&core.pages, ClineComponent::UiMessages);
    let fanout_pages = component_pages(&fanout.pages, ClineComponent::UiMessages);
    assert_eq!(core_pages.len(), 1);
    assert_eq!(fanout_pages.len(), 1);
    assert_eq!(
        outcome(&core.outcomes, ClineComponent::UiMessages).transition,
        Some(ClineComponentTransition::ControlOnlyRewrite)
    );
    assert_eq!(core_pages[0].identity, fanout_pages[0].identity);
    assert_eq!(
        core_pages[0].expected_frontier,
        fanout_pages[0].expected_frontier
    );
    assert_eq!(
        core_pages[0].next_safe_frontier,
        fanout_pages[0].next_safe_frontier
    );
    assert!(core_pages[0].core.events.is_empty());
    assert!(fanout_pages[0].core.events.is_empty());
    assert!(core_pages[0].transient.is_none());
    let outputs = &fanout_pages[0]
        .transient
        .as_ref()
        .expect("Core+Pro output-only page")
        .observations;
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].content, b"new-output-longer");
    assert_eq!(core.stats.output_bodies_hydrated, 0);
    assert_eq!(fanout.stats.output_bodies_hydrated, 1);
}

#[test]
fn append_noop_rewrite_deletion_incomplete_and_corrupt_are_reader_local() {
    let fixture = Fixture::new(api_messages(&["one", "two"]), json!([]));
    let cold = read_all(&fixture.root, &[], ClineNativeProfile::CoreOnly);
    let cold_checkpoint = cold.catalog.live_checkpoints.to_vec();
    assert_eq!(
        outcome(&cold.outcomes, ClineComponent::ApiHistory).transition,
        Some(ClineComponentTransition::Cold)
    );

    let noop = read_all(
        &fixture.root,
        &cold_checkpoint,
        ClineNativeProfile::CoreOnly,
    );
    assert!(noop.pages.is_empty());
    assert_eq!(noop.stats.component_hydrations, 1);
    assert_eq!(noop.stats.component_parse_passes, 1);
    assert_eq!(
        outcome(&noop.outcomes, ClineComponent::ApiHistory).transition,
        Some(ClineComponentTransition::Unchanged)
    );

    write_json(&fixture.api, &api_messages(&["one", "two", "three"]));
    let appended = read_all(
        &fixture.root,
        &cold_checkpoint,
        ClineNativeProfile::CoreOnly,
    );
    let append_pages = component_pages(&appended.pages, ClineComponent::ApiHistory);
    assert_eq!(append_pages.len(), 3);
    assert_eq!(
        outcome(&appended.outcomes, ClineComponent::ApiHistory).transition,
        Some(ClineComponentTransition::Append { prior_items: 2 })
    );
    assert_eq!(
        append_pages[0].expected_frontier,
        ClinePageFrontier::zero(ClineEventComponent::ApiHistory)
    );
    assert!(append_pages
        .windows(2)
        .all(|pages| pages[0].next_safe_frontier == pages[1].expected_frontier));
    assert_eq!(
        append_pages
            .last()
            .expect("append terminal")
            .next_safe_frontier,
        appended.catalog.live_checkpoints[0]
            .api_history
            .as_ref()
            .expect("append checkpoint")
            .final_frontier
    );

    write_json(&fixture.api, &api_messages(&["changed", "two", "three"]));
    let rewritten = read_all(
        &fixture.root,
        &appended.catalog.live_checkpoints,
        ClineNativeProfile::CoreOnly,
    );
    assert_eq!(
        outcome(&rewritten.outcomes, ClineComponent::ApiHistory).transition,
        Some(ClineComponentTransition::Rewrite)
    );
    assert_eq!(
        component_pages(&rewritten.pages, ClineComponent::ApiHistory).len(),
        3
    );

    fs::remove_file(&fixture.api).expect("delete API component");
    let deleted = read_all(
        &fixture.root,
        &rewritten.catalog.live_checkpoints,
        ClineNativeProfile::CoreOnly,
    );
    let deletion_pages = component_pages(&deleted.pages, ClineComponent::ApiHistory);
    assert_eq!(deletion_pages.len(), 1);
    assert_eq!(
        deletion_pages[0].core.transition,
        ClineComponentTransition::MissingPhysical
    );
    assert_eq!(
        deletion_pages[0].terminal_evidence,
        Some(ClineTerminalEvidence::Deleted)
    );

    fs::write(&fixture.api, br#"[{"id":"tail","role":"user""#).expect("write incomplete JSON");
    let incomplete = read_all(
        &fixture.root,
        &rewritten.catalog.live_checkpoints,
        ClineNativeProfile::CoreOnly,
    );
    let failure = outcome(&incomplete.outcomes, ClineComponent::ApiHistory)
        .failure
        .as_ref()
        .expect("incomplete failure");
    assert_eq!(failure.kind, ClineComponentFailureKind::IncompleteJson);
    assert!(failure.retryable);
    assert!(component_pages(&incomplete.pages, ClineComponent::ApiHistory).is_empty());
    assert!(incomplete.catalog.live_checkpoints[0].api_history.is_some());

    fs::write(
        &fixture.api,
        br#"[{"id":"before","role":"user","content":"safe"},42,{"id": }]"#,
    )
    .expect("write corrupt records");
    let corrupt = read_all(
        &fixture.root,
        &rewritten.catalog.live_checkpoints,
        ClineNativeProfile::CoreOnly,
    );
    let corrupt_outcome = outcome(&corrupt.outcomes, ClineComponent::ApiHistory);
    assert!(corrupt_outcome.failure.is_none());
    let corrupt_pages = component_pages(&corrupt.pages, ClineComponent::ApiHistory);
    assert_eq!(corrupt_pages.len(), 3);
    assert_eq!(
        corrupt_pages
            .iter()
            .flat_map(|page| page.core.rejections.iter())
            .count(),
        2
    );
    assert_eq!(
        corrupt_pages
            .iter()
            .flat_map(|page| page.core.events.iter())
            .filter_map(|event| event.body.as_deref())
            .collect::<Vec<_>>(),
        vec!["safe"]
    );
}

#[test]
fn a_file_appearing_after_missing_observation_cannot_publish_stale_deletion() {
    let fixture = Fixture::new(api_messages(&["baseline"]), json!([]));
    let baseline = read_all(&fixture.root, &[], ClineNativeProfile::CoreOnly);
    fs::remove_file(&fixture.api).expect("remove API before discovery");
    let discovery = discover_cline_root(&fixture.root).expect("discover missing API");
    let appeared = Arc::new(AtomicBool::new(false));
    let appeared_flag = Arc::clone(&appeared);
    let mut reader = ClineNativeReader::new(
        discovery,
        &baseline.catalog.live_checkpoints,
        ClineNativeProfile::CoreOnly,
    );
    reader.set_before_exposure_hook(move |path, component| {
        if component == ClineComponent::ApiHistory && !appeared_flag.swap(true, Ordering::SeqCst) {
            write_json(path, &api_messages(&["appeared"]));
        }
    });
    let mut pages = Vec::new();
    while let Some(page) = reader.next_page().expect("source-local mutation") {
        pages.push(page);
    }

    assert!(appeared.load(Ordering::SeqCst));
    assert!(component_pages(&pages, ClineComponent::ApiHistory).is_empty());
    assert_eq!(
        outcome(reader.outcomes(), ClineComponent::ApiHistory)
            .failure
            .as_ref()
            .map(|failure| failure.kind),
        Some(ClineComponentFailureKind::SourceChanged)
    );
}

#[test]
fn prior_array_deletion_is_refused_while_metadata_remains_missing() {
    let fixture = Fixture::new(api_messages(&["baseline"]), json!([]));
    fs::remove_file(&fixture.metadata).expect("start without metadata");
    let baseline = read_all(&fixture.root, &[], ClineNativeProfile::CoreOnly);
    fs::remove_file(&fixture.api).expect("remove prior API array");

    let refused = read_all(
        &fixture.root,
        &baseline.catalog.live_checkpoints,
        ClineNativeProfile::CoreOnly,
    );
    assert!(refused.pages.is_empty());
    let api = outcome(&refused.outcomes, ClineComponent::ApiHistory);
    let failure = api.failure.as_ref().expect("typed deletion refusal");
    assert_eq!(failure.kind, ClineComponentFailureKind::SourceChanged);
    assert!(failure.retryable);
    assert!(refused.catalog.live_checkpoints[0].api_history.is_some());
}

#[test]
fn prior_array_deletion_is_refused_while_metadata_task_id_remains_invalid() {
    let fixture = Fixture::new(api_messages(&["baseline"]), json!([]));
    write_json(&fixture.metadata, &json!({"taskId": "\n"}));
    let baseline = read_all(&fixture.root, &[], ClineNativeProfile::CoreOnly);
    fs::remove_file(&fixture.api).expect("remove prior API array");

    let refused = read_all(
        &fixture.root,
        &baseline.catalog.live_checkpoints,
        ClineNativeProfile::CoreOnly,
    );
    assert!(refused.pages.is_empty());
    let api = outcome(&refused.outcomes, ClineComponent::ApiHistory);
    let failure = api.failure.as_ref().expect("typed deletion refusal");
    assert_eq!(failure.kind, ClineComponentFailureKind::SourceChanged);
    assert!(failure.retryable);
    assert!(refused.catalog.live_checkpoints[0].api_history.is_some());
}

#[test]
fn metadata_change_at_exposure_cannot_publish_a_prior_array_deletion() {
    let fixture = Fixture::new(api_messages(&["baseline"]), json!([]));
    write_json(&fixture.metadata, &json!({"taskId": "task-1"}));
    let baseline = read_all(&fixture.root, &[], ClineNativeProfile::CoreOnly);
    fs::remove_file(&fixture.api).expect("remove API before discovery");
    let discovery = discover_cline_root(&fixture.root).expect("discover missing API");
    let mutated = Arc::new(AtomicBool::new(false));
    let mutated_flag = Arc::clone(&mutated);
    let metadata_path = fixture.metadata.clone();
    let mut reader = ClineNativeReader::new(
        discovery,
        &baseline.catalog.live_checkpoints,
        ClineNativeProfile::CoreOnly,
    );
    reader.set_before_exposure_hook(move |_path, component| {
        if component == ClineComponent::ApiHistory && !mutated_flag.swap(true, Ordering::SeqCst) {
            write_json(&metadata_path, &json!({"taskId": "task-2"}));
        }
    });
    let mut pages = Vec::new();
    while let Some(page) = reader.next_page().expect("source-local metadata mutation") {
        pages.push(page);
    }

    assert!(mutated.load(Ordering::SeqCst));
    assert!(component_pages(&pages, ClineComponent::ApiHistory).is_empty());
    let api = outcome(reader.outcomes(), ClineComponent::ApiHistory);
    let failure = api.failure.as_ref().expect("typed deletion refusal");
    assert_eq!(failure.kind, ClineComponentFailureKind::SourceChanged);
    assert!(failure.retryable);
}

#[test]
fn metadata_deletion_preserves_the_previously_certified_public_identity() {
    let fixture = Fixture::new(api_messages(&["identity"]), json!([]));
    write_json(
        &fixture.metadata,
        &json!({
            "taskId": "public-task-id",
            "createdAt": "2026-07-25T00:00:00Z"
        }),
    );
    let baseline = read_all(&fixture.root, &[], ClineNativeProfile::CoreOnly);
    assert_eq!(
        baseline.catalog.live_checkpoints[0].identity.as_str(),
        "public-task-id"
    );

    fs::remove_file(&fixture.metadata).expect("remove metadata");
    let deleted = read_all(
        &fixture.root,
        &baseline.catalog.live_checkpoints,
        ClineNativeProfile::CoreOnly,
    );
    let metadata_page = component_pages(&deleted.pages, ClineComponent::TaskMetadata)
        .pop()
        .expect("metadata deletion page");
    assert_eq!(metadata_page.source.task.as_str(), "public-task-id");
    assert_eq!(
        metadata_page.core.transition,
        ClineComponentTransition::MissingPhysical
    );
    assert_eq!(
        deleted.catalog.live_checkpoints[0].identity.as_str(),
        "public-task-id"
    );
}

#[test]
fn authoritative_metadata_identity_upgrade_reprojects_unchanged_components_once() {
    let fixture = Fixture::new(api_messages(&["upgrade"]), json!([]));
    fs::remove_file(&fixture.metadata).expect("start with directory identity");
    let degraded = read_all(&fixture.root, &[], ClineNativeProfile::CoreOnly);
    assert_eq!(
        degraded.catalog.live_checkpoints[0].identity.as_str(),
        "task-1"
    );

    write_json(
        &fixture.metadata,
        &json!({
            "taskId": "public-upgraded-id",
            "createdAt": "2026-07-25T00:00:00Z"
        }),
    );
    let upgraded = read_all(
        &fixture.root,
        &degraded.catalog.live_checkpoints,
        ClineNativeProfile::CoreOnly,
    );
    assert_eq!(
        upgraded.catalog.live_checkpoints[0].identity.as_str(),
        "public-upgraded-id"
    );
    let api_pages = component_pages(&upgraded.pages, ClineComponent::ApiHistory);
    assert_eq!(api_pages.len(), 1);
    assert_eq!(
        outcome(&upgraded.outcomes, ClineComponent::ApiHistory).transition,
        Some(ClineComponentTransition::Rewrite)
    );
    assert!(api_pages[0]
        .core
        .events
        .iter()
        .all(|event| event.identity.task.as_str() == "public-upgraded-id"));
    // Metadata, API, UI, and the terminal root index are each read once.
    assert_eq!(upgraded.stats.component_hydrations, 4);
    assert_eq!(upgraded.stats.component_parse_passes, 4);
}

#[test]
fn malformed_independent_items_are_rejected_locally_and_valid_siblings_survive() {
    let fixture = Fixture::new(
        json!([
            {"id":"before","role":"user","content":"before"},
            42,
            {
                "id":"after",
                "role":"assistant",
                "content":[
                    {"type":"text","text":"after"},
                    {
                        "type":"tool_result",
                        "tool_use_id":"call-1",
                        "content":"done",
                        "status":"success"
                    }
                ]
            }
        ]),
        json!([]),
    );
    let result = read_all(&fixture.root, &[], ClineNativeProfile::CoreAndPro);
    let pages = component_pages(&result.pages, ClineComponent::ApiHistory);
    assert_eq!(pages.len(), 3);
    assert_eq!(
        pages
            .iter()
            .flat_map(|page| page.core.events.iter())
            .filter_map(|event| event.body.as_deref())
            .collect::<Vec<_>>(),
        vec!["before", "after"]
    );
    assert_eq!(
        pages
            .iter()
            .flat_map(|page| page.core.rejections.iter())
            .map(|rejection| rejection.kind)
            .collect::<Vec<_>>(),
        vec![ClineItemRejectionKind::MalformedRecord]
    );
    assert_eq!(
        pages
            .iter()
            .flat_map(|page| {
                page.transient
                    .as_ref()
                    .expect("transient")
                    .observations
                    .iter()
            })
            .count(),
        1
    );
}

#[test]
fn multi_call_file_touches_attach_only_to_their_owning_tool_rows() {
    let fixture = Fixture::new(
        json!([{
            "id": "multi-call",
            "role": "assistant",
            "content": [
                {
                    "type": "tool_use",
                    "tool_use_id": "call-a",
                    "name": "apply_patch",
                    "input": {
                        "patch": "*** Begin Patch\n*** Update File: src/a.rs\n@@\n-old\n+new\n*** End Patch"
                    }
                },
                {
                    "type": "tool_use",
                    "tool_use_id": "call-b",
                    "name": "apply_patch",
                    "input": {
                        "patch": "*** Begin Patch\n*** Add File: src/b.rs\n+new\n*** End Patch"
                    }
                },
                {
                    "type": "text",
                    "text": "*** Begin Patch\n*** Delete File: src/unrelated.rs\n*** End Patch"
                }
            ]
        }]),
        json!([]),
    );

    let result = read_all(&fixture.root, &[], ClineNativeProfile::CoreOnly);
    let tool_rows = component_pages(&result.pages, ClineComponent::ApiHistory)
        .into_iter()
        .flat_map(|page| page.core.events.iter())
        .filter_map(|event| {
            event.tool_call.as_ref().map(|tool_call| {
                (
                    tool_call.call_id.as_deref().expect("tool call id"),
                    event
                        .file_touches
                        .iter()
                        .map(|touch| touch.path.as_ref())
                        .collect::<Vec<_>>(),
                )
            })
        })
        .collect::<Vec<_>>();

    assert_eq!(
        tool_rows,
        vec![("call-a", vec!["src/a.rs"]), ("call-b", vec!["src/b.rs"]),]
    );
}

#[test]
fn local_component_io_failure_preserves_siblings_while_resource_io_aborts() {
    let fixture = Fixture::new(api_messages(&["api"]), api_messages(&["ui"]));
    let discovery = discover_cline_root(&fixture.root).expect("discover");
    inject_cline_io_failure(
        ClineInjectedIoOperation::ComponentOpen,
        fixture.api.clone(),
        io::Error::new(io::ErrorKind::PermissionDenied, "denied"),
        1,
    );
    let mut reader = ClineNativeReader::new(discovery, &[], ClineNativeProfile::CoreOnly);
    let mut pages = Vec::new();
    while let Some(page) = reader.next_page().expect("local failure continues") {
        pages.push(page);
    }
    clear_cline_io_failure();
    assert!(component_pages(&pages, ClineComponent::ApiHistory).is_empty());
    assert!(!component_pages(&pages, ClineComponent::UiMessages).is_empty());
    assert_eq!(
        outcome(reader.outcomes(), ClineComponent::ApiHistory)
            .failure
            .as_ref()
            .map(|failure| failure.kind),
        Some(ClineComponentFailureKind::LocalIo)
    );

    let discovery = discover_cline_root(&fixture.root).expect("rediscover post-parse case");
    inject_cline_io_failure(
        ClineInjectedIoOperation::ComponentPostParseStat,
        fixture.api.clone(),
        io::Error::new(io::ErrorKind::PermissionDenied, "post-parse denied"),
        1,
    );
    let mut reader = ClineNativeReader::new(discovery, &[], ClineNativeProfile::CoreOnly);
    let mut pages = Vec::new();
    while let Some(page) = reader
        .next_page()
        .expect("post-parse local failure continues")
    {
        pages.push(page);
    }
    clear_cline_io_failure();
    assert!(component_pages(&pages, ClineComponent::ApiHistory).is_empty());
    assert!(!component_pages(&pages, ClineComponent::UiMessages).is_empty());
    assert_eq!(
        outcome(reader.outcomes(), ClineComponent::ApiHistory)
            .failure
            .as_ref()
            .map(|failure| failure.kind),
        Some(ClineComponentFailureKind::LocalIo)
    );

    let discovery = discover_cline_root(&fixture.root).expect("rediscover");
    inject_cline_io_failure(
        ClineInjectedIoOperation::ComponentOpen,
        fixture.api.clone(),
        io::Error::new(io::ErrorKind::OutOfMemory, "resource exhausted"),
        1,
    );
    let mut reader = ClineNativeReader::new(discovery, &[], ClineNativeProfile::CoreOnly);
    let error = loop {
        match reader.next_page() {
            Ok(Some(_)) => {}
            Ok(None) => panic!("resource I/O failure did not abort the source"),
            Err(error) => break error,
        }
    };
    clear_cline_io_failure();
    assert!(matches!(
        error,
        ClineNativePathError::SourceIo {
            kind: io::ErrorKind::OutOfMemory,
            ..
        }
    ));
}

#[test]
fn directory_inventory_completion_is_terminal_and_does_not_revoke_safe_file_pages() {
    let fixture = Fixture::new(api_messages(&["safe"]), json!([]));
    let discovery = discover_cline_root(&fixture.root).expect("discover");
    let mut reader = ClineNativeReader::new(discovery, &[], ClineNativeProfile::CoreOnly);
    let mut pages = Vec::new();
    while let Some(page) = reader.next_page().expect("file pages") {
        pages.push(page);
    }
    assert!(!pages.is_empty());
    let inserted = fixture.root.join("tasks").join("late-task");
    fs::create_dir_all(&inserted).expect("late task directory");
    write_json(
        &inserted.join("task_metadata.json"),
        &json!({"taskId":"late-task"}),
    );
    let error = reader
        .finish_catalog()
        .expect_err("catalog inventory changed");
    assert!(matches!(error, ClineNativePathError::SourceChanged { .. }));
    assert!(!component_pages(&pages, ClineComponent::ApiHistory).is_empty());
}

#[test]
fn arrays_larger_than_sixteen_mib_stream_one_item_and_page_at_a_time() {
    let fixture = Fixture::new(json!([]), json!([]));
    let body = "x".repeat(1024 * 1024);
    let mut writer = BufWriter::new(fs::File::create(&fixture.api).expect("large API source"));
    writer.write_all(b"[").expect("array start");
    for index in 0..17 {
        if index != 0 {
            writer.write_all(b",").expect("array separator");
        }
        serde_json::to_writer(
            &mut writer,
            &json!({
                "id": format!("large-{index}"),
                "role": "tool",
                "tool_use_id": format!("call-{index}"),
                "exit_code": 0,
                "content": [{
                    "type": "tool_result",
                    "text": body,
                }],
            }),
        )
        .expect("large API item");
    }
    writer.write_all(b"]").expect("array end");
    writer.flush().expect("large API flush");
    let source_bytes = fs::metadata(&fixture.api)
        .expect("large API metadata")
        .len();
    assert!(source_bytes > 16 * 1024 * 1024);

    let result = read_all(&fixture.root, &[], ClineNativeProfile::CoreOnly);
    let pages = component_pages(&result.pages, ClineComponent::ApiHistory);
    assert_eq!(pages.len(), 17);
    assert!(pages.last().expect("terminal page").terminal);
    assert!(pages
        .windows(2)
        .all(|pages| pages[0].next_safe_frontier == pages[1].expected_frontier));
    assert_eq!(result.stats.component_hydrations, 4);
    assert_eq!(result.stats.component_parse_passes, 4);
    assert_eq!(result.stats.array_item_parse_attempts, 17);
    assert_eq!(result.stats.output_bodies_hydrated, 0);
    assert!(result.stats.max_array_item_bytes_retained < 2 * 1024 * 1024);
    assert!(u64::try_from(result.stats.max_array_item_bytes_retained).unwrap() < source_bytes);
    assert!(result.stats.max_pages_buffered <= 1);
    let checkpoint = result.catalog.live_checkpoints[0]
        .api_history
        .as_ref()
        .expect("large API checkpoint");
    assert_eq!(checkpoint.observed_items, 17);
    assert!(checkpoint.estimated_bytes() < 1024);
}

#[test]
fn failure_diagnostics_are_profile_invariant_through_multi_mib_bodies() {
    let fixture = Fixture::new(
        json!([]),
        json!([
            {
                "id": "tiny",
                "type": "command_output",
                "text": "tiny\nerror",
                "exitCode": 1
            },
            {
                "id": "medium",
                "type": "command_output",
                "text": "m".repeat(128 * 1024),
                "exitCode": 2
            },
            {
                "id": "large",
                "type": "command_output",
                "text": "l".repeat(3 * 1024 * 1024),
                "exitCode": 3
            },
            {
                "id": "over-output-lane",
                "type": "command_output",
                "text": "z".repeat(5 * 1024 * 1024),
                "timedOut": true
            }
        ]),
    );
    let core = read_all(&fixture.root, &[], ClineNativeProfile::CoreOnly);
    let fanout = read_all(&fixture.root, &[], ClineNativeProfile::CoreAndPro);
    let core_pages = component_pages(&core.pages, ClineComponent::UiMessages);
    let fanout_pages = component_pages(&fanout.pages, ClineComponent::UiMessages);
    assert_eq!(
        core_pages
            .iter()
            .map(|page| (
                page.identity,
                &page.expected_frontier,
                &page.next_safe_frontier
            ))
            .collect::<Vec<_>>(),
        fanout_pages
            .iter()
            .map(|page| (
                page.identity,
                &page.expected_frontier,
                &page.next_safe_frontier
            ))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        core_pages
            .iter()
            .flat_map(|page| page.core.events.iter())
            .cloned()
            .collect::<Vec<_>>(),
        fanout_pages
            .iter()
            .flat_map(|page| page.core.events.iter())
            .cloned()
            .collect::<Vec<_>>()
    );
    let diagnostics = core_pages
        .iter()
        .flat_map(|page| page.core.events.iter())
        .map(|event| event.sparse_output.as_ref().expect("sparse diagnostic"))
        .collect::<Vec<_>>();
    assert_eq!(diagnostics.len(), 4);
    assert_eq!(diagnostics[0].preview.as_deref(), Some("tiny\nerror"));
    assert_eq!(
        diagnostics[1]
            .preview
            .as_deref()
            .expect("medium preview")
            .chars()
            .count(),
        4_000
    );
    assert_eq!(
        diagnostics[3]
            .preview
            .as_deref()
            .expect("oversized preview")
            .chars()
            .count(),
        4_000
    );
    assert_eq!(core.stats.output_bodies_hydrated, 0);
    let transient = fanout_pages
        .iter()
        .map(|page| page.transient.as_ref().expect("transient lane"))
        .collect::<Vec<_>>();
    assert_eq!(
        transient
            .iter()
            .flat_map(|payload| payload.observations.iter())
            .count(),
        3
    );
    assert_eq!(
        transient
            .iter()
            .flat_map(|payload| payload.rejected_outputs.iter())
            .count(),
        1
    );
}

#[test]
fn tool_result_arrays_preserve_explicit_result_semantics() {
    let item = json!({
        "id": "result-array",
        "role": "user",
        "tool_use_id": "call-result-array",
        "exit_code": 0,
        "content": [
            {"type": "text", "text": "not output"},
            {"type": "tool_result", "text": "single"},
            {"type": "tool_result", "content": [
                {"text": "first"},
                {"output": "second"},
                null
            ]},
            {"type": "tool_result", "result": {"kind": "structured", "ok": true}},
            {"type": "tool_result", "text": ""},
            {"type": "tool_result", "content": {"redacted": true}},
            {"type": "tool_result", "tool_name": "label-only"},
            42
        ]
    });
    let fixture = Fixture::new(json!([item.clone()]), json!([]));
    let result = read_all(&fixture.root, &[], ClineNativeProfile::CoreAndPro);
    let api_pages = component_pages(&result.pages, ClineComponent::ApiHistory);
    let outputs = api_pages
        .iter()
        .flat_map(|page| {
            page.transient
                .as_ref()
                .expect("transient result lane")
                .observations
                .iter()
        })
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), 6);
    assert_eq!(
        outputs
            .iter()
            .map(|output| String::from_utf8(output.content.clone()).expect("text output"))
            .collect::<Vec<_>>()
            .join("\n"),
        "single\nfirst\nsecond\n{\"kind\":\"structured\",\"ok\":true}\n\n{\"redacted\":true}"
    );
    assert_eq!(
        outputs
            .iter()
            .map(|output| output.content.as_slice())
            .collect::<Vec<_>>(),
        vec![
            b"single".as_slice(),
            b"first".as_slice(),
            b"second".as_slice(),
            br#"{"kind":"structured","ok":true}"#.as_slice(),
            b"".as_slice(),
            br#"{"redacted":true}"#.as_slice(),
        ]
    );
    assert!(outputs.windows(2).all(|outputs| {
        outputs[0].coordinate.byte_start < outputs[1].coordinate.byte_start
            && outputs[0].coordinate.source_record_subrecord_index
                < outputs[1].coordinate.source_record_subrecord_index
    }));
    assert!(api_pages
        .iter()
        .flat_map(|page| page.core.rejections.iter())
        .next()
        .is_none());
}

#[test]
fn direct_tool_results_preserve_absence_null_and_empty_semantics() {
    let items = vec![
        json!({"id":"label-only","type":"tool_result","tool_name":"shell"}),
        json!({
            "id":"no-body-leaf",
            "type":"tool_result",
            "content":[{"type":"tool_result","tool_name":"shell"}]
        }),
        json!({"id":"null-body","type":"tool_result","result":null}),
        json!({"id":"empty-body","type":"tool_result","result":""}),
    ];
    let fixture = Fixture::new(Value::Array(items.clone()), json!([]));
    let result = read_all(&fixture.root, &[], ClineNativeProfile::CoreAndPro);
    let pages = component_pages(&result.pages, ClineComponent::ApiHistory);
    assert_eq!(pages.len(), items.len());
    let expected = [None, None, None, Some("")];
    for ((page, item), expected) in pages.into_iter().zip(&items).zip(expected) {
        let outputs = page
            .transient
            .as_ref()
            .expect("transient result lane")
            .observations
            .as_slice();
        assert_eq!(
            outputs.len(),
            usize::from(expected.is_some()),
            "direct result mismatch: {item}"
        );
        assert_eq!(page.accounting.potential_output_units, outputs.len());
        if let Some(expected) = expected {
            assert_eq!(outputs[0].content, expected.as_bytes());
        }
    }
}

#[test]
fn accounting_admits_exact_output_boundary_and_counts_four_kib_identifiers() {
    let identifier = "i".repeat(4 * 1024);
    let fixture = Fixture::new(
        json!([{
            "id": "tool-call",
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "tool_use_id": identifier.clone(),
                "name": identifier
            }]
        }]),
        json!([{
            "id": "exact-output",
            "type": "command_output",
            "text": "",
            "exitCode": 0
        }]),
    );
    let baseline = read_all(&fixture.root, &[], ClineNativeProfile::CoreAndPro);
    let call_page = component_pages(&baseline.pages, ClineComponent::ApiHistory)
        .pop()
        .expect("tool-call page");
    let call = call_page
        .core
        .events
        .iter()
        .find_map(|event| event.tool_call.as_ref().map(|call| (event, call)))
        .expect("retained 4 KiB tool call");
    assert_eq!(call.1.call_id.as_deref().map(str::len), Some(4 * 1024));
    assert_eq!(call.1.name.as_deref().map(str::len), Some(4 * 1024));
    assert!(
        super::normalize::estimated_event_bytes(call.0)
            >= call.1.call_id.as_deref().unwrap().len() + call.1.name.as_deref().unwrap().len()
    );

    let empty_output = component_pages(&baseline.pages, ClineComponent::UiMessages)
        .pop()
        .expect("empty output page")
        .transient
        .as_ref()
        .expect("transient output")
        .observations
        .first()
        .expect("empty output observation");
    let empty_encoded = super::normalize::estimated_output_bytes(empty_output);
    let exact_content_bytes = super::normalize::CLINE_NATIVE_TRANSIENT_PAGE_MAX_BYTES
        .checked_sub(16)
        .expect("transient payload wrapper fits lane")
        .checked_sub(empty_encoded)
        .expect("output envelope fits transient lane");
    write_json(
        &fixture.ui,
        &json!([{
            "id": "exact-output",
            "type": "command_output",
            "text": "e".repeat(exact_content_bytes),
            "exitCode": 0
        }]),
    );
    let exact = read_all(&fixture.root, &[], ClineNativeProfile::CoreAndPro);
    let exact_page = component_pages(&exact.pages, ClineComponent::UiMessages)
        .pop()
        .expect("exact output page");
    let exact_output = exact_page
        .transient
        .as_ref()
        .expect("exact transient lane")
        .observations
        .first()
        .expect("exact-boundary output");
    assert_eq!(
        super::normalize::estimated_output_bytes(exact_output),
        super::normalize::CLINE_NATIVE_TRANSIENT_PAGE_MAX_BYTES - 16
    );
    assert_eq!(
        exact_page.accounting.transient_output_bytes,
        super::normalize::CLINE_NATIVE_TRANSIENT_PAGE_MAX_BYTES
    );
    assert!(exact_page.accounting.conservative_serialized_bytes <= CLINE_NATIVE_PAGE_MAX_BYTES);

    write_json(
        &fixture.ui,
        &json!([{
            "id": "exact-output",
            "type": "command_output",
            "text": "e".repeat(exact_content_bytes + 1),
            "exitCode": 0
        }]),
    );
    let over = read_all(&fixture.root, &[], ClineNativeProfile::CoreAndPro);
    let over_page = component_pages(&over.pages, ClineComponent::UiMessages)
        .pop()
        .expect("over-boundary page");
    let transient = over_page.transient.as_ref().expect("over transient lane");
    assert!(transient.observations.is_empty());
    assert_eq!(transient.rejected_outputs.len(), 1);
}

#[test]
fn final_owned_page_bounds_accept_exact_limits_and_reject_plus_one() {
    let core_limit = super::normalize::CLINE_NATIVE_CORE_PAGE_MAX_BYTES;
    let total_limit = super::normalize::CLINE_NATIVE_PAGE_MAX_BYTES;
    let transient_at_total = total_limit - core_limit;
    assert!(super::reader::owned_page_bounds_are_valid(
        core_limit,
        transient_at_total,
        CLINE_NATIVE_PAGE_MAX_UNITS,
    ));
    assert!(!super::reader::owned_page_bounds_are_valid(
        core_limit + 1,
        0,
        CLINE_NATIVE_PAGE_MAX_UNITS,
    ));
    assert!(!super::reader::owned_page_bounds_are_valid(
        core_limit,
        transient_at_total + 1,
        CLINE_NATIVE_PAGE_MAX_UNITS,
    ));
    assert!(!super::reader::owned_page_bounds_are_valid(
        0,
        0,
        CLINE_NATIVE_PAGE_MAX_UNITS + 1,
    ));
}
