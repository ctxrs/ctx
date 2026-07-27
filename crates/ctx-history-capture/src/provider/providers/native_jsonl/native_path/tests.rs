use std::{
    collections::BTreeSet,
    fs,
    sync::atomic::{AtomicUsize, Ordering},
};

use chrono::DateTime;
use ctx_history_core::CaptureProvider;
use serde_json::json;

use super::open_direct_jsonl_pages;
use crate::{
    test_support_paths::tempdir, CaptureError, OutputSourceIdentity, ProOutputMaterializationPage,
    ProOutputPageResult, ProOutputProgress, ProOutputSink, ProOutputSinkError,
};

#[test]
fn direct_jsonl_nativepath_withholds_incomplete_tail() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("transcript").join("events.jsonl");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        concat!(
            "{\"type\":\"session.start\",\"data\":{\"sessionId\":\"copilot-session\"}}\n",
            "{\"type\":\"assistant.message\",\"data\":{\"content\":\"complete\"}}\n",
            "{\"type\":\"assistant.message\",\"data\":{\"content\":\"incomplete\"}}"
        ),
    )
    .unwrap();
    let mut reader = open_direct_jsonl_pages(
        CaptureProvider::CopilotCli,
        crate::COPILOT_CLI_SOURCE_FORMAT,
        &path,
        Some(temp.path().to_path_buf()),
        DateTime::from_timestamp(0, 0).unwrap(),
        false,
        None,
    )
    .unwrap();
    let page = reader.next_page().unwrap().unwrap();
    assert_eq!(page.events.len(), 2);
    assert!(!page.terminal);
    assert!(reader.next_page().unwrap().is_none());
    assert!(!reader.outcome().unwrap().checkpoint.terminal);
    assert_eq!(reader.outcome().unwrap().checkpoint.next_raw_ordinal, 2);
}

#[test]
fn qoder_nonzero_result_subrecords_keep_outer_identity_separate_from_calls() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("transcript/qoder.jsonl");
    write_lines(
        &path,
        &[
            json!({
                "type": "session_meta",
                "sessionId": "qoder-subrecords",
                "uuid": "qoder-header",
                "timestamp": "2026-07-25T12:00:00Z",
                "cwd": "/workspace/qoder",
                "data": {"meta_type": "session_info"}
            }),
            json!({
                "type": "user",
                "sessionId": "qoder-subrecords",
                "uuid": "outer-result-record",
                "timestamp": "2026-07-25T12:00:01Z",
                "message": {
                    "role": "user",
                    "content": [
                        {
                            "type": "tool_result",
                            "tool_use_id": "call-zero",
                            "content": "first-output",
                            "is_error": false
                        },
                        {
                            "type": "tool_result",
                            "tool_use_id": "call-one",
                            "content": "second-output",
                            "is_error": false
                        }
                    ]
                }
            }),
        ],
    );
    let mut reader = open_direct_jsonl_pages(
        CaptureProvider::Qoder,
        crate::QODER_SOURCE_FORMAT,
        &path,
        Some(temp.path().to_path_buf()),
        DateTime::from_timestamp(0, 0).unwrap(),
        true,
        None,
    )
    .unwrap();
    let page = reader.next_page().unwrap().unwrap();
    assert_eq!(page.outputs.len(), 2);
    assert_eq!(
        page.outputs
            .iter()
            .map(|output| output.sub_ordinal)
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert!(page
        .outputs
        .iter()
        .all(|output| output.native_record_id.as_deref() == Some("outer-result-record")));
    assert_eq!(page.outputs[0].call_id.as_deref(), Some("call-zero"));
    assert_eq!(page.outputs[1].call_id.as_deref(), Some("call-one"));
}

#[test]
fn tabnine_failed_result_keeps_only_typed_core_metadata() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("transcript/tabnine.jsonl");
    write_lines(
        &path,
        &[
            json!({
                "sessionId": "tabnine-private-failure",
                "projectHash": "project",
                "startTime": "2026-07-25T12:00:00Z",
                "kind": "main"
            }),
            json!({
                "id": "tabnine-failed-result",
                "timestamp": "2026-07-25T12:00:01Z",
                "type": "tabnine",
                "toolCalls": [{
                    "id": "call-private",
                    "name": "exec",
                    "result": "TABNINE_PRIVATE_FAILURE_BODY",
                    "isError": true,
                    "exitCode": 9,
                    "durationMs": 17
                }]
            }),
        ],
    );
    let mut reader = open_direct_jsonl_pages(
        CaptureProvider::Tabnine,
        crate::TABNINE_CLI_SOURCE_FORMAT,
        &path,
        Some(temp.path().to_path_buf()),
        DateTime::from_timestamp(0, 0).unwrap(),
        false,
        None,
    )
    .unwrap();
    let page = reader.next_page().unwrap().unwrap();
    let failure = page
        .events
        .iter()
        .find(|event| event.sub_ordinal == 0 && event.payload.get("call_id").is_some())
        .unwrap();
    let rendered = serde_json::to_string(&failure.payload).unwrap();
    assert_eq!(failure.payload["call_id"], "call-private");
    assert_eq!(failure.payload["exit_code"], 9);
    assert_eq!(failure.payload["duration_ms"], 17);
    assert!(failure.payload.get("result_content_ref").is_none());
    assert!(!rendered.contains("TABNINE_PRIVATE_FAILURE_BODY"));
    assert!(!rendered.contains("output_preview"));
}

#[test]
fn copilot_omitted_physical_records_still_cap_each_page_at_64_units() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("transcript/events.jsonl");
    let mut records = vec![json!({
        "id": "copilot-unit-header",
        "timestamp": "2026-07-25T12:00:00Z",
        "type": "session.start",
        "data": {
            "sessionId": "copilot-unit-cap",
            "startTime": "2026-07-25T12:00:00Z",
            "context": {"cwd": "/workspace/copilot"}
        }
    })];
    records.extend((0..65).map(|index| {
        json!({
            "id": format!("copilot-result-{index}"),
            "timestamp": "2026-07-25T12:00:01Z",
            "type": "tool.execution_complete",
            "data": {
                "toolCallId": format!("call-{index}"),
                "success": true,
                "result": {"content": format!("omitted-output-{index}")}
            }
        })
    }));
    write_lines(&path, &records);
    let mut reader = open_direct_jsonl_pages(
        CaptureProvider::CopilotCli,
        crate::COPILOT_CLI_SOURCE_FORMAT,
        &path,
        Some(temp.path().to_path_buf()),
        DateTime::from_timestamp(0, 0).unwrap(),
        false,
        None,
    )
    .unwrap();
    let first = reader.next_page().unwrap().unwrap();
    assert_eq!(first.logical_units, 64);
    assert_eq!(first.next_checkpoint.next_raw_ordinal, 64);
    let second = reader.next_page().unwrap().unwrap();
    assert_eq!(second.logical_units, 2);
    assert_eq!(second.next_checkpoint.next_raw_ordinal, 66);
    assert!(second.terminal);
    assert!(reader.next_page().unwrap().is_none());
}

#[test]
fn copilot_output_pages_roll_over_before_the_8_mib_encoded_bound() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("transcript/events.jsonl");
    let output = "x".repeat(5 * 1024 * 1024);
    let records = [
        json!({
            "id": "copilot-byte-header",
            "timestamp": "2026-07-25T12:00:00Z",
            "type": "session.start",
            "data": {
                "sessionId": "copilot-byte-cap",
                "startTime": "2026-07-25T12:00:00Z",
                "context": {"cwd": "/workspace/copilot"}
            }
        }),
        json!({
            "id": "copilot-byte-result-1",
            "timestamp": "2026-07-25T12:00:01Z",
            "type": "tool.execution_complete",
            "data": {
                "toolCallId": "call-byte-1",
                "success": true,
                "result": {"content": output.clone()}
            }
        }),
        json!({
            "id": "copilot-byte-result-2",
            "timestamp": "2026-07-25T12:00:02Z",
            "type": "tool.execution_complete",
            "data": {
                "toolCallId": "call-byte-2",
                "success": true,
                "result": {"content": output}
            }
        }),
    ];
    write_lines(&path, &records);
    let mut reader = open_direct_jsonl_pages(
        CaptureProvider::CopilotCli,
        crate::COPILOT_CLI_SOURCE_FORMAT,
        &path,
        Some(temp.path().to_path_buf()),
        DateTime::from_timestamp(0, 0).unwrap(),
        true,
        None,
    )
    .unwrap();

    let first = reader.next_page().unwrap().unwrap();
    assert_eq!(first.next_checkpoint.next_raw_ordinal, 2);
    assert_eq!(first.outputs.len(), 1);
    assert!(first.conservative_serialized_bytes <= super::reader::DIRECT_JSONL_PAGE_MAX_BYTES);
    let second = reader.next_page().unwrap().unwrap();
    assert_eq!(second.next_checkpoint.next_raw_ordinal, 3);
    assert_eq!(second.outputs.len(), 1);
    assert!(second.conservative_serialized_bytes <= super::reader::DIRECT_JSONL_PAGE_MAX_BYTES);
    assert!(second.terminal);
    assert!(reader.next_page().unwrap().is_none());
}

#[test]
fn selected_output_source_failures_continue_but_system_failures_abort() {
    let paths = ["/tmp/a.jsonl", "/tmp/b.jsonl"]
        .into_iter()
        .map(std::path::PathBuf::from)
        .collect::<BTreeSet<_>>();
    let sink = IsolationSink::default();
    let visited = AtomicUsize::new(0);
    super::driver::replay_selected_output_sources(&paths, &sink, "test_source_failure", |path| {
        visited.fetch_add(1, Ordering::SeqCst);
        if path.ends_with("a.jsonl") {
            Err(CaptureError::SourceChangedDuringCapture)
        } else {
            Ok(())
        }
    })
    .unwrap();
    assert_eq!(visited.load(Ordering::SeqCst), 2);
    assert_eq!(sink.behind.load(Ordering::SeqCst), 1);

    let visited = AtomicUsize::new(0);
    let error =
        super::driver::replay_selected_output_sources(&paths, &sink, "test_system_failure", |_| {
            visited.fetch_add(1, Ordering::SeqCst);
            Err(CaptureError::SystemInvariant("injected invariant failure"))
        })
        .unwrap_err();
    assert!(matches!(error, CaptureError::SystemInvariant(_)));
    assert_eq!(visited.load(Ordering::SeqCst), 1);
}

#[derive(Default)]
struct IsolationSink {
    behind: AtomicUsize,
}

impl ProOutputSink for IsolationSink {
    fn inventory_generation(&self) -> u64 {
        1
    }

    fn materializer_revision(&self) -> &str {
        "isolation-test-v1"
    }

    fn observe_source(
        &self,
        _source: &OutputSourceIdentity,
    ) -> std::result::Result<Option<ProOutputProgress>, ProOutputSinkError> {
        Ok(None)
    }

    fn materialize_page(
        &self,
        _page: ProOutputMaterializationPage,
    ) -> std::result::Result<ProOutputPageResult, ProOutputSinkError> {
        Err(ProOutputSinkError::new(
            "unexpected_materialization",
            "isolation helper must not materialize",
        ))
    }

    fn mark_behind(&self, _error: ProOutputSinkError) {
        self.behind.fetch_add(1, Ordering::SeqCst);
    }
}

fn write_lines(path: &std::path::Path, records: &[serde_json::Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = Vec::new();
    for record in records {
        serde_json::to_writer(&mut bytes, record).unwrap();
        bytes.push(b'\n');
    }
    fs::write(path, bytes).unwrap();
}
