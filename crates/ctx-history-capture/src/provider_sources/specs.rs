// Keep the provider matrix as one cohesive table: splitting it alphabetically
// would obscure cross-provider policy defaults and make updates harder to audit.
use ctx_history_core::{CaptureProvider, ProviderRawRetention, ProviderRedactionBoundary};

use super::types::{
    ProviderCatalogSupport, ProviderDefaultLocation, ProviderImportSupport, ProviderSourceKind,
    ProviderSourceSpec,
};

const CODEX_DEFAULTS: &[ProviderDefaultLocation] = &[
    ProviderDefaultLocation {
        path_components: &[".codex", "sessions"],
        source_format: "codex_session_jsonl_tree",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[".codex", "history.jsonl"],
        source_format: "codex_history_jsonl",
        source_kind: ProviderSourceKind::NativeHistory,
    },
];

const PI_DEFAULTS: &[ProviderDefaultLocation] = &[
    ProviderDefaultLocation {
        path_components: &[".pi", "agent", "sessions"],
        source_format: "pi_session_jsonl",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[".omp", "agent", "sessions"],
        source_format: "pi_session_jsonl",
        source_kind: ProviderSourceKind::NativeHistory,
    },
];

const CLAUDE_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".claude", "projects"],
    source_format: "claude_projects_jsonl_tree",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const OPENCODE_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".local", "share", "opencode", "opencode.db"],
    source_format: "opencode_sqlite",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const KILO_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".local", "share", "kilo", "kilo.db"],
    source_format: "kilo_sqlite",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const KIRO_DEFAULTS: &[ProviderDefaultLocation] = &[
    ProviderDefaultLocation {
        path_components: &[".local", "share", "kiro-cli", "data.sqlite3"],
        source_format: "kiro_cli_sqlite",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &["Library", "Application Support", "kiro-cli", "data.sqlite3"],
        source_format: "kiro_cli_sqlite",
        source_kind: ProviderSourceKind::NativeHistory,
    },
];

const CRUSH_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".local", "share", "crush", "crush.db"],
    source_format: "crush_sqlite",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const GOOSE_DEFAULTS: &[ProviderDefaultLocation] = &[
    ProviderDefaultLocation {
        path_components: &[".local", "share", "goose", "sessions", "sessions.db"],
        source_format: "goose_sessions_sqlite",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            ".local",
            "share",
            "Block",
            "goose",
            "sessions",
            "sessions.db",
        ],
        source_format: "goose_sessions_sqlite",
        source_kind: ProviderSourceKind::NativeHistory,
    },
];

const WARP_DEFAULTS: &[ProviderDefaultLocation] = &[
    ProviderDefaultLocation {
        path_components: &[".local", "state", "warp-terminal", "warp.sqlite"],
        source_format: "warp_sqlite",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            "Library",
            "Group Containers",
            "2BBY89MBSN.dev.warp",
            "Library",
            "Application Support",
            "dev.warp.Warp-Stable",
            "warp.sqlite",
        ],
        source_format: "warp_sqlite",
        source_kind: ProviderSourceKind::NativeHistory,
    },
];

const LINGMA_DEFAULTS: &[ProviderDefaultLocation] = &[
    ProviderDefaultLocation {
        path_components: &[
            ".lingma",
            "vscode",
            "sharedClientCache",
            "cache",
            "db",
            "local.db",
        ],
        source_format: "lingma_sqlite",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            ".lingma",
            "vscode-insiders",
            "sharedClientCache",
            "cache",
            "db",
            "local.db",
        ],
        source_format: "lingma_sqlite",
        source_kind: ProviderSourceKind::NativeHistory,
    },
];

pub(super) const TRAE_STATE_VSCDB_SOURCE_FORMAT: &str = "trae_state_vscdb";
const TRAE_DEFAULTS: &[ProviderDefaultLocation] = &[
    ProviderDefaultLocation {
        path_components: &[
            "Library",
            "Application Support",
            "Trae",
            "User",
            "workspaceStorage",
        ],
        source_format: TRAE_STATE_VSCDB_SOURCE_FORMAT,
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            "Library",
            "Application Support",
            "Trae CN",
            "User",
            "workspaceStorage",
        ],
        source_format: TRAE_STATE_VSCDB_SOURCE_FORMAT,
        source_kind: ProviderSourceKind::NativeHistory,
    },
];

const QODER_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".qoder", "projects"],
    source_format: "qoder_transcript_jsonl_tree",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const ROVODEV_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".rovodev", "sessions"],
    source_format: "rovodev_session_json_tree",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const ANTIGRAVITY_DEFAULTS: &[ProviderDefaultLocation] = &[
    ProviderDefaultLocation {
        path_components: &[".gemini", "antigravity-cli", "brain"],
        source_format: "antigravity_cli_transcript_jsonl_tree",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[".gemini", "antigravity-ide", "brain"],
        source_format: "antigravity_cli_transcript_jsonl_tree",
        source_kind: ProviderSourceKind::NativeHistory,
    },
];

const GEMINI_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".gemini"],
    source_format: "gemini_cli_chat_recording_jsonl",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const TABNINE_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".tabnine", "agent"],
    source_format: "tabnine_cli_chat_recording_jsonl",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const CURSOR_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".cursor", "projects"],
    source_format: "cursor_agent_transcript_jsonl_tree",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const WINDSURF_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".windsurf", "transcripts"],
    source_format: "windsurf_cascade_hook_transcript_jsonl_tree",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const ZED_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".local", "share", "zed", "threads", "threads.db"],
    source_format: "zed_threads_sqlite",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const COPILOT_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".copilot", "session-state"],
    source_format: "copilot_cli_session_events_jsonl",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const FACTORY_DROID_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".factory", "sessions"],
    source_format: "factory_ai_droid_sessions_jsonl",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const QWEN_CODE_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".qwen", "projects"],
    source_format: "qwen_code_chat_jsonl_tree",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const KIMI_CODE_CLI_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".kimi-code"],
    source_format: "kimi_code_cli_wire_jsonl_tree",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const AUGGIE_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".augment", "sessions"],
    source_format: "auggie_session_json",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const JUNIE_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".junie", "sessions"],
    source_format: "junie_session_events_jsonl_tree",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const FIREBENDER_DEFAULTS: &[ProviderDefaultLocation] = &[];

const FORGECODE_DEFAULTS: &[ProviderDefaultLocation] = &[];

const DEEPAGENTS_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".deepagents", ".state", "sessions.db"],
    source_format: "deepagents_sessions_sqlite",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const MISTRAL_VIBE_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".vibe", "logs", "session"],
    source_format: "mistral_vibe_session_jsonl_tree",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const MUX_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".mux", "sessions"],
    source_format: "mux_session_jsonl_tree",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const OPENCLAW_DEFAULTS: &[ProviderDefaultLocation] = &[
    ProviderDefaultLocation {
        path_components: &[".openclaw"],
        source_format: "openclaw_session_jsonl_tree",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[".clawdbot"],
        source_format: "openclaw_session_jsonl_tree",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[".moltbot"],
        source_format: "openclaw_session_jsonl_tree",
        source_kind: ProviderSourceKind::NativeHistory,
    },
];

const HERMES_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".hermes", "state.db"],
    source_format: "hermes_state_sqlite",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const NANOCLAW_DEFAULTS: &[ProviderDefaultLocation] = &[];

const ASTRBOT_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".astrbot", "data", "data_v4.db"],
    source_format: "astrbot_data_v4_sqlite",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const SHELLEY_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".config", "shelley", "shelley.db"],
    source_format: "shelley_sqlite",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const CONTINUE_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".continue", "sessions"],
    source_format: "continue_cli_sessions_json",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const OPENHANDS_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".openhands"],
    source_format: "openhands_file_events",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const CLINE_DEFAULTS: &[ProviderDefaultLocation] = &[
    ProviderDefaultLocation {
        path_components: &[".cline", "data"],
        source_format: "cline_task_directory_json",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            ".config",
            "Code",
            "User",
            "globalStorage",
            "saoudrizwan.claude-dev",
        ],
        source_format: "cline_task_directory_json",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            ".config",
            "Code - Insiders",
            "User",
            "globalStorage",
            "saoudrizwan.claude-dev",
        ],
        source_format: "cline_task_directory_json",
        source_kind: ProviderSourceKind::NativeHistory,
    },
];

const ROO_DEFAULTS: &[ProviderDefaultLocation] = &[
    ProviderDefaultLocation {
        path_components: &[
            ".config",
            "Code",
            "User",
            "globalStorage",
            "rooveterinaryinc.roo-cline",
        ],
        source_format: "roo_task_directory_json",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            ".config",
            "Code",
            "User",
            "globalStorage",
            "RooVeterinaryInc.roo-cline",
        ],
        source_format: "roo_task_directory_json",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            ".config",
            "Code - Insiders",
            "User",
            "globalStorage",
            "rooveterinaryinc.roo-cline",
        ],
        source_format: "roo_task_directory_json",
        source_kind: ProviderSourceKind::NativeHistory,
    },
];

const CODEBUDDY_DEFAULTS: &[ProviderDefaultLocation] = &[
    ProviderDefaultLocation {
        path_components: &[".codebuddy"],
        source_format: "codebuddy_history_json",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            "Library",
            "Application Support",
            "CodeBuddyExtension",
            "Data",
        ],
        source_format: "codebuddy_history_json",
        source_kind: ProviderSourceKind::NativeHistory,
    },
];

pub(super) const PROVIDER_SPECS: &[ProviderSourceSpec] = &[
    ProviderSourceSpec {
        provider: CaptureProvider::Codex,
        display_name: "Codex",
        default_locations: CODEX_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::Native,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Pi,
        display_name: "Pi",
        default_locations: PI_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Claude,
        display_name: "Claude",
        default_locations: CLAUDE_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::OpenCode,
        display_name: "OpenCode",
        default_locations: OPENCODE_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Kilo,
        display_name: "Kilo Code",
        default_locations: KILO_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::KiroCli,
        display_name: "Kiro CLI",
        default_locations: KIRO_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Crush,
        display_name: "Crush",
        default_locations: CRUSH_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Goose,
        display_name: "Goose",
        default_locations: GOOSE_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Antigravity,
        display_name: "Antigravity",
        default_locations: ANTIGRAVITY_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Gemini,
        display_name: "Gemini",
        default_locations: GEMINI_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Tabnine,
        display_name: "Tabnine",
        default_locations: TABNINE_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Cursor,
        display_name: "Cursor",
        default_locations: CURSOR_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Windsurf,
        display_name: "Windsurf",
        default_locations: WINDSURF_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Zed,
        display_name: "Zed",
        default_locations: ZED_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::CopilotCli,
        display_name: "Copilot CLI",
        default_locations: COPILOT_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::FactoryAiDroid,
        display_name: "Factory AI Droid",
        default_locations: FACTORY_DROID_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::QwenCode,
        display_name: "Qwen Code",
        default_locations: QWEN_CODE_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::KimiCodeCli,
        display_name: "Kimi Code CLI",
        default_locations: KIMI_CODE_CLI_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Auggie,
        display_name: "Auggie",
        default_locations: AUGGIE_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Junie,
        display_name: "Junie",
        default_locations: JUNIE_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Firebender,
        display_name: "Firebender",
        default_locations: FIREBENDER_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::ForgeCode,
        display_name: "ForgeCode",
        default_locations: FORGECODE_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::DeepAgents,
        display_name: "Deep Agents",
        default_locations: DEEPAGENTS_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::MistralVibe,
        display_name: "Mistral Vibe",
        default_locations: MISTRAL_VIBE_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Mux,
        display_name: "Mux",
        default_locations: MUX_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::RovoDev,
        display_name: "Rovo Dev",
        default_locations: ROVODEV_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::OpenClaw,
        display_name: "OpenClaw",
        default_locations: OPENCLAW_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Hermes,
        display_name: "Hermes Agent",
        default_locations: HERMES_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::NanoClaw,
        display_name: "NanoClaw",
        default_locations: NANOCLAW_DEFAULTS,
        import_support: ProviderImportSupport::Explicit,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::AstrBot,
        display_name: "AstrBot",
        default_locations: ASTRBOT_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Shelley,
        display_name: "Shelley",
        default_locations: SHELLEY_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Continue,
        display_name: "Continue",
        default_locations: CONTINUE_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::OpenHands,
        display_name: "OpenHands",
        default_locations: OPENHANDS_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Cline,
        display_name: "Cline",
        default_locations: CLINE_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::RooCode,
        display_name: "Roo Code",
        default_locations: ROO_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Lingma,
        display_name: "Lingma",
        default_locations: LINGMA_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Trae,
        display_name: "Trae",
        default_locations: TRAE_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Qoder,
        display_name: "Qoder",
        default_locations: QODER_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Warp,
        display_name: "Warp",
        default_locations: WARP_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::CodeBuddy,
        display_name: "CodeBuddy",
        default_locations: CODEBUDDY_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
];

pub fn provider_source_specs() -> &'static [ProviderSourceSpec] {
    PROVIDER_SPECS
}

pub fn provider_source_spec(provider: CaptureProvider) -> Option<&'static ProviderSourceSpec> {
    PROVIDER_SPECS.iter().find(|spec| spec.provider == provider)
}
