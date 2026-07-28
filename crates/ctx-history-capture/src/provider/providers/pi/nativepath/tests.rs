use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    sync::Mutex,
};

use ctx_history_core::EventType;

use crate::{
    provider::native_ingestion::{process_pro_replay_only, NativeProReplayPage},
    test_support_paths::tempdir,
    ProOutputMaterializationPage, ProOutputPageResult, ProOutputProgress, ProOutputSink,
    ProOutputSinkError, ProviderAdapterContext,
};

use super::*;

fn context(path: &Path) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "pi-nativepath-machine".to_owned(),
        source_path: Some(path.to_path_buf()),
        source_root: path.parent().map(Path::to_path_buf),
        imported_at: "2026-07-25T12:00:00Z".parse().unwrap(),
    }
}

fn write_lines(path: &Path, lines: &[String]) {
    fs::write(path, format!("{}\n", lines.join("\n"))).unwrap();
}

fn header(session_id: &str) -> String {
    format!(
        r#"{{"type":"session","id":"{session_id}","version":3,"timestamp":"2026-07-25T12:00:00Z","cwd":"/workspace","parentSession":"parent-pi"}}"#
    )
}

fn message(id: &str, role: &str, content: &str, second: u64) -> String {
    serde_json::json!({
        "type": "message",
        "id": id,
        "timestamp": format!("2026-07-25T12:00:{second:02}Z"),
        "message": {"role": role, "content": content},
    })
    .to_string()
}

struct Scanned {
    core: Vec<crate::provider::native_ingestion::NativeIngestionPage<PiNativeCorePage>>,
    output: Vec<NativeProReplayPage>,
    outcome: PiNativeScanOutcome,
}

fn scan(
    path: &Path,
    profile: PiNativeProfile,
    resume: PiNativeResume,
) -> Result<Scanned, PiNativePathError> {
    let mut options = PiNativeScanOptions::new(context(path), profile);
    options.resume = resume;
    options.inventory_generation = 7;
    options.output_materializer_revision = "pi-nativepath-test-materializer".to_owned();
    let PiNativeOpenOutcome::Ready(mut scanner) = open_pi_native_session(path, options)? else {
        panic!("expected a live Pi source");
    };
    let mut core = Vec::new();
    let mut output = Vec::new();
    while let Some(page) = scanner.next_page()? {
        match page {
            PiNativeOwnedPage::Core(page) => core.push(page),
            PiNativeOwnedPage::Output(page) => output.push(*page),
        }
    }
    let outcome = scanner.outcome().expect("exhausted scanner outcome");
    Ok(Scanned {
        core,
        output,
        outcome,
    })
}

fn flattened_units(scanned: &Scanned) -> Vec<&PiNativeCoreUnit> {
    scanned
        .core
        .iter()
        .flat_map(|page| page.core.units.iter())
        .collect()
}

#[derive(Default)]
struct RecordingOutputSink {
    fail_once: Mutex<bool>,
    pages: Mutex<Vec<ProOutputMaterializationPage>>,
}

impl ProOutputSink for RecordingOutputSink {
    fn inventory_generation(&self) -> u64 {
        7
    }

    fn materializer_revision(&self) -> &str {
        "pi-nativepath-test-materializer"
    }

    fn observe_source(
        &self,
        _source: &crate::OutputSourceIdentity,
    ) -> Result<Option<ProOutputProgress>, ProOutputSinkError> {
        Ok(None)
    }

    fn materialize_page(
        &self,
        page: ProOutputMaterializationPage,
    ) -> Result<ProOutputPageResult, ProOutputSinkError> {
        if std::mem::take(&mut *self.fail_once.lock().unwrap()) {
            return Err(ProOutputSinkError::new("pi_test", "retry this page"));
        }
        let source_epoch = page.source_epoch;
        let committed_cursor = page.next_safe_cursor.clone();
        let accepted_outputs = u32::try_from(page.observations.len()).unwrap();
        self.pages.lock().unwrap().push(page);
        Ok(ProOutputPageResult {
            source_epoch,
            committed_cursor,
            accepted_outputs,
            materialized_facts: accepted_outputs,
            replayed: false,
        })
    }
}

fn materialize_output_pages(pages: Vec<NativeProReplayPage>) -> Vec<ProOutputMaterializationPage> {
    let sink = RecordingOutputSink::default();
    for page in pages {
        process_pro_replay_only(page, &sink).unwrap();
    }
    let captured = std::mem::take(&mut *sink.pages.lock().unwrap());
    captured
}

#[test]
fn successful_and_unknown_results_are_pro_only_and_failures_are_bounded_core_diagnostics() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("privacy.jsonl");
    let secret = "successful-output-must-never-enter-core";
    let lines = vec![
        header("pi-private"),
        serde_json::json!({
            "type": "message",
            "id": "success",
            "timestamp": "2026-07-25T12:00:01Z",
            "message": {
                "role": "toolResult",
                "success": true,
                "content": format!("*** Begin Patch\n*** Update File: src/private.rs\n@@\n-{secret}\n+new\n*** End Patch")
            }
        })
        .to_string(),
        serde_json::json!({
            "type": "message",
            "id": "unknown",
            "timestamp": "2026-07-25T12:00:02Z",
            "message": {"role": "toolResult", "content": "unknown private output"}
        })
        .to_string(),
        serde_json::json!({
            "type": "message",
            "id": "failure",
            "timestamp": "2026-07-25T12:00:03Z",
            "message": {
                "role": "bashExecution",
                "command": "false",
                "output": "bounded failure",
                "exitCode": 1
            }
        })
        .to_string(),
    ];
    write_lines(&path, &lines);
    let scanned = scan(
        &path,
        PiNativeProfile::CoreAndPro,
        PiNativeResume::default(),
    )
    .unwrap();
    let core_json = serde_json::to_string(
        &scanned
            .core
            .iter()
            .flat_map(|page| &page.core.units)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    assert!(!core_json.contains(secret));
    assert!(!core_json.contains("unknown private output"));
    assert!(!core_json.contains("src/private.rs"));
    let units = flattened_units(&scanned);
    assert_eq!(
        units
            .iter()
            .filter(|unit| matches!(unit, PiNativeCoreUnit::Event(_)))
            .count(),
        1
    );
    let failure = units
        .iter()
        .find_map(|unit| match unit {
            PiNativeCoreUnit::Event(event) => Some(event),
            _ => None,
        })
        .unwrap();
    assert_eq!(failure.event_type, EventType::CommandOutput);
    assert_eq!(failure.payload["output_preview"], "bounded failure");
    assert_eq!(failure.payload["exit_code"], 1);
    assert_eq!(scanned.outcome.stats.successful_or_unknown_core_bodies, 0);
    assert_eq!(scanned.outcome.stats.successful_or_unknown_core_hashes, 0);
    assert_eq!(scanned.outcome.stats.successful_or_unknown_core_previews, 0);
    assert_eq!(scanned.outcome.stats.successful_or_unknown_core_touches, 0);
    assert_eq!(
        scanned
            .outcome
            .stats
            .successful_or_unknown_core_fts_documents,
        0
    );
    let pages = materialize_output_pages(scanned.output);
    assert_eq!(
        pages
            .iter()
            .map(|page| page.observations.len())
            .sum::<usize>(),
        3
    );
}

#[test]
fn core_pages_are_profile_invariant_and_independent_pages_are_group_ready() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("bounds.jsonl");
    let mut lines = vec![header("pi-bounds")];
    for index in 0..65_u64 {
        lines.push(message(
            &format!("user-{index}"),
            "user",
            &format!("message {index}"),
            (index % 50) + 1,
        ));
        lines.push(
            serde_json::json!({
                "type": "message",
                "id": format!("result-{index}"),
                "timestamp": format!("2026-07-25T12:01:{:02}Z", index % 50),
                "message": {
                    "role": "toolResult",
                    "toolCallId": format!("call-{index}"),
                    "success": true,
                    "content": format!("output {index}")
                }
            })
            .to_string(),
        );
    }
    write_lines(&path, &lines);
    let core_only = scan(&path, PiNativeProfile::CoreOnly, PiNativeResume::default()).unwrap();
    let core_and_pro = scan(
        &path,
        PiNativeProfile::CoreAndPro,
        PiNativeResume::default(),
    )
    .unwrap();
    assert_eq!(core_only.core.len(), core_and_pro.core.len());
    for (core_only_page, core_and_pro_page) in core_only.core.iter().zip(&core_and_pro.core) {
        assert_eq!(
            core_only_page.expected_frontier,
            core_and_pro_page.expected_frontier
        );
        assert_eq!(
            core_only_page.next_safe_frontier,
            core_and_pro_page.next_safe_frontier
        );
        assert_eq!(core_only_page.terminal, core_and_pro_page.terminal);
        assert_eq!(core_only_page.core.units, core_and_pro_page.core.units);
        assert_eq!(
            core_only_page.core.encoded_bytes,
            core_and_pro_page.core.encoded_bytes
        );
    }
    assert_eq!(core_and_pro.core.len(), 2);
    assert_eq!(core_and_pro.output.len(), 2);
    for page in &core_and_pro.core {
        assert!(page.accounting.logical_units <= PI_NATIVE_PAGE_MAX_UNITS);
        assert!(page.accounting.conservative_serialized_bytes <= PI_NATIVE_PAGE_MAX_BYTES);
    }
    for page in &core_and_pro.output {
        assert!(page.accounting.logical_units <= PI_NATIVE_PAGE_MAX_UNITS);
        assert!(page.accounting.conservative_serialized_bytes <= PI_NATIVE_PAGE_MAX_BYTES);
    }
    assert!(core_and_pro.outcome.stats.peak_ready_page_bytes <= PI_NATIVE_PAGE_MAX_BYTES);
}

#[test]
fn output_replay_is_core_free_and_failed_handoff_retries_without_reparse() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("replay.jsonl");
    write_lines(
        &path,
        &[
            header("pi-replay"),
            serde_json::json!({
                "type": "message",
                "id": "result-1",
                "timestamp": "2026-07-25T12:00:01Z",
                "message": {"role": "toolResult", "success": true, "content": "replay me"}
            })
            .to_string(),
        ],
    );
    let replay = scan(
        &path,
        PiNativeProfile::ProReplayOnly,
        PiNativeResume::default(),
    )
    .unwrap();
    assert!(replay.core.is_empty());
    assert_eq!(replay.outcome.stats.semantic_records_parsed, 2);
    assert_eq!(replay.outcome.stats.successful_or_unknown_core_bodies, 0);
    let mut pages = replay.output;
    assert_eq!(pages.len(), 1);
    let page = pages.pop().unwrap();
    let identity = page.identity;
    let sink = RecordingOutputSink {
        fail_once: Mutex::new(true),
        ..RecordingOutputSink::default()
    };
    let failure = process_pro_replay_only(page, &sink).unwrap_err();
    assert_eq!(failure.page.identity, identity);
    let receipt = process_pro_replay_only(failure.page, &sink).unwrap();
    assert_eq!(receipt.output_page_identity, identity);
    assert_eq!(sink.pages.lock().unwrap().len(), 1);
}

#[test]
fn lifecycle_fresh_noop_append_restart_and_exact_rewrite() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("lifecycle.jsonl");
    let first_lines = vec![
        header("pi-lifecycle"),
        message("first", "user", "first body", 1),
    ];
    write_lines(&path, &first_lines);
    let fresh = scan(&path, PiNativeProfile::CoreOnly, PiNativeResume::default()).unwrap();
    assert_eq!(fresh.outcome.core_lifecycle, Some(PiSourceLifecycle::Fresh));
    let checkpoint = fresh.outcome.core_checkpoint.clone().unwrap();

    let no_op = scan(
        &path,
        PiNativeProfile::CoreOnly,
        PiNativeResume {
            core: Some(checkpoint.clone()),
            output: None,
        },
    )
    .unwrap();
    assert!(no_op.core.is_empty());
    assert_eq!(no_op.outcome.core_lifecycle, Some(PiSourceLifecycle::NoOp));
    assert_eq!(no_op.outcome.stats.semantic_records_parsed, 0);
    assert!(no_op.outcome.stats.prefix_bytes_hashed > 0);
    assert_eq!(no_op.outcome.stats.source_file_opens, 1);

    let encoded = serde_json::to_vec(&checkpoint).unwrap();
    let restarted: PiNativeCheckpoint = serde_json::from_slice(&encoded).unwrap();
    let restart = scan(
        &path,
        PiNativeProfile::CoreOnly,
        PiNativeResume {
            core: Some(restarted),
            output: None,
        },
    )
    .unwrap();
    assert_eq!(restart.outcome.stats.semantic_records_parsed, 0);

    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    writeln!(
        file,
        "{}",
        message("second", "assistant", "append delta", 2)
    )
    .unwrap();
    file.sync_all().unwrap();
    let appended = scan(
        &path,
        PiNativeProfile::CoreOnly,
        PiNativeResume {
            core: Some(checkpoint.clone()),
            output: None,
        },
    )
    .unwrap();
    assert_eq!(
        appended.outcome.core_lifecycle,
        Some(PiSourceLifecycle::Append)
    );
    assert_eq!(appended.outcome.stats.semantic_records_parsed, 1);
    assert_eq!(
        flattened_units(&appended)
            .iter()
            .filter(|unit| matches!(unit, PiNativeCoreUnit::Event(_)))
            .count(),
        1
    );

    let rewritten_lines = vec![
        header("pi-lifecycle"),
        message("rewrite", "user", "rewritten", 3),
    ];
    write_lines(&path, &rewritten_lines);
    let rewritten = scan(
        &path,
        PiNativeProfile::CoreOnly,
        PiNativeResume {
            core: Some(checkpoint),
            output: None,
        },
    )
    .unwrap();
    assert_eq!(
        rewritten.outcome.core_lifecycle,
        Some(PiSourceLifecycle::Rewrite)
    );
    assert_eq!(
        rewritten.outcome.stats.semantic_records_parsed,
        u64::try_from(rewritten_lines.len()).unwrap()
    );
}

#[test]
fn lifecycle_truncate_replace_relocate_copy_and_delete() {
    let temp = tempdir().unwrap();

    let truncate_path = temp.path().join("truncate.jsonl");
    write_lines(
        &truncate_path,
        &[header("pi-truncate"), message("one", "user", "truncate", 1)],
    );
    let truncate_fresh = scan(
        &truncate_path,
        PiNativeProfile::CoreOnly,
        PiNativeResume::default(),
    )
    .unwrap();
    let truncate_checkpoint = truncate_fresh.outcome.core_checkpoint.unwrap();
    OpenOptions::new()
        .write(true)
        .open(&truncate_path)
        .unwrap()
        .set_len(8)
        .unwrap();
    let truncated = scan(
        &truncate_path,
        PiNativeProfile::CoreOnly,
        PiNativeResume {
            core: Some(truncate_checkpoint),
            output: None,
        },
    )
    .unwrap();
    assert_eq!(
        truncated.outcome.core_lifecycle,
        Some(PiSourceLifecycle::Truncate)
    );

    let replace_path = temp.path().join("replace.jsonl");
    write_lines(&replace_path, &[header("pi-replace")]);
    let replace_fresh = scan(
        &replace_path,
        PiNativeProfile::CoreOnly,
        PiNativeResume::default(),
    )
    .unwrap();
    let replace_checkpoint = replace_fresh.outcome.core_checkpoint.unwrap();
    let replacement_path = temp.path().join("replacement-source.jsonl");
    write_lines(
        &replacement_path,
        &[
            header("pi-replace"),
            message("new", "user", "replacement", 1),
        ],
    );
    fs::remove_file(&replace_path).unwrap();
    fs::rename(&replacement_path, &replace_path).unwrap();
    let replaced = scan(
        &replace_path,
        PiNativeProfile::CoreOnly,
        PiNativeResume {
            core: Some(replace_checkpoint),
            output: None,
        },
    )
    .unwrap();
    assert_eq!(
        replaced.outcome.core_lifecycle,
        Some(PiSourceLifecycle::Replace)
    );

    let old_path = temp.path().join("old.jsonl");
    let relocated_path = temp.path().join("relocated.jsonl");
    write_lines(&old_path, &[header("pi-relocate")]);
    let relocation_fresh = scan(
        &old_path,
        PiNativeProfile::CoreOnly,
        PiNativeResume::default(),
    )
    .unwrap();
    let relocation_checkpoint = relocation_fresh.outcome.core_checkpoint.unwrap();
    fs::rename(&old_path, &relocated_path).unwrap();
    let relocated = scan(
        &relocated_path,
        PiNativeProfile::CoreOnly,
        PiNativeResume {
            core: Some(relocation_checkpoint.clone()),
            output: None,
        },
    )
    .unwrap();
    assert_eq!(
        relocated.outcome.core_lifecycle,
        Some(PiSourceLifecycle::Relocate)
    );
    assert_eq!(relocated.core.len(), 1);
    assert_ne!(
        relocated
            .outcome
            .core_checkpoint
            .as_ref()
            .unwrap()
            .route_sha256,
        relocation_checkpoint.route_sha256
    );

    let original_path = temp.path().join("original.jsonl");
    let copied_path = temp.path().join("copied.jsonl");
    write_lines(&original_path, &[header("pi-copy")]);
    let copy_fresh = scan(
        &original_path,
        PiNativeProfile::CoreOnly,
        PiNativeResume::default(),
    )
    .unwrap();
    let copy_checkpoint = copy_fresh.outcome.core_checkpoint.unwrap();
    fs::copy(&original_path, &copied_path).unwrap();
    let copied = scan(
        &copied_path,
        PiNativeProfile::CoreOnly,
        PiNativeResume {
            core: Some(copy_checkpoint.clone()),
            output: None,
        },
    )
    .unwrap();
    assert_eq!(copied.outcome.core_lifecycle, Some(PiSourceLifecycle::Copy));
    fs::remove_file(&copied_path).unwrap();
    let options = PiNativeScanOptions {
        resume: PiNativeResume {
            core: Some(copy_checkpoint),
            output: None,
        },
        ..PiNativeScanOptions::new(context(&copied_path), PiNativeProfile::CoreOnly)
    };
    assert!(matches!(
        open_pi_native_session(&copied_path, options).unwrap(),
        PiNativeOpenOutcome::Deleted
    ));
}

#[test]
fn incomplete_corrupt_and_append_completion_preserve_exact_prefix_authority() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("incomplete.jsonl");
    let complete = format!(
        "{}\n{}\n",
        header("pi-incomplete"),
        message("complete", "user", "complete body", 1)
    );
    let partial = r#"{"type":"message","id":"tail","timestamp":"2026-07-25T12:00:02Z","message":{"role":"assistant","content":"incom"#;
    fs::write(&path, format!("{complete}{partial}")).unwrap();
    let first = scan(&path, PiNativeProfile::CoreOnly, PiNativeResume::default()).unwrap();
    assert!(!first.outcome.complete);
    assert!(first.outcome.stats.incomplete_tail_bytes > 0);
    let checkpoint = first.outcome.core_checkpoint.clone().unwrap();
    assert_eq!(
        checkpoint.complete_offset,
        u64::try_from(complete.len()).unwrap()
    );
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    writeln!(file, "plete\"}}}}").unwrap();
    file.sync_all().unwrap();
    let completed = scan(
        &path,
        PiNativeProfile::CoreOnly,
        PiNativeResume {
            core: Some(checkpoint),
            output: None,
        },
    )
    .unwrap();
    assert!(completed.outcome.complete);
    assert_eq!(completed.outcome.stats.semantic_records_parsed, 1);

    let corrupt_path = temp.path().join("corrupt.jsonl");
    fs::write(
        &corrupt_path,
        format!("{}\n{{\"type\":\"message\"\n", header("pi-corrupt")),
    )
    .unwrap();
    let corrupt = scan(
        &corrupt_path,
        PiNativeProfile::CoreOnly,
        PiNativeResume::default(),
    )
    .unwrap();
    assert!(corrupt.outcome.complete);
    assert_eq!(corrupt.outcome.stats.malformed_records, 1);
    assert!(flattened_units(&corrupt).iter().any(|unit| matches!(
        unit,
        PiNativeCoreUnit::Rejection(PiNativeRejection {
            kind: PiNativeRejectionKind::MalformedJson,
            ..
        })
    )));
}

#[test]
fn checkpoints_are_content_free_bounded_and_append_parses_only_delta() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("checkpoint.jsonl");
    let secret = "checkpoint-must-not-retain-transcript-content";
    let mut lines = vec![header("secret-session-id")];
    for index in 0..100_u64 {
        lines.push(message(
            &format!("id-{index}"),
            "user",
            &format!("{secret}-{index}"),
            (index % 50) + 1,
        ));
    }
    write_lines(&path, &lines);
    let first = scan(&path, PiNativeProfile::CoreOnly, PiNativeResume::default()).unwrap();
    let checkpoint = first.outcome.core_checkpoint.unwrap();
    let encoded = serde_json::to_vec(&checkpoint).unwrap();
    let text = String::from_utf8_lossy(&encoded);
    assert!(encoded.len() < 1_024);
    assert!(!text.contains(secret));
    assert!(!text.contains("secret-session-id"));
    assert!(!text.contains("checkpoint.jsonl"));

    let previous_len = fs::metadata(&path).unwrap().len();
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    writeln!(
        file,
        "{}",
        message("delta", "assistant", "only delta parses", 2)
    )
    .unwrap();
    file.sync_all().unwrap();
    let appended = scan(
        &path,
        PiNativeProfile::CoreOnly,
        PiNativeResume {
            core: Some(checkpoint),
            output: None,
        },
    )
    .unwrap();
    assert_eq!(appended.outcome.stats.semantic_records_parsed, 1);
    assert_eq!(appended.outcome.stats.prefix_bytes_hashed, previous_len);
    assert_eq!(appended.outcome.stats.source_file_opens, 1);
}

#[test]
fn source_mutation_is_fenced_before_first_page_exposure() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("mutation.jsonl");
    write_lines(
        &path,
        &[
            header("pi-mutation"),
            message("event", "user", "must stay fenced", 1),
        ],
    );
    let options = PiNativeScanOptions::new(context(&path), PiNativeProfile::CoreOnly);
    let PiNativeOpenOutcome::Ready(mut scanner) = open_pi_native_session(&path, options).unwrap()
    else {
        panic!("expected scanner");
    };
    let mutation_path = path.clone();
    scanner.set_before_exposure(move || {
        let mut file = OpenOptions::new()
            .append(true)
            .open(&mutation_path)
            .unwrap();
        file.write_all(b" ").unwrap();
        file.sync_all().unwrap();
    });
    assert!(matches!(
        scanner.next_page(),
        Err(PiNativePathError::SourceChanged)
    ));
}

#[test]
fn provider_private_discovery_is_sorted_bounded_and_rejects_symlinks() {
    let temp = tempdir().unwrap();
    let nested = temp.path().join("2026").join("07");
    fs::create_dir_all(&nested).unwrap();
    fs::write(nested.join("b.jsonl"), b"").unwrap();
    fs::write(nested.join("a.jsonl"), b"").unwrap();
    fs::write(nested.join("ignored.txt"), b"").unwrap();
    let discovery = discover_pi_sessions(temp.path()).unwrap();
    assert_eq!(discovery.sessions.len(), 2);
    assert!(discovery.sessions[0].ends_with("a.jsonl"));
    assert!(discovery.sessions[1].ends_with("b.jsonl"));
    assert_eq!(discovery.stats.selected_files, 2);
    assert!(discovery.stats.visited_entries >= 5);

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&nested, temp.path().join("linked")).unwrap();
        assert!(matches!(
            discover_pi_sessions(temp.path()),
            Err(PiNativePathError::InvalidSource { .. })
        ));
    }
}

#[test]
fn retained_discovery_rejects_same_path_root_replacement() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let source = root.join("2026/07/session.jsonl");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(&source, b"").unwrap();
    let discovery = discover_pi_sessions(&root).unwrap();

    let displaced = temp.path().join("sessions-displaced");
    fs::rename(&root, &displaced).unwrap();
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(&source, b"").unwrap();

    assert!(discovery.rediscover().is_err());
}
