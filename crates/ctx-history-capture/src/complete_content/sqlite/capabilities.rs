//! Provider capability accounting for bounded SQLite complete-content recovery.

use ctx_history_core::CaptureProvider;

use crate::{
    provider::providers::trae::TRAE_STATE_VSCDB_SOURCE_FORMAT, FIREBENDER_SQLITE_SOURCE_FORMAT,
    KIRO_SQLITE_SOURCE_FORMAT, ZED_THREADS_SQLITE_SOURCE_FORMAT,
};

/// Stable public accounting for every provider format in the SQLite family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SqliteCompleteContentCapability {
    pub provider: CaptureProvider,
    pub source_format: &'static str,
    pub cohort: &'static str,
    pub supported: bool,
    pub unsupported_reason: Option<&'static str>,
}

pub(super) const CAPABILITIES: &[SqliteCompleteContentCapability] = &[
    SqliteCompleteContentCapability {
        provider: CaptureProvider::Firebender,
        source_format: FIREBENDER_SQLITE_SOURCE_FORMAT,
        cohort: "embedded-json-message-array",
        supported: true,
        unsupported_reason: None,
    },
    SqliteCompleteContentCapability {
        provider: CaptureProvider::KiroCli,
        source_format: KIRO_SQLITE_SOURCE_FORMAT,
        cohort: "versioned-conversation-json",
        supported: true,
        unsupported_reason: None,
    },
    SqliteCompleteContentCapability {
        provider: CaptureProvider::Zed,
        source_format: ZED_THREADS_SQLITE_SOURCE_FORMAT,
        cohort: "typed-thread-blob",
        supported: true,
        unsupported_reason: None,
    },
    unsupported(
        CaptureProvider::OpenCode,
        "opencode_sqlite",
        "relational-message-parts",
        "message bodies span versioned message and part rows; an exact bounded join resolver is not yet retained",
    ),
    unsupported(
        CaptureProvider::Kilo,
        "kilo_sqlite",
        "relational-message-parts",
        "OpenCode-compatible message/part locators are not yet retained for complete hydration",
    ),
    unsupported(
        CaptureProvider::MiMoCode,
        "mimocode_sqlite",
        "relational-message-parts",
        "OpenCode-compatible message/part locators are not yet retained for complete hydration",
    ),
    unsupported(
        CaptureProvider::Crush,
        "crush_sqlite",
        "relational-parent-child",
        "ordinary messages require a versioned session/message join that is not yet a complete-content route",
    ),
    unsupported(
        CaptureProvider::Goose,
        "goose_sessions_sqlite",
        "relational-parent-child",
        "ordinary messages require a versioned session/message join that is not yet a complete-content route",
    ),
    unsupported(
        CaptureProvider::Hermes,
        "hermes_state_sqlite",
        "relational-parent-child",
        "message rows need visibility/content decoding plus parent verification not yet retained by the locator",
    ),
    unsupported(
        CaptureProvider::AstrBot,
        "astrbot_data_v4_sqlite",
        "relational-checkpoint",
        "platform messages depend on a checkpoint-parent join not yet represented by one bounded locator",
    ),
    unsupported(
        CaptureProvider::Shelley,
        "shelley_sqlite",
        "relational-parent-child",
        "message ordering and parent classification require a versioned compound locator not yet retained",
    ),
    unsupported(
        CaptureProvider::Lingma,
        "lingma_sqlite",
        "partial-summary-row",
        "assistant rows are summaries/errors rather than proven original assistant bodies; no mixed-fidelity resolver is advertised",
    ),
    unsupported(
        CaptureProvider::ForgeCode,
        "forgecode_sqlite",
        "embedded-conversation-json",
        "conversation context decoding has no persisted message-level native identity yet",
    ),
    unsupported(
        CaptureProvider::DeepAgents,
        "deepagents_sessions_sqlite",
        "checkpoint-writes",
        "messages are reconstructed from checkpoint writes and need a compound write/message locator",
    ),
    unsupported(
        CaptureProvider::NanoClaw,
        "nanoclaw_project",
        "compound-sqlite-directory",
        "one session spans central, inbound, and outbound databases; a single-file resolver cannot prove the compound snapshot",
    ),
    unsupported(
        CaptureProvider::Warp,
        "warp_sqlite",
        "protobuf-task-rows",
        "task protobuf messages require a versioned nested-message locator not yet retained",
    ),
    unsupported(
        CaptureProvider::Trae,
        TRAE_STATE_VSCDB_SOURCE_FORMAT,
        "electron-state-compound",
        "captured locators address whole ItemTable chat-value rows, not individual messages; no bounded message-level resolver is advertised",
    ),
];

const fn unsupported(
    provider: CaptureProvider,
    source_format: &'static str,
    cohort: &'static str,
    reason: &'static str,
) -> SqliteCompleteContentCapability {
    SqliteCompleteContentCapability {
        provider,
        source_format,
        cohort,
        supported: false,
        unsupported_reason: Some(reason),
    }
}

pub fn sqlite_complete_content_capabilities() -> &'static [SqliteCompleteContentCapability] {
    CAPABILITIES
}
