use std::{
    fs::{self, File},
    io::{BufWriter, Write},
    path::Path,
};

use ctx_history_core::{CaptureProvider, EntityTimestamps, EventType, SyncCursor};
use ctx_history_store::Store;
use serde_json::{json, Value};

use crate::captured_batch::{
    CapturedRecordPayload, ProviderRecordKind, SourceObservation,
    CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES, CAPTURE_BATCH_MAX_PAYLOAD_BYTES,
    CAPTURE_BATCH_MAX_RECORDS,
};
use crate::provider::importer::{
    captured_batch_cursor_stream, provider_path_identity, provider_source_cursor_stream_for_path,
    BoundedParserCheckpoint, CapturedBatchCursorFinish, CapturedBatchProjector,
    CertifiedProviderCursor, ProviderProjectionOutput, ProviderProjectionResult,
};
use crate::test_support_paths::tempdir;
use crate::{
    CaptureError, NormalizedProviderImportOptions, ProviderAdapterContext,
    ProviderNormalizationResult, MAX_PROVIDER_JSONL_LINE_BYTES,
};

use super::dialect::{
    task_json_decode_locator, task_json_decode_position, task_json_native_position,
    TaskJsonMessagePhase, TaskJsonRecordClass, TaskJsonStreamPosition, TASK_JSON_CAPTURE_REVISION,
    TASK_JSON_POLICY_REVISION, TASK_JSON_RECORD_KIND,
};
use super::projector::{
    task_json_root_history_fragment, task_json_session_state, TaskJsonCapturedBatchProjector,
    TaskJsonParserCheckpoint,
};
use super::scanner::{TaskJsonBatchProducer, TaskJsonMessageSource};
use super::source::{
    task_json_root_history_candidate_paths, visit_task_json_dirs, TaskJsonTaskObservation,
};
use super::{
    import_task_json_history_batched, task_json_event, task_json_provider,
    task_json_result_content, TaskJsonEventInput, TaskJsonProviderSpec,
};

mod capture;
mod provider;

#[derive(Default)]
struct CollectingProjectionOutput {
    normalizations: Vec<ProviderNormalizationResult>,
    rejections: Vec<(usize, String)>,
}

impl ProviderProjectionOutput for CollectingProjectionOutput {
    fn emit_normalization(
        &mut self,
        normalization: ProviderNormalizationResult,
    ) -> ProviderProjectionResult<()> {
        self.normalizations.push(normalization);
        Ok(())
    }

    fn reject_record(&mut self, line_number: usize, reason: String) {
        self.rejections.push((line_number, reason));
    }
}

fn test_context(path: &Path) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "task-json-batch-test".to_owned(),
        source_path: Some(path.to_path_buf()),
        source_root: None,
        imported_at: "2026-07-18T12:00:00Z".parse().unwrap(),
    }
}

fn test_source(
    observation: &TaskJsonTaskObservation,
    spec: TaskJsonProviderSpec,
) -> SourceObservation {
    SourceObservation::new(
        spec.provider,
        spec.source_format,
        "task-json-test-source",
        observation.source_revision(spec),
        "provider:task-json:test",
        TASK_JSON_CAPTURE_REVISION,
        TASK_JSON_POLICY_REVISION,
        None,
    )
    .unwrap()
}

fn test_message_sources(
    observation: &TaskJsonTaskObservation,
    spec: TaskJsonProviderSpec,
) -> Vec<TaskJsonMessageSource> {
    [
        TaskJsonMessagePhase::Api,
        TaskJsonMessagePhase::Ui,
        TaskJsonMessagePhase::Fallback,
    ]
    .into_iter()
    .filter_map(|phase| {
        let observed = observation.message_file(spec, phase)?;
        Some(TaskJsonMessageSource {
            phase,
            path: observed.path.clone(),
            frozen: observed.frozen.clone()?,
        })
    })
    .collect()
}

fn write_messages(path: &Path, count: usize, text_bytes: usize) {
    let messages = (0..count)
        .map(|index| {
            json!({
                "id": format!("message-{index}"),
                "role": "user",
                "content": "x".repeat(text_bytes),
            })
        })
        .collect::<Vec<_>>();
    fs::write(path, serde_json::to_vec(&messages).unwrap()).unwrap();
}

fn write_exact_limit_message_array(path: &Path, array_suffix: &[u8]) {
    const ITEM_PREFIX: &[u8] = br#"{"id":"exact-limit","role":"user","content":""#;
    const ITEM_SUFFIX: &[u8] = br#""}"#;
    let content_bytes = CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES
        .checked_sub(ITEM_PREFIX.len() + ITEM_SUFFIX.len())
        .unwrap();
    let mut writer = BufWriter::new(File::create(path).unwrap());
    writer.write_all(b"[").unwrap();
    writer.write_all(ITEM_PREFIX).unwrap();
    writer.write_all(&vec![b'x'; content_bytes]).unwrap();
    writer.write_all(ITEM_SUFFIX).unwrap();
    writer.write_all(array_suffix).unwrap();
    writer.flush().unwrap();
    assert_eq!(
        fs::metadata(path).unwrap().len(),
        u64::try_from(1 + CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES + array_suffix.len()).unwrap()
    );
}

fn write_large_message_array(path: &Path, count: usize, padding_bytes: usize) {
    let mut writer = BufWriter::new(File::create(path).unwrap());
    let padding = "x".repeat(padding_bytes);
    writer.write_all(b"[").unwrap();
    for index in 0..count {
        if index != 0 {
            writer.write_all(b",").unwrap();
        }
        let content = if index + 1 == count {
            "uniquetailsentinel"
        } else {
            "bounded message"
        };
        serde_json::to_writer(
            &mut writer,
            &json!({
                "id": format!("large-message-{index}"),
                "role": "user",
                "content": content,
                "padding": padding,
            }),
        )
        .unwrap();
    }
    writer.write_all(b"]").unwrap();
    writer.flush().unwrap();
}

fn write_large_root_history_array(
    path: &Path,
    count: usize,
    padding_bytes: usize,
    target_id: &str,
) {
    let mut writer = BufWriter::new(File::create(path).unwrap());
    let padding = "x".repeat(padding_bytes);
    writer.write_all(b"[").unwrap();
    for index in 0..count {
        if index != 0 {
            writer.write_all(b",").unwrap();
        }
        let is_target = index + 1 == count;
        serde_json::to_writer(
            &mut writer,
            &json!({
                "id": if is_target {
                    target_id.to_owned()
                } else {
                    format!("other-task-{index}")
                },
                "task": if is_target {
                    "large root history fallback sentinel"
                } else {
                    "other task"
                },
                "padding": padding,
            }),
        )
        .unwrap();
    }
    writer.write_all(b"]").unwrap();
    writer.flush().unwrap();
}
