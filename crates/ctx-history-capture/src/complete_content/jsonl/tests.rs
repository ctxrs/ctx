use super::*;

#[test]
fn range_encoding_is_fixed_width_and_big_endian() {
    let range = JsonlRange {
        byte_start: 0x0102_0304_0506_0708,
        byte_end_exclusive: 0x1112_1314_1516_1718,
    };
    assert_eq!(
        range.encode(),
        [1, 2, 3, 4, 5, 6, 7, 8, 17, 18, 19, 20, 21, 22, 23, 24,]
    );
}

#[test]
fn provider_matrix_is_exact() {
    let resolver = JsonlCompleteContentResolver::new();
    for (provider, source_format) in SUPPORTED_JSONL_SOURCES {
        assert!(resolver.supports(*provider, source_format));
    }
    assert!(!resolver.supports(CaptureProvider::Claude, "claude_projects_jsonl_tree"));
    assert!(!resolver.supports(CaptureProvider::Pi, "pi_session_jsonl"));
    assert!(!resolver.supports(CaptureProvider::Codex, "codex_history_jsonl"));
}

#[test]
fn provider_fixtures_preserve_unicode_and_escaping() {
    const FIXTURES: &[(CaptureProvider, &str, &str, usize, &str)] = &[
        (
            CaptureProvider::Codex,
            CODEX_SESSION_SOURCE_FORMAT,
            include_str!("../../../../../tests/fixtures/provider-history/complete-content-jsonl/v1/codex.jsonl"),
            1,
            "Codex snowman ☃\nquoted \"body\" and escaped \\\\ path",
        ),
        (
            CaptureProvider::Antigravity,
            crate::ANTIGRAVITY_CLI_SOURCE_FORMAT,
            include_str!("../../../../../tests/fixtures/provider-history/complete-content-jsonl/v1/antigravity.jsonl"),
            0,
            "Antigravity snowman ☃\nquoted \"body\" and escaped \\\\ path",
        ),
        (
            CaptureProvider::Gemini,
            crate::GEMINI_CLI_SOURCE_FORMAT,
            include_str!("../../../../../tests/fixtures/provider-history/complete-content-jsonl/v1/gemini.jsonl"),
            1,
            "Gemini snowman ☃\nquoted \"body\" and escaped \\\\ path",
        ),
        (
            CaptureProvider::Tabnine,
            crate::TABNINE_CLI_SOURCE_FORMAT,
            include_str!("../../../../../tests/fixtures/provider-history/complete-content-jsonl/v1/tabnine.jsonl"),
            1,
            "Tabnine snowman ☃\nquoted \"body\" and escaped \\\\ path",
        ),
        (
            CaptureProvider::FactoryAiDroid,
            crate::FACTORY_DROID_SOURCE_FORMAT,
            include_str!("../../../../../tests/fixtures/provider-history/complete-content-jsonl/v1/factory-droid.jsonl"),
            1,
            "Droid snowman ☃\nquoted \"body\" and escaped \\\\ path",
        ),
        (
            CaptureProvider::Cursor,
            crate::CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
            include_str!("../../../../../tests/fixtures/provider-history/complete-content-jsonl/v1/cursor.jsonl"),
            0,
            "Cursor snowman ☃\nquoted \"body\" and escaped \\\\ path",
        ),
        (
            CaptureProvider::Windsurf,
            crate::WINDSURF_CASCADE_HOOK_TRANSCRIPT_SOURCE_FORMAT,
            include_str!("../../../../../tests/fixtures/provider-history/complete-content-jsonl/v1/windsurf.jsonl"),
            0,
            "Windsurf snowman ☃\nquoted \"body\" and escaped \\\\ path",
        ),
        (
            CaptureProvider::Qoder,
            crate::QODER_SOURCE_FORMAT,
            include_str!("../../../../../tests/fixtures/provider-history/complete-content-jsonl/v1/qoder.jsonl"),
            1,
            "Qoder snowman ☃\nquoted \"body\" and escaped \\\\ path",
        ),
        (
            CaptureProvider::CopilotCli,
            crate::COPILOT_CLI_SOURCE_FORMAT,
            include_str!("../../../../../tests/fixtures/provider-history/complete-content-jsonl/v1/copilot-cli.jsonl"),
            1,
            "Copilot snowman ☃\nquoted \"body\" and escaped \\\\ path",
        ),
        (
            CaptureProvider::QwenCode,
            crate::QWEN_CODE_SOURCE_FORMAT,
            include_str!("../../../../../tests/fixtures/provider-history/complete-content-jsonl/v1/qwen-code.jsonl"),
            0,
            "Qwen snowman ☃\nquoted \"body\" and escaped \\\\ path",
        ),
    ];

    for (provider, source_format, fixture, line_index, expected) in FIXTURES {
        let line = fixture.lines().nth(*line_index).unwrap();
        let value: Value = serde_json::from_str(line).unwrap();
        let (text, _) =
            complete_message_text_and_id(*provider, source_format, &value, line_index + 1)
                .unwrap_or_else(|| panic!("missing fixture message for {provider:?}"));
        assert_eq!(&text, expected, "provider {provider:?}");
    }
}
