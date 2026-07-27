use super::*;

#[derive(Default)]
struct CountingProjectionOutput {
    captures: usize,
    existing_session_events: usize,
    file_touches: usize,
    explicit_file_touch_declarations: usize,
    touch_before_capture: bool,
    first_touch_index: Option<u64>,
    last_touch_index: Option<u64>,
    rejections: Vec<(usize, String)>,
}

impl ProviderProjectionOutput for CountingProjectionOutput {
    fn emit_normalization(
        &mut self,
        normalization: ProviderNormalizationResult,
    ) -> ProviderProjectionResult<()> {
        self.captures += normalization.captures.len();
        for (_, touch) in normalization.files_touched {
            self.touch_before_capture |= self.captures == 0;
            self.first_touch_index
                .get_or_insert(touch.provider_touch_index);
            self.last_touch_index = Some(touch.provider_touch_index);
            self.file_touches += 1;
        }
        Ok(())
    }

    fn emit_existing_session_event(
        &mut self,
        line_number: usize,
        capture: ctx_history_core::ProviderCaptureEnvelope,
    ) -> ProviderProjectionResult<ExistingSessionEventOutcome> {
        self.existing_session_events += 1;
        self.emit_normalization(ProviderNormalizationResult {
            captures: vec![(line_number, capture)],
            ..ProviderNormalizationResult::default()
        })?;
        Ok(ExistingSessionEventOutcome::Accepted)
    }

    fn use_explicit_file_touches(&mut self) {
        self.explicit_file_touch_declarations += 1;
    }

    fn reject_record(&mut self, line_number: usize, reason: String) {
        self.rejections.push((line_number, reason));
    }
}

#[test]
fn codex_streams_exact_touch_identity_prefix_and_rejects_limit_overflow() {
    let paths = (0..=crate::provider::file_touches::MAX_PROVIDER_FILE_TOUCHES_PER_EVENT)
        .map(|index| json!({"path": format!("src/generated-{index}.rs")}))
        .collect::<Vec<_>>();
    let arguments = json!({"files": paths});
    let (preview, preview_truncated, raw_arguments_retained) =
        crate::provider::codex::events::codex_tool_arguments_preview(&arguments);
    assert!(
        preview.starts_with("file touches: unknown:src/generated-0.rs"),
        "{preview}"
    );
    assert!(preview.ends_with("+65525 more"));
    assert!(preview_truncated);
    assert!(!raw_arguments_retained);
    let payload = serde_json::to_vec(&json!({
        "timestamp": "2026-07-18T12:00:01Z",
        "type": "response_item",
        "payload": {
            "type": "function_call",
            "name": "edit_file",
            "call_id": "touch-limit-call",
            "arguments": arguments
        }
    }))
    .unwrap();
    assert!(payload.len() < MAX_PROVIDER_JSONL_LINE_BYTES);
    let record = CapturedRecord::content(
        1,
        crate::captured_batch::NativeLocator::new("jsonl-line", b"touch-limit".to_vec()).unwrap(),
        ProviderRecordKind::new(CODEX_RECORD_KIND).unwrap(),
        payload,
    )
    .unwrap();
    let mut projector = CodexCapturedBatchProjector::fresh(ProviderAdapterContext::default());
    projector.next_ordinal = 1;
    projector.header = Some(
        codex_session_header(serde_json::from_str(&session_meta("touch-limit", None)).unwrap())
            .unwrap(),
    );
    let mut output = CountingProjectionOutput::default();

    projector.project_record(&record, &mut output).unwrap();

    assert_eq!(output.captures, 1);
    assert_eq!(output.explicit_file_touch_declarations, 1);
    assert!(!output.touch_before_capture);
    assert_eq!(
        output.file_touches,
        crate::provider::file_touches::MAX_PROVIDER_FILE_TOUCHES_PER_EVENT
    );
    assert_eq!(output.first_touch_index, Some(2_u64 << 16));
    assert_eq!(
        output.last_touch_index,
        Some(
            (2_u64 << 16)
                | (u64::try_from(
                    crate::provider::file_touches::MAX_PROVIDER_FILE_TOUCHES_PER_EVENT
                )
                .unwrap()
                    - 1)
        )
    );
    assert_eq!(
        output.rejections,
        vec![(2, PROVIDER_FILE_TOUCH_LIMIT_REJECTION.to_owned())]
    );
    assert_eq!(
        projector.accepted_file_touches,
        u64::try_from(crate::provider::file_touches::MAX_PROVIDER_FILE_TOUCHES_PER_EVENT).unwrap()
    );
}

#[test]
fn codex_reuses_the_admitted_session_after_the_first_event() {
    let mut projector = CodexCapturedBatchProjector::fresh(ProviderAdapterContext::default());
    projector.header = Some(
        codex_session_header(
            serde_json::from_str(&session_meta("existing-session-fast-path", None)).unwrap(),
        )
        .unwrap(),
    );
    let mut output = CountingProjectionOutput::default();
    for ordinal in 0..2_u64 {
        let payload: Value =
            serde_json::from_str(&message(usize::try_from(ordinal).unwrap())).unwrap();
        let record = CapturedRecord::content(
            ordinal,
            crate::captured_batch::NativeLocator::new(
                "jsonl-line",
                format!("existing-session-{ordinal}").into_bytes(),
            )
            .unwrap(),
            ProviderRecordKind::new(CODEX_RECORD_KIND).unwrap(),
            serde_json::to_vec(&payload).unwrap(),
        )
        .unwrap();
        projector.project_record(&record, &mut output).unwrap();
    }

    assert_eq!(output.captures, 2);
    assert_eq!(output.existing_session_events, 1);
    assert_eq!(projector.accepted_captures, 2);
    assert_eq!(projector.accepted_events, 2);
}
