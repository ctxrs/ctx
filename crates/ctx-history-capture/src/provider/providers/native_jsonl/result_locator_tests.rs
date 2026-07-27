use std::path::PathBuf;

use ctx_history_core::{CaptureProvider, ContentRef, EventType, ProviderEventEnvelope};
use serde_json::{json, Value};

use crate::captured_batch::{
    CapturedRecord, CapturedRecordPayload, NativeLocator, ProviderRecordKind,
};
use crate::complete_content::{
    CompleteContentBodyDigest, VerifiedContentLocatorsV1, VerifiedContentRole,
    VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
};
use crate::provider::importer::{
    CapturedBatchProjector, ProviderProjectionOutput, ProviderProjectionResult,
};
use crate::{ProviderAdapterContext, ProviderNormalizationResult};

use super::dialect::native_jsonl_record_kind;
use super::normalization::native_jsonl_event;
use super::projector::{NativeJsonlCapturedBatchProjector, NATIVE_JSONL_LOCATOR_KIND};
use super::result_content::{
    extract_native_jsonl_result_content, native_jsonl_result_content_profile,
};

struct NativeResultCase {
    provider: CaptureProvider,
    source_format: &'static str,
    header: Option<Value>,
    result: Value,
    expected_content: &'static str,
    expected_native_id: &'static str,
}

#[derive(Default)]
struct TestProjectionOutput {
    normalizations: Vec<ProviderNormalizationResult>,
}

impl ProviderProjectionOutput for TestProjectionOutput {
    fn emit_normalization(
        &mut self,
        normalization: ProviderNormalizationResult,
    ) -> ProviderProjectionResult<()> {
        self.normalizations.push(normalization);
        Ok(())
    }

    fn reject_record(&mut self, line_number: usize, reason: String) {
        panic!("unexpected rejection at line {line_number}: {reason}");
    }
}

fn native_record(
    provider: CaptureProvider,
    source_format: &str,
    ordinal: u64,
    value: &Value,
) -> CapturedRecord {
    let payload = serde_json::to_vec(value).unwrap();
    let start = ordinal.saturating_mul(4_096);
    let end = start
        .saturating_add(u64::try_from(payload.len()).unwrap())
        .saturating_add(1);
    native_record_with_range(provider, source_format, ordinal, payload, start, end)
}

fn native_record_with_range(
    provider: CaptureProvider,
    source_format: &str,
    ordinal: u64,
    payload: Vec<u8>,
    start: u64,
    end: u64,
) -> CapturedRecord {
    let source_item = b"native-result-test-source";
    let mut locator = Vec::new();
    locator.extend_from_slice(&u32::try_from(source_item.len()).unwrap().to_be_bytes());
    locator.extend_from_slice(source_item);
    locator.extend_from_slice(&start.to_be_bytes());
    locator.extend_from_slice(&end.to_be_bytes());
    CapturedRecord::content(
        ordinal,
        NativeLocator::new(NATIVE_JSONL_LOCATOR_KIND, locator).unwrap(),
        ProviderRecordKind::new(native_jsonl_record_kind(provider, source_format)).unwrap(),
        payload,
    )
    .unwrap()
}

fn assert_no_result_reference_or_locator(event: &ProviderEventEnvelope) {
    assert!(event
        .payload
        .get("result_content_ref")
        .is_none_or(Value::is_null));
    assert!(event
        .metadata
        .get(VERIFIED_CONTENT_LOCATORS_METADATA_KEY)
        .is_none());
}

fn project_result(case: &NativeResultCase) -> (ProviderEventEnvelope, CapturedRecord) {
    let path = PathBuf::from(format!(
        "/tmp/native-result-{}.jsonl",
        case.provider.as_str()
    ));
    let mut projector = NativeJsonlCapturedBatchProjector::fresh(
        case.provider,
        case.source_format,
        &path,
        ProviderAdapterContext {
            machine_id: "native-result-test-machine".to_owned(),
            source_path: Some(path.clone()),
            source_root: Some(path.clone()),
            imported_at: "2026-07-22T12:00:00Z".parse().unwrap(),
        },
    );
    let mut ordinal = 0;
    if let Some(header) = case.header.as_ref() {
        let mut output = TestProjectionOutput::default();
        projector
            .project_record(
                &native_record(case.provider, case.source_format, ordinal, header),
                &mut output,
            )
            .unwrap();
        ordinal += 1;
    }
    let record = native_record(case.provider, case.source_format, ordinal, &case.result);
    let mut output = TestProjectionOutput::default();
    projector.project_record(&record, &mut output).unwrap();
    let event = output
        .normalizations
        .into_iter()
        .flat_map(|normalization| normalization.captures)
        .find_map(|(_, capture)| capture.event)
        .expect("result record produces one event");
    (event, record)
}

fn cases() -> Vec<NativeResultCase> {
    vec![
        NativeResultCase {
            provider: CaptureProvider::Gemini,
            source_format: crate::GEMINI_CLI_SOURCE_FORMAT,
            header: Some(
                json!({"sessionId":"gemini-session","startTime":"2026-07-22T12:00:00Z","directories":["/workspace"]}),
            ),
            result: json!({"id":"gemini-result","timestamp":"2026-07-22T12:00:01Z","type":"gemini","toolCalls":[{"result":{"content":"gemini-result-canary"}}]}),
            expected_content: "gemini-result-canary",
            expected_native_id: "gemini-result",
        },
        NativeResultCase {
            provider: CaptureProvider::Tabnine,
            source_format: crate::TABNINE_CLI_SOURCE_FORMAT,
            header: Some(
                json!({"sessionId":"tabnine-session","startTime":"2026-07-22T12:00:00Z","directories":["/workspace"]}),
            ),
            result: json!({"id":"tabnine-result","timestamp":"2026-07-22T12:00:01Z","type":"tabnine","toolCalls":[{"result":"tabnine-result-canary"}]}),
            expected_content: "tabnine-result-canary",
            expected_native_id: "tabnine-result",
        },
        NativeResultCase {
            provider: CaptureProvider::FactoryAiDroid,
            source_format: crate::FACTORY_DROID_SOURCE_FORMAT,
            header: Some(
                json!({"type":"session_start","id":"droid-session","timestamp":"2026-07-22T12:00:00Z","cwd":"/workspace"}),
            ),
            result: json!({"type":"message","id":"droid-result","timestamp":"2026-07-22T12:00:01Z","message":{"role":"tool","content":[{"type":"tool_result","content":"droid-result-canary"}]}}),
            expected_content: "droid-result-canary",
            expected_native_id: "droid-result",
        },
        NativeResultCase {
            provider: CaptureProvider::Cursor,
            source_format: crate::CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
            header: None,
            result: json!({"id":"cursor-result","timestamp":"2026-07-22T12:00:01Z","role":"user","message":{"role":"user","content":[{"type":"tool_result","content":"cursor-result-canary"}]}}),
            expected_content: "cursor-result-canary",
            expected_native_id: "cursor-result",
        },
        NativeResultCase {
            provider: CaptureProvider::Qoder,
            source_format: crate::QODER_SOURCE_FORMAT,
            header: None,
            result: json!({"type":"user","sessionId":"qoder-session","uuid":"qoder-result","timestamp":"2026-07-22T12:00:01Z","message":{"role":"user","content":[{"type":"tool_result","content":"lower-priority"}]},"toolUseResult":"qoder-result-canary"}),
            expected_content: "qoder-result-canary",
            expected_native_id: "qoder-result",
        },
        NativeResultCase {
            provider: CaptureProvider::CopilotCli,
            source_format: crate::COPILOT_CLI_SOURCE_FORMAT,
            header: Some(
                json!({"id":"copilot-start","timestamp":"2026-07-22T12:00:00Z","type":"session.start","data":{"sessionId":"copilot-session","startTime":"2026-07-22T12:00:00Z","context":{"cwd":"/workspace"}}}),
            ),
            result: json!({"id":"copilot-result","timestamp":"2026-07-22T12:00:01Z","type":"tool.execution_complete","data":{"result":{"content":"copilot-result-canary"}}}),
            expected_content: "copilot-result-canary",
            expected_native_id: "copilot-result",
        },
        NativeResultCase {
            provider: CaptureProvider::QwenCode,
            source_format: crate::QWEN_CODE_SOURCE_FORMAT,
            header: None,
            result: json!({"uuid":"qwen-result","sessionId":"qwen-session","timestamp":"2026-07-22T12:00:01Z","type":"tool_result","message":{"role":"tool","content":[{"type":"tool_result","content":"qwen-result-canary"}]},"toolCallResult":{"output":"lower-priority"}}),
            expected_content: "qwen-result-canary",
            expected_native_id: "qwen-result",
        },
    ]
}

#[test]
fn native_import_attaches_verified_result_locators_without_persisting_bodies() {
    for case in cases() {
        let (event, record) = project_result(&case);
        assert_eq!(
            event.event_type,
            EventType::ToolOutput,
            "{:?}",
            case.provider
        );
        let content_ref =
            serde_json::from_value::<ContentRef>(event.payload["result_content_ref"].clone())
                .unwrap();
        assert!(content_ref.verifies(case.expected_content.as_bytes()));
        let locators = VerifiedContentLocatorsV1::from_metadata_value(
            &event.metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY],
        )
        .unwrap();
        let locator = locators.locator(VerifiedContentRole::ResultBody).unwrap();
        assert_eq!(locator.content_ref(), &content_ref);
        assert_eq!(locator.native_record_id(), case.expected_native_id);
        assert_eq!(
            locator.content_profile(),
            native_jsonl_result_content_profile(case.provider).unwrap()
        );
        let CapturedRecordPayload::NativeBytes(record_bytes) = record.payload() else {
            panic!("native test record contains bytes");
        };
        assert_eq!(
            locator.record_sha256(),
            &CompleteContentBodyDigest::from_text(std::str::from_utf8(record_bytes).unwrap())
        );
        let reopened = extract_native_jsonl_result_content(locator.content_profile(), &case.result)
            .unwrap()
            .unwrap();
        assert_eq!(reopened, case.expected_content);
        assert!(!event.payload.to_string().contains(case.expected_content));
    }
}

#[test]
fn absent_ambiguous_and_redacted_results_emit_no_reference_or_locator() {
    let mut base = cases().remove(0);
    for result in [
        json!({"id":"absent","type":"gemini","toolCalls":[{"result":{"unknown":"absent-secret"}}]}),
        json!({"id":"ambiguous","type":"gemini","toolCalls":[{"result":"ambiguous-secret-one"},{"result":"ambiguous-secret-two"}]}),
        json!({"id":"redacted","type":"gemini","toolCalls":[{"result":{"redacted":true,"content":"redacted-secret"}}]}),
    ] {
        base.result = result;
        let (event, _) = project_result(&base);
        assert_eq!(event.event_type, EventType::ToolOutput);
        assert_no_result_reference_or_locator(&event);
        let rendered = event.payload.to_string();
        assert!(!rendered.contains("absent-secret"));
        assert!(!rendered.contains("ambiguous-secret"));
        assert!(!rendered.contains("redacted-secret"));
    }
}

#[test]
fn invalid_native_ids_publish_neither_reference_nor_locator() {
    let mut base = cases().remove(0);
    for native_id in [String::new(), "x".repeat(1_025), "control\nid".to_owned()] {
        base.result["id"] = Value::String(native_id);
        let (event, _) = project_result(&base);
        assert_eq!(event.event_type, EventType::ToolOutput);
        assert_no_result_reference_or_locator(&event);
    }
}

#[test]
fn profile_and_address_validation_abstentions_are_atomic() {
    let case = cases().remove(0);
    let occurred_at = "2026-07-22T12:00:01Z".parse().unwrap();
    let record = native_record(case.provider, case.source_format, 0, &case.result);

    let mut profile_mismatch = native_jsonl_event(
        case.provider,
        case.source_format,
        &case.result,
        1,
        occurred_at,
    )
    .unwrap();
    let content_ref = ContentRef::from_bytes(case.expected_content.as_bytes()).unwrap();
    assert_no_result_reference_or_locator(&profile_mismatch);
    crate::complete_content::jsonl::attach_native_jsonl_result_content_locator(
        &mut profile_mismatch,
        case.provider,
        "unregistered-result-source-format",
        &case.result,
        &record,
        1,
        Some(&content_ref),
    )
    .unwrap();
    assert_no_result_reference_or_locator(&profile_mismatch);

    let payload = serde_json::to_vec(&case.result).unwrap();
    let invalid_range =
        native_record_with_range(case.provider, case.source_format, 0, payload, 7, 7);
    let mut address_mismatch = native_jsonl_event(
        case.provider,
        case.source_format,
        &case.result,
        1,
        occurred_at,
    )
    .unwrap();
    assert_no_result_reference_or_locator(&address_mismatch);
    crate::complete_content::jsonl::attach_native_jsonl_result_content_locator(
        &mut address_mismatch,
        case.provider,
        case.source_format,
        &case.result,
        &invalid_range,
        1,
        Some(&content_ref),
    )
    .unwrap();
    assert_no_result_reference_or_locator(&address_mismatch);
}
