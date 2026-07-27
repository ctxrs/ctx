use ctx_history_core::CaptureProvider;

use super::{
    VerifiedContentContract, VerifiedContentPlatform, VerifiedContentPlatformDisposition,
    VerifiedContentRoute, VerifiedContentRouteStatus,
};
use crate::complete_content::{CompleteContentSourceFamily, VerifiedContentRole};

macro_rules! platform_dispositions {
    ($status:expr, $reason:expr) => {
        [
            VerifiedContentPlatformDisposition {
                platform: VerifiedContentPlatform::Linux,
                status: $status,
                reason: $reason,
            },
            VerifiedContentPlatformDisposition {
                platform: VerifiedContentPlatform::MacOs,
                status: $status,
                reason: $reason,
            },
            VerifiedContentPlatformDisposition {
                platform: VerifiedContentPlatform::Windows,
                status: $status,
                reason: $reason,
            },
            VerifiedContentPlatformDisposition {
                platform: VerifiedContentPlatform::FreeBsd,
                status: $status,
                reason: $reason,
            },
        ]
    };
}

macro_rules! supported_route {
    ($provider:expr, $format:expr, $role:expr, $fixture:expr; $(($family:expr, $profile:expr, $kind:expr)),+ $(,)?) => {
        VerifiedContentRoute {
            provider: $provider,
            source_format: $format,
            role: $role,
            platform_dispositions: platform_dispositions!(VerifiedContentRouteStatus::Supported, ""),
            contracts: &[$(VerifiedContentContract {
                family: $family,
                content_profile: $profile,
                locator_kind: $kind,
                fixture_reference: $fixture,
            }),+],
        }
    };
}

macro_rules! unsupported_route {
    ($provider:expr, $format:expr, $role:expr, $reason:expr) => {
        VerifiedContentRoute {
            provider: $provider,
            source_format: $format,
            role: $role,
            platform_dispositions: platform_dispositions!(
                VerifiedContentRouteStatus::Unsupported,
                $reason
            ),
            contracts: &[],
        }
    };
}

/// Single authority for verified-content support. Every provider source format
/// in `docs/provider-support-matrix.json` has one complete-message row. v0.26 uses one
/// documented uniform rule: provider-source decoding is platform-independent,
/// so all four release-platform dispositions on a row are identical. A future
/// exception must replace the explicit per-platform entries rather than infer
/// support from the host OS. Tuples may carry multiple executable contracts
/// when one provider format has multiple native encodings.
pub const VERIFIED_CONTENT_ROUTES: &[VerifiedContentRoute] = &[
    supported_route!(
        CaptureProvider::Codex,
        "codex_session_jsonl_tree",
        VerifiedContentRole::MessageBody,
        "tests/fixtures/provider-history/complete-content-jsonl/v1/codex.jsonl";
        (CompleteContentSourceFamily::Jsonl, "codex.message-body.v1", "jsonl-range-v1")
    ),
    unsupported_route!(
        CaptureProvider::Codex,
        "codex_history_jsonl",
        VerifiedContentRole::MessageBody,
        "history prompts do not retain verified source coordinates"
    ),
    supported_route!(
        CaptureProvider::Claude,
        crate::CLAUDE_PROJECTS_SOURCE_FORMAT,
        VerifiedContentRole::MessageBody,
        "crates/ctx-cli/tests/native_provider_real_shapes.rs::claude_long_message_reopens_and_rejects_historical_rewrite";
        (CompleteContentSourceFamily::Jsonl, "claude-jsonl.message-body.v1", "jsonl-range-v1")
    ),
    supported_route!(
        CaptureProvider::Pi,
        "pi_session_jsonl",
        VerifiedContentRole::MessageBody,
        "crates/ctx-cli/tests/native_provider_real_shapes.rs::pi_long_message_reopens_after_append_without_storing_the_tail";
        (CompleteContentSourceFamily::Jsonl, "pi-jsonl.message-body.v1", "jsonl-range-v1")
    ),
    supported_route!(
        CaptureProvider::OpenCode,
        crate::OPENCODE_SQLITE_SOURCE_FORMAT,
        VerifiedContentRole::MessageBody,
        "complete_content::sqlite::tests::opencode_family_recovers_compound_message_rows";
        (CompleteContentSourceFamily::Sqlite, "opencode-sqlite.message-body.v1", "opencode-sqlite-logical-row-v1")
    ),
    supported_route!(
        CaptureProvider::Kilo,
        crate::KILO_SQLITE_SOURCE_FORMAT,
        VerifiedContentRole::MessageBody,
        "complete_content::sqlite::tests::opencode_family_recovers_compound_message_rows";
        (CompleteContentSourceFamily::Sqlite, "kilo-sqlite.message-body.v1", "opencode-sqlite-logical-row-v1")
    ),
    supported_route!(
        CaptureProvider::KiroCli,
        crate::KIRO_SQLITE_SOURCE_FORMAT,
        VerifiedContentRole::MessageBody,
        "tests/fixtures/provider-history/kiro-cli/v2/data.sqlite3";
        (CompleteContentSourceFamily::Sqlite, "kiro-sqlite.message-body.v1", "kiro-conversation-row-v1")
    ),
    supported_route!(
        CaptureProvider::Antigravity,
        crate::ANTIGRAVITY_CLI_SOURCE_FORMAT,
        VerifiedContentRole::MessageBody,
        "tests/fixtures/provider-history/complete-content-jsonl/v1/antigravity.jsonl";
        (CompleteContentSourceFamily::Jsonl, "antigravity.message-body.v1", "jsonl-range-v1")
    ),
    supported_route!(
        CaptureProvider::Gemini,
        crate::GEMINI_CLI_SOURCE_FORMAT,
        VerifiedContentRole::MessageBody,
        "tests/fixtures/provider-history/complete-content-jsonl/v1/gemini.jsonl";
        (CompleteContentSourceFamily::Jsonl, "gemini-jsonl.message-body.v1", "jsonl-range-v1")
    ),
    supported_route!(
        CaptureProvider::Tabnine,
        crate::TABNINE_CLI_SOURCE_FORMAT,
        VerifiedContentRole::MessageBody,
        "tests/fixtures/provider-history/complete-content-jsonl/v1/tabnine.jsonl";
        (CompleteContentSourceFamily::Jsonl, "tabnine.message-body.v1", "jsonl-range-v1")
    ),
    supported_route!(
        CaptureProvider::FactoryAiDroid,
        crate::FACTORY_DROID_SOURCE_FORMAT,
        VerifiedContentRole::MessageBody,
        "tests/fixtures/provider-history/complete-content-jsonl/v1/factory-droid.jsonl";
        (CompleteContentSourceFamily::Jsonl, "factory-droid.message-body.v1", "jsonl-range-v1")
    ),
    supported_route!(
        CaptureProvider::Cursor,
        crate::CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
        VerifiedContentRole::MessageBody,
        "provider::providers::cursor::tests::production_long_message_publishes_exact_locator_and_fails_closed_after_mutation";
        (CompleteContentSourceFamily::Jsonl, "cursor.message-body.v1", "jsonl-exact-range-v1")
    ),
    supported_route!(
        CaptureProvider::Windsurf,
        "windsurf_cascade_hook_transcript_jsonl_tree",
        VerifiedContentRole::MessageBody,
        "tests/fixtures/provider-history/complete-content-jsonl/v1/windsurf.jsonl";
        (CompleteContentSourceFamily::Jsonl, "windsurf-hook.message-body.v1", "jsonl-range-v1")
    ),
    supported_route!(
        CaptureProvider::Qoder,
        "qoder_transcript_jsonl_tree",
        VerifiedContentRole::MessageBody,
        "tests/fixtures/provider-history/complete-content-jsonl/v1/qoder.jsonl";
        (CompleteContentSourceFamily::Jsonl, "qoder.message-body.v1", "jsonl-range-v1")
    ),
    supported_route!(
        CaptureProvider::CopilotCli,
        crate::COPILOT_CLI_SOURCE_FORMAT,
        VerifiedContentRole::MessageBody,
        "tests/fixtures/provider-history/complete-content-jsonl/v1/copilot-cli.jsonl";
        (CompleteContentSourceFamily::Jsonl, "copilot-cli.message-body.v1", "jsonl-range-v1")
    ),
    supported_route!(
        CaptureProvider::QwenCode,
        "qwen_code_chat_jsonl_tree",
        VerifiedContentRole::MessageBody,
        "tests/fixtures/provider-history/complete-content-jsonl/v1/qwen-code.jsonl";
        (CompleteContentSourceFamily::Jsonl, "qwen-code.message-body.v1", "jsonl-range-v1")
    ),
    supported_route!(
        CaptureProvider::CodeBuddy,
        crate::CODEBUDDY_SOURCE_FORMAT,
        VerifiedContentRole::MessageBody,
        "ctx-history-capture/tests/complete_content_jsonl.rs::four_provider_capability_matrix_imports_persists_and_recovers_exact_content";
        (CompleteContentSourceFamily::Jsonl, "codebuddy-jsonl.message-body.v1", "jsonl-exact-range-v1"),
        (CompleteContentSourceFamily::Structured, "codebuddy-history.message-body.v1", "structured-message-v1")
    ),
    supported_route!(
        CaptureProvider::MistralVibe,
        "mistral_vibe_session_jsonl_tree",
        VerifiedContentRole::MessageBody,
        "ctx-history-capture/tests/complete_content_jsonl.rs::four_provider_capability_matrix_imports_persists_and_recovers_exact_content";
        (CompleteContentSourceFamily::Jsonl, "mistral-vibe.message-body.v1", "jsonl-exact-range-v1")
    ),
    supported_route!(
        CaptureProvider::OpenClaw,
        crate::OPENCLAW_SOURCE_FORMAT,
        VerifiedContentRole::MessageBody,
        "ctx-history-capture/tests/complete_content_jsonl.rs::four_provider_capability_matrix_imports_persists_and_recovers_exact_content";
        (CompleteContentSourceFamily::Jsonl, "openclaw.message-body.v1", "jsonl-exact-range-v1")
    ),
    supported_route!(
        CaptureProvider::KimiCodeCli,
        "kimi_code_cli_wire_jsonl_tree",
        VerifiedContentRole::MessageBody,
        "ctx-history-capture/tests/complete_content_jsonl.rs::four_provider_capability_matrix_imports_persists_and_recovers_exact_content";
        (CompleteContentSourceFamily::Jsonl, "kimi-code-cli.message-body.v1", "jsonl-exact-range-v1")
    ),
    supported_route!(
        CaptureProvider::Auggie,
        crate::AUGGIE_SESSION_JSON_SOURCE_FORMAT,
        VerifiedContentRole::MessageBody,
        "complete_content::structured::tests::resolves_one_file_and_compound_provider_families";
        (CompleteContentSourceFamily::Structured, "auggie-session.message-body.v1", "structured-message-v1")
    ),
    supported_route!(
        CaptureProvider::Continue,
        crate::CONTINUE_CLI_SOURCE_FORMAT,
        VerifiedContentRole::MessageBody,
        "complete_content::structured::tests::resolves_one_file_and_compound_provider_families";
        (CompleteContentSourceFamily::Structured, "continue-session.message-body.v1", "structured-message-v1")
    ),
    supported_route!(
        CaptureProvider::OpenHands,
        crate::OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
        VerifiedContentRole::MessageBody,
        "complete_content::structured::tests::openhands_recovery_matches_authoritative_current_and_legacy_decoding";
        (CompleteContentSourceFamily::Structured, "openhands-events.message-body.v1", "structured-message-v1")
    ),
    supported_route!(
        CaptureProvider::RovoDev,
        crate::ROVODEV_SOURCE_FORMAT,
        VerifiedContentRole::MessageBody,
        "ctx-history-capture/tests/structured_complete_content.rs::public_resolver_recovers_verified_rovo_body";
        (CompleteContentSourceFamily::Structured, "rovodev-tree.message-body.v1", "structured-message-v1")
    ),
    supported_route!(
        CaptureProvider::Cline,
        crate::CLINE_TASK_JSON_SOURCE_FORMAT,
        VerifiedContentRole::MessageBody,
        "complete_content::structured::tests::resolves_one_file_and_compound_provider_families";
        (CompleteContentSourceFamily::Structured, "cline-task.message-body.v1", "structured-message-v1")
    ),
    supported_route!(
        CaptureProvider::RooCode,
        crate::ROO_TASK_JSON_SOURCE_FORMAT,
        VerifiedContentRole::MessageBody,
        "complete_content::structured::tests::resolves_one_file_and_compound_provider_families";
        (CompleteContentSourceFamily::Structured, "roo-task.message-body.v1", "structured-message-v1")
    ),
    supported_route!(
        CaptureProvider::Firebender,
        crate::FIREBENDER_SQLITE_SOURCE_FORMAT,
        VerifiedContentRole::MessageBody,
        "complete_content::sqlite::tests::firebender_recovers_unicode_escaped_multiline_bytes_and_retains_only_truncated_locator";
        (CompleteContentSourceFamily::Sqlite, "firebender-sqlite.message-body.v1", "firebender-chat-session-row-v1")
    ),
    supported_route!(
        CaptureProvider::Zed,
        crate::ZED_THREADS_SQLITE_SOURCE_FORMAT,
        VerifiedContentRole::MessageBody,
        "complete_content::sqlite::tests::kiro_and_zed_row_contained_cohorts_recover_exact_message_text";
        (CompleteContentSourceFamily::Sqlite, "zed-sqlite.message-body.v1", "zed-thread-row-v1")
    ),
    supported_route!(
        CaptureProvider::Junie,
        crate::JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
        VerifiedContentRole::MessageBody,
        "provider::providers::junie::nativepath::tests::message_locator_reopens_exact_long_body_and_fails_closed";
        (CompleteContentSourceFamily::Jsonl, "junie.message-body.v1", "junie-jsonl-record-set-v1")
    ),
    supported_route!(
        CaptureProvider::ForgeCode,
        crate::FORGECODE_SQLITE_SOURCE_FORMAT,
        VerifiedContentRole::MessageBody,
        "complete_content::sqlite::tests::forgecode_recovers_compound_message_rows";
        (CompleteContentSourceFamily::Sqlite, "forgecode-sqlite.message-body.v1", "forgecode-conversation-row-v1")
    ),
    supported_route!(
        CaptureProvider::DeepAgents,
        crate::DEEPAGENTS_SQLITE_SOURCE_FORMAT,
        VerifiedContentRole::MessageBody,
        "complete_content::sqlite::tests::deepagents_compound_message_locator_reopens_verified_content";
        (CompleteContentSourceFamily::Sqlite, "deepagents-sqlite.message-body.v1", "deepagents-write-message-v1")
    ),
    supported_route!(
        CaptureProvider::Mux,
        "mux_session_jsonl_tree",
        VerifiedContentRole::MessageBody,
        "complete_content::jsonl::mux::tests::chat_message_survives_append_but_not_record_rewrite";
        (CompleteContentSourceFamily::Jsonl, "mux.message-body.v1", "mux-record-v1")
    ),
    supported_route!(
        CaptureProvider::Hermes,
        crate::HERMES_SQLITE_SOURCE_FORMAT,
        VerifiedContentRole::MessageBody,
        "complete_content::sqlite::tests::hermes_recovers_visible_parented_messages";
        (CompleteContentSourceFamily::Sqlite, "hermes-sqlite.message-body.v1", "hermes-sqlite-row-v1")
    ),
    supported_route!(
        CaptureProvider::NanoClaw,
        crate::NANOCLAW_SOURCE_FORMAT,
        VerifiedContentRole::MessageBody,
        "provider::providers::nanoclaw::tests::compound_locator_recovers_exact_inbound_and_outbound_content_without_paths";
        (CompleteContentSourceFamily::Sqlite, "nanoclaw-sqlite.message-body.v1", "nanoclaw-project-message-v1")
    ),
    supported_route!(
        CaptureProvider::AstrBot,
        crate::ASTRBOT_SQLITE_SOURCE_FORMAT,
        VerifiedContentRole::MessageBody,
        "complete_content::sqlite::tests::astrbot_conversation_message_round_trips_and_binds_original_item";
        (CompleteContentSourceFamily::Sqlite, "astrbot-conversation.message-body.v1", "astrbot-conversation-message-v1")
    ),
    supported_route!(
        CaptureProvider::Crush,
        crate::CRUSH_SQLITE_SOURCE_FORMAT,
        VerifiedContentRole::MessageBody,
        "complete_content::sqlite::tests::crush_recovers_parented_messages";
        (CompleteContentSourceFamily::Sqlite, "crush-sqlite.message-body.v1", "crush-sqlite-row-v1")
    ),
    supported_route!(
        CaptureProvider::Goose,
        crate::GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
        VerifiedContentRole::MessageBody,
        "complete_content::sqlite::tests::goose_recovers_parented_messages";
        (CompleteContentSourceFamily::Sqlite, "goose-sqlite.message-body.v1", "goose-logical-row-v3")
    ),
    supported_route!(
        CaptureProvider::Lingma,
        crate::LINGMA_SQLITE_SOURCE_FORMAT,
        VerifiedContentRole::MessageBody,
        "complete_content::sqlite::tests::lingma_user_prompt_round_trips_and_changed_row_fails_closed";
        (CompleteContentSourceFamily::Sqlite, "lingma-user-prompt.message-body.v1", "lingma-chat-record-v1")
    ),
    supported_route!(
        CaptureProvider::Warp,
        crate::WARP_SQLITE_SOURCE_FORMAT,
        VerifiedContentRole::MessageBody,
        "provider::providers::warp::integration_tests::source_backed_content_locators_reopen_exact_messages";
        (CompleteContentSourceFamily::Sqlite, "warp-sqlite.message-body.v1", "warp-task-message-v1")
    ),
    supported_route!(
        CaptureProvider::Shelley,
        crate::SHELLEY_SQLITE_SOURCE_FORMAT,
        VerifiedContentRole::MessageBody,
        "provider::providers::shelley::tests::source_backed_content_locators_reopen_exact_messages";
        (CompleteContentSourceFamily::Sqlite, "shelley-sqlite.message-body.v1", "shelley-compound-message-row-v1")
    ),
    supported_route!(
        CaptureProvider::Trae,
        crate::provider::providers::trae::TRAE_STATE_VSCDB_SOURCE_FORMAT,
        VerifiedContentRole::MessageBody,
        "complete_content::sqlite::tests::trae_nested_itemtable_message_round_trips_without_storing_parent_body";
        (CompleteContentSourceFamily::Sqlite, "trae-itemtable.message-body.v1", "trae-itemtable-message-v1")
    ),
    supported_route!(
        CaptureProvider::MiMoCode,
        crate::MIMOCODE_SQLITE_SOURCE_FORMAT,
        VerifiedContentRole::MessageBody,
        "complete_content::sqlite::tests::opencode_family_recovers_compound_message_rows";
        (CompleteContentSourceFamily::Sqlite, "mimocode-sqlite.message-body.v1", "opencode-sqlite-logical-row-v1")
    ),
];
