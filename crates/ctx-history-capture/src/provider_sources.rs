use std::{
    collections::HashSet,
    env, fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use ctx_history_core::{CaptureProvider, ProviderRawRetention, ProviderRedactionBoundary};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderSourceKind {
    NativeHistory,
    DetectionOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderImportSupport {
    Native,
    Explicit,
    Preview,
    Unsupported,
}

impl ProviderImportSupport {
    pub fn is_importable(self) -> bool {
        matches!(self, Self::Native | Self::Explicit | Self::Preview)
    }

    pub fn is_auto_importable(self) -> bool {
        matches!(self, Self::Native)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderCatalogSupport {
    Native,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderSourceStatus {
    Available,
    Empty,
    Unknown,
    Missing,
    Unsupported,
}

impl ProviderSourceStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Empty => "empty",
            Self::Unknown => "unknown",
            Self::Missing => "missing",
            Self::Unsupported => "unsupported",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ProviderDefaultLocation {
    pub path_components: &'static [&'static str],
    pub source_format: &'static str,
    pub source_kind: ProviderSourceKind,
}

#[derive(Debug, Clone, Copy)]
pub struct ProviderSourceSpec {
    pub provider: CaptureProvider,
    pub display_name: &'static str,
    pub default_locations: &'static [ProviderDefaultLocation],
    pub import_support: ProviderImportSupport,
    pub catalog_support: ProviderCatalogSupport,
    pub raw_retention: ProviderRawRetention,
    pub redaction_boundary: ProviderRedactionBoundary,
    pub unsupported_reason: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSource {
    pub provider: CaptureProvider,
    pub path: PathBuf,
    pub exists: bool,
    pub source_format: &'static str,
    pub source_kind: ProviderSourceKind,
    pub import_support: ProviderImportSupport,
    pub catalog_support: ProviderCatalogSupport,
    pub status: ProviderSourceStatus,
    pub raw_retention: ProviderRawRetention,
    pub redaction_boundary: ProviderRedactionBoundary,
    pub unsupported_reason: Option<&'static str>,
}

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

const CODEARTS_AGENT_DEFAULTS: &[ProviderDefaultLocation] = &[
    ProviderDefaultLocation {
        path_components: &[".codeartsdoer", "vscode-data", "opencode.db"],
        source_format: "codearts_agent_kernel_sqlite",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[".codeartsdoer", "codearts-data", "opencode.db"],
        source_format: "codearts_agent_kernel_sqlite",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[".local", "share", "codeartsdoer", "opencode.db"],
        source_format: "codearts_agent_kernel_sqlite",
        source_kind: ProviderSourceKind::NativeHistory,
    },
];

const OPENLOAF_DEFAULTS: &[ProviderDefaultLocation] = &[
    ProviderDefaultLocation {
        path_components: &[".openloaf", "chat-history"],
        source_format: "openloaf_chat_jsonl_tree",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &["OpenLoafData", "projects"],
        source_format: "openloaf_chat_jsonl_tree",
        source_kind: ProviderSourceKind::NativeHistory,
    },
];

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

const DEXTO_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".dexto", "database"],
    source_format: "dexto_sqlite",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const POCHI_DEFAULTS: &[ProviderDefaultLocation] = &[];

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

const TRAE_STATE_VSCDB_SOURCE_FORMAT: &str = "trae_state_vscdb";
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

const TINYCLOUD_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".tinycloud"],
    source_format: "tinycloud_session_jsonl_tree",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const ZENCODER_DEFAULTS: &[ProviderDefaultLocation] = &[
    ProviderDefaultLocation {
        path_components: &[
            ".config",
            "Code",
            "User",
            "globalStorage",
            "ZencoderAI.zencoder",
            "zencoder-chat",
        ],
        source_format: "zencoder_chat_sessions_json_tree",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            ".config",
            "Code - Insiders",
            "User",
            "globalStorage",
            "ZencoderAI.zencoder",
            "zencoder-chat",
        ],
        source_format: "zencoder_chat_sessions_json_tree",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            ".config",
            "VSCodium",
            "User",
            "globalStorage",
            "ZencoderAI.zencoder",
            "zencoder-chat",
        ],
        source_format: "zencoder_chat_sessions_json_tree",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            ".config",
            "Code - OSS",
            "User",
            "globalStorage",
            "ZencoderAI.zencoder",
            "zencoder-chat",
        ],
        source_format: "zencoder_chat_sessions_json_tree",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            "Library",
            "Application Support",
            "Code",
            "User",
            "globalStorage",
            "ZencoderAI.zencoder",
            "zencoder-chat",
        ],
        source_format: "zencoder_chat_sessions_json_tree",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            "Library",
            "Application Support",
            "Code - Insiders",
            "User",
            "globalStorage",
            "ZencoderAI.zencoder",
            "zencoder-chat",
        ],
        source_format: "zencoder_chat_sessions_json_tree",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            "Library",
            "Application Support",
            "VSCodium",
            "User",
            "globalStorage",
            "ZencoderAI.zencoder",
            "zencoder-chat",
        ],
        source_format: "zencoder_chat_sessions_json_tree",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            "Library",
            "Application Support",
            "Code - OSS",
            "User",
            "globalStorage",
            "ZencoderAI.zencoder",
            "zencoder-chat",
        ],
        source_format: "zencoder_chat_sessions_json_tree",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            "AppData",
            "Roaming",
            "Code",
            "User",
            "globalStorage",
            "ZencoderAI.zencoder",
            "zencoder-chat",
        ],
        source_format: "zencoder_chat_sessions_json_tree",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            "AppData",
            "Roaming",
            "Code - Insiders",
            "User",
            "globalStorage",
            "ZencoderAI.zencoder",
            "zencoder-chat",
        ],
        source_format: "zencoder_chat_sessions_json_tree",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            "AppData",
            "Roaming",
            "VSCodium",
            "User",
            "globalStorage",
            "ZencoderAI.zencoder",
            "zencoder-chat",
        ],
        source_format: "zencoder_chat_sessions_json_tree",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            "AppData",
            "Roaming",
            "Code - OSS",
            "User",
            "globalStorage",
            "ZencoderAI.zencoder",
            "zencoder-chat",
        ],
        source_format: "zencoder_chat_sessions_json_tree",
        source_kind: ProviderSourceKind::NativeHistory,
    },
];

const ZENFLOW_SQLITE_SOURCE_FORMAT: &str = "zenflow_sqlite";
const ZENFLOW_DEFAULTS: &[ProviderDefaultLocation] = &[
    ProviderDefaultLocation {
        path_components: &[".local", "share", "zenflow", "db.sqlite"],
        source_format: ZENFLOW_SQLITE_SOURCE_FORMAT,
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            "Library",
            "Application Support",
            "ai.forgoodai.zenflow",
            "db.sqlite",
        ],
        source_format: ZENFLOW_SQLITE_SOURCE_FORMAT,
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            "AppData",
            "Roaming",
            "forgoodai",
            "zenflow",
            "data",
            "db.sqlite",
        ],
        source_format: ZENFLOW_SQLITE_SOURCE_FORMAT,
        source_kind: ProviderSourceKind::NativeHistory,
    },
];

const CODESTUDIO_DEFAULTS: &[ProviderDefaultLocation] = &[
    ProviderDefaultLocation {
        path_components: &[
            ".config",
            "Code Studio",
            "User",
            "globalStorage",
            "session-store.db",
        ],
        source_format: "codestudio_session_store_sqlite",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            "Library",
            "Application Support",
            "Code Studio",
            "User",
            "globalStorage",
            "session-store.db",
        ],
        source_format: "codestudio_session_store_sqlite",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            "AppData",
            "Roaming",
            "Code Studio",
            "User",
            "globalStorage",
            "session-store.db",
        ],
        source_format: "codestudio_session_store_sqlite",
        source_kind: ProviderSourceKind::NativeHistory,
    },
];

const QODER_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".qoder", "projects"],
    source_format: "qoder_transcript_jsonl_tree",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const CORTEX_CODE_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".snowflake", "cortex", "conversations"],
    source_format: "cortex_code_conversations_json",
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

const AUTOHAND_CODE_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".autohand", "sessions"],
    source_format: "autohand_code_sessions_jsonl",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const ADAL_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".adal", "sessions"],
    source_format: "adal_session_jsonl",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const IFLOW_CLI_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".iflow", "projects"],
    source_format: "iflow_cli_session_jsonl_tree",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const JAZZ_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".jazz", "history"],
    source_format: "jazz_history_json",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const AUGGIE_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".augment", "sessions"],
    source_format: "auggie_session_json",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const EVE_DEFAULTS: &[ProviderDefaultLocation] = &[];

const JUNIE_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".junie", "sessions"],
    source_format: "junie_session_events_jsonl_tree",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const FIREBENDER_DEFAULTS: &[ProviderDefaultLocation] = &[];

const KODE_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".kode", "projects"],
    source_format: "kode_session_jsonl_tree",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const NEOVATE_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".neovate", "projects"],
    source_format: "neovate_session_jsonl_tree",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const COMMAND_CODE_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".commandcode", "projects"],
    source_format: "command_code_session_jsonl_tree",
    source_kind: ProviderSourceKind::NativeHistory,
}];

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

const MOXBY_DEFAULTS: &[ProviderDefaultLocation] = &[
    ProviderDefaultLocation {
        path_components: &[".local", "share", "com.moxby.agent", "moxby_chats.db"],
        source_format: "moxby_chats_sqlite",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            "Library",
            "Application Support",
            "com.moxby.agent",
            "moxby_chats.db",
        ],
        source_format: "moxby_chats_sqlite",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &["AppData", "Roaming", "com.moxby.agent", "moxby_chats.db"],
        source_format: "moxby_chats_sqlite",
        source_kind: ProviderSourceKind::NativeHistory,
    },
];

const REASONIX_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".reasonix", "sessions"],
    source_format: "reasonix_session_jsonl_tree",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const TERRAMIND_DEFAULTS: &[ProviderDefaultLocation] = &[
    ProviderDefaultLocation {
        path_components: &[".config", "Nucleus", "data", "agents.db"],
        source_format: "terramind_agents_sqlite",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            "Library",
            "Application Support",
            "Nucleus",
            "data",
            "agents.db",
        ],
        source_format: "terramind_agents_sqlite",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &["AppData", "Roaming", "Nucleus", "data", "agents.db"],
        source_format: "terramind_agents_sqlite",
        source_kind: ProviderSourceKind::NativeHistory,
    },
];

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

const BOB_DEFAULTS: &[ProviderDefaultLocation] = &[
    ProviderDefaultLocation {
        path_components: &[
            ".config",
            "IBM Bob",
            "User",
            "globalStorage",
            "ibm.bob-code",
        ],
        source_format: "bob_task_directory_json",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            ".config",
            "Bob-IDE",
            "User",
            "globalStorage",
            "ibm.bob-code",
        ],
        source_format: "bob_task_directory_json",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            "Library",
            "Application Support",
            "IBM Bob",
            "User",
            "globalStorage",
            "ibm.bob-code",
        ],
        source_format: "bob_task_directory_json",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            "Library",
            "Application Support",
            "Bob-IDE",
            "User",
            "globalStorage",
            "ibm.bob-code",
        ],
        source_format: "bob_task_directory_json",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            "AppData",
            "Roaming",
            "IBM Bob",
            "User",
            "globalStorage",
            "ibm.bob-code",
        ],
        source_format: "bob_task_directory_json",
        source_kind: ProviderSourceKind::NativeHistory,
    },
    ProviderDefaultLocation {
        path_components: &[
            "AppData",
            "Roaming",
            "Bob-IDE",
            "User",
            "globalStorage",
            "ibm.bob-code",
        ],
        source_format: "bob_task_directory_json",
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

const AIDER_DESK_DEFAULTS: &[ProviderDefaultLocation] = &[];

const AMP_DEFAULTS: &[ProviderDefaultLocation] = &[];

const PROVIDER_SPECS: &[ProviderSourceSpec] = &[
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
        provider: CaptureProvider::CodeArtsAgent,
        display_name: "CodeArts Agent",
        default_locations: CODEARTS_AGENT_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::OpenLoaf,
        display_name: "OpenLoaf",
        default_locations: OPENLOAF_DEFAULTS,
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
        provider: CaptureProvider::AutohandCode,
        display_name: "Autohand Code",
        default_locations: AUTOHAND_CODE_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::IflowCli,
        display_name: "iFlow CLI",
        default_locations: IFLOW_CLI_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Jazz,
        display_name: "Jazz",
        default_locations: JAZZ_DEFAULTS,
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
        provider: CaptureProvider::Eve,
        display_name: "Eve",
        default_locations: EVE_DEFAULTS,
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
        provider: CaptureProvider::Kode,
        display_name: "Kode",
        default_locations: KODE_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Neovate,
        display_name: "Neovate",
        default_locations: NEOVATE_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::CommandCode,
        display_name: "Command Code",
        default_locations: COMMAND_CODE_DEFAULTS,
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
        provider: CaptureProvider::Moxby,
        display_name: "Moxby",
        default_locations: MOXBY_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Reasonix,
        display_name: "Reasonix",
        default_locations: REASONIX_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Adal,
        display_name: "AdaL",
        default_locations: ADAL_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Terramind,
        display_name: "Terramind",
        default_locations: TERRAMIND_DEFAULTS,
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
        import_support: ProviderImportSupport::Preview,
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
        provider: CaptureProvider::Bob,
        display_name: "IBM Bob",
        default_locations: BOB_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Dexto,
        display_name: "Dexto",
        default_locations: DEXTO_DEFAULTS,
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
        provider: CaptureProvider::TinyCloud,
        display_name: "TinyCloud",
        default_locations: TINYCLOUD_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Zencoder,
        display_name: "Zencoder",
        default_locations: ZENCODER_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Zenflow,
        display_name: "Zenflow",
        default_locations: ZENFLOW_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::CodeStudio,
        display_name: "Syncfusion Code Studio",
        default_locations: CODESTUDIO_DEFAULTS,
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
        provider: CaptureProvider::Pochi,
        display_name: "Pochi",
        default_locations: POCHI_DEFAULTS,
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
        provider: CaptureProvider::CortexCode,
        display_name: "Cortex Code",
        default_locations: CORTEX_CODE_DEFAULTS,
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
    ProviderSourceSpec {
        provider: CaptureProvider::AiderDesk,
        display_name: "Aider Desk",
        default_locations: AIDER_DESK_DEFAULTS,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::PathReference,
        redaction_boundary: ProviderRedactionBoundary::BeforeExport,
        unsupported_reason: None,
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Amp,
        display_name: "Amp",
        default_locations: AMP_DEFAULTS,
        import_support: ProviderImportSupport::Explicit,
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

pub fn discover_provider_sources(home: &Path) -> Vec<ProviderSource> {
    dedupe_sources(
        PROVIDER_SPECS
            .iter()
            .flat_map(|spec| discover_provider_sources_for_spec(home, spec))
            .collect(),
    )
}

pub fn discover_provider_sources_for_provider(
    home: &Path,
    provider: CaptureProvider,
) -> Vec<ProviderSource> {
    dedupe_sources(
        PROVIDER_SPECS
            .iter()
            .filter(|spec| spec.provider == provider)
            .flat_map(|spec| discover_provider_sources_for_spec(home, spec))
            .collect(),
    )
}

fn discover_provider_sources_for_spec(
    home: &Path,
    spec: &ProviderSourceSpec,
) -> Vec<ProviderSource> {
    if spec.provider == CaptureProvider::Kilo {
        return discover_kilo_sources(home, spec);
    }
    if spec.provider == CaptureProvider::ForgeCode {
        return discover_forgecode_sources(home, spec);
    }

    let mut sources = spec
        .default_locations
        .iter()
        .map(|location| {
            let path = location
                .path_components
                .iter()
                .fold(home.to_path_buf(), |path, component| path.join(component));
            let mut source = provider_source_from_location(spec, location, path);
            if spec.provider == CaptureProvider::Trae {
                source.import_support = ProviderImportSupport::Native;
            }
            source
        })
        .collect::<Vec<_>>();

    match spec.provider {
        CaptureProvider::OpenClaw => {
            if let Some(path) = env_path("OPENCLAW_STATE_DIR") {
                sources.push(provider_source_from_parts(
                    spec,
                    path,
                    "openclaw_session_jsonl_tree",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::CodeArtsAgent => {
            if let Some(path) = env_path("XDG_DATA_HOME") {
                sources.push(provider_source_from_parts(
                    spec,
                    path.join("codeartsdoer").join("opencode.db"),
                    "codearts_agent_kernel_sqlite",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::Pi => {
            sources.extend(discover_pi_custom_session_sources(home, spec));
        }
        CaptureProvider::Crush => {
            if let Some(path) = env_path("CRUSH_GLOBAL_DATA") {
                sources.push(crush_db_source(spec, path.join("crush.db")));
            }
            if let Some(path) = env_path("XDG_DATA_HOME") {
                sources.push(crush_db_source(spec, path.join("crush").join("crush.db")));
            }
            for config_path in crush_config_paths(home) {
                if let Some(data_dir) = crush_config_data_dir(&config_path, home) {
                    let relative_base = config_path
                        .parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| home.to_path_buf());
                    let data_dir =
                        resolve_pi_config_path(&data_dir.to_string_lossy(), home, &relative_base);
                    sources.push(crush_db_source(spec, data_dir.join("crush.db")));
                }
            }
            for root in current_dir_ancestors_with(|candidate| {
                candidate.join(".crush").join("crush.db").is_file()
                    || candidate.join("crush.json").is_file()
                    || candidate.join(".crush.json").is_file()
            }) {
                sources.push(crush_db_source(spec, root.join(".crush").join("crush.db")));
                for config_name in ["crush.json", ".crush.json"] {
                    let config_path = root.join(config_name);
                    if let Some(data_dir) = crush_config_data_dir(&config_path, home) {
                        let data_dir =
                            resolve_pi_config_path(&data_dir.to_string_lossy(), home, &root);
                        sources.push(crush_db_source(spec, data_dir.join("crush.db")));
                    }
                }
            }
        }
        CaptureProvider::KiroCli => {
            if let Some(path) = env_path("XDG_DATA_HOME") {
                sources.push(provider_source_from_parts(
                    spec,
                    path.join("kiro-cli").join("data.sqlite3"),
                    "kiro_cli_sqlite",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::Goose => {
            if let Some(path) = env_path("GOOSE_PATH_ROOT") {
                sources.push(goose_db_source(
                    spec,
                    path.join("data").join("sessions").join("sessions.db"),
                ));
            }
            if let Some(path) = env_path("XDG_DATA_HOME") {
                sources.push(goose_db_source(
                    spec,
                    path.join("goose").join("sessions").join("sessions.db"),
                ));
                sources.push(goose_db_source(
                    spec,
                    path.join("Block")
                        .join("goose")
                        .join("sessions")
                        .join("sessions.db"),
                ));
            }
        }
        CaptureProvider::Zed => {
            if let Some(path) = env_path("XDG_DATA_HOME") {
                sources.push(provider_source_from_parts(
                    spec,
                    path.join("zed").join("threads").join("threads.db"),
                    "zed_threads_sqlite",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::Hermes => {
            if let Some(path) = env_path("HERMES_HOME") {
                sources.push(provider_source_from_parts(
                    spec,
                    path.join("state.db"),
                    "hermes_state_sqlite",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::QwenCode => {
            if let Some(path) = env_path_resolved("QWEN_RUNTIME_DIR", home) {
                sources.push(provider_source_from_parts(
                    spec,
                    path.join("projects"),
                    "qwen_code_chat_jsonl_tree",
                    ProviderSourceKind::NativeHistory,
                ));
            }
            if let Some(path) = env_path_resolved("QWEN_HOME", home) {
                sources.push(provider_source_from_parts(
                    spec,
                    path.join("projects"),
                    "qwen_code_chat_jsonl_tree",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::KimiCodeCli => {
            if let Some(path) = env_path_resolved("KIMI_CODE_HOME", home) {
                sources.push(provider_source_from_parts(
                    spec,
                    path,
                    "kimi_code_cli_wire_jsonl_tree",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::AutohandCode => {
            if let Some(path) = env_path_resolved("AUTOHAND_HOME", home) {
                sources.push(provider_source_from_parts(
                    spec,
                    path.join("sessions"),
                    "autohand_code_sessions_jsonl",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::IflowCli => {
            if let Some(path) = env_path_resolved("IFLOW_HOME", home) {
                sources.push(provider_source_from_parts(
                    spec,
                    path.join("projects"),
                    "iflow_cli_session_jsonl_tree",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::Jazz => {
            if let Some(path) = env_path_resolved("JAZZ_HOME", home) {
                sources.push(provider_source_from_parts(
                    spec,
                    path.join("history"),
                    "jazz_history_json",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::Auggie => {}
        CaptureProvider::Eve => {
            if let Some(path) = env_path_resolved("WORKFLOW_LOCAL_DATA_DIR", home) {
                sources.push(eve_workflow_data_source(spec, path));
            }
            for root in current_dir_ancestors_with(|candidate| {
                candidate
                    .join(".workflow-data")
                    .join("streams")
                    .join("runs")
                    .is_dir()
            }) {
                sources.push(eve_workflow_data_source(spec, root.join(".workflow-data")));
            }
        }
        CaptureProvider::Junie => {
            if let Some(path) = env_path_resolved("JUNIE_SESSIONS_DIR", home) {
                sources.push(provider_source_from_parts(
                    spec,
                    path,
                    "junie_session_events_jsonl_tree",
                    ProviderSourceKind::NativeHistory,
                ));
            }
            if let Some(path) = env_path_resolved("JUNIE_HOME", home) {
                sources.push(provider_source_from_parts(
                    spec,
                    path.join("sessions"),
                    "junie_session_events_jsonl_tree",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::Firebender => {
            for root in current_dir_ancestors_with(|candidate| {
                candidate
                    .join(".idea")
                    .join("firebender")
                    .join("chat_history.db")
                    .is_file()
            }) {
                sources.push(provider_source_from_parts(
                    spec,
                    root.join(".idea")
                        .join("firebender")
                        .join("chat_history.db"),
                    "firebender_chat_history_sqlite",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::Kode => {
            for env_name in ["KODE_CONFIG_DIR", "CLAUDE_CONFIG_DIR"] {
                if let Some(path) = env_path_resolved(env_name, home) {
                    sources.push(provider_source_from_parts(
                        spec,
                        path.join("projects"),
                        "kode_session_jsonl_tree",
                        ProviderSourceKind::NativeHistory,
                    ));
                }
            }
        }
        CaptureProvider::MistralVibe => {
            if let Some(path) = env_path_resolved("VIBE_HOME", home) {
                sources.push(provider_source_from_parts(
                    spec,
                    path.join("logs").join("session"),
                    "mistral_vibe_session_jsonl_tree",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::Mux => {
            if let Some(path) = env_path_resolved("MUX_ROOT", home) {
                sources.push(provider_source_from_parts(
                    spec,
                    path.join("sessions"),
                    "mux_session_jsonl_tree",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::Moxby => {
            if let Some(path) = env_path_resolved("MOXBY_STATE_DIR", home) {
                sources.push(provider_source_from_parts(
                    spec,
                    path.join("moxby_chats.db"),
                    "moxby_chats_sqlite",
                    ProviderSourceKind::NativeHistory,
                ));
            }
            if let Some(path) = env_path("XDG_DATA_HOME") {
                sources.push(provider_source_from_parts(
                    spec,
                    path.join("com.moxby.agent").join("moxby_chats.db"),
                    "moxby_chats_sqlite",
                    ProviderSourceKind::NativeHistory,
                ));
            }
            if let Some(path) = env_path("APPDATA") {
                sources.push(provider_source_from_parts(
                    spec,
                    path.join("com.moxby.agent").join("moxby_chats.db"),
                    "moxby_chats_sqlite",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::Terramind => {
            if let Some(path) = env_path("XDG_CONFIG_HOME") {
                sources.push(terramind_db_source(
                    spec,
                    path.join("Nucleus").join("data").join("agents.db"),
                ));
            }
            if let Some(path) = env_path("APPDATA") {
                sources.push(terramind_db_source(
                    spec,
                    path.join("Nucleus").join("data").join("agents.db"),
                ));
            }
            if env::var_os("NUCLEUS_DEV").is_some() {
                sources.push(terramind_db_source(
                    spec,
                    home.join(".config")
                        .join("Nucleus Dev")
                        .join("data")
                        .join("agents.db"),
                ));
                if let Some(path) = env_path("XDG_CONFIG_HOME") {
                    sources.push(terramind_db_source(
                        spec,
                        path.join("Nucleus Dev").join("data").join("agents.db"),
                    ));
                }
                if let Some(path) = env_path("APPDATA") {
                    sources.push(terramind_db_source(
                        spec,
                        path.join("Nucleus Dev").join("data").join("agents.db"),
                    ));
                }
            }
        }
        CaptureProvider::NanoClaw => {
            for root in current_dir_ancestors_with(|candidate| {
                candidate.join("data").join("v2.db").is_file()
                    && candidate.join("data").join("v2-sessions").is_dir()
            }) {
                sources.push(provider_source_from_parts(
                    spec,
                    root,
                    "nanoclaw_project",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::AstrBot => {
            if let Some(path) = env_path("ASTRBOT_ROOT") {
                sources.push(provider_source_from_parts(
                    spec,
                    path.join("data").join("data_v4.db"),
                    "astrbot_data_v4_sqlite",
                    ProviderSourceKind::NativeHistory,
                ));
            }
            for root in current_dir_ancestors_with(|candidate| {
                candidate.join("data").join("data_v4.db").is_file()
            }) {
                sources.push(provider_source_from_parts(
                    spec,
                    root.join("data").join("data_v4.db"),
                    "astrbot_data_v4_sqlite",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::Shelley => {
            if let Some(path) = env_path("SHELLEY_DB") {
                sources.push(provider_source_from_parts(
                    spec,
                    path,
                    "shelley_sqlite",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::Continue => {
            if let Some(path) = env_path("CONTINUE_GLOBAL_DIR") {
                sources.push(provider_source_from_parts(
                    spec,
                    path.join("sessions"),
                    "continue_cli_sessions_json",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::OpenHands => {
            if let Some(path) = env_path("OH_PERSISTENCE_DIR") {
                sources.push(provider_source_from_parts(
                    spec,
                    path,
                    "openhands_file_events",
                    ProviderSourceKind::NativeHistory,
                ));
            }
            if let Some(path) = env_path("FILE_STORE_PATH") {
                sources.push(provider_source_from_parts(
                    spec,
                    path,
                    "openhands_file_events",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::Dexto => {
            for root in current_dir_ancestors_with(|candidate| {
                candidate.join(".dexto").join("database").is_dir()
            }) {
                sources.push(provider_source_from_parts(
                    spec,
                    root.join(".dexto").join("database"),
                    "dexto_sqlite",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::Cline => {
            sources.extend(discover_cline_task_json_sources(home, spec));
        }
        CaptureProvider::RooCode => {
            sources.extend(discover_roo_task_json_sources(home, spec));
        }
        CaptureProvider::Bob => {
            sources.extend(discover_bob_task_json_sources(home, spec));
        }
        CaptureProvider::Pochi => {
            let storage = home.join(".pochi").join("storage");
            if storage.exists() {
                sources.push(provider_source_from_parts(
                    spec,
                    storage,
                    "pochi_livestore_state_sqlite",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::Warp => {
            if let Some(path) = env_path("XDG_STATE_HOME") {
                sources.push(provider_source_from_parts(
                    spec,
                    path.join("warp-terminal").join("warp.sqlite"),
                    "warp_sqlite",
                    ProviderSourceKind::NativeHistory,
                ));
            }
            if let Some(path) = env_path("LOCALAPPDATA") {
                sources.push(provider_source_from_parts(
                    spec,
                    path.join("warp")
                        .join("Warp")
                        .join("data")
                        .join("warp.sqlite"),
                    "warp_sqlite",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::CodeBuddy => {
            if let Some(path) = env_path("LOCALAPPDATA") {
                sources.push(provider_source_from_parts(
                    spec,
                    path.join("CodeBuddyExtension"),
                    "codebuddy_history_json",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::Trae => {
            if let Some(path) = env_path("APPDATA") {
                sources.push(trae_workspace_storage_source(
                    spec,
                    path.join("Trae").join("User").join("workspaceStorage"),
                ));
                sources.push(trae_workspace_storage_source(
                    spec,
                    path.join("Trae CN").join("User").join("workspaceStorage"),
                ));
            }
        }
        CaptureProvider::TinyCloud => {
            if let Some(path) = env_path_resolved("TINYCLOUD_HOME", home) {
                sources.push(provider_source_from_parts(
                    spec,
                    path,
                    "tinycloud_session_jsonl_tree",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        CaptureProvider::Zencoder => {
            if let Some(path) = env_path("XDG_CONFIG_HOME") {
                sources.extend(zencoder_app_data_dirs(path, spec));
            }
            if let Some(path) = env_path("APPDATA") {
                sources.extend(zencoder_app_data_dirs(path, spec));
            }
        }
        CaptureProvider::Zenflow => {
            if let Some(path) = env_path("ZENFLOW_DATA_DIR") {
                sources.push(zenflow_db_source(spec, path.join("db.sqlite")));
            }
            if let Some(path) = env_path("XDG_DATA_HOME") {
                sources.push(zenflow_db_source(
                    spec,
                    path.join("zenflow").join("db.sqlite"),
                ));
            }
            if let Some(path) = env_path("APPDATA") {
                sources.push(zenflow_db_source(
                    spec,
                    path.join("forgoodai")
                        .join("zenflow")
                        .join("data")
                        .join("db.sqlite"),
                ));
            }
        }
        CaptureProvider::AiderDesk => {
            let aider_dir = env::var_os("AIDER_DESK_DIR")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(".aider-desk"));
            for root in current_dir_ancestors_with(|candidate| {
                candidate.join(&aider_dir).join("tasks").is_dir()
            }) {
                sources.push(provider_source_from_parts(
                    spec,
                    root.join(&aider_dir).join("tasks"),
                    "aider_desk_task_context_json",
                    ProviderSourceKind::NativeHistory,
                ));
            }
        }
        _ => {}
    }

    sources
}

fn discover_forgecode_sources(home: &Path, spec: &ProviderSourceSpec) -> Vec<ProviderSource> {
    if let Some(path) = env_path_with_home("FORGE_CONFIG", home) {
        return vec![forgecode_db_source(spec, path.join(".forge.db"))];
    }

    let legacy = home.join("forge");
    let base = if legacy.try_exists().unwrap_or(false) {
        legacy
    } else {
        home.join(".forge")
    };
    vec![forgecode_db_source(spec, base.join(".forge.db"))]
}

fn forgecode_db_source(spec: &ProviderSourceSpec, path: PathBuf) -> ProviderSource {
    provider_source_from_parts(
        spec,
        path,
        "forgecode_sqlite",
        ProviderSourceKind::NativeHistory,
    )
}

fn eve_workflow_data_source(spec: &ProviderSourceSpec, path: PathBuf) -> ProviderSource {
    provider_source_from_parts(
        spec,
        path,
        "eve_workflow_data_streams",
        ProviderSourceKind::NativeHistory,
    )
}

fn discover_kilo_sources(home: &Path, spec: &ProviderSourceSpec) -> Vec<ProviderSource> {
    if let Some(raw) = env::var_os("KILO_DB").filter(|value| !value.is_empty()) {
        if raw.to_string_lossy().trim() == ":memory:" {
            return Vec::new();
        }
        return vec![provider_source_from_parts(
            spec,
            resolve_kilo_db_path(PathBuf::from(raw), home),
            "kilo_sqlite",
            ProviderSourceKind::NativeHistory,
        )];
    }

    let data_dir = kilo_data_dir(home);
    let mut sources = vec![provider_source_from_parts(
        spec,
        data_dir.join("kilo.db"),
        "kilo_sqlite",
        ProviderSourceKind::NativeHistory,
    )];

    if !env_truthy("KILO_DISABLE_CHANNEL_DB") {
        sources.extend(kilo_channel_db_paths(&data_dir).into_iter().map(|path| {
            provider_source_from_parts(spec, path, "kilo_sqlite", ProviderSourceKind::NativeHistory)
        }));
    }

    sources
}

fn resolve_kilo_db_path(path: PathBuf, home: &Path) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        kilo_data_dir(home).join(path)
    }
}

fn kilo_data_dir(home: &Path) -> PathBuf {
    env_path("XDG_DATA_HOME")
        .unwrap_or_else(|| home.join(".local").join("share"))
        .join("kilo")
}

fn kilo_channel_db_paths(data_dir: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let Ok(entries) = fs::read_dir(data_dir) else {
        return paths;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !entry
            .file_type()
            .map_or(false, |file_type| file_type.is_file())
        {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.starts_with("kilo-") && name.ends_with(".db") {
            paths.push(path);
        }
    }
    paths.sort();
    paths
}

fn env_truthy(name: &str) -> bool {
    env::var(name)
        .map(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true"))
        .unwrap_or(false)
}

fn discover_cline_task_json_sources(home: &Path, spec: &ProviderSourceSpec) -> Vec<ProviderSource> {
    let mut sources = Vec::new();
    if let Some(path) = env_path_with_home("CLINE_DATA_DIR", home) {
        sources.push(task_json_source(spec, path));
    }
    if let Some(path) = env_path_with_home("CLINE_DIR", home) {
        sources.push(task_json_source(spec, path.join("data")));
    }
    if let Some(path) = env_path_with_home("CLINE_SESSION_DATA_DIR", home) {
        sources.push(task_json_source(spec, path.clone()));
        if let Some(parent) = path.parent() {
            sources.push(task_json_source(spec, parent.to_path_buf()));
        }
    }
    if let Some(path) = env_path_with_home("CLINE_DB_DATA_DIR", home) {
        if let Some(parent) = path.parent() {
            sources.push(task_json_source(spec, parent.to_path_buf()));
        } else {
            sources.push(task_json_source(spec, path));
        }
    }
    sources
}

fn discover_roo_task_json_sources(home: &Path, spec: &ProviderSourceSpec) -> Vec<ProviderSource> {
    let mut sources = Vec::new();
    for env_name in ["ROO_CODE_DATA_DIR", "ROO_DATA_DIR", "ROO_CLINE_DATA_DIR"] {
        if let Some(path) = env_path_with_home(env_name, home) {
            sources.push(task_json_source(spec, path));
        }
    }
    for settings_path in vscode_settings_paths(home) {
        if let Some(path) = roo_custom_storage_path(&settings_path, home) {
            sources.push(task_json_source(spec, path));
        }
    }
    sources
}

fn discover_bob_task_json_sources(_home: &Path, spec: &ProviderSourceSpec) -> Vec<ProviderSource> {
    let mut sources = Vec::new();
    if let Some(path) = env_path("XDG_CONFIG_HOME") {
        sources.extend(bob_app_data_dirs(path, spec));
    }
    if let Some(path) = env_path("APPDATA") {
        sources.extend(bob_app_data_dirs(path, spec));
    }
    sources
}

fn bob_app_data_dirs(base: PathBuf, spec: &ProviderSourceSpec) -> Vec<ProviderSource> {
    ["IBM Bob", "Bob-IDE"]
        .into_iter()
        .map(|app_name| {
            task_json_source(
                spec,
                base.join(app_name)
                    .join("User")
                    .join("globalStorage")
                    .join("ibm.bob-code"),
            )
        })
        .collect()
}

fn zencoder_app_data_dirs(base: PathBuf, spec: &ProviderSourceSpec) -> Vec<ProviderSource> {
    ["Code", "Code - Insiders", "VSCodium", "Code - OSS"]
        .into_iter()
        .map(|app_name| {
            provider_source_from_parts(
                spec,
                base.join(app_name)
                    .join("User")
                    .join("globalStorage")
                    .join("ZencoderAI.zencoder")
                    .join("zencoder-chat"),
                "zencoder_chat_sessions_json_tree",
                ProviderSourceKind::NativeHistory,
            )
        })
        .collect()
}

fn zenflow_db_source(spec: &ProviderSourceSpec, path: PathBuf) -> ProviderSource {
    provider_source_from_parts(
        spec,
        path,
        ZENFLOW_SQLITE_SOURCE_FORMAT,
        ProviderSourceKind::NativeHistory,
    )
}

fn task_json_source(spec: &ProviderSourceSpec, path: PathBuf) -> ProviderSource {
    provider_source_from_parts(
        spec,
        path,
        match spec.provider {
            CaptureProvider::RooCode => "roo_task_directory_json",
            CaptureProvider::Bob => "bob_task_directory_json",
            _ => "cline_task_directory_json",
        },
        ProviderSourceKind::NativeHistory,
    )
}

fn vscode_settings_paths(home: &Path) -> Vec<PathBuf> {
    let mut paths = vec![
        home.join(".config/Code/User/settings.json"),
        home.join(".config/Code - Insiders/User/settings.json"),
        home.join(".vscode-server/data/User/settings.json"),
        home.join(".vscode-server-insiders/data/User/settings.json"),
    ];
    if let Some(appdata) = env_path("APPDATA") {
        paths.push(appdata.join("Code/User/settings.json"));
        paths.push(appdata.join("Code - Insiders/User/settings.json"));
    }
    paths
}

fn roo_custom_storage_path(settings_path: &Path, home: &Path) -> Option<PathBuf> {
    let settings = fs::read_to_string(settings_path).ok()?;
    let value: Value = serde_json::from_str(&settings).ok()?;
    let path = value
        .get("roo-cline.customStoragePath")
        .or_else(|| value.pointer("/roo-cline/customStoragePath"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())?;
    let relative_base = settings_path.parent().unwrap_or(home);
    Some(resolve_pi_config_path(path, home, relative_base))
}

fn discover_pi_custom_session_sources(
    home: &Path,
    spec: &ProviderSourceSpec,
) -> Vec<ProviderSource> {
    let project_settings_dirs = env::current_dir()
        .ok()
        .map(|current_dir| {
            current_dir
                .ancestors()
                .map(|candidate| candidate.join(".pi"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    discover_pi_custom_session_sources_with_project_settings(home, spec, &project_settings_dirs)
}

fn discover_pi_custom_session_sources_with_project_settings(
    home: &Path,
    spec: &ProviderSourceSpec,
    project_settings_dirs: &[PathBuf],
) -> Vec<ProviderSource> {
    let mut sources = Vec::new();
    if let Some(path) = env_path_with_home("PI_CODING_AGENT_SESSION_DIR", home) {
        sources.push(pi_session_source(spec, path));
    }

    let agent_dir = pi_agent_dir(home);
    if let Some(path) = pi_settings_session_dir(&agent_dir.join("settings.json"), home, &agent_dir)
    {
        sources.push(pi_session_source(spec, path));
    }

    for project_settings_dir in project_settings_dirs {
        if let Some(path) = pi_settings_session_dir(
            &project_settings_dir.join("settings.json"),
            home,
            project_settings_dir,
        ) {
            sources.push(pi_session_source(spec, path));
        }
    }

    sources
}

fn pi_session_source(spec: &ProviderSourceSpec, path: PathBuf) -> ProviderSource {
    provider_source_from_parts(
        spec,
        path,
        "pi_session_jsonl",
        ProviderSourceKind::NativeHistory,
    )
}

fn crush_db_source(spec: &ProviderSourceSpec, path: PathBuf) -> ProviderSource {
    provider_source_from_parts(
        spec,
        path,
        "crush_sqlite",
        ProviderSourceKind::NativeHistory,
    )
}

fn goose_db_source(spec: &ProviderSourceSpec, path: PathBuf) -> ProviderSource {
    provider_source_from_parts(
        spec,
        path,
        "goose_sessions_sqlite",
        ProviderSourceKind::NativeHistory,
    )
}

fn terramind_db_source(spec: &ProviderSourceSpec, path: PathBuf) -> ProviderSource {
    provider_source_from_parts(
        spec,
        path,
        "terramind_agents_sqlite",
        ProviderSourceKind::NativeHistory,
    )
}

fn crush_config_paths(home: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(path) = env_path("CRUSH_GLOBAL_CONFIG") {
        paths.push(path);
    }
    paths.push(home.join(".config").join("crush").join("crush.json"));
    paths
}

fn crush_config_data_dir(config_path: &Path, home: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(config_path).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    let raw = value
        .pointer("/options/data_directory")
        .or_else(|| value.pointer("/options/dataDirectory"))
        .or_else(|| value.get("data_directory"))
        .or_else(|| value.get("dataDirectory"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())?;
    let relative_base = config_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home.to_path_buf());
    Some(resolve_pi_config_path(raw, home, &relative_base))
}

fn pi_agent_dir(home: &Path) -> PathBuf {
    env_path_with_home("PI_CODING_AGENT_DIR", home).unwrap_or_else(|| home.join(".pi/agent"))
}

fn pi_settings_session_dir(
    settings_path: &Path,
    home: &Path,
    relative_base: &Path,
) -> Option<PathBuf> {
    let settings = fs::read_to_string(settings_path).ok()?;
    let value: Value = serde_json::from_str(&settings).ok()?;
    let session_dir = value
        .get("sessionDir")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())?;
    Some(resolve_pi_config_path(session_dir, home, relative_base))
}

fn env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn env_path_with_home(name: &str, home: &Path) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(|value| resolve_home_relative_path(&value.to_string_lossy(), home, home))
}

fn env_path_resolved(name: &str, home: &Path) -> Option<PathBuf> {
    let relative_base = env::current_dir().unwrap_or_else(|_| home.to_path_buf());
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(|value| resolve_home_relative_path(&value.to_string_lossy(), home, &relative_base))
}

fn resolve_pi_config_path(value: &str, home: &Path, relative_base: &Path) -> PathBuf {
    resolve_home_relative_path(value, home, relative_base)
}

fn resolve_home_relative_path(value: &str, home: &Path, relative_base: &Path) -> PathBuf {
    let trimmed = value.trim();
    if trimmed == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = trimmed.strip_prefix("~/") {
        return home.join(rest);
    }
    let path = PathBuf::from(trimmed);
    if path.is_absolute() {
        path
    } else {
        relative_base.join(path)
    }
}

fn current_dir_ancestors_with(matches: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let Ok(current_dir) = env::current_dir() else {
        return Vec::new();
    };
    current_dir
        .ancestors()
        .filter(|candidate| matches(candidate))
        .map(Path::to_path_buf)
        .collect()
}

fn dedupe_sources(sources: Vec<ProviderSource>) -> Vec<ProviderSource> {
    let mut seen = HashSet::new();
    sources
        .into_iter()
        .filter(|source| seen.insert((source.provider, source.path.clone(), source.source_format)))
        .collect()
}

fn provider_source_from_parts(
    spec: &ProviderSourceSpec,
    path: PathBuf,
    source_format: &'static str,
    source_kind: ProviderSourceKind,
) -> ProviderSource {
    let location = ProviderDefaultLocation {
        path_components: &[],
        source_format,
        source_kind,
    };
    provider_source_from_location(spec, &location, path)
}

fn trae_workspace_storage_source(spec: &ProviderSourceSpec, path: PathBuf) -> ProviderSource {
    let mut source = provider_source_from_parts(
        spec,
        path,
        TRAE_STATE_VSCDB_SOURCE_FORMAT,
        ProviderSourceKind::NativeHistory,
    );
    source.import_support = ProviderImportSupport::Native;
    source
}

pub fn provider_source_for_path(provider: CaptureProvider, path: PathBuf) -> ProviderSource {
    let unknown_spec = ProviderSourceSpec {
        provider,
        display_name: "unknown",
        default_locations: &[],
        import_support: ProviderImportSupport::Unsupported,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::None,
        redaction_boundary: ProviderRedactionBoundary::ManualReview,
        unsupported_reason: Some("provider is not registered for native local-history import"),
    };
    let spec = provider_source_spec(provider).unwrap_or(&unknown_spec);
    let exists = path.exists();

    let source_format = match provider {
        CaptureProvider::Codex if path.is_dir() => "codex_session_jsonl_tree",
        CaptureProvider::Codex => {
            if path.file_name().and_then(|name| name.to_str()) == Some("history.jsonl") {
                "codex_history_jsonl"
            } else {
                "codex_session_jsonl"
            }
        }
        CaptureProvider::Pi => "pi_session_jsonl",
        CaptureProvider::Claude => "claude_projects_jsonl_tree",
        CaptureProvider::OpenCode => "opencode_sqlite",
        CaptureProvider::CodeArtsAgent => "codearts_agent_kernel_sqlite",
        CaptureProvider::OpenLoaf if path.is_dir() => "openloaf_chat_jsonl_tree",
        CaptureProvider::OpenLoaf => "openloaf_chat_jsonl",
        CaptureProvider::Kilo => "kilo_sqlite",
        CaptureProvider::KiroCli => "kiro_cli_sqlite",
        CaptureProvider::Crush => "crush_sqlite",
        CaptureProvider::Goose => "goose_sessions_sqlite",
        CaptureProvider::Antigravity => "antigravity_cli_transcript_jsonl_tree",
        CaptureProvider::Gemini => "gemini_cli_chat_recording_jsonl",
        CaptureProvider::Tabnine => "tabnine_cli_chat_recording_jsonl",
        CaptureProvider::Cursor
            if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") =>
        {
            "cursor_agent_transcript_jsonl"
        }
        CaptureProvider::Cursor => "cursor_agent_transcript_jsonl_tree",
        CaptureProvider::Windsurf
            if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") =>
        {
            "windsurf_cascade_hook_transcript_jsonl"
        }
        CaptureProvider::Windsurf => "windsurf_cascade_hook_transcript_jsonl_tree",
        CaptureProvider::Zed => "zed_threads_sqlite",
        CaptureProvider::CopilotCli => "copilot_cli_session_events_jsonl",
        CaptureProvider::FactoryAiDroid => "factory_ai_droid_sessions_jsonl",
        CaptureProvider::QwenCode if path.is_dir() => "qwen_code_chat_jsonl_tree",
        CaptureProvider::QwenCode => "qwen_code_chat_jsonl",
        CaptureProvider::KimiCodeCli if path.is_dir() => "kimi_code_cli_wire_jsonl_tree",
        CaptureProvider::KimiCodeCli => "kimi_code_cli_wire_jsonl",
        CaptureProvider::AutohandCode => "autohand_code_sessions_jsonl",
        CaptureProvider::IflowCli if path.is_dir() => "iflow_cli_session_jsonl_tree",
        CaptureProvider::IflowCli => "iflow_cli_session_jsonl",
        CaptureProvider::Jazz => "jazz_history_json",
        CaptureProvider::Auggie => "auggie_session_json",
        CaptureProvider::Eve => "eve_workflow_data_streams",
        CaptureProvider::Junie if path.is_dir() => "junie_session_events_jsonl_tree",
        CaptureProvider::Junie => "junie_session_events_jsonl",
        CaptureProvider::Firebender => "firebender_chat_history_sqlite",
        CaptureProvider::Kode if path.is_dir() => "kode_session_jsonl_tree",
        CaptureProvider::Kode => "kode_session_jsonl",
        CaptureProvider::Neovate if path.is_dir() => "neovate_session_jsonl_tree",
        CaptureProvider::Neovate => "neovate_session_jsonl",
        CaptureProvider::CommandCode if path.is_dir() => "command_code_session_jsonl_tree",
        CaptureProvider::CommandCode => "command_code_session_jsonl",
        CaptureProvider::ForgeCode => "forgecode_sqlite",
        CaptureProvider::DeepAgents => "deepagents_sessions_sqlite",
        CaptureProvider::MistralVibe if path.is_dir() => "mistral_vibe_session_jsonl_tree",
        CaptureProvider::MistralVibe => "mistral_vibe_session_jsonl",
        CaptureProvider::Mux if path.is_dir() => "mux_session_jsonl_tree",
        CaptureProvider::Mux => "mux_session_jsonl",
        CaptureProvider::Moxby => "moxby_chats_sqlite",
        CaptureProvider::Reasonix if path.is_dir() => "reasonix_session_jsonl_tree",
        CaptureProvider::Reasonix => "reasonix_session_jsonl",
        CaptureProvider::Adal => "adal_session_jsonl",
        CaptureProvider::Terramind => "terramind_agents_sqlite",
        CaptureProvider::RovoDev if path.is_dir() => "rovodev_session_json_tree",
        CaptureProvider::RovoDev => "rovodev_session_json",
        CaptureProvider::OpenClaw => "openclaw_session_jsonl_tree",
        CaptureProvider::Hermes => "hermes_state_sqlite",
        CaptureProvider::NanoClaw => "nanoclaw_project",
        CaptureProvider::AstrBot => "astrbot_data_v4_sqlite",
        CaptureProvider::Shelley => "shelley_sqlite",
        CaptureProvider::Continue => "continue_cli_sessions_json",
        CaptureProvider::OpenHands => "openhands_file_events",
        CaptureProvider::Cline => "cline_task_directory_json",
        CaptureProvider::RooCode => "roo_task_directory_json",
        CaptureProvider::Bob => "bob_task_directory_json",
        CaptureProvider::Dexto => "dexto_sqlite",
        CaptureProvider::Lingma => "lingma_sqlite",
        CaptureProvider::Trae => "trae_state_vscdb",
        CaptureProvider::Qoder if path.is_dir() => "qoder_transcript_jsonl_tree",
        CaptureProvider::Qoder => "qoder_transcript_jsonl",
        CaptureProvider::Pochi => "pochi_livestore_state_sqlite",
        CaptureProvider::Warp => "warp_sqlite",
        CaptureProvider::CortexCode if path.is_dir() => "cortex_code_conversations_json",
        CaptureProvider::CortexCode => {
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".history.jsonl"))
            {
                "cortex_code_history_jsonl"
            } else {
                "cortex_code_session_json"
            }
        }
        CaptureProvider::CodeBuddy => "codebuddy_history_json",
        CaptureProvider::AiderDesk => "aider_desk_task_context_json",
        CaptureProvider::Amp => "amp_threads_export_json",
        CaptureProvider::TinyCloud => "tinycloud_session_jsonl_tree",
        CaptureProvider::Zencoder if path.is_dir() => "zencoder_chat_sessions_json_tree",
        CaptureProvider::Zencoder => {
            if path.file_name().and_then(|name| name.to_str()) == Some("sessions.json") {
                "zencoder_chat_sessions_json_tree"
            } else {
                "zencoder_chat_session_json"
            }
        }
        CaptureProvider::Zenflow => ZENFLOW_SQLITE_SOURCE_FORMAT,
        CaptureProvider::CodeStudio => "codestudio_session_store_sqlite",
        _ => "unsupported",
    };
    let explicit_import_support = spec.import_support;
    let source_kind = if explicit_import_support.is_importable() {
        ProviderSourceKind::NativeHistory
    } else {
        ProviderSourceKind::DetectionOnly
    };

    ProviderSource {
        provider,
        exists,
        path,
        source_format,
        source_kind,
        import_support: explicit_import_support,
        catalog_support: spec.catalog_support,
        status: if matches!(explicit_import_support, ProviderImportSupport::Unsupported) {
            ProviderSourceStatus::Unsupported
        } else if exists {
            ProviderSourceStatus::Available
        } else {
            ProviderSourceStatus::Missing
        },
        raw_retention: spec.raw_retention,
        redaction_boundary: spec.redaction_boundary,
        unsupported_reason: spec.unsupported_reason,
    }
}

fn provider_source_from_location(
    spec: &ProviderSourceSpec,
    location: &ProviderDefaultLocation,
    path: PathBuf,
) -> ProviderSource {
    let path_exists = path.try_exists();
    let exists = path_exists.as_ref().copied().unwrap_or(true);
    let (status, unsupported_reason) =
        if matches!(spec.import_support, ProviderImportSupport::Unsupported) {
            (ProviderSourceStatus::Unsupported, spec.unsupported_reason)
        } else {
            match path_exists {
                Ok(false) => (ProviderSourceStatus::Missing, spec.unsupported_reason),
                Err(_) => (
                    ProviderSourceStatus::Unknown,
                    probe_io_error_reason(spec.provider),
                ),
                Ok(true) => match default_location_import_probe(spec.provider, location, &path) {
                    BoundedProbe::Found => {
                        (ProviderSourceStatus::Available, spec.unsupported_reason)
                    }
                    BoundedProbe::NotFound => (
                        ProviderSourceStatus::Empty,
                        empty_source_reason(spec.provider),
                    ),
                    BoundedProbe::BudgetExhausted => (
                        ProviderSourceStatus::Unknown,
                        unknown_source_reason(spec.provider),
                    ),
                    BoundedProbe::IoError => (
                        ProviderSourceStatus::Unknown,
                        probe_io_error_reason(spec.provider),
                    ),
                },
            }
        };
    ProviderSource {
        provider: spec.provider,
        path,
        exists,
        source_format: location.source_format,
        source_kind: location.source_kind,
        import_support: spec.import_support,
        catalog_support: spec.catalog_support,
        status,
        raw_retention: spec.raw_retention,
        redaction_boundary: spec.redaction_boundary,
        unsupported_reason,
    }
}

fn empty_source_reason(provider: CaptureProvider) -> Option<&'static str> {
    match provider {
        CaptureProvider::Codex => Some("path exists but no Codex JSONL sessions were found"),
        CaptureProvider::Pi => Some("path exists but no Pi session JSONL files were found"),
        CaptureProvider::Claude => {
            Some("path exists but no Claude project JSONL transcripts were found")
        }
        CaptureProvider::OpenCode => Some("path exists but no OpenCode SQLite database was found"),
        CaptureProvider::CodeArtsAgent => {
            Some("path exists but no CodeArts Agent opencode.db database was found")
        }
        CaptureProvider::OpenLoaf => Some(
            "path exists but no OpenLoaf chat-history messages.jsonl session directories were found",
        ),
        CaptureProvider::Kilo => Some("path exists but no Kilo SQLite database was found"),
        CaptureProvider::Crush => Some("path exists but no Crush SQLite database was found"),
        CaptureProvider::Goose => {
            Some("path exists but no Goose sessions SQLite database was found")
        }
        CaptureProvider::Antigravity => {
            Some("path exists but no Antigravity transcript JSONL files were found")
        }
        CaptureProvider::Gemini => Some(
            "path exists but no Gemini CLI chat JSONL transcripts were found under tmp/*/chats",
        ),
        CaptureProvider::Tabnine => Some(
            "path exists but no Tabnine CLI chat JSONL transcripts were found under tmp/*/chats",
        ),
        CaptureProvider::Cursor => {
            Some("path exists but no Cursor agent JSONL transcripts were found")
        }
        CaptureProvider::Zed => Some("path exists but no Zed threads SQLite database was found"),
        CaptureProvider::CopilotCli => {
            Some("path exists but no Copilot CLI session event JSONL files were found")
        }
        CaptureProvider::FactoryAiDroid => {
            Some("path exists but no Factory AI Droid session JSONL files were found")
        }
        CaptureProvider::QwenCode => {
            Some("path exists but no Qwen Code chat JSONL files were found under projects/*/chats")
        }
        CaptureProvider::KimiCodeCli => {
            Some("path exists but no Kimi Code CLI agents/*/wire.jsonl files were found")
        }
        CaptureProvider::AutohandCode => {
            Some("path exists but no Autohand Code session conversation.jsonl files were found")
        }
        CaptureProvider::IflowCli => {
            Some("path exists but no iFlow CLI session-*.jsonl files were found under projects")
        }
        CaptureProvider::Jazz => {
            Some("path exists but no Jazz agent history JSON files were found")
        }
        CaptureProvider::Auggie => {
            Some("path exists but no Auggie session JSON files with chatHistory were found")
        }
        CaptureProvider::Eve => {
            Some("path exists but no Eve .workflow-data default session streams were found")
        }
        CaptureProvider::Firebender => {
            Some("path exists but no Firebender chat_sessions table was found")
        }
        CaptureProvider::Kode => {
            Some("path exists but no Kode session JSONL files were found under projects")
        }
        CaptureProvider::Neovate => {
            Some("path exists but no Neovate session JSONL files were found under projects")
        }
        CaptureProvider::CommandCode => {
            Some("path exists but no Command Code session JSONL files were found under projects")
        }
        CaptureProvider::ForgeCode => {
            Some("path exists but no ForgeCode conversations table was found")
        }
        CaptureProvider::DeepAgents => {
            Some("path exists but no Deep Agents checkpoints/writes tables were found")
        }
        CaptureProvider::MistralVibe => {
            Some("path exists but no Mistral Vibe meta.json/messages.jsonl session directories were found")
        }
        CaptureProvider::Junie => {
            Some("path exists but no Junie index.jsonl entries with session events.jsonl files were found")
        }
        CaptureProvider::Mux => {
            Some("path exists but no Mux chat.jsonl or partial.json session files were found")
        }
        CaptureProvider::Moxby => {
            Some("path exists but no Moxby moxby_chats.db chat_messages schema was found")
        }
        CaptureProvider::Reasonix => {
            Some("path exists but no Reasonix session JSONL files were found")
        }
        CaptureProvider::Adal => {
            Some("path exists but no AdaL conversation_*.jsonl files were found")
        }
        CaptureProvider::Terramind => {
            Some("path exists but no Terramind agents.db chat tables were found")
        }
        CaptureProvider::RovoDev => {
            Some("path exists but no Rovo Dev session_context.json files were found")
        }
        CaptureProvider::OpenClaw => {
            Some("path exists but no OpenClaw agent session JSONL files were found")
        }
        CaptureProvider::Hermes => Some("path exists but no Hermes state.db file was found"),
        CaptureProvider::NanoClaw => {
            Some("path exists but no NanoClaw data/v2.db and data/v2-sessions store was found")
        }
        CaptureProvider::AstrBot => Some("path exists but no AstrBot data/data_v4.db was found"),
        CaptureProvider::Shelley => Some("path exists but no Shelley SQLite database was found"),
        CaptureProvider::Continue => {
            Some("path exists but no Continue CLI session JSON files were found")
        }
        CaptureProvider::OpenHands => {
            Some("path exists but no OpenHands v1_conversations event JSON files were found")
        }
        CaptureProvider::Cline => Some("path exists but no Cline task JSON files were found"),
        CaptureProvider::RooCode => Some("path exists but no Roo Code task JSON files were found"),
        CaptureProvider::Bob => Some("path exists but no IBM Bob IDE task JSON files were found"),
        CaptureProvider::Dexto => Some("path exists but no Dexto SQLite database was found"),
        CaptureProvider::Lingma => {
            Some("path exists but no Lingma chat_record table with the expected columns was found")
        }
        CaptureProvider::Trae => {
            Some("path exists but no Trae workspace state.vscdb with known chat ItemTable keys was found")
        }
        CaptureProvider::Qoder => {
            Some("path exists but no Qoder transcript JSONL files were found")
        }
        CaptureProvider::Pochi => {
            Some("path exists but no Pochi LiveStore state SQLite database was found")
        }
        CaptureProvider::Warp => Some("path exists but no Warp SQLite database was found"),
        CaptureProvider::CortexCode => {
            Some("path exists but no Cortex Code session JSON or history JSONL files were found")
        }
        CaptureProvider::CodeBuddy => {
            Some("path exists but no CodeBuddy history sessions were found")
        }
        CaptureProvider::AiderDesk => {
            Some("path exists but no Aider Desk task context.json files were found")
        }
        CaptureProvider::TinyCloud => {
            Some("path exists but no TinyCloud session JSONL files were found")
        }
        CaptureProvider::Zencoder => {
            Some("path exists but no Zencoder sessions.json or sessions/*.json files were found")
        }
        CaptureProvider::CodeStudio => {
            Some("path exists but no Syncfusion Code Studio session-store.db database was found")
        }
        _ => None,
    }
}

fn unknown_source_reason(provider: CaptureProvider) -> Option<&'static str> {
    match provider {
        CaptureProvider::Codex => {
            Some("path exists but the Codex session transcript probe hit its scan budget")
        }
        CaptureProvider::Pi => {
            Some("path exists but the Pi session transcript probe hit its scan budget")
        }
        CaptureProvider::Claude => {
            Some("path exists but the Claude transcript probe hit its scan budget")
        }
        CaptureProvider::Antigravity => {
            Some("path exists but the Antigravity transcript probe hit its scan budget")
        }
        CaptureProvider::Gemini => {
            Some("path exists but the Gemini transcript probe hit its scan budget")
        }
        CaptureProvider::Tabnine => {
            Some("path exists but the Tabnine transcript probe hit its scan budget")
        }
        CaptureProvider::Cursor => {
            Some("path exists but the Cursor transcript probe hit its scan budget")
        }
        CaptureProvider::OpenLoaf => {
            Some("path exists but the OpenLoaf chat-history probe hit its scan budget")
        }
        CaptureProvider::CodeArtsAgent => None,
        CaptureProvider::Zed => None,
        CaptureProvider::CopilotCli => {
            Some("path exists but the Copilot CLI transcript probe hit its scan budget")
        }
        CaptureProvider::FactoryAiDroid => {
            Some("path exists but the Factory AI Droid transcript probe hit its scan budget")
        }
        CaptureProvider::Continue => {
            Some("path exists but the Continue CLI session probe hit its scan budget")
        }
        CaptureProvider::OpenHands => {
            Some("path exists but the OpenHands event JSON probe hit its scan budget")
        }
        CaptureProvider::QwenCode => {
            Some("path exists but the Qwen Code chat transcript probe hit its scan budget")
        }
        CaptureProvider::KimiCodeCli => {
            Some("path exists but the Kimi Code CLI wire transcript probe hit its scan budget")
        }
        CaptureProvider::AutohandCode => {
            Some("path exists but the Autohand Code session probe hit its scan budget")
        }
        CaptureProvider::IflowCli => {
            Some("path exists but the iFlow CLI session probe hit its scan budget")
        }
        CaptureProvider::Jazz => {
            Some("path exists but the Jazz history JSON probe hit its scan budget")
        }
        CaptureProvider::Auggie => {
            Some("path exists but the Auggie session JSON probe hit its scan budget")
        }
        CaptureProvider::Eve => {
            Some("path exists but the Eve .workflow-data stream probe hit its scan budget")
        }
        CaptureProvider::Firebender => {
            Some("path exists but the Firebender database could not be fully probed")
        }
        CaptureProvider::MistralVibe => {
            Some("path exists but the Mistral Vibe session probe hit its scan budget")
        }
        CaptureProvider::Junie => {
            Some("path exists but the Junie session index probe hit its scan budget")
        }
        CaptureProvider::Mux => Some("path exists but the Mux session probe hit its scan budget"),
        CaptureProvider::Reasonix => {
            Some("path exists but the Reasonix session probe hit its scan budget")
        }
        CaptureProvider::Adal => Some("path exists but the AdaL session probe hit its scan budget"),
        CaptureProvider::CommandCode => {
            Some("path exists but the Command Code session probe hit its scan budget")
        }
        CaptureProvider::Bob => {
            Some("path exists but the IBM Bob IDE task JSON probe hit its scan budget")
        }
        CaptureProvider::RovoDev => {
            Some("path exists but the Rovo Dev session probe hit its scan budget")
        }
        CaptureProvider::OpenClaw => {
            Some("path exists but the OpenClaw transcript probe hit its scan budget")
        }
        CaptureProvider::Cline => {
            Some("path exists but the Cline task JSON probe hit its scan budget")
        }
        CaptureProvider::RooCode => {
            Some("path exists but the Roo Code task JSON probe hit its scan budget")
        }
        CaptureProvider::CodeBuddy => {
            Some("path exists but the CodeBuddy history probe hit its scan budget")
        }
        CaptureProvider::AiderDesk => {
            Some("path exists but the Aider Desk task context probe hit its scan budget")
        }
        CaptureProvider::TinyCloud => {
            Some("path exists but the TinyCloud session probe hit its scan budget")
        }
        CaptureProvider::Zencoder => {
            Some("path exists but the Zencoder session probe hit its scan budget")
        }
        CaptureProvider::CodeStudio => None,
        CaptureProvider::Trae => {
            Some("path exists but the Trae workspaceStorage probe hit its scan budget")
        }
        CaptureProvider::DeepAgents => {
            Some("path exists but the Deep Agents database could not be fully probed")
        }
        _ => None,
    }
}

fn probe_io_error_reason(provider: CaptureProvider) -> Option<&'static str> {
    match provider {
        CaptureProvider::Codex => {
            Some("path exists but Codex session transcripts could not be read; check permissions")
        }
        CaptureProvider::Pi => {
            Some("path exists but Pi session transcripts could not be read; check permissions")
        }
        CaptureProvider::Claude => {
            Some("path exists but Claude project transcripts could not be read; check permissions")
        }
        CaptureProvider::OpenCode => {
            Some("path exists but the OpenCode database could not be read; check permissions")
        }
        CaptureProvider::CodeArtsAgent => {
            Some("path exists but the CodeArts Agent database could not be read; check permissions")
        }
        CaptureProvider::OpenLoaf => Some(
            "path exists but OpenLoaf chat-history files could not be read; check permissions",
        ),
        CaptureProvider::Kilo => {
            Some("path exists but the Kilo database could not be read; check permissions")
        }
        CaptureProvider::KiroCli => {
            Some("path exists but the Kiro CLI database could not be read; check permissions")
        }
        CaptureProvider::Crush => {
            Some("path exists but the Crush database could not be read; check permissions")
        }
        CaptureProvider::Goose => {
            Some("path exists but the Goose sessions database could not be read; check permissions")
        }
        CaptureProvider::Antigravity => {
            Some("path exists but Antigravity transcripts could not be read; check permissions")
        }
        CaptureProvider::Gemini => {
            Some("path exists but Gemini CLI chat transcripts could not be read; check permissions")
        }
        CaptureProvider::Tabnine => {
            Some("path exists but Tabnine CLI chat transcripts could not be read; check permissions")
        }
        CaptureProvider::Cursor => {
            Some("path exists but Cursor agent transcripts could not be read; check permissions")
        }
        CaptureProvider::Zed => {
            Some("path exists but the Zed threads database could not be read; check permissions")
        }
        CaptureProvider::CopilotCli => {
            Some("path exists but Copilot CLI session events could not be read; check permissions")
        }
        CaptureProvider::FactoryAiDroid => {
            Some("path exists but Factory AI Droid sessions could not be read; check permissions")
        }
        CaptureProvider::QwenCode => {
            Some("path exists but Qwen Code chat transcripts could not be read; check permissions")
        }
        CaptureProvider::KimiCodeCli => Some(
            "path exists but Kimi Code CLI wire transcripts could not be read; check permissions",
        ),
        CaptureProvider::AutohandCode => Some(
            "path exists but Autohand Code session transcripts could not be read; check permissions",
        ),
        CaptureProvider::IflowCli => {
            Some("path exists but iFlow CLI session transcripts could not be read; check permissions")
        }
        CaptureProvider::Jazz => {
            Some("path exists but Jazz history JSON files could not be read; check permissions")
        }
        CaptureProvider::Auggie => {
            Some("path exists but Auggie session JSON files could not be read; check permissions")
        }
        CaptureProvider::Eve => {
            Some("path exists but Eve .workflow-data streams could not be read; check permissions")
        }
        CaptureProvider::Junie => {
            Some("path exists but Junie session files could not be read; check permissions")
        }
        CaptureProvider::Firebender => {
            Some("path exists but the Firebender chat history database could not be read; check permissions")
        }
        CaptureProvider::CommandCode => Some(
            "path exists but Command Code session transcripts could not be read; check permissions",
        ),
        CaptureProvider::ForgeCode => {
            Some("path exists but the ForgeCode database could not be read; check permissions")
        }
        CaptureProvider::DeepAgents => {
            Some("path exists but the Deep Agents database could not be read; check permissions")
        }
        CaptureProvider::MistralVibe => {
            Some("path exists but Mistral Vibe session files could not be read; check permissions")
        }
        CaptureProvider::Mux => {
            Some("path exists but Mux session files could not be read; check permissions")
        }
        CaptureProvider::Reasonix => {
            Some("path exists but Reasonix session files could not be read; check permissions")
        }
        CaptureProvider::Adal => {
            Some("path exists but AdaL session files could not be read; check permissions")
        }
        CaptureProvider::Terramind => {
            Some("path exists but the Terramind agents database could not be read; check permissions")
        }
        CaptureProvider::RovoDev => {
            Some("path exists but Rovo Dev session files could not be read; check permissions")
        }
        CaptureProvider::OpenClaw => Some(
            "path exists but OpenClaw session transcripts could not be read; check permissions",
        ),
        CaptureProvider::Hermes => {
            Some("path exists but the Hermes state database could not be read; check permissions")
        }
        CaptureProvider::NanoClaw => {
            Some("path exists but the NanoClaw project store could not be read; check permissions")
        }
        CaptureProvider::AstrBot => {
            Some("path exists but the AstrBot data database could not be read; check permissions")
        }
        CaptureProvider::Shelley => {
            Some("path exists but the Shelley database could not be read; check permissions")
        }
        CaptureProvider::Continue => {
            Some("path exists but Continue CLI sessions could not be read; check permissions")
        }
        CaptureProvider::OpenHands => {
            Some("path exists but OpenHands event JSON files could not be read; check permissions")
        }
        CaptureProvider::Cline => {
            Some("path exists but Cline task JSON files could not be read; check permissions")
        }
        CaptureProvider::RooCode => {
            Some("path exists but Roo Code task JSON files could not be read; check permissions")
        }
        CaptureProvider::Dexto => {
            Some("path exists but the Dexto database could not be read; check permissions")
        }
        CaptureProvider::Lingma => {
            Some("path exists but the Lingma chat_record SQLite database could not be read")
        }
        CaptureProvider::Trae => {
            Some("path exists but Trae workspace state.vscdb files could not be read")
        }
        CaptureProvider::Qoder => {
            Some("path exists but Qoder transcript JSONL files could not be read; check permissions")
        }
        CaptureProvider::Pochi => {
            Some("path exists but the Pochi LiveStore state database could not be read")
        }
        CaptureProvider::CodeBuddy => Some(
            "path exists but CodeBuddy history JSON files could not be read; check permissions",
        ),
        CaptureProvider::AiderDesk => Some(
            "path exists but Aider Desk task context JSON files could not be read; check permissions",
        ),
        CaptureProvider::TinyCloud => {
            Some("path exists but TinyCloud session JSONL files could not be read; check permissions")
        }
        CaptureProvider::Zencoder => {
            Some("path exists but Zencoder session JSON files could not be read; check permissions")
        }
        CaptureProvider::CodeStudio => {
            Some("path exists but the Syncfusion Code Studio database could not be read; check permissions")
        }
        CaptureProvider::Bob => {
            Some("path exists but IBM Bob IDE task JSON files could not be read; check permissions")
        }
        _ => None,
    }
}

fn default_location_import_probe(
    provider: CaptureProvider,
    location: &ProviderDefaultLocation,
    path: &Path,
) -> BoundedProbe {
    match provider {
        CaptureProvider::Codex if location.source_format == "codex_history_jsonl" => {
            path_is_file_probe(path)
        }
        CaptureProvider::Codex => has_jsonl_file_under_matching(path, 10_000, |_| true),
        CaptureProvider::Pi => has_jsonl_file_under_matching(path, 10_000, |_| true),
        CaptureProvider::OpenCode => path_is_file_probe(path),
        CaptureProvider::CodeArtsAgent => path_is_file_probe(path),
        CaptureProvider::OpenLoaf => has_openloaf_session_files(path, 10_000),
        CaptureProvider::Kilo => path_is_file_probe(path),
        CaptureProvider::KiroCli => path_is_file_probe(path),
        CaptureProvider::Crush => path_is_file_probe(path),
        CaptureProvider::Goose => path_is_file_probe(path),
        CaptureProvider::Claude => has_jsonl_file_under_matching(path, 10_000, |_| true),
        CaptureProvider::OpenClaw => has_openclaw_session_jsonl(path, 10_000),
        CaptureProvider::Hermes => path_is_file_probe(path),
        CaptureProvider::NanoClaw => has_nanoclaw_project(path),
        CaptureProvider::AstrBot => path_is_file_probe(path),
        CaptureProvider::Shelley => path_is_file_probe(path),
        CaptureProvider::Continue => has_json_file_under_matching(path, 10_000, |candidate| {
            candidate.file_name().and_then(|name| name.to_str()) != Some("sessions.json")
        }),
        CaptureProvider::OpenHands => has_openhands_event_json(path, 10_000),
        CaptureProvider::Dexto => has_dexto_sqlite_db(path, 10_000),
        CaptureProvider::Pochi => has_pochi_state_db(path, 10_000),
        CaptureProvider::Antigravity => has_jsonl_file_under_matching(path, 10_000, |candidate| {
            matches!(
                candidate.file_name().and_then(|name| name.to_str()),
                Some("transcript_full.jsonl" | "transcript.jsonl")
            )
        }),
        CaptureProvider::Gemini | CaptureProvider::Tabnine => has_gemini_chat_jsonl(path, 10_000),
        CaptureProvider::Cursor => has_jsonl_file_under_matching(path, 10_000, |candidate| {
            path_has_component(candidate, "agent-transcripts")
        }),
        CaptureProvider::Windsurf => has_jsonl_file_under_matching(path, 10_000, |_| true),
        CaptureProvider::Qoder => has_jsonl_file_under_matching(path, 10_000, |candidate| {
            path_has_component(candidate, "transcript")
        }),
        CaptureProvider::Zed => path_is_file_probe(path),
        CaptureProvider::CopilotCli => has_jsonl_file_under_matching(path, 10_000, |candidate| {
            candidate.file_name().and_then(|name| name.to_str()) == Some("events.jsonl")
        }),
        CaptureProvider::FactoryAiDroid => has_jsonl_file_under_matching(path, 10_000, |_| true),
        CaptureProvider::QwenCode => has_jsonl_file_under_matching(path, 10_000, |candidate| {
            path_has_component(candidate, "chats")
        }),
        CaptureProvider::KimiCodeCli => has_jsonl_file_under_matching(path, 10_000, |candidate| {
            candidate.file_name().and_then(|name| name.to_str()) == Some("wire.jsonl")
                && path_has_component(candidate, "agents")
        }),
        CaptureProvider::AutohandCode => has_jsonl_file_under_matching(path, 10_000, |candidate| {
            candidate.file_name().and_then(|name| name.to_str()) == Some("conversation.jsonl")
                && candidate
                    .parent()
                    .is_some_and(|parent| parent.join("metadata.json").is_file())
        }),
        CaptureProvider::IflowCli => has_jsonl_file_under_matching(path, 10_000, |candidate| {
            candidate
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("session-") && name.ends_with(".jsonl"))
        }),
        CaptureProvider::Jazz => has_json_file_under_matching(path, 10_000, |candidate| {
            candidate
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| !name.starts_with(".history-") && name.ends_with(".json"))
        }),
        CaptureProvider::Auggie => has_json_file_under_matching(path, 10_000, |candidate| {
            candidate.extension().and_then(|ext| ext.to_str()) == Some("json")
        }),
        CaptureProvider::Eve => has_eve_workflow_stream(path, 10_000),
        CaptureProvider::Junie => has_junie_session_events(path, 10_000),
        CaptureProvider::Firebender => has_firebender_chat_sessions_table(path),
        CaptureProvider::Kode => has_jsonl_file_under_matching(path, 10_000, |candidate| {
            !path_has_component(candidate, "requests")
                && !path_has_component(candidate, "file-history")
        }),
        CaptureProvider::Neovate => has_jsonl_file_under_matching(path, 10_000, |candidate| {
            !path_has_component(candidate, "requests")
                && !path_has_component(candidate, "file-history")
        }),
        CaptureProvider::CommandCode => has_jsonl_file_under_matching(path, 10_000, |candidate| {
            candidate
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.ends_with(".jsonl")
                        && !name.ends_with(".checkpoints.jsonl")
                        && !name.contains(".checkpoints.")
                        && !name.contains(".prompts.")
                })
        }),
        CaptureProvider::ForgeCode => has_forgecode_conversations_table(path),
        CaptureProvider::DeepAgents => has_deepagents_checkpoint_tables(path),
        CaptureProvider::MistralVibe => has_jsonl_file_under_matching(path, 10_000, |candidate| {
            candidate.file_name().and_then(|name| name.to_str()) == Some("messages.jsonl")
                && candidate
                    .parent()
                    .is_some_and(|parent| parent.join("meta.json").is_file())
        }),
        CaptureProvider::Mux => has_mux_session_files(path, 10_000),
        CaptureProvider::Moxby => has_moxby_chat_tables(path),
        CaptureProvider::Reasonix => has_jsonl_file_under_matching(path, 10_000, |candidate| {
            candidate
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".jsonl") && !name.ends_with(".events.jsonl"))
        }),
        CaptureProvider::Adal => has_adal_session_jsonl(path, 10_000),
        CaptureProvider::Terramind => has_terramind_chat_tables(path),
        CaptureProvider::RovoDev => has_json_file_under_matching(path, 10_000, |candidate| {
            candidate.file_name().and_then(|name| name.to_str()) == Some("session_context.json")
        }),
        CaptureProvider::Cline => has_task_json_file_under_matching(path, 10_000, |name| {
            matches!(
                name,
                "api_conversation_history.json"
                    | "ui_messages.json"
                    | "context_history.json"
                    | "task_metadata.json"
            )
        }),
        CaptureProvider::RooCode => has_task_json_file_under_matching(path, 10_000, |name| {
            matches!(
                name,
                "api_conversation_history.json"
                    | "ui_messages.json"
                    | "history_item.json"
                    | "_index.json"
                    | "claude_messages.json"
            )
        }),
        CaptureProvider::Bob => has_task_json_file_under_matching(path, 10_000, |name| {
            matches!(
                name,
                "api_conversation_history.json" | "ui_messages.json" | "task_metadata.json"
            )
        }),
        CaptureProvider::Lingma => has_lingma_chat_record_table(path),
        CaptureProvider::Trae => has_trae_state_vscdb_chat_history(path, 10_000),
        CaptureProvider::Warp => path_is_file_probe(path),
        CaptureProvider::CortexCode => has_cortex_code_session_files(path, 10_000),
        CaptureProvider::CodeBuddy => has_codebuddy_history_json(path, 10_000),
        CaptureProvider::AiderDesk => {
            has_task_json_file_under_matching(path, 10_000, |name| name == "context.json")
        }
        CaptureProvider::Amp => path_is_file_probe(path),
        CaptureProvider::TinyCloud => has_tinycloud_session_jsonl(path, 10_000),
        CaptureProvider::Zencoder => has_zencoder_session_json(path, 10_000),
        CaptureProvider::Zenflow => path_is_file_probe(path),
        CaptureProvider::CodeStudio => has_codestudio_session_store(path),
        CaptureProvider::Shell
        | CaptureProvider::Git
        | CaptureProvider::Jj
        | CaptureProvider::Gh
        | CaptureProvider::Custom
        | CaptureProvider::Unknown => BoundedProbe::NotFound,
    }
}

fn has_tinycloud_session_jsonl(root: &Path, max_entries: usize) -> BoundedProbe {
    has_jsonl_file_under_matching(root, max_entries, |candidate| {
        candidate
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            == Some("sessions")
    })
}

fn has_zencoder_session_json(root: &Path, max_entries: usize) -> BoundedProbe {
    match path_metadata_probe(root) {
        PathProbe::File => {
            return BoundedProbe::from_bool(
                root.file_name().and_then(|name| name.to_str()) == Some("sessions.json")
                    || (root.extension().and_then(|ext| ext.to_str()) == Some("json")
                        && root
                            .parent()
                            .and_then(Path::file_name)
                            .and_then(|name| name.to_str())
                            == Some("sessions")),
            );
        }
        PathProbe::Dir => {}
        PathProbe::Missing | PathProbe::Other => return BoundedProbe::NotFound,
        PathProbe::IoError => return BoundedProbe::IoError,
    }
    let index_probe = path_is_file_probe(&root.join("sessions.json"));
    if matches!(index_probe, BoundedProbe::Found | BoundedProbe::IoError) {
        return index_probe;
    }
    has_json_file_under_matching(root, max_entries, |candidate| {
        candidate.extension().and_then(|ext| ext.to_str()) == Some("json")
            && candidate
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                == Some("sessions")
    })
}

fn has_codestudio_session_store(path: &Path) -> BoundedProbe {
    match path_is_file_probe(path) {
        BoundedProbe::Found => {}
        other => return other,
    }
    match Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .and_then(|conn| {
        conn.query_row(
            "select \
                (select count(*) from sqlite_schema \
                 where type = 'table' and name = 'schema_version'), \
                (select count(*) from pragma_table_info('sessions') \
                 where name in ('id')), \
                (select count(*) from pragma_table_info('turns') \
                 where name in ('id', 'session_id', 'role', 'content'))",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
    }) {
        Ok((1, 1, 4)) => BoundedProbe::Found,
        Ok(_) => BoundedProbe::NotFound,
        Err(_) => BoundedProbe::IoError,
    }
}

fn has_gemini_chat_jsonl(root: &Path, max_entries: usize) -> BoundedProbe {
    let tmp = root.join("tmp");
    match path_is_dir_probe(&tmp) {
        BoundedProbe::Found => {}
        BoundedProbe::IoError => return BoundedProbe::IoError,
        _ => return BoundedProbe::NotFound,
    }
    has_jsonl_file_under_matching(&tmp, max_entries, |path| path_has_component(path, "chats"))
}

fn has_dexto_sqlite_db(root: &Path, max_entries: usize) -> BoundedProbe {
    match path_metadata_probe(root) {
        PathProbe::File => return has_dexto_sqlite_store(root),
        PathProbe::Dir => {}
        PathProbe::Missing | PathProbe::Other => return BoundedProbe::NotFound,
        PathProbe::IoError => return BoundedProbe::IoError,
    }

    let mut stack = vec![root.to_path_buf()];
    let mut visited = 0usize;
    let mut saw_io_error = false;
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => return BoundedProbe::IoError,
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            visited = visited.saturating_add(1);
            if visited > max_entries {
                return BoundedProbe::BudgetExhausted;
            }

            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file()
                && path.extension().and_then(|ext| ext.to_str()) == Some("db")
            {
                match has_dexto_sqlite_store(&path) {
                    BoundedProbe::Found => return BoundedProbe::Found,
                    BoundedProbe::IoError => saw_io_error = true,
                    BoundedProbe::NotFound | BoundedProbe::BudgetExhausted => {}
                }
            }
        }
    }

    if saw_io_error {
        BoundedProbe::IoError
    } else {
        BoundedProbe::NotFound
    }
}

fn has_dexto_sqlite_store(path: &Path) -> BoundedProbe {
    match path_is_file_probe(path) {
        BoundedProbe::Found => {}
        other => return other,
    }
    match Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .and_then(|conn| {
        conn.query_row(
            "select \
                (select count(*) from pragma_table_info('kv_store') \
                 where name in ('key', 'value')), \
                (select count(*) from pragma_table_info('list_store') \
                 where name in ('key', 'value', 'sequence'))",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
    }) {
        Ok((2, 3)) => BoundedProbe::Found,
        Ok(_) => BoundedProbe::NotFound,
        Err(_) => BoundedProbe::IoError,
    }
}

fn has_openloaf_session_files(root: &Path, max_entries: usize) -> BoundedProbe {
    match path_metadata_probe(root) {
        PathProbe::File => {
            return BoundedProbe::from_bool(
                root.file_name().and_then(|name| name.to_str()) == Some("messages.jsonl"),
            );
        }
        PathProbe::Dir => {}
        PathProbe::Missing | PathProbe::Other => return BoundedProbe::NotFound,
        PathProbe::IoError => return BoundedProbe::IoError,
    }

    if openloaf_is_projects_root(root) {
        return has_openloaf_project_chat_histories(root, max_entries);
    }

    has_jsonl_file_under_matching(root, max_entries, |candidate| {
        candidate.file_name().and_then(|name| name.to_str()) == Some("messages.jsonl")
            && path_has_component(candidate, "chat-history")
    })
}

fn openloaf_is_projects_root(path: &Path) -> bool {
    path.file_name().and_then(|name| name.to_str()) == Some("projects")
        && path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            == Some("OpenLoafData")
}

fn has_openloaf_project_chat_histories(root: &Path, max_entries: usize) -> BoundedProbe {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(_) => return BoundedProbe::IoError,
    };
    let mut visited = 0usize;
    let mut saw_budget = false;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        visited = visited.saturating_add(1);
        if visited > max_entries {
            return BoundedProbe::BudgetExhausted;
        }
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };
        if !file_type.is_dir() {
            continue;
        }
        match has_openloaf_session_files(&path.join(".openloaf").join("chat-history"), max_entries)
        {
            BoundedProbe::Found => return BoundedProbe::Found,
            BoundedProbe::BudgetExhausted => saw_budget = true,
            BoundedProbe::IoError | BoundedProbe::NotFound => {}
        }
    }
    if saw_budget {
        BoundedProbe::BudgetExhausted
    } else {
        BoundedProbe::NotFound
    }
}

fn has_adal_session_jsonl(root: &Path, max_entries: usize) -> BoundedProbe {
    has_jsonl_file_under_matching(root, max_entries, |candidate| {
        let Some(name) = candidate.file_name().and_then(|name| name.to_str()) else {
            return false;
        };
        if !name.starts_with("conversation_") || !name.ends_with(".jsonl") {
            return false;
        }
        fs::metadata(candidate)
            .map(|metadata| metadata.len() > 0)
            .unwrap_or(false)
    })
}

fn has_firebender_chat_sessions_table(path: &Path) -> BoundedProbe {
    let db_path = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => path.to_path_buf(),
        Ok(metadata) if metadata.file_type().is_dir() => path
            .join(".idea")
            .join("firebender")
            .join("chat_history.db"),
        Ok(_) => return BoundedProbe::NotFound,
        Err(err) if err.kind() == ErrorKind::NotFound => return BoundedProbe::NotFound,
        Err(_) => return BoundedProbe::IoError,
    };
    match path_is_file_probe(&db_path) {
        BoundedProbe::Found => {}
        other => return other,
    }
    match Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .and_then(|conn| {
        conn.query_row(
            "select count(*) from sqlite_schema where type = 'table' and name = 'chat_sessions'",
            [],
            |row| row.get::<_, i64>(0),
        )
    }) {
        Ok(count) if count > 0 => BoundedProbe::Found,
        Ok(_) => BoundedProbe::NotFound,
        Err(_) => BoundedProbe::IoError,
    }
}

fn has_eve_workflow_stream(root: &Path, max_entries: usize) -> BoundedProbe {
    let data_dir = if root.file_name().and_then(|name| name.to_str()) == Some(".workflow-data") {
        root.to_path_buf()
    } else {
        let nested = root.join(".workflow-data");
        if nested.is_dir() {
            nested
        } else {
            root.to_path_buf()
        }
    };

    match path_is_dir_probe(&data_dir) {
        BoundedProbe::Found => {}
        other => return other,
    }
    let runs_dir = data_dir.join("streams").join("runs");
    let chunks_dir = data_dir.join("streams").join("chunks");
    match (path_is_dir_probe(&runs_dir), path_is_dir_probe(&chunks_dir)) {
        (BoundedProbe::Found, BoundedProbe::Found) => {}
        (BoundedProbe::IoError, _) | (_, BoundedProbe::IoError) => return BoundedProbe::IoError,
        _ => return BoundedProbe::NotFound,
    }

    let entries = match fs::read_dir(&runs_dir) {
        Ok(entries) => entries,
        Err(_) => return BoundedProbe::IoError,
    };
    let mut visited = 0usize;
    let mut candidate_streams = HashSet::new();
    let mut saw_read_error = false;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        visited = visited.saturating_add(1);
        if visited > max_entries {
            return BoundedProbe::BudgetExhausted;
        }
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };
        if !file_type.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Some(stream_name) = eve_default_workflow_stream_name(&path) else {
            continue;
        };
        match eve_stream_map_contains(&path, &stream_name) {
            Ok(true) => {
                candidate_streams.insert(stream_name);
            }
            Ok(false) => {}
            Err(_) => saw_read_error = true,
        }
    }
    if candidate_streams.is_empty() {
        return if saw_read_error {
            BoundedProbe::IoError
        } else {
            BoundedProbe::NotFound
        };
    }

    let entries = match fs::read_dir(&chunks_dir) {
        Ok(entries) => entries,
        Err(_) => return BoundedProbe::IoError,
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        visited = visited.saturating_add(1);
        if visited > max_entries {
            return BoundedProbe::BudgetExhausted;
        }
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };
        if !file_type.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if candidate_streams.iter().any(|stream_name| {
            name.starts_with(&format!("{stream_name}-"))
                && (name.ends_with(".bin") || name.ends_with(".json"))
        }) {
            return BoundedProbe::Found;
        }
    }

    BoundedProbe::NotFound
}

fn eve_default_workflow_stream_name(stream_map_path: &Path) -> Option<String> {
    let run_id = stream_map_path
        .file_stem()
        .and_then(|stem| stem.to_str())?
        .split('.')
        .next()
        .unwrap_or_default();
    let suffix = run_id.strip_prefix("wrun_")?;
    if suffix.is_empty() {
        None
    } else {
        Some(format!("strm_{suffix}_user"))
    }
}

fn eve_stream_map_contains(path: &Path, stream_name: &str) -> std::io::Result<bool> {
    let text = fs::read_to_string(path)?;
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return Ok(false);
    };
    Ok(value
        .get("streams")
        .and_then(Value::as_array)
        .is_some_and(|streams| {
            streams
                .iter()
                .any(|stream| stream.as_str() == Some(stream_name))
        }))
}

fn has_junie_session_events(root: &Path, max_entries: usize) -> BoundedProbe {
    match path_metadata_probe(root) {
        PathProbe::File => {
            return BoundedProbe::from_bool(
                root.file_name().and_then(|name| name.to_str()) == Some("events.jsonl"),
            );
        }
        PathProbe::Dir => {}
        PathProbe::Missing | PathProbe::Other => return BoundedProbe::NotFound,
        PathProbe::IoError => return BoundedProbe::IoError,
    }

    if path_is_file_probe(&root.join("events.jsonl")) == BoundedProbe::Found {
        return BoundedProbe::Found;
    }

    let index_path = root.join("index.jsonl");
    match path_is_file_probe(&index_path) {
        BoundedProbe::Found => {}
        BoundedProbe::NotFound => return BoundedProbe::NotFound,
        other => return other,
    }

    let text = match fs::read_to_string(&index_path) {
        Ok(text) => text,
        Err(_) => return BoundedProbe::IoError,
    };
    let mut visited = 0usize;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        visited = visited.saturating_add(1);
        if visited > max_entries {
            return BoundedProbe::BudgetExhausted;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(session_id) = value.get("sessionId").and_then(Value::as_str) else {
            continue;
        };
        if !junie_session_id_is_safe(session_id) {
            continue;
        }
        match path_is_file_probe(&root.join(session_id).join("events.jsonl")) {
            BoundedProbe::Found => return BoundedProbe::Found,
            BoundedProbe::IoError => return BoundedProbe::IoError,
            BoundedProbe::NotFound | BoundedProbe::BudgetExhausted => {}
        }
    }
    BoundedProbe::NotFound
}

fn junie_session_id_is_safe(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id != "."
        && session_id != ".."
        && !session_id.contains('/')
        && !session_id.contains('\\')
}

fn has_cortex_code_session_files(root: &Path, max_entries: usize) -> BoundedProbe {
    let json_probe = has_json_file_under_matching(root, max_entries, |candidate| {
        candidate
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| !name.ends_with(".history.json"))
    });
    if json_probe == BoundedProbe::Found {
        return json_probe;
    }
    let jsonl_probe = has_jsonl_file_under_matching(root, max_entries, |candidate| {
        candidate
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".history.jsonl"))
    });
    if jsonl_probe == BoundedProbe::Found {
        return jsonl_probe;
    }
    if matches!(
        (json_probe, jsonl_probe),
        (BoundedProbe::IoError, _) | (_, BoundedProbe::IoError)
    ) {
        return BoundedProbe::IoError;
    }
    if matches!(
        (json_probe, jsonl_probe),
        (BoundedProbe::BudgetExhausted, _) | (_, BoundedProbe::BudgetExhausted)
    ) {
        return BoundedProbe::BudgetExhausted;
    }
    BoundedProbe::NotFound
}

fn has_forgecode_conversations_table(path: &Path) -> BoundedProbe {
    match path_is_file_probe(path) {
        BoundedProbe::Found => {}
        other => return other,
    }
    match Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .and_then(|conn| {
        conn.query_row(
            "select count(*) from sqlite_schema where type = 'table' and name = 'conversations'",
            [],
            |row| row.get::<_, i64>(0),
        )
    }) {
        Ok(count) if count > 0 => BoundedProbe::Found,
        Ok(_) => BoundedProbe::NotFound,
        Err(_) => BoundedProbe::IoError,
    }
}

fn has_terramind_chat_tables(path: &Path) -> BoundedProbe {
    match path_is_file_probe(path) {
        BoundedProbe::Found => {}
        other => return other,
    }
    match Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .and_then(|conn| {
        conn.query_row(
            "select count(*) from sqlite_schema where type = 'table' and name in ('projects', 'chats', 'sub_chats')",
            [],
            |row| row.get::<_, i64>(0),
        )
    }) {
        Ok(3) => BoundedProbe::Found,
        Ok(_) => BoundedProbe::NotFound,
        Err(_) => BoundedProbe::IoError,
    }
}

fn has_moxby_chat_tables(path: &Path) -> BoundedProbe {
    match path_is_file_probe(path) {
        BoundedProbe::Found => {}
        other => return other,
    }
    match Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .and_then(|conn| {
        conn.query_row(
            "select \
             (select count(*) from sqlite_schema where type = 'table' and name in ('chats', 'chat_threads', 'chat_messages')) as table_count, \
             (select count(*) from pragma_table_info('chat_messages') \
                where name in ('id', 'chat_id', 'thread_id', 'message_index', 'role', 'content_json', 'text', 'created_at', 'updated_at')) as column_count",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
    }) {
        Ok((3, count)) if count >= 9 => BoundedProbe::Found,
        Ok(_) => BoundedProbe::NotFound,
        Err(_) => BoundedProbe::IoError,
    }
}

fn has_lingma_chat_record_table(path: &Path) -> BoundedProbe {
    match path_is_file_probe(path) {
        BoundedProbe::Found => {}
        other => return other,
    }
    match Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .and_then(|conn| {
        conn.query_row(
            "select count(*) from pragma_table_info('chat_record') \
             where name in ('session_id', 'request_id', 'chat_prompt', 'summary', \
                            'error_result', 'gmt_create', 'extra')",
            [],
            |row| row.get::<_, i64>(0),
        )
    }) {
        Ok(count) if count >= 7 => BoundedProbe::Found,
        Ok(_) => BoundedProbe::NotFound,
        Err(_) => BoundedProbe::IoError,
    }
}

fn has_trae_state_vscdb_chat_history(root: &Path, max_entries: usize) -> BoundedProbe {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_symlink() => return BoundedProbe::NotFound,
        Ok(metadata) if metadata.is_file() => {
            if root.file_name().and_then(|name| name.to_str()) != Some("state.vscdb") {
                return BoundedProbe::NotFound;
            }
            return has_trae_state_vscdb_chat_keys(root);
        }
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return BoundedProbe::NotFound,
        Err(err) if err.kind() == ErrorKind::NotFound => return BoundedProbe::NotFound,
        Err(_) => return BoundedProbe::IoError,
    }

    let direct = root.join("state.vscdb");
    if direct.is_file() {
        return has_trae_state_vscdb_chat_keys(&direct);
    }

    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(_) => return BoundedProbe::IoError,
    };
    let mut visited = 0usize;
    let mut saw_io_error = false;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                saw_io_error = true;
                continue;
            }
        };
        visited = visited.saturating_add(1);
        if visited > max_entries {
            return BoundedProbe::BudgetExhausted;
        }
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => {
                saw_io_error = true;
                continue;
            }
        };
        if !file_type.is_dir() {
            continue;
        }
        let candidate = entry.path().join("state.vscdb");
        if !candidate.is_file() {
            continue;
        }
        match has_trae_state_vscdb_chat_keys(&candidate) {
            BoundedProbe::Found => return BoundedProbe::Found,
            BoundedProbe::IoError => saw_io_error = true,
            BoundedProbe::NotFound | BoundedProbe::BudgetExhausted => {}
        }
    }

    if saw_io_error {
        BoundedProbe::IoError
    } else {
        BoundedProbe::NotFound
    }
}

fn has_trae_state_vscdb_chat_keys(path: &Path) -> BoundedProbe {
    match path_is_file_probe(path) {
        BoundedProbe::Found => {}
        other => return other,
    }
    match Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .and_then(|conn| {
        let (table_count, column_count) = conn.query_row(
            "select \
                (select count(*) from sqlite_schema where type = 'table' and name = 'ItemTable'), \
                (select count(*) from pragma_table_info('ItemTable') where name in ('key', 'value'))",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )?;
        if table_count != 1 || column_count < 2 {
            return Ok(false);
        }

        let key_count = conn.query_row(
            "select count(*) from ItemTable \
             where [key] in (
                'memento/icube-ai-agent-storage',
                'icube-ai-agent-storage-input-history',
                'chat.ChatSessionStore.index',
                'ChatStore',
                'memento/icube-ai-chat-storage-7467774676505887760',
                'memento/icube-ai-ng-chat-storage-7467774676505887760'
             ) and length(trim(cast(coalesce(value, '') as text))) > 0",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(key_count > 0)
    }) {
        Ok(true) => BoundedProbe::Found,
        Ok(false) => BoundedProbe::NotFound,
        Err(_) => BoundedProbe::IoError,
    }
}

fn has_deepagents_checkpoint_tables(path: &Path) -> BoundedProbe {
    match path_is_file_probe(path) {
        BoundedProbe::Found => {}
        other => return other,
    }
    match Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .and_then(|conn| {
        conn.query_row(
            "select count(*) from sqlite_schema \
             where type = 'table' and name in ('checkpoints', 'writes')",
            [],
            |row| row.get::<_, i64>(0),
        )
    }) {
        Ok(2) => BoundedProbe::Found,
        Ok(_) => BoundedProbe::NotFound,
        Err(_) => BoundedProbe::IoError,
    }
}

fn has_openclaw_session_jsonl(root: &Path, max_entries: usize) -> BoundedProbe {
    match path_metadata_probe(root) {
        PathProbe::File => {
            return BoundedProbe::from_bool(
                root.extension().and_then(|ext| ext.to_str()) == Some("jsonl"),
            );
        }
        PathProbe::Dir => {}
        PathProbe::Missing | PathProbe::Other => return BoundedProbe::NotFound,
        PathProbe::IoError => return BoundedProbe::IoError,
    }
    let agents = root.join("agents");
    match path_is_dir_probe(&agents) {
        BoundedProbe::Found => {
            return has_jsonl_file_under_matching(&agents, max_entries, |path| {
                path_has_component(path, "sessions")
            });
        }
        BoundedProbe::IoError => return BoundedProbe::IoError,
        _ => {}
    }
    has_jsonl_file_under_matching(root, max_entries, |path| {
        path_has_component(path, "sessions")
    })
}

fn has_mux_session_files(root: &Path, max_entries: usize) -> BoundedProbe {
    match has_jsonl_file_under_matching(root, max_entries, |candidate| {
        candidate.file_name().and_then(|name| name.to_str()) == Some("chat.jsonl")
    }) {
        BoundedProbe::Found => BoundedProbe::Found,
        BoundedProbe::IoError => BoundedProbe::IoError,
        _ => has_json_file_under_matching(root, max_entries, |candidate| {
            candidate.file_name().and_then(|name| name.to_str()) == Some("partial.json")
        }),
    }
}

fn has_openhands_event_json(root: &Path, max_entries: usize) -> BoundedProbe {
    has_json_file_under_matching(root, max_entries, |path| {
        path_has_component(path, "v1_conversations")
    })
}

fn has_codebuddy_history_json(root: &Path, max_entries: usize) -> BoundedProbe {
    has_json_file_under_matching(root, max_entries, |path| {
        path.file_name().and_then(|name| name.to_str()) == Some("index.json")
            && path_has_component(path, "history")
    })
}

fn has_pochi_state_db(root: &Path, max_entries: usize) -> BoundedProbe {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_symlink() => return BoundedProbe::NotFound,
        Ok(metadata) if metadata.is_file() => {
            if !root
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("state") && name.ends_with(".db"))
            {
                return BoundedProbe::NotFound;
            }
            return has_pochi_livestore_state_db(root);
        }
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return BoundedProbe::NotFound,
        Err(err) if err.kind() == ErrorKind::NotFound => return BoundedProbe::NotFound,
        Err(_) => return BoundedProbe::IoError,
    }

    let mut saw_io_error = false;
    let mut visited = 0usize;
    let mut stack = vec![(root.to_path_buf(), true)];
    while let Some((dir, is_root)) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) if is_root => return BoundedProbe::IoError,
            Err(_) => continue,
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            visited = visited.saturating_add(1);
            if visited > max_entries {
                return BoundedProbe::BudgetExhausted;
            }
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };
            if file_type.is_dir() {
                stack.push((path, false));
            } else if file_type.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("state") && name.ends_with(".db"))
            {
                match has_pochi_livestore_state_db(&path) {
                    BoundedProbe::Found => return BoundedProbe::Found,
                    BoundedProbe::IoError => saw_io_error = true,
                    _ => {}
                }
            }
        }
    }
    if saw_io_error {
        BoundedProbe::IoError
    } else {
        BoundedProbe::NotFound
    }
}

fn has_pochi_livestore_state_db(path: &Path) -> BoundedProbe {
    match path_is_file_probe(path) {
        BoundedProbe::Found => {}
        other => return other,
    }
    match Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .and_then(|conn| {
        conn.query_row(
            "select \
                (select count(*) from pragma_table_info('tasks') where name in ('id')), \
                (select count(*) from pragma_table_info('messages') where name in ('id', 'taskId', 'data'))",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
    }) {
        Ok((1, 3)) => BoundedProbe::Found,
        Ok(_) => BoundedProbe::NotFound,
        Err(_) => BoundedProbe::IoError,
    }
}

fn has_nanoclaw_project(root: &Path) -> BoundedProbe {
    match (
        path_is_file_probe(&root.join("data").join("v2.db")),
        path_is_dir_probe(&root.join("data").join("v2-sessions")),
    ) {
        (BoundedProbe::Found, BoundedProbe::Found) => BoundedProbe::Found,
        (BoundedProbe::IoError, _) | (_, BoundedProbe::IoError) => BoundedProbe::IoError,
        _ => BoundedProbe::NotFound,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BoundedProbe {
    Found,
    NotFound,
    BudgetExhausted,
    IoError,
}

impl BoundedProbe {
    fn from_bool(value: bool) -> Self {
        if value {
            Self::Found
        } else {
            Self::NotFound
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathProbe {
    File,
    Dir,
    Other,
    Missing,
    IoError,
}

fn path_metadata_probe(path: &Path) -> PathProbe {
    match path.metadata() {
        Ok(metadata) if metadata.is_file() => PathProbe::File,
        Ok(metadata) if metadata.is_dir() => PathProbe::Dir,
        Ok(_) => PathProbe::Other,
        Err(err) if err.kind() == ErrorKind::NotFound => PathProbe::Missing,
        Err(_) => PathProbe::IoError,
    }
}

fn path_is_file_probe(path: &Path) -> BoundedProbe {
    match path_metadata_probe(path) {
        PathProbe::File => BoundedProbe::Found,
        PathProbe::IoError => BoundedProbe::IoError,
        _ => BoundedProbe::NotFound,
    }
}

fn path_is_dir_probe(path: &Path) -> BoundedProbe {
    match path_metadata_probe(path) {
        PathProbe::Dir => BoundedProbe::Found,
        PathProbe::IoError => BoundedProbe::IoError,
        _ => BoundedProbe::NotFound,
    }
}

fn has_jsonl_file_under_matching(
    root: &Path,
    max_entries: usize,
    matches_path: impl Fn(&Path) -> bool,
) -> BoundedProbe {
    has_file_with_extension_under_matching(root, "jsonl", max_entries, matches_path)
}

fn has_json_file_under_matching(
    root: &Path,
    max_entries: usize,
    matches_path: impl Fn(&Path) -> bool,
) -> BoundedProbe {
    has_file_with_extension_under_matching(root, "json", max_entries, matches_path)
}

fn has_file_with_extension_under_matching(
    root: &Path,
    extension: &str,
    max_entries: usize,
    matches_path: impl Fn(&Path) -> bool,
) -> BoundedProbe {
    match path_metadata_probe(root) {
        PathProbe::File => {
            return if root.extension().and_then(|ext| ext.to_str()) == Some(extension)
                && matches_path(root)
            {
                BoundedProbe::Found
            } else {
                BoundedProbe::NotFound
            };
        }
        PathProbe::Dir => {}
        PathProbe::Missing | PathProbe::Other => return BoundedProbe::NotFound,
        PathProbe::IoError => return BoundedProbe::IoError,
    }

    let mut visited = 0usize;
    let mut stack = vec![(root.to_path_buf(), true)];
    while let Some((dir, is_root)) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) if is_root => return BoundedProbe::IoError,
            Err(_) => continue,
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            visited = visited.saturating_add(1);
            if visited > max_entries {
                return BoundedProbe::BudgetExhausted;
            }
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };
            if file_type.is_dir() {
                stack.push((path, false));
            } else if file_type.is_file()
                && path.extension().and_then(|ext| ext.to_str()) == Some(extension)
                && matches_path(&path)
            {
                return BoundedProbe::Found;
            }
        }
    }
    BoundedProbe::NotFound
}

fn has_task_json_file_under_matching(
    root: &Path,
    max_entries: usize,
    matches_name: impl Fn(&str) -> bool,
) -> BoundedProbe {
    match path_metadata_probe(root) {
        PathProbe::File => {
            return BoundedProbe::from_bool(
                root.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| matches_name(name)),
            );
        }
        PathProbe::Dir => {}
        PathProbe::Missing | PathProbe::Other => return BoundedProbe::NotFound,
        PathProbe::IoError => return BoundedProbe::IoError,
    }

    let mut visited = 0usize;
    let mut stack = vec![(root.to_path_buf(), true)];
    while let Some((dir, is_root)) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) if is_root => return BoundedProbe::IoError,
            Err(_) => continue,
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            visited = visited.saturating_add(1);
            if visited > max_entries {
                return BoundedProbe::BudgetExhausted;
            }
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };
            if file_type.is_dir() {
                stack.push((path, false));
            } else if file_type.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| matches_name(name))
            {
                return BoundedProbe::Found;
            }
        }
    }
    BoundedProbe::NotFound
}

fn path_has_component(path: &Path, expected: &str) -> bool {
    path.components()
        .any(|component| component.as_os_str().to_str() == Some(expected))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        name: &'static str,
        original: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(name: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let original = env::var_os(name);
            env::set_var(name, value);
            Self { name, original }
        }

        fn remove(name: &'static str) -> Self {
            let original = env::var_os(name);
            env::remove_var(name);
            Self { name, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(value) = &self.original {
                env::set_var(self.name, value);
            } else {
                env::remove_var(self.name);
            }
        }
    }

    struct CwdGuard {
        original: PathBuf,
    }

    impl CwdGuard {
        fn set(path: &Path) -> Self {
            let original = env::current_dir().unwrap();
            env::set_current_dir(path).unwrap();
            Self { original }
        }
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            env::set_current_dir(&self.original).unwrap();
        }
    }

    #[test]
    fn gemini_default_source_is_empty_until_chat_transcripts_exist() {
        let temp = tempfile::tempdir().unwrap();
        let gemini = temp.path().join(".gemini");
        std::fs::create_dir_all(&gemini).unwrap();

        let source = discover_provider_sources(temp.path())
            .into_iter()
            .find(|source| source.provider == CaptureProvider::Gemini)
            .unwrap();
        assert!(source.exists);
        assert_eq!(source.status, ProviderSourceStatus::Empty);
        assert_eq!(source.import_support, ProviderImportSupport::Native);
        assert!(source
            .unsupported_reason
            .unwrap()
            .contains("no Gemini CLI chat JSONL transcripts"));

        let chats = gemini.join("tmp/project/chats");
        std::fs::create_dir_all(&chats).unwrap();
        std::fs::write(chats.join("session.jsonl"), "{}\n").unwrap();

        let source = discover_provider_sources(temp.path())
            .into_iter()
            .find(|source| source.provider == CaptureProvider::Gemini)
            .unwrap();
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.unsupported_reason, None);
    }

    #[test]
    fn tabnine_default_source_is_empty_until_chat_transcripts_exist() {
        let temp = tempfile::tempdir().unwrap();
        let tabnine = temp.path().join(".tabnine/agent");
        std::fs::create_dir_all(&tabnine).unwrap();

        let source = discover_provider_sources(temp.path())
            .into_iter()
            .find(|source| source.provider == CaptureProvider::Tabnine)
            .unwrap();
        assert!(source.exists);
        assert_eq!(source.status, ProviderSourceStatus::Empty);
        assert_eq!(source.import_support, ProviderImportSupport::Native);
        assert!(source
            .unsupported_reason
            .unwrap()
            .contains("no Tabnine CLI chat JSONL transcripts"));

        let chats = tabnine.join("tmp/project/chats");
        std::fs::create_dir_all(&chats).unwrap();
        std::fs::write(
            chats.join("session-2026-07-05T12-00-00000000.jsonl"),
            "{}\n",
        )
        .unwrap();

        let source = discover_provider_sources(temp.path())
            .into_iter()
            .find(|source| source.provider == CaptureProvider::Tabnine)
            .unwrap();
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.unsupported_reason, None);
    }

    #[test]
    fn codex_default_source_is_empty_until_jsonl_sessions_exist() {
        let temp = tempfile::tempdir().unwrap();
        let sessions = temp.path().join(".codex/sessions");
        std::fs::create_dir_all(&sessions).unwrap();

        let source = discover_provider_sources(temp.path())
            .into_iter()
            .find(|source| {
                source.provider == CaptureProvider::Codex
                    && source.source_format == "codex_session_jsonl_tree"
            })
            .unwrap();
        assert_eq!(source.status, ProviderSourceStatus::Empty);

        std::fs::write(sessions.join("session.jsonl"), "{}\n").unwrap();
        let source = discover_provider_sources(temp.path())
            .into_iter()
            .find(|source| {
                source.provider == CaptureProvider::Codex
                    && source.source_format == "codex_session_jsonl_tree"
            })
            .unwrap();
        assert_eq!(source.status, ProviderSourceStatus::Available);
    }

    #[test]
    fn codearts_agent_discovers_kernel_opencode_dbs_as_native_sources() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let xdg = temp.path().join("xdg-data");
        let _xdg_data_home = EnvGuard::set("XDG_DATA_HOME", &xdg);

        for db in [
            temp.path()
                .join(".codeartsdoer")
                .join("vscode-data")
                .join("opencode.db"),
            temp.path()
                .join(".codeartsdoer")
                .join("codearts-data")
                .join("opencode.db"),
            temp.path()
                .join(".local")
                .join("share")
                .join("codeartsdoer")
                .join("opencode.db"),
            xdg.join("codeartsdoer").join("opencode.db"),
        ] {
            std::fs::create_dir_all(db.parent().unwrap()).unwrap();
            std::fs::write(db, b"sqlite fixture marker").unwrap();
        }

        let sources =
            discover_provider_sources_for_provider(temp.path(), CaptureProvider::CodeArtsAgent);
        assert_eq!(sources.len(), 4, "{sources:#?}");
        for source in sources {
            assert_eq!(source.status, ProviderSourceStatus::Available);
            assert_eq!(source.source_format, "codearts_agent_kernel_sqlite");
            assert_eq!(source.import_support, ProviderImportSupport::Native);
            assert!(source.import_support.is_auto_importable());
            assert!(source.unsupported_reason.is_none());
        }
    }

    #[test]
    fn native_provider_default_discovery_uses_importer_specific_file_predicates() {
        let temp = tempfile::tempdir().unwrap();

        let pi = temp.path().join(".pi/agent/sessions");
        std::fs::create_dir_all(pi.join("--workspace--")).unwrap();
        assert_source_status(
            temp.path(),
            CaptureProvider::Pi,
            ProviderSourceStatus::Empty,
        );
        std::fs::write(pi.join("--workspace--/session.jsonl"), "{}\n").unwrap();
        let pi_source = discover_provider_sources(temp.path())
            .into_iter()
            .find(|source| source.provider == CaptureProvider::Pi)
            .unwrap();
        assert_eq!(pi_source.status, ProviderSourceStatus::Available);
        assert_eq!(pi_source.path, temp.path().join(".pi/agent/sessions"));

        let omp = temp.path().join(".omp/agent/sessions");
        std::fs::create_dir_all(omp.join("--workspace--")).unwrap();
        let omp_source = discover_provider_sources(temp.path())
            .into_iter()
            .find(|source| source.provider == CaptureProvider::Pi && source.path == omp)
            .unwrap();
        assert_eq!(omp_source.status, ProviderSourceStatus::Empty);
        assert_eq!(omp_source.source_format, "pi_session_jsonl");
        std::fs::write(omp.join("--workspace--/session.jsonl"), "{}\n").unwrap();
        let omp_source = discover_provider_sources(temp.path())
            .into_iter()
            .find(|source| source.provider == CaptureProvider::Pi && source.path == omp)
            .unwrap();
        assert_eq!(omp_source.status, ProviderSourceStatus::Available);

        let antigravity = temp.path().join(".gemini/antigravity-cli/brain");
        std::fs::create_dir_all(antigravity.join("session/.system_generated/logs")).unwrap();
        std::fs::write(
            antigravity.join("session/.system_generated/logs/not-a-transcript.jsonl"),
            "{}\n",
        )
        .unwrap();
        assert_source_status(
            temp.path(),
            CaptureProvider::Antigravity,
            ProviderSourceStatus::Empty,
        );
        std::fs::write(
            antigravity.join("session/.system_generated/logs/transcript_full.jsonl"),
            "{}\n",
        )
        .unwrap();
        assert_source_status(
            temp.path(),
            CaptureProvider::Antigravity,
            ProviderSourceStatus::Available,
        );

        let antigravity_ide = temp.path().join(".gemini/antigravity-ide/brain");
        std::fs::create_dir_all(antigravity_ide.join("ide-session/.system_generated/logs"))
            .unwrap();
        std::fs::write(
            antigravity_ide.join("ide-session/.system_generated/logs/transcript.jsonl"),
            "{}\n",
        )
        .unwrap();
        let ide_source = discover_provider_sources(temp.path())
            .into_iter()
            .find(|source| {
                source.provider == CaptureProvider::Antigravity && source.path == antigravity_ide
            })
            .unwrap();
        assert_eq!(ide_source.status, ProviderSourceStatus::Available);
        assert_eq!(
            ide_source.source_format,
            "antigravity_cli_transcript_jsonl_tree"
        );

        let cursor = temp.path().join(".cursor/projects");
        std::fs::create_dir_all(cursor.join("project")).unwrap();
        std::fs::write(cursor.join("project/session.jsonl"), "{}\n").unwrap();
        assert_source_status(
            temp.path(),
            CaptureProvider::Cursor,
            ProviderSourceStatus::Empty,
        );
        std::fs::create_dir_all(cursor.join("project/agent-transcripts/session")).unwrap();
        std::fs::write(
            cursor.join("project/agent-transcripts/session/events.jsonl"),
            "{}\n",
        )
        .unwrap();
        assert_source_status(
            temp.path(),
            CaptureProvider::Cursor,
            ProviderSourceStatus::Available,
        );

        let copilot = temp.path().join(".copilot/session-state");
        std::fs::create_dir_all(copilot.join("session")).unwrap();
        std::fs::write(copilot.join("session/session.jsonl"), "{}\n").unwrap();
        assert_source_status(
            temp.path(),
            CaptureProvider::CopilotCli,
            ProviderSourceStatus::Empty,
        );
        std::fs::write(copilot.join("session/events.jsonl"), "{}\n").unwrap();
        assert_source_status(
            temp.path(),
            CaptureProvider::CopilotCli,
            ProviderSourceStatus::Available,
        );

        let qwen = temp.path().join(".qwen/projects/project/chats");
        std::fs::create_dir_all(&qwen).unwrap();
        assert_source_status(
            temp.path(),
            CaptureProvider::QwenCode,
            ProviderSourceStatus::Empty,
        );
        std::fs::write(qwen.join("session.jsonl"), "{}\n").unwrap();
        assert_source_status(
            temp.path(),
            CaptureProvider::QwenCode,
            ProviderSourceStatus::Available,
        );

        let iflow = temp.path().join(".iflow/projects/project");
        std::fs::create_dir_all(&iflow).unwrap();
        assert_source_status(
            temp.path(),
            CaptureProvider::IflowCli,
            ProviderSourceStatus::Empty,
        );
        std::fs::write(iflow.join("not-session.jsonl"), "{}\n").unwrap();
        assert_source_status(
            temp.path(),
            CaptureProvider::IflowCli,
            ProviderSourceStatus::Empty,
        );
        std::fs::write(iflow.join("session-iflow-discovery.jsonl"), "{}\n").unwrap();
        assert_source_status(
            temp.path(),
            CaptureProvider::IflowCli,
            ProviderSourceStatus::Available,
        );

        let kode = temp.path().join(".kode/projects/-workspace-kode");
        std::fs::create_dir_all(&kode).unwrap();
        assert_source_status(
            temp.path(),
            CaptureProvider::Kode,
            ProviderSourceStatus::Empty,
        );
        std::fs::write(kode.join("kode-session.jsonl"), "{}\n").unwrap();
        assert_source_status(
            temp.path(),
            CaptureProvider::Kode,
            ProviderSourceStatus::Available,
        );

        let neovate = temp.path().join(".neovate/projects/-workspace-neovate");
        std::fs::create_dir_all(neovate.join("requests")).unwrap();
        std::fs::create_dir_all(neovate.join("file-history")).unwrap();
        std::fs::write(neovate.join("requests/request.jsonl"), "{}\n").unwrap();
        std::fs::write(neovate.join("file-history/snapshot.jsonl"), "{}\n").unwrap();
        assert_source_status(
            temp.path(),
            CaptureProvider::Neovate,
            ProviderSourceStatus::Empty,
        );
        std::fs::write(neovate.join("neovate-session.jsonl"), "{}\n").unwrap();
        assert_source_status(
            temp.path(),
            CaptureProvider::Neovate,
            ProviderSourceStatus::Available,
        );

        let command_code = temp.path().join(".commandcode/projects/-workspace-command");
        std::fs::create_dir_all(&command_code).unwrap();
        std::fs::write(
            command_code.join("command-session.checkpoints.jsonl"),
            "{}\n",
        )
        .unwrap();
        assert_source_status(
            temp.path(),
            CaptureProvider::CommandCode,
            ProviderSourceStatus::Empty,
        );
        std::fs::write(command_code.join("command-session.jsonl"), "{}\n").unwrap();
        assert_source_status(
            temp.path(),
            CaptureProvider::CommandCode,
            ProviderSourceStatus::Available,
        );

        let rovodev = temp.path().join(".rovodev/sessions/rovo-session");
        std::fs::create_dir_all(&rovodev).unwrap();
        std::fs::write(rovodev.join("metadata.json"), "{}\n").unwrap();
        assert_source_status(
            temp.path(),
            CaptureProvider::RovoDev,
            ProviderSourceStatus::Empty,
        );
        std::fs::write(rovodev.join("session_context.json"), "{}\n").unwrap();
        assert_source_status(
            temp.path(),
            CaptureProvider::RovoDev,
            ProviderSourceStatus::Available,
        );

        let cortex = temp.path().join(".snowflake/cortex/conversations");
        std::fs::create_dir_all(&cortex).unwrap();
        std::fs::write(cortex.join("cortex-session.history.json"), "{}\n").unwrap();
        assert_source_status(
            temp.path(),
            CaptureProvider::CortexCode,
            ProviderSourceStatus::Empty,
        );
        std::fs::write(cortex.join("cortex-session.history.jsonl"), "{}\n").unwrap();
        assert_source_status(
            temp.path(),
            CaptureProvider::CortexCode,
            ProviderSourceStatus::Available,
        );

        let kimi = temp
            .path()
            .join(".kimi-code/sessions/wd_project_abc123/kimi-session/agents/main");
        std::fs::create_dir_all(&kimi).unwrap();
        assert_source_status(
            temp.path(),
            CaptureProvider::KimiCodeCli,
            ProviderSourceStatus::Empty,
        );
        std::fs::write(kimi.join("wire.jsonl"), "{}\n").unwrap();
        assert_source_status(
            temp.path(),
            CaptureProvider::KimiCodeCli,
            ProviderSourceStatus::Available,
        );

        let codebuddy = temp.path().join(".codebuddy");
        std::fs::create_dir_all(&codebuddy).unwrap();
        assert_source_status(
            temp.path(),
            CaptureProvider::CodeBuddy,
            ProviderSourceStatus::Empty,
        );
        let codebuddy_session = codebuddy.join(
            "Data/VSCode/default/history/11112222333344445555666677778888/session-alpha/messages",
        );
        std::fs::create_dir_all(&codebuddy_session).unwrap();
        std::fs::write(
            codebuddy_session.parent().unwrap().join("index.json"),
            r#"{"messages":[{"id":"msg-1","role":"user"}]}"#,
        )
        .unwrap();
        std::fs::write(
            codebuddy_session.join("msg-1.json"),
            r#"{"message":"hello"}"#,
        )
        .unwrap();
        assert_source_status(
            temp.path(),
            CaptureProvider::CodeBuddy,
            ProviderSourceStatus::Available,
        );

        let openclaw = temp.path().join(".openclaw/agents/personal/sessions");
        std::fs::create_dir_all(&openclaw).unwrap();
        assert_source_status(
            temp.path(),
            CaptureProvider::OpenClaw,
            ProviderSourceStatus::Empty,
        );
        std::fs::write(openclaw.join("session.jsonl"), "{}\n").unwrap();
        assert_source_status(
            temp.path(),
            CaptureProvider::OpenClaw,
            ProviderSourceStatus::Available,
        );

        let hermes = temp.path().join(".hermes");
        std::fs::create_dir_all(&hermes).unwrap();
        std::fs::write(hermes.join("state.db"), b"sqlite fixture marker").unwrap();
        let hermes_source = discover_provider_sources(temp.path())
            .into_iter()
            .find(|source| source.provider == CaptureProvider::Hermes)
            .unwrap();
        assert_eq!(hermes_source.status, ProviderSourceStatus::Available);
        assert_eq!(hermes_source.import_support, ProviderImportSupport::Native);

        let astrbot = temp.path().join(".astrbot/data");
        std::fs::create_dir_all(&astrbot).unwrap();
        std::fs::write(astrbot.join("data_v4.db"), b"sqlite fixture marker").unwrap();
        let astrbot_source = discover_provider_sources(temp.path())
            .into_iter()
            .find(|source| source.provider == CaptureProvider::AstrBot)
            .unwrap();
        assert_eq!(astrbot_source.status, ProviderSourceStatus::Available);
        assert_eq!(astrbot_source.import_support, ProviderImportSupport::Native);
        assert!(astrbot_source.import_support.is_importable());
        assert!(astrbot_source.import_support.is_auto_importable());

        let shelley = temp.path().join(".config/shelley");
        std::fs::create_dir_all(&shelley).unwrap();
        std::fs::write(shelley.join("shelley.db"), b"sqlite fixture marker").unwrap();
        let shelley_source = discover_provider_sources(temp.path())
            .into_iter()
            .find(|source| source.provider == CaptureProvider::Shelley)
            .unwrap();
        assert_eq!(shelley_source.status, ProviderSourceStatus::Available);
        assert_eq!(shelley_source.import_support, ProviderImportSupport::Native);
        assert!(shelley_source.import_support.is_auto_importable());

        let continue_sessions = temp.path().join(".continue/sessions");
        std::fs::create_dir_all(&continue_sessions).unwrap();
        std::fs::write(continue_sessions.join("sessions.json"), "[]\n").unwrap();
        assert_source_status(
            temp.path(),
            CaptureProvider::Continue,
            ProviderSourceStatus::Empty,
        );
        std::fs::write(continue_sessions.join("session.json"), "{}\n").unwrap();
        let continue_source = discover_provider_sources(temp.path())
            .into_iter()
            .find(|source| source.provider == CaptureProvider::Continue)
            .unwrap();
        assert_eq!(continue_source.status, ProviderSourceStatus::Available);
        assert_eq!(continue_source.source_format, "continue_cli_sessions_json");
        assert_eq!(
            continue_source.import_support,
            ProviderImportSupport::Native
        );
        assert!(continue_source.import_support.is_auto_importable());

        let openhands = temp.path().join(".openhands/local-user");
        std::fs::create_dir_all(&openhands).unwrap();
        assert_source_status(
            temp.path(),
            CaptureProvider::OpenHands,
            ProviderSourceStatus::Empty,
        );
        let openhands_events = openhands.join("v1_conversations/12345678123456781234567812345678");
        std::fs::create_dir_all(&openhands_events).unwrap();
        std::fs::write(
            openhands_events.join("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.json"),
            "{}\n",
        )
        .unwrap();
        assert_source_status(
            temp.path(),
            CaptureProvider::OpenHands,
            ProviderSourceStatus::Available,
        );

        let cline = temp.path().join(".cline/data/tasks/cline-discovery");
        std::fs::create_dir_all(&cline).unwrap();
        std::fs::write(cline.join("api_conversation_history.json"), "[]").unwrap();
        assert_source_status(
            temp.path(),
            CaptureProvider::Cline,
            ProviderSourceStatus::Available,
        );

        let roo = temp
            .path()
            .join(".config/Code/User/globalStorage/rooveterinaryinc.roo-cline/tasks/roo-discovery");
        std::fs::create_dir_all(&roo).unwrap();
        std::fs::write(roo.join("history_item.json"), "{}").unwrap();
        assert_source_status(
            temp.path(),
            CaptureProvider::RooCode,
            ProviderSourceStatus::Available,
        );
    }

    #[test]
    fn continue_discovery_uses_global_dir_env_sessions_subdir() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let continue_home = temp.path().join("continue-home");
        let sessions = continue_home.join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(sessions.join("session.json"), "{}\n").unwrap();
        let _global_dir = EnvGuard::set("CONTINUE_GLOBAL_DIR", continue_home.as_os_str());

        let sources = discover_provider_sources(temp.path());
        let source = sources
            .iter()
            .find(|source| source.provider == CaptureProvider::Continue && source.path == sessions)
            .unwrap();

        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.source_format, "continue_cli_sessions_json");
        assert_eq!(source.import_support, ProviderImportSupport::Native);
    }

    #[test]
    fn kilo_discovery_uses_xdg_kilo_db_env_override_and_channel_dbs() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let _kilo_db = EnvGuard::remove("KILO_DB");
        let _xdg_data = EnvGuard::remove("XDG_DATA_HOME");
        let _config_dir = EnvGuard::remove("KILO_CONFIG_DIR");
        let _disable_channel = EnvGuard::remove("KILO_DISABLE_CHANNEL_DB");

        let data_dir = temp.path().join(".local/share/kilo");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::write(data_dir.join("kilo.db"), b"sqlite fixture marker").unwrap();
        std::fs::write(data_dir.join("kilo-dev.db"), b"sqlite fixture marker").unwrap();
        std::fs::write(data_dir.join("opencode-dev.db"), b"ignored").unwrap();

        let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Kilo);
        assert_eq!(
            sources
                .iter()
                .map(|source| source.path.clone())
                .collect::<Vec<_>>(),
            vec![data_dir.join("kilo.db"), data_dir.join("kilo-dev.db")]
        );
        assert!(sources
            .iter()
            .all(|source| source.status == ProviderSourceStatus::Available));

        let xdg_data = temp.path().join("xdg-data");
        let xdg_kilo = xdg_data.join("kilo");
        std::fs::create_dir_all(&xdg_kilo).unwrap();
        std::fs::write(xdg_kilo.join("kilo.db"), b"sqlite fixture marker").unwrap();
        let _xdg_data_set = EnvGuard::set("XDG_DATA_HOME", xdg_data.as_os_str());
        let _config_dir_set = EnvGuard::set("KILO_CONFIG_DIR", temp.path().join("config"));

        let xdg_sources =
            discover_provider_sources_for_provider(temp.path(), CaptureProvider::Kilo);
        assert_eq!(xdg_sources[0].path, xdg_kilo.join("kilo.db"));
        assert_ne!(
            xdg_sources[0].path,
            temp.path().join("config").join("kilo.db")
        );

        let _relative_db = EnvGuard::set("KILO_DB", "relative-kilo.db");
        std::fs::write(xdg_kilo.join("relative-kilo.db"), b"sqlite fixture marker").unwrap();
        let relative_sources =
            discover_provider_sources_for_provider(temp.path(), CaptureProvider::Kilo);
        assert_eq!(relative_sources.len(), 1);
        assert_eq!(relative_sources[0].path, xdg_kilo.join("relative-kilo.db"));
        assert_eq!(relative_sources[0].status, ProviderSourceStatus::Available);

        let absolute_db = temp.path().join("absolute-kilo.db");
        std::fs::write(&absolute_db, b"sqlite fixture marker").unwrap();
        let _absolute_db = EnvGuard::set("KILO_DB", absolute_db.as_os_str());
        let absolute_sources =
            discover_provider_sources_for_provider(temp.path(), CaptureProvider::Kilo);
        assert_eq!(absolute_sources.len(), 1);
        assert_eq!(absolute_sources[0].path, absolute_db);
        assert_eq!(absolute_sources[0].status, ProviderSourceStatus::Available);
    }

    #[test]
    fn qwen_discovery_uses_runtime_and_home_env_overrides() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let runtime = temp.path().join("qwen-runtime");
        write_qwen_discovery_chat(&runtime.join("projects"));
        let qwen_home = temp.path().join("qwen-home");
        write_qwen_discovery_chat(&qwen_home.join("projects"));
        let _runtime = EnvGuard::set("QWEN_RUNTIME_DIR", runtime.as_os_str());
        let _home = EnvGuard::set("QWEN_HOME", qwen_home.as_os_str());

        let sources = discover_provider_sources(temp.path());
        for path in [runtime.join("projects"), qwen_home.join("projects")] {
            let source = sources
                .iter()
                .find(|source| source.provider == CaptureProvider::QwenCode && source.path == path)
                .unwrap_or_else(|| panic!("missing Qwen Code source for {path:?}: {sources:#?}"));
            assert_eq!(source.status, ProviderSourceStatus::Available);
            assert_eq!(source.import_support, ProviderImportSupport::Native);
        }
    }

    #[test]
    fn kimi_discovery_uses_home_env_override() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let kimi_home = temp.path().join("kimi-home");
        write_kimi_discovery_wire(&kimi_home);
        let _home = EnvGuard::set("KIMI_CODE_HOME", kimi_home.as_os_str());

        let sources = discover_provider_sources(temp.path());
        let source = sources
            .iter()
            .find(|source| {
                source.provider == CaptureProvider::KimiCodeCli && source.path == kimi_home
            })
            .unwrap_or_else(|| panic!("missing Kimi Code CLI source in {sources:#?}"));
        assert_eq!(source.status, ProviderSourceStatus::Available);
        let crush = temp.path().join(".local/share/crush");
        std::fs::create_dir_all(&crush).unwrap();
        std::fs::write(crush.join("crush.db"), b"sqlite fixture marker").unwrap();
        let crush_source = discover_provider_sources(temp.path())
            .into_iter()
            .find(|source| source.provider == CaptureProvider::Crush)
            .unwrap();
        assert_eq!(crush_source.status, ProviderSourceStatus::Available);
        assert_eq!(crush_source.source_format, "crush_sqlite");

        let goose = temp.path().join(".local/share/goose/sessions");
        std::fs::create_dir_all(&goose).unwrap();
        std::fs::write(goose.join("sessions.db"), b"sqlite fixture marker").unwrap();
        let goose_source = discover_provider_sources(temp.path())
            .into_iter()
            .find(|source| source.provider == CaptureProvider::Goose)
            .unwrap();
        assert_eq!(goose_source.status, ProviderSourceStatus::Available);
        assert_eq!(goose_source.source_format, "goose_sessions_sqlite");
    }

    #[test]
    fn autohand_code_discovery_uses_default_and_home_env_sessions() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let _home = EnvGuard::remove("AUTOHAND_HOME");

        let default_sessions = temp.path().join(".autohand/sessions");
        std::fs::create_dir_all(&default_sessions).unwrap();
        let empty_source =
            discover_provider_sources_for_provider(temp.path(), CaptureProvider::AutohandCode)
                .into_iter()
                .find(|source| source.path == default_sessions)
                .unwrap();
        assert_eq!(empty_source.status, ProviderSourceStatus::Empty);

        write_autohand_discovery_session(&default_sessions);
        let source =
            discover_provider_sources_for_provider(temp.path(), CaptureProvider::AutohandCode)
                .into_iter()
                .find(|source| source.path == default_sessions)
                .unwrap();
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.source_format, "autohand_code_sessions_jsonl");
        assert_eq!(source.import_support, ProviderImportSupport::Native);

        let custom_home = temp.path().join("custom-autohand");
        let custom_sessions = custom_home.join("sessions");
        write_autohand_discovery_session(&custom_sessions);
        let _home = EnvGuard::set("AUTOHAND_HOME", custom_home.as_os_str());
        let sources =
            discover_provider_sources_for_provider(temp.path(), CaptureProvider::AutohandCode);
        assert!(sources.iter().any(|source| {
            source.path == custom_sessions && source.status == ProviderSourceStatus::Available
        }));
    }

    #[test]
    fn codebuddy_discovery_uses_localappdata_override() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let local_app_data = temp.path().join("local-app-data");
        let codebuddy = local_app_data.join("CodeBuddyExtension");
        let session = codebuddy
            .join("CodeBuddyIDE/default/history/11112222333344445555666677778888/session-alpha");
        std::fs::create_dir_all(session.join("messages")).unwrap();
        std::fs::write(
            session.join("index.json"),
            r#"{"messages":[{"id":"msg-1","role":"user"}]}"#,
        )
        .unwrap();
        std::fs::write(
            session.join("messages/msg-1.json"),
            r#"{"message":"hello"}"#,
        )
        .unwrap();
        let _local_app_data = EnvGuard::set("LOCALAPPDATA", local_app_data.as_os_str());

        let sources =
            discover_provider_sources_for_provider(temp.path(), CaptureProvider::CodeBuddy);
        let source = sources
            .iter()
            .find(|source| {
                source.provider == CaptureProvider::CodeBuddy && source.path == codebuddy
            })
            .unwrap_or_else(|| panic!("missing CodeBuddy LOCALAPPDATA source in {sources:#?}"));

        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.import_support, ProviderImportSupport::Native);
    }

    #[test]
    fn aider_desk_discovery_uses_current_project_tasks_root() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        let task = project.join(".aider-desk/tasks/aider-task-1");
        std::fs::create_dir_all(&task).unwrap();
        std::fs::write(
            task.join("context.json"),
            r#"{"version":2,"contextMessages":[],"contextFiles":[]}"#,
        )
        .unwrap();
        let _aider_dir = EnvGuard::remove("AIDER_DESK_DIR");
        let _cwd = CwdGuard::set(&project);

        let sources =
            discover_provider_sources_for_provider(temp.path(), CaptureProvider::AiderDesk);
        let expected = project.join(".aider-desk/tasks");
        let source = sources
            .iter()
            .find(|source| source.provider == CaptureProvider::AiderDesk && source.path == expected)
            .unwrap_or_else(|| panic!("missing Aider Desk cwd source in {sources:#?}"));

        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.source_format, "aider_desk_task_context_json");
        assert_eq!(source.import_support, ProviderImportSupport::Native);
    }

    #[test]
    fn iflow_cli_discovery_uses_default_and_home_env_projects() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let _home = EnvGuard::remove("IFLOW_HOME");

        let default_projects = temp.path().join(".iflow/projects");
        std::fs::create_dir_all(&default_projects).unwrap();
        let empty_source =
            discover_provider_sources_for_provider(temp.path(), CaptureProvider::IflowCli)
                .into_iter()
                .find(|source| source.path == default_projects)
                .unwrap();
        assert_eq!(empty_source.status, ProviderSourceStatus::Empty);

        write_iflow_discovery_session(&default_projects);
        let source = discover_provider_sources_for_provider(temp.path(), CaptureProvider::IflowCli)
            .into_iter()
            .find(|source| source.path == default_projects)
            .unwrap();
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.source_format, "iflow_cli_session_jsonl_tree");
        assert_eq!(source.import_support, ProviderImportSupport::Native);

        let custom_home = temp.path().join("custom-iflow");
        let custom_projects = custom_home.join("projects");
        write_iflow_discovery_session(&custom_projects);
        let _home = EnvGuard::set("IFLOW_HOME", custom_home.as_os_str());
        let sources =
            discover_provider_sources_for_provider(temp.path(), CaptureProvider::IflowCli);
        assert!(sources.iter().any(|source| {
            source.path == custom_projects && source.status == ProviderSourceStatus::Available
        }));
    }

    #[test]
    fn jazz_discovery_uses_default_and_home_env_history() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let _home = EnvGuard::remove("JAZZ_HOME");

        let default_history = temp.path().join(".jazz/history");
        std::fs::create_dir_all(&default_history).unwrap();
        let empty_source =
            discover_provider_sources_for_provider(temp.path(), CaptureProvider::Jazz)
                .into_iter()
                .find(|source| source.path == default_history)
                .unwrap();
        assert_eq!(empty_source.status, ProviderSourceStatus::Empty);

        write_jazz_discovery_history(&default_history);
        let source = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Jazz)
            .into_iter()
            .find(|source| source.path == default_history)
            .unwrap();
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.source_format, "jazz_history_json");
        assert_eq!(source.import_support, ProviderImportSupport::Native);

        let custom_home = temp.path().join("custom-jazz");
        let custom_history = custom_home.join("history");
        write_jazz_discovery_history(&custom_history);
        let _home = EnvGuard::set("JAZZ_HOME", custom_home.as_os_str());
        let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Jazz);
        assert!(sources.iter().any(|source| {
            source.path == custom_history && source.status == ProviderSourceStatus::Available
        }));
    }

    #[test]
    fn firebender_discovery_uses_current_project_chat_history_db() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        let nested = project.join("src/module");
        let db = project.join(".idea/firebender/chat_history.db");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        Connection::open(&db)
            .unwrap()
            .execute_batch(
                r#"
                CREATE TABLE chat_sessions (
                    id TEXT PRIMARY KEY,
                    messages_json TEXT NOT NULL
                );
                "#,
            )
            .unwrap();
        let _cwd = CwdGuard::set(&nested);

        let sources =
            discover_provider_sources_for_provider(temp.path(), CaptureProvider::Firebender);
        let source = sources
            .iter()
            .find(|source| source.provider == CaptureProvider::Firebender && source.path == db)
            .unwrap_or_else(|| panic!("missing Firebender cwd source in {sources:#?}"));

        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.source_format, "firebender_chat_history_sqlite");
        assert_eq!(source.import_support, ProviderImportSupport::Native);
    }

    #[test]
    fn eve_discovery_uses_workflow_data_env_and_current_project() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let _workflow_data = EnvGuard::remove("WORKFLOW_LOCAL_DATA_DIR");

        let env_data = temp.path().join("custom-workflow-data");
        std::fs::create_dir_all(&env_data).unwrap();
        let _workflow_data = EnvGuard::set("WORKFLOW_LOCAL_DATA_DIR", env_data.as_os_str());
        let empty_source =
            discover_provider_sources_for_provider(temp.path(), CaptureProvider::Eve)
                .into_iter()
                .find(|source| source.path == env_data)
                .unwrap();
        assert_eq!(empty_source.status, ProviderSourceStatus::Empty);

        write_eve_discovery_workflow_data(&env_data, "envdiscovery");
        let source = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Eve)
            .into_iter()
            .find(|source| source.path == env_data)
            .unwrap();
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.source_format, "eve_workflow_data_streams");
        assert_eq!(source.import_support, ProviderImportSupport::Native);

        let project = temp.path().join("project");
        let nested = project.join("agent");
        std::fs::create_dir_all(&nested).unwrap();
        let project_data = project.join(".workflow-data");
        write_eve_discovery_workflow_data(&project_data, "projectdiscovery");
        let _cwd = CwdGuard::set(&nested);

        let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Eve);
        let project_source = sources
            .iter()
            .find(|source| source.path == project_data)
            .unwrap_or_else(|| panic!("missing Eve project source in {sources:#?}"));
        assert_eq!(project_source.status, ProviderSourceStatus::Available);
        assert_eq!(project_source.source_format, "eve_workflow_data_streams");
        assert_eq!(project_source.import_support, ProviderImportSupport::Native);
    }

    #[test]
    fn junie_discovery_uses_default_sessions_and_env_overrides() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let _sessions_dir = EnvGuard::remove("JUNIE_SESSIONS_DIR");
        let _junie_home = EnvGuard::remove("JUNIE_HOME");

        let default_sessions = temp.path().join(".junie/sessions");
        std::fs::create_dir_all(&default_sessions).unwrap();
        let empty_source =
            discover_provider_sources_for_provider(temp.path(), CaptureProvider::Junie)
                .into_iter()
                .find(|source| source.path == default_sessions)
                .unwrap();
        assert_eq!(empty_source.status, ProviderSourceStatus::Empty);
        assert_eq!(
            empty_source.source_format,
            "junie_session_events_jsonl_tree"
        );

        write_junie_discovery_session(&default_sessions, "session-260607-110000-default");
        let source = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Junie)
            .into_iter()
            .find(|source| source.path == default_sessions)
            .unwrap();
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.import_support, ProviderImportSupport::Native);

        let env_sessions = temp.path().join("junie-env-sessions");
        write_junie_discovery_session(&env_sessions, "session-260607-110001-env");
        let _sessions_dir = EnvGuard::set("JUNIE_SESSIONS_DIR", env_sessions.as_os_str());

        let junie_home = temp.path().join("junie-home");
        let home_sessions = junie_home.join("sessions");
        write_junie_discovery_session(&home_sessions, "session-260607-110002-home");
        let _junie_home = EnvGuard::set("JUNIE_HOME", junie_home.as_os_str());

        let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Junie);
        for path in [&env_sessions, &home_sessions] {
            let source = sources
                .iter()
                .find(|source| source.path == *path)
                .unwrap_or_else(|| panic!("missing Junie source {path:?} in {sources:#?}"));
            assert_eq!(source.status, ProviderSourceStatus::Available);
            assert_eq!(source.source_format, "junie_session_events_jsonl_tree");
            assert_eq!(source.import_support, ProviderImportSupport::Native);
        }
    }

    #[test]
    fn openloaf_discovery_uses_global_chat_history_and_bounded_data_projects() {
        let temp = tempfile::tempdir().unwrap();

        let global_history = temp.path().join(".openloaf/chat-history");
        std::fs::create_dir_all(&global_history).unwrap();
        let empty_source =
            discover_provider_sources_for_provider(temp.path(), CaptureProvider::OpenLoaf)
                .into_iter()
                .find(|source| source.path == global_history)
                .unwrap();
        assert_eq!(empty_source.status, ProviderSourceStatus::Empty);
        assert_eq!(empty_source.source_format, "openloaf_chat_jsonl_tree");

        write_openloaf_discovery_session(&global_history, "openloaf-global-discovery");
        let source = discover_provider_sources_for_provider(temp.path(), CaptureProvider::OpenLoaf)
            .into_iter()
            .find(|source| source.path == global_history)
            .unwrap();
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.import_support, ProviderImportSupport::Native);

        let projects = temp.path().join("OpenLoafData/projects");
        let project_history = projects.join("project-a/.openloaf/chat-history");
        write_openloaf_discovery_session(&project_history, "openloaf-project-discovery");
        let project_source =
            discover_provider_sources_for_provider(temp.path(), CaptureProvider::OpenLoaf)
                .into_iter()
                .find(|source| source.path == projects)
                .unwrap();
        assert_eq!(project_source.status, ProviderSourceStatus::Available);
        assert_eq!(project_source.source_format, "openloaf_chat_jsonl_tree");

        let explicit_file = provider_source_for_path(
            CaptureProvider::OpenLoaf,
            project_history
                .join("openloaf-project-discovery")
                .join("messages.jsonl"),
        );
        assert_eq!(explicit_file.source_format, "openloaf_chat_jsonl");
        assert_eq!(explicit_file.status, ProviderSourceStatus::Available);
    }

    #[test]
    fn kode_and_neovate_discovery_use_default_and_env_project_roots() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let _kode_config = EnvGuard::remove("KODE_CONFIG_DIR");
        let _claude_config = EnvGuard::remove("CLAUDE_CONFIG_DIR");

        let default_kode_projects = temp.path().join(".kode/projects");
        write_generic_project_jsonl(&default_kode_projects, "kode-session.jsonl");
        let source = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Kode)
            .into_iter()
            .find(|source| source.path == default_kode_projects)
            .unwrap();
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.source_format, "kode_session_jsonl_tree");
        assert_eq!(source.import_support, ProviderImportSupport::Native);

        let custom_kode = temp.path().join("custom-kode");
        let custom_kode_projects = custom_kode.join("projects");
        write_generic_project_jsonl(&custom_kode_projects, "custom-kode.jsonl");
        let _kode_config = EnvGuard::set("KODE_CONFIG_DIR", custom_kode.as_os_str());
        let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Kode);
        assert!(sources.iter().any(|source| {
            source.path == custom_kode_projects && source.status == ProviderSourceStatus::Available
        }));

        let claude_config = temp.path().join("claude-config-for-kode");
        let claude_projects = claude_config.join("projects");
        write_generic_project_jsonl(&claude_projects, "claude-fallback.jsonl");
        let _claude_config = EnvGuard::set("CLAUDE_CONFIG_DIR", claude_config.as_os_str());
        let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Kode);
        assert!(sources.iter().any(|source| {
            source.path == claude_projects && source.status == ProviderSourceStatus::Available
        }));

        let neovate_projects = temp.path().join(".neovate/projects");
        let project = neovate_projects.join("-workspace-neovate");
        std::fs::create_dir_all(project.join("requests")).unwrap();
        std::fs::create_dir_all(project.join("file-history")).unwrap();
        std::fs::write(project.join("requests/request-only.jsonl"), "{}\n").unwrap();
        std::fs::write(project.join("file-history/snapshot.jsonl"), "{}\n").unwrap();
        let source = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Neovate)
            .into_iter()
            .find(|source| source.path == neovate_projects)
            .unwrap();
        assert_eq!(source.status, ProviderSourceStatus::Empty);

        std::fs::write(project.join("neovate-session.jsonl"), "{}\n").unwrap();
        let source = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Neovate)
            .into_iter()
            .find(|source| source.path == neovate_projects)
            .unwrap();
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.source_format, "neovate_session_jsonl_tree");
        assert_eq!(source.import_support, ProviderImportSupport::Native);
    }

    #[test]
    fn mistral_vibe_discovery_uses_default_and_home_env_sessions() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let _home = EnvGuard::remove("VIBE_HOME");

        let default_sessions = temp.path().join(".vibe/logs/session");
        std::fs::create_dir_all(&default_sessions).unwrap();
        let empty_source =
            discover_provider_sources_for_provider(temp.path(), CaptureProvider::MistralVibe)
                .into_iter()
                .find(|source| source.path == default_sessions)
                .unwrap();
        assert_eq!(empty_source.status, ProviderSourceStatus::Empty);

        write_mistral_vibe_discovery_session(&default_sessions);
        let source =
            discover_provider_sources_for_provider(temp.path(), CaptureProvider::MistralVibe)
                .into_iter()
                .find(|source| source.path == default_sessions)
                .unwrap();
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.source_format, "mistral_vibe_session_jsonl_tree");
        assert_eq!(source.import_support, ProviderImportSupport::Native);

        let custom_home = temp.path().join("custom-vibe");
        let custom_sessions = custom_home.join("logs/session");
        write_mistral_vibe_discovery_session(&custom_sessions);
        let _home = EnvGuard::set("VIBE_HOME", custom_home.as_os_str());
        let sources =
            discover_provider_sources_for_provider(temp.path(), CaptureProvider::MistralVibe);
        assert!(sources.iter().any(|source| {
            source.path == custom_sessions && source.status == ProviderSourceStatus::Available
        }));
    }

    #[test]
    fn mux_discovery_uses_default_and_mux_root_sessions() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let _home = EnvGuard::remove("MUX_ROOT");

        let default_sessions = temp.path().join(".mux/sessions");
        std::fs::create_dir_all(&default_sessions).unwrap();
        let empty_source =
            discover_provider_sources_for_provider(temp.path(), CaptureProvider::Mux)
                .into_iter()
                .find(|source| source.path == default_sessions)
                .unwrap();
        assert_eq!(empty_source.status, ProviderSourceStatus::Empty);

        write_mux_discovery_session(&default_sessions);
        let source = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Mux)
            .into_iter()
            .find(|source| source.path == default_sessions)
            .unwrap();
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.source_format, "mux_session_jsonl_tree");
        assert_eq!(source.import_support, ProviderImportSupport::Native);

        let custom_home = temp.path().join("custom-mux");
        let custom_sessions = custom_home.join("sessions");
        write_mux_discovery_session(&custom_sessions);
        let _home = EnvGuard::set("MUX_ROOT", custom_home.as_os_str());
        let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Mux);
        assert!(sources.iter().any(|source| {
            source.path == custom_sessions && source.status == ProviderSourceStatus::Available
        }));
    }

    #[test]
    fn moxby_discovery_uses_default_and_state_dir_db() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let _state_dir = EnvGuard::remove("MOXBY_STATE_DIR");

        let default_db = temp
            .path()
            .join(".local/share/com.moxby.agent/moxby_chats.db");
        std::fs::create_dir_all(default_db.parent().unwrap()).unwrap();
        std::fs::copy(
            shared_provider_history_fixture("moxby/v2.3.0/moxby_chats.db"),
            &default_db,
        )
        .unwrap();
        let source = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Moxby)
            .into_iter()
            .find(|source| source.path == default_db)
            .unwrap();
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.source_format, "moxby_chats_sqlite");
        assert_eq!(source.import_support, ProviderImportSupport::Native);

        let custom_state = temp.path().join("custom-moxby-state");
        let custom_db = custom_state.join("moxby_chats.db");
        std::fs::create_dir_all(&custom_state).unwrap();
        std::fs::copy(
            shared_provider_history_fixture("moxby/v2.3.0/moxby_chats.db"),
            &custom_db,
        )
        .unwrap();
        let _state_dir = EnvGuard::set("MOXBY_STATE_DIR", custom_state.as_os_str());
        let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Moxby);
        assert!(sources.iter().any(|source| {
            source.path == custom_db && source.status == ProviderSourceStatus::Available
        }));
    }

    #[test]
    fn reasonix_discovery_uses_default_sessions() {
        let temp = tempfile::tempdir().unwrap();
        let default_sessions = temp.path().join(".reasonix/sessions");
        std::fs::create_dir_all(&default_sessions).unwrap();
        std::fs::write(
            default_sessions.join("reasonix-discovery.events.jsonl"),
            "{}\n",
        )
        .unwrap();
        let empty_source =
            discover_provider_sources_for_provider(temp.path(), CaptureProvider::Reasonix)
                .into_iter()
                .find(|source| source.path == default_sessions)
                .unwrap();
        assert_eq!(empty_source.status, ProviderSourceStatus::Empty);

        write_reasonix_discovery_session(&default_sessions);
        let source = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Reasonix)
            .into_iter()
            .find(|source| source.path == default_sessions)
            .unwrap();
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.source_format, "reasonix_session_jsonl_tree");
        assert_eq!(source.import_support, ProviderImportSupport::Native);
    }

    #[test]
    fn adal_discovery_uses_default_sessions_and_ignores_empty_bootstrap_jsonl() {
        let temp = tempfile::tempdir().unwrap();
        let default_sessions = temp.path().join(".adal/sessions");
        std::fs::create_dir_all(&default_sessions).unwrap();

        let empty_source =
            discover_provider_sources_for_provider(temp.path(), CaptureProvider::Adal)
                .into_iter()
                .find(|source| source.path == default_sessions)
                .unwrap();
        assert_eq!(empty_source.status, ProviderSourceStatus::Empty);

        std::fs::write(default_sessions.join("conversation_empty.jsonl"), "").unwrap();
        let still_empty =
            discover_provider_sources_for_provider(temp.path(), CaptureProvider::Adal)
                .into_iter()
                .find(|source| source.path == default_sessions)
                .unwrap();
        assert_eq!(still_empty.status, ProviderSourceStatus::Empty);

        std::fs::write(
            default_sessions.join("conversation_adal-discovery.jsonl"),
            r#"{"type":"message","message_id":"m","turn_id":"t","role":"user","content":"adal discovery oracle","metadata":{},"timestamp":"2026-07-01T00:00:00Z"}"#,
        )
        .unwrap();
        let source = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Adal)
            .into_iter()
            .find(|source| source.path == default_sessions)
            .unwrap();
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.source_format, "adal_session_jsonl");
        assert_eq!(source.import_support, ProviderImportSupport::Native);
    }

    #[test]
    fn deepagents_discovery_uses_default_sessions_db() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join(".deepagents/.state/sessions.db");
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();

        let empty_source =
            discover_provider_sources_for_provider(temp.path(), CaptureProvider::DeepAgents)
                .into_iter()
                .find(|source| source.path == db)
                .unwrap();
        assert_eq!(empty_source.status, ProviderSourceStatus::Missing);

        std::fs::write(&db, b"not sqlite").unwrap();
        let unreadable_source =
            discover_provider_sources_for_provider(temp.path(), CaptureProvider::DeepAgents)
                .into_iter()
                .find(|source| source.path == db)
                .unwrap();
        assert_eq!(unreadable_source.status, ProviderSourceStatus::Unknown);

        std::fs::copy(
            shared_provider_history_fixture("deepagents/v1/sessions.db"),
            &db,
        )
        .unwrap();
        let source =
            discover_provider_sources_for_provider(temp.path(), CaptureProvider::DeepAgents)
                .into_iter()
                .find(|source| source.path == db)
                .unwrap();
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.source_format, "deepagents_sessions_sqlite");
        assert_eq!(source.import_support, ProviderImportSupport::Native);
    }

    #[test]
    fn crush_discovery_uses_global_config_data_directory() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("crush.json");
        let data_dir = temp.path().join("custom-crush-data");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::write(data_dir.join("crush.db"), b"sqlite fixture marker").unwrap();
        std::fs::write(
            &config,
            format!(
                "{{\"options\":{{\"data_directory\":\"{}\"}}}}",
                data_dir.display()
            ),
        )
        .unwrap();
        let _config = EnvGuard::set("CRUSH_GLOBAL_CONFIG", &config);
        let _data = EnvGuard::remove("CRUSH_GLOBAL_DATA");

        let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Crush);
        let source = sources
            .iter()
            .find(|source| source.path == data_dir.join("crush.db"))
            .unwrap_or_else(|| panic!("missing Crush config source in {sources:#?}"));
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.source_format, "crush_sqlite");
    }

    #[test]
    fn goose_discovery_uses_path_root_data_sessions_db() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("goose-root");
        let sessions = root.join("data/sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(sessions.join("sessions.db"), b"sqlite fixture marker").unwrap();
        let _path_root = EnvGuard::set("GOOSE_PATH_ROOT", &root);

        let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Goose);
        let source = sources
            .iter()
            .find(|source| source.path == sessions.join("sessions.db"))
            .unwrap_or_else(|| panic!("missing Goose path-root source in {sources:#?}"));
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.source_format, "goose_sessions_sqlite");
    }

    #[test]
    fn dexto_discovery_uses_home_database_root_when_schema_matches() {
        let temp = tempfile::tempdir().unwrap();
        let database_root = temp.path().join(".dexto").join("database");
        std::fs::create_dir_all(&database_root).unwrap();
        let unrelated = database_root.join("not-dexto.db");
        Connection::open(&unrelated).unwrap();

        let discovered =
            discover_provider_sources_for_provider(temp.path(), CaptureProvider::Dexto);
        let source = discovered
            .iter()
            .find(|source| source.path == database_root)
            .unwrap_or_else(|| panic!("missing Dexto database root in {discovered:#?}"));
        assert_eq!(source.status, ProviderSourceStatus::Empty);
        assert_eq!(source.source_format, "dexto_sqlite");
        assert_eq!(source.import_support, ProviderImportSupport::Native);
        assert!(source.import_support.is_auto_importable());

        let db = database_root.join("coding-agent.db");
        std::fs::copy(shared_provider_history_fixture("dexto/v1/dexto.db"), &db).unwrap();
        let discovered =
            discover_provider_sources_for_provider(temp.path(), CaptureProvider::Dexto);
        let source = discovered
            .iter()
            .find(|source| source.path == database_root)
            .unwrap_or_else(|| panic!("missing Dexto database root in {discovered:#?}"));
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.source_format, "dexto_sqlite");
        assert_eq!(source.import_support, ProviderImportSupport::Native);
        assert!(source.import_support.is_auto_importable());

        let source = provider_source_for_path(CaptureProvider::Dexto, db.clone());
        assert_eq!(source.path, db);
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.source_format, "dexto_sqlite");
        assert_eq!(source.import_support, ProviderImportSupport::Native);
        assert!(source.import_support.is_auto_importable());
    }

    #[test]
    fn dexto_discovery_uses_current_project_database_root() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let project = temp.path().join("project");
        let database_root = project.join(".dexto").join("database");
        std::fs::create_dir_all(&database_root).unwrap();
        std::fs::copy(
            shared_provider_history_fixture("dexto/v1/dexto.db"),
            database_root.join("custom-agent.db"),
        )
        .unwrap();
        let _cwd = CwdGuard::set(&project);

        let discovered = discover_provider_sources_for_provider(&home, CaptureProvider::Dexto);
        let source = discovered
            .iter()
            .find(|source| source.path == database_root)
            .unwrap_or_else(|| panic!("missing Dexto project database root in {discovered:#?}"));
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.source_format, "dexto_sqlite");
        assert_eq!(source.import_support, ProviderImportSupport::Native);
        assert!(source.import_support.is_auto_importable());
    }

    #[test]
    fn pochi_discovery_uses_synced_storage_root_when_present() {
        let temp = tempfile::tempdir().unwrap();

        let discovered =
            discover_provider_sources_for_provider(temp.path(), CaptureProvider::Pochi);
        assert!(discovered.is_empty(), "{discovered:#?}");

        let storage = temp.path().join(".pochi/storage");
        std::fs::create_dir_all(storage.join("store-alpha")).unwrap();
        let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Pochi);
        let source = sources
            .iter()
            .find(|source| source.path == storage)
            .unwrap_or_else(|| panic!("missing Pochi storage source in {sources:#?}"));
        assert_eq!(source.status, ProviderSourceStatus::Empty);
        assert_eq!(source.source_format, "pochi_livestore_state_sqlite");
        assert_eq!(source.import_support, ProviderImportSupport::Native);
        assert!(source.import_support.is_auto_importable());

        std::fs::copy(
            shared_provider_history_fixture("pochi/v1/storage/store-alpha/statep0chifixture@6.db"),
            temp.path()
                .join(".pochi/storage/store-alpha/statep0chifixture@6.db"),
        )
        .unwrap();
        let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Pochi);
        let source = sources
            .iter()
            .find(|source| source.path == storage)
            .unwrap_or_else(|| panic!("missing Pochi storage source in {sources:#?}"));
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.import_support, ProviderImportSupport::Native);
        assert!(source.import_support.is_auto_importable());
    }

    #[cfg(unix)]
    #[test]
    fn pochi_discovery_does_not_follow_storage_symlink() {
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_db = outside.path().join("store-alpha/statep0chifixture@6.db");
        std::fs::create_dir_all(outside_db.parent().unwrap()).unwrap();
        std::fs::write(&outside_db, b"sqlite fixture marker").unwrap();
        std::fs::create_dir_all(temp.path().join(".pochi")).unwrap();
        std::os::unix::fs::symlink(outside.path(), temp.path().join(".pochi/storage")).unwrap();

        let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Pochi);
        let source = sources
            .iter()
            .find(|source| source.path == temp.path().join(".pochi/storage"))
            .unwrap_or_else(|| panic!("missing Pochi storage source in {sources:#?}"));
        assert_eq!(source.status, ProviderSourceStatus::Empty);
        assert_eq!(
            source.unsupported_reason,
            Some("path exists but no Pochi LiveStore state SQLite database was found")
        );
    }

    #[test]
    fn warp_discovery_uses_documented_state_and_localappdata_paths() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let xdg_state = temp.path().join("xdg-state");
        let local_app_data = temp.path().join("local-app-data");
        let linux_db = xdg_state.join("warp-terminal/warp.sqlite");
        let windows_db = local_app_data.join("warp/Warp/data/warp.sqlite");
        std::fs::create_dir_all(linux_db.parent().unwrap()).unwrap();
        std::fs::create_dir_all(windows_db.parent().unwrap()).unwrap();
        std::fs::write(&linux_db, b"sqlite fixture marker").unwrap();
        std::fs::write(&windows_db, b"sqlite fixture marker").unwrap();
        let _xdg_state = EnvGuard::set("XDG_STATE_HOME", xdg_state.as_os_str());
        let _local_app_data = EnvGuard::set("LOCALAPPDATA", local_app_data.as_os_str());

        let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Warp);
        for path in [&linux_db, &windows_db] {
            let source = sources
                .iter()
                .find(|source| source.path == *path)
                .unwrap_or_else(|| panic!("missing Warp source {path:?} in {sources:#?}"));
            assert_eq!(source.status, ProviderSourceStatus::Available);
            assert_eq!(source.source_format, "warp_sqlite");
            assert_eq!(source.import_support, ProviderImportSupport::Native);
            assert!(source.import_support.is_auto_importable());
        }
    }

    #[test]
    fn lingma_discovery_uses_waylog_default_local_db_paths() {
        let temp = tempfile::tempdir().unwrap();
        let stable = temp
            .path()
            .join(".lingma/vscode/sharedClientCache/cache/db/local.db");
        let insiders = temp
            .path()
            .join(".lingma/vscode-insiders/sharedClientCache/cache/db/local.db");
        write_lingma_discovery_db(&stable);
        write_lingma_discovery_db(&insiders);

        let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Lingma);
        for path in [&stable, &insiders] {
            let source = sources
                .iter()
                .find(|source| source.path == *path)
                .unwrap_or_else(|| panic!("missing Lingma source {path:?} in {sources:#?}"));
            assert_eq!(source.status, ProviderSourceStatus::Available);
            assert_eq!(source.source_format, "lingma_sqlite");
            assert_eq!(source.import_support, ProviderImportSupport::Native);
        }
    }

    #[test]
    fn codestudio_discovery_uses_app_data_session_store_with_schema_probe() {
        let temp = tempfile::tempdir().unwrap();
        let stable = temp
            .path()
            .join(".config/Code Studio/User/globalStorage/session-store.db");
        let drifted = temp
            .path()
            .join("Library/Application Support/Code Studio/User/globalStorage/session-store.db");
        write_codestudio_discovery_db(&stable);
        write_codestudio_schema_drift_db(&drifted);

        let sources =
            discover_provider_sources_for_provider(temp.path(), CaptureProvider::CodeStudio);
        let source = sources
            .iter()
            .find(|source| source.provider == CaptureProvider::CodeStudio && source.path == stable)
            .unwrap_or_else(|| panic!("missing Code Studio source {stable:?} in {sources:#?}"));
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.source_format, "codestudio_session_store_sqlite");
        assert_eq!(source.import_support, ProviderImportSupport::Native);
        assert!(source.import_support.is_auto_importable());

        let drifted_source = sources
            .iter()
            .find(|source| source.provider == CaptureProvider::CodeStudio && source.path == drifted)
            .unwrap_or_else(|| {
                panic!("missing drifted Code Studio source {drifted:?} in {sources:#?}")
            });
        assert_eq!(drifted_source.status, ProviderSourceStatus::Empty);
        assert_eq!(
            drifted_source.unsupported_reason,
            Some("path exists but no Syncfusion Code Studio session-store.db database was found")
        );
    }

    #[test]
    fn zencoder_discovery_uses_vscode_app_data_session_trees_as_native_sources() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let xdg = temp.path().join("xdg-config");
        let appdata = temp.path().join("appdata");
        let _xdg_config_home = EnvGuard::set("XDG_CONFIG_HOME", xdg.as_os_str());
        let _appdata = EnvGuard::set("APPDATA", appdata.as_os_str());

        let home_root = temp
            .path()
            .join(".config/Code/User/globalStorage/ZencoderAI.zencoder/zencoder-chat");
        let xdg_root = xdg.join("Code/User/globalStorage/ZencoderAI.zencoder/zencoder-chat");
        let appdata_root =
            appdata.join("Code/User/globalStorage/ZencoderAI.zencoder/zencoder-chat");
        for root in [&home_root, &xdg_root, &appdata_root] {
            write_zencoder_discovery_session(root);
        }

        let sources =
            discover_provider_sources_for_provider(temp.path(), CaptureProvider::Zencoder);
        for path in [&home_root, &xdg_root, &appdata_root] {
            let source = sources
                .iter()
                .find(|source| source.path == *path)
                .unwrap_or_else(|| panic!("missing Zencoder source {path:?} in {sources:#?}"));
            assert_eq!(source.status, ProviderSourceStatus::Available);
            assert_eq!(source.source_format, "zencoder_chat_sessions_json_tree");
            assert_eq!(source.import_support, ProviderImportSupport::Native);
            assert!(source.import_support.is_auto_importable());
            assert!(source.unsupported_reason.is_none());
        }
    }

    #[test]
    fn trae_discovery_uses_workspace_storage_roots_as_native_sources() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let appdata = temp.path().join("appdata");
        let _appdata = EnvGuard::set("APPDATA", appdata.as_os_str());

        let standard_mac_root = temp
            .path()
            .join("Library/Application Support/Trae/User/workspaceStorage");
        let mac_root = temp
            .path()
            .join("Library/Application Support/Trae CN/User/workspaceStorage");
        let standard_appdata_root = appdata.join("Trae/User/workspaceStorage");
        let appdata_root = appdata.join("Trae CN/User/workspaceStorage");
        for root in [
            &standard_mac_root,
            &mac_root,
            &standard_appdata_root,
            &appdata_root,
        ] {
            write_trae_discovery_db(&root.join("workspace-hash/state.vscdb"));
        }

        let empty_root = temp
            .path()
            .join("Library/Application Support/Trae/User/workspaceStorage-empty");
        write_trae_non_chat_state_db(&empty_root.join("workspace-hash/state.vscdb"));
        assert_eq!(
            has_trae_state_vscdb_chat_history(&empty_root, 10_000),
            BoundedProbe::NotFound
        );

        let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Trae);
        for path in [
            &standard_mac_root,
            &mac_root,
            &standard_appdata_root,
            &appdata_root,
        ] {
            let source = sources
                .iter()
                .find(|source| source.provider == CaptureProvider::Trae && source.path == *path)
                .unwrap_or_else(|| panic!("missing Trae source {path:?} in {sources:#?}"));
            assert_eq!(source.status, ProviderSourceStatus::Available);
            assert_eq!(source.source_format, TRAE_STATE_VSCDB_SOURCE_FORMAT);
            assert_eq!(source.import_support, ProviderImportSupport::Native);
            assert!(source.import_support.is_auto_importable());
            assert!(source.unsupported_reason.is_none());
        }
    }

    #[test]
    fn pi_discovery_uses_env_session_dir() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let custom = temp.path().join("pi-env-sessions");
        write_pi_discovery_session(&custom);
        let _session_dir = EnvGuard::set("PI_CODING_AGENT_SESSION_DIR", custom.as_os_str());
        let _agent_dir = EnvGuard::remove("PI_CODING_AGENT_DIR");

        let sources = discover_provider_sources(temp.path());
        let source = sources
            .iter()
            .find(|source| source.provider == CaptureProvider::Pi && source.path == custom)
            .unwrap();

        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.import_support, ProviderImportSupport::Native);
    }

    #[test]
    fn pi_discovery_uses_global_and_project_settings_session_dirs() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let _session_dir = EnvGuard::remove("PI_CODING_AGENT_SESSION_DIR");
        let _agent_dir = EnvGuard::remove("PI_CODING_AGENT_DIR");

        let global = temp.path().join("global-pi-sessions");
        write_pi_discovery_session(&global);
        std::fs::create_dir_all(temp.path().join(".pi/agent")).unwrap();
        std::fs::write(
            temp.path().join(".pi/agent/settings.json"),
            r#"{"sessionDir":"~/global-pi-sessions"}"#,
        )
        .unwrap();

        let project_sessions = project.path().join(".pi/custom-sessions");
        write_pi_discovery_session(&project_sessions);
        std::fs::write(
            project.path().join(".pi/settings.json"),
            r#"{"sessionDir":"custom-sessions"}"#,
        )
        .unwrap();

        let spec = provider_source_spec(CaptureProvider::Pi).unwrap();
        let project_settings_dirs = [
            project.path().join("subdir/.pi"),
            project.path().join(".pi"),
        ];
        let sources = discover_pi_custom_session_sources_with_project_settings(
            temp.path(),
            spec,
            &project_settings_dirs,
        );
        for path in [&global, &project_sessions] {
            let source = sources
                .iter()
                .find(|source| source.provider == CaptureProvider::Pi && source.path == *path)
                .unwrap();
            assert_eq!(source.status, ProviderSourceStatus::Available);
        }
    }

    #[test]
    fn cline_discovery_uses_env_data_dirs() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let custom = temp.path().join("custom-cline-data");
        write_task_json_discovery_task(&custom, "cline-env-task", "api_conversation_history.json");
        let _data_dir = EnvGuard::set("CLINE_DATA_DIR", custom.as_os_str());
        let _cline_dir = EnvGuard::remove("CLINE_DIR");
        let _session_dir = EnvGuard::remove("CLINE_SESSION_DATA_DIR");
        let _db_dir = EnvGuard::remove("CLINE_DB_DATA_DIR");

        let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Cline);
        let source = sources
            .iter()
            .find(|source| source.provider == CaptureProvider::Cline && source.path == custom)
            .unwrap();

        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.import_support, ProviderImportSupport::Native);
    }

    #[test]
    fn roo_discovery_uses_custom_storage_setting() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let custom = temp.path().join("roo-custom-storage");
        write_task_json_discovery_task(&custom, "roo-custom-task", "history_item.json");
        let settings = temp.path().join(".config/Code/User/settings.json");
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(
            &settings,
            r#"{"roo-cline.customStoragePath":"~/roo-custom-storage"}"#,
        )
        .unwrap();
        let _roo_code = EnvGuard::remove("ROO_CODE_DATA_DIR");
        let _roo = EnvGuard::remove("ROO_DATA_DIR");
        let _roo_cline = EnvGuard::remove("ROO_CLINE_DATA_DIR");

        let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::RooCode);
        let source = sources
            .iter()
            .find(|source| source.provider == CaptureProvider::RooCode && source.path == custom)
            .unwrap();

        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.import_support, ProviderImportSupport::Native);
    }

    #[test]
    fn bob_discovery_uses_ide_app_data_task_storage() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let _xdg_config_home = EnvGuard::remove("XDG_CONFIG_HOME");
        let _appdata = EnvGuard::remove("APPDATA");
        let ibm_bob = temp
            .path()
            .join(".config/IBM Bob/User/globalStorage/ibm.bob-code");
        let preview_bob = temp
            .path()
            .join(".config/Bob-IDE/User/globalStorage/ibm.bob-code");
        write_task_json_discovery_task(&ibm_bob, "bob-ga-task", "ui_messages.json");
        write_task_json_discovery_task(
            &preview_bob,
            "bob-preview-task",
            "api_conversation_history.json",
        );

        let sources = discover_provider_sources_for_provider(temp.path(), CaptureProvider::Bob);
        for path in [&ibm_bob, &preview_bob] {
            let source = sources
                .iter()
                .find(|source| source.provider == CaptureProvider::Bob && source.path == *path)
                .unwrap_or_else(|| panic!("missing Bob source {path:?} in {sources:#?}"));
            assert_eq!(source.source_format, "bob_task_directory_json");
            assert_eq!(source.status, ProviderSourceStatus::Available);
            assert_eq!(source.import_support, ProviderImportSupport::Native);
        }
        assert!(
            sources.iter().all(|source| !source
                .path
                .components()
                .any(|component| { component.as_os_str().to_str() == Some(".bob") })),
            "Bob IDE discovery must not scan Bob Shell ~/.bob paths: {sources:#?}"
        );
    }

    #[test]
    fn bounded_probe_reports_budget_exhausted_source_as_unknown() {
        let temp = tempfile::tempdir().unwrap();
        let claude = temp.path().join(".claude/projects");
        std::fs::create_dir_all(&claude).unwrap();
        for index in 0..10_001 {
            std::fs::create_dir(claude.join(format!("project-{index:05}"))).unwrap();
        }

        assert_source_status(
            temp.path(),
            CaptureProvider::Claude,
            ProviderSourceStatus::Unknown,
        );
    }

    #[test]
    fn default_location_probe_does_not_fallback_to_path_existence_for_unhandled_providers() {
        let temp = tempfile::tempdir().unwrap();
        let existing = temp.path().join("shell-history");
        std::fs::write(&existing, "{}\n").unwrap();
        let location = ProviderDefaultLocation {
            path_components: &["shell-history"],
            source_format: "shell_history",
            source_kind: ProviderSourceKind::NativeHistory,
        };

        assert_eq!(
            default_location_import_probe(CaptureProvider::Shell, &location, &existing),
            BoundedProbe::NotFound
        );
    }

    #[cfg(unix)]
    #[test]
    fn default_source_probe_reports_unreadable_directory_as_unknown() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let sessions = temp.path().join(".codex/sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let original_permissions = std::fs::metadata(&sessions).unwrap().permissions();
        std::fs::set_permissions(&sessions, std::fs::Permissions::from_mode(0o000)).unwrap();

        if std::fs::read_dir(&sessions).is_ok() {
            std::fs::set_permissions(&sessions, original_permissions).unwrap();
            return;
        }

        let source = discover_provider_sources(temp.path())
            .into_iter()
            .find(|source| {
                source.provider == CaptureProvider::Codex
                    && source.source_format == "codex_session_jsonl_tree"
            })
            .unwrap();
        std::fs::set_permissions(&sessions, original_permissions).unwrap();

        assert_eq!(source.status, ProviderSourceStatus::Unknown);
        assert!(source
            .unsupported_reason
            .unwrap()
            .contains("could not be read"));
    }

    #[cfg(unix)]
    #[test]
    fn default_source_probe_skips_unreadable_child_directory() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let sessions = temp.path().join(".codex/sessions");
        let readable = sessions.join("readable");
        let unreadable = sessions.join("unreadable");
        std::fs::create_dir_all(&readable).unwrap();
        std::fs::create_dir_all(&unreadable).unwrap();
        std::fs::write(readable.join("session.jsonl"), "{}\n").unwrap();

        let original_permissions = std::fs::metadata(&unreadable).unwrap().permissions();
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();

        if std::fs::read_dir(&unreadable).is_ok() {
            std::fs::set_permissions(&unreadable, original_permissions).unwrap();
            return;
        }

        let source = discover_provider_sources(temp.path())
            .into_iter()
            .find(|source| {
                source.provider == CaptureProvider::Codex
                    && source.source_format == "codex_session_jsonl_tree"
            });
        std::fs::set_permissions(&unreadable, original_permissions).unwrap();

        let source = source.unwrap();
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.unsupported_reason, None);
    }

    fn write_pi_discovery_session(root: &Path) {
        let project = root.join("--workspace--");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("2026-07-03T12-00-00-000Z_pi-discovery.jsonl"),
            "{}\n",
        )
        .unwrap();
    }

    fn write_qwen_discovery_chat(projects: &Path) {
        let chats = projects.join("project/chats");
        std::fs::create_dir_all(&chats).unwrap();
        std::fs::write(chats.join("qwen-discovery.jsonl"), "{}\n").unwrap();
    }

    fn write_kimi_discovery_wire(home: &Path) {
        let agent = home.join("sessions/wd_project_abc123/kimi-session/agents/main");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(agent.join("wire.jsonl"), "{}\n").unwrap();
    }

    fn write_autohand_discovery_session(sessions: &Path) {
        let session = sessions.join("autohand-discovery");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(
            session.join("metadata.json"),
            r#"{"sessionId":"autohand-discovery","createdAt":"2026-07-01T12:00:00Z","lastActiveAt":"2026-07-01T12:00:01Z","projectPath":"/workspace","projectName":"workspace","model":"fixture","messageCount":1,"status":"completed"}"#,
        )
        .unwrap();
        std::fs::write(session.join("conversation.jsonl"), "{}\n").unwrap();
    }

    fn write_iflow_discovery_session(projects: &Path) {
        let project = projects.join("sanitized-workspace");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("session-iflow-discovery.jsonl"), "{}\n").unwrap();
    }

    fn write_jazz_discovery_history(history: &Path) {
        std::fs::create_dir_all(history).unwrap();
        std::fs::write(
            history.join("jazz-agent.json"),
            r#"{"agentId":"jazz-agent","conversations":[]}"#,
        )
        .unwrap();
    }

    fn write_eve_discovery_workflow_data(data_dir: &Path, suffix: &str) {
        let run_id = format!("wrun_{suffix}");
        let stream_name = format!("strm_{suffix}_user");
        std::fs::create_dir_all(data_dir.join("streams/runs")).unwrap();
        std::fs::create_dir_all(data_dir.join("streams/chunks")).unwrap();
        std::fs::write(
            data_dir.join("streams/runs").join(format!("{run_id}.json")),
            format!(r#"{{"streams":["{stream_name}"]}}"#),
        )
        .unwrap();
        std::fs::write(
            data_dir
                .join("streams/chunks")
                .join(format!("{stream_name}-chnk_01.bin")),
            b"\0",
        )
        .unwrap();
    }

    fn write_junie_discovery_session(sessions: &Path, session_id: &str) {
        std::fs::create_dir_all(sessions.join(session_id)).unwrap();
        std::fs::write(
            sessions.join("index.jsonl"),
            format!(r#"{{"sessionId":"{session_id}","createdAt":1783339200000}}"#),
        )
        .unwrap();
        std::fs::write(
            sessions.join(session_id).join("events.jsonl"),
            "{\"kind\":\"UserPromptEvent\",\"prompt\":\"Junie discovery\"}\n",
        )
        .unwrap();
    }

    fn write_openloaf_discovery_session(history: &Path, session_id: &str) {
        let session = history.join(session_id);
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(
            session.join("session.json"),
            format!(r#"{{"id":"{session_id}","createdAt":"2026-07-04T19:00:00Z"}}"#),
        )
        .unwrap();
        std::fs::write(
            session.join("messages.jsonl"),
            format!(
                r#"{{"id":"{session_id}-msg","role":"user","messageKind":"message","parts":[{{"type":"text","text":"openloaf discovery"}}],"createdAt":"2026-07-04T19:00:01Z"}}"#
            ),
        )
        .unwrap();
    }

    fn write_generic_project_jsonl(projects: &Path, file_name: &str) {
        let project = projects.join("sanitized-workspace");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join(file_name), "{}\n").unwrap();
    }

    fn write_mistral_vibe_discovery_session(sessions: &Path) {
        let session = sessions.join("session_20260704_120000_vibe1234");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(
            session.join("meta.json"),
            r#"{"session_id":"mistral-vibe-discovery","start_time":"2026-07-04T12:00:00Z","end_time":null,"git_commit":null,"git_branch":null,"environment":{"working_directory":"/workspace"},"username":"fixture"}"#,
        )
        .unwrap();
        std::fs::write(session.join("messages.jsonl"), "{}\n").unwrap();
    }

    fn write_mux_discovery_session(sessions: &Path) {
        let session = sessions.join("mux-discovery");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(
            session.join("chat.jsonl"),
            r#"{"id":"msg-mux-discovery","role":"user","parts":[{"type":"text","text":"mux discovery"}],"metadata":{"historySequence":0},"workspaceId":"mux-discovery"}"#,
        )
        .unwrap();
    }

    fn write_reasonix_discovery_session(sessions: &Path) {
        std::fs::create_dir_all(sessions).unwrap();
        std::fs::write(
            sessions.join("reasonix-discovery.jsonl"),
            r#"{"role":"user","content":"reasonix discovery"}"#,
        )
        .unwrap();
    }

    fn shared_provider_history_fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("tests/fixtures/provider-history")
            .join(name)
    }

    fn write_task_json_discovery_task(root: &Path, task_id: &str, file_name: &str) {
        let task = root.join("tasks").join(task_id);
        std::fs::create_dir_all(&task).unwrap();
        std::fs::write(task.join(file_name), "[]").unwrap();
    }

    fn write_lingma_discovery_db(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE chat_record (
                session_id TEXT,
                request_id TEXT,
                chat_prompt TEXT,
                summary TEXT,
                error_result TEXT,
                gmt_create INTEGER,
                extra TEXT
            );
            "#,
        )
        .unwrap();
    }

    fn write_trae_discovery_db(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(path).unwrap();
        conn.execute(
            "CREATE TABLE ItemTable ([key] TEXT PRIMARY KEY, value TEXT)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ItemTable ([key], value) VALUES (?1, ?2)",
            rusqlite::params![
                "memento/icube-ai-agent-storage",
                r#"{"list":[{"id":"input-1","messages":[{"role":"user","content":"trae discovery"}]}]}"#
            ],
        )
        .unwrap();
    }

    fn write_trae_non_chat_state_db(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(path).unwrap();
        conn.execute(
            "CREATE TABLE ItemTable ([key] TEXT PRIMARY KEY, value TEXT)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ItemTable ([key], value) VALUES ('workbench.view.extension', '{}')",
            [],
        )
        .unwrap();
    }

    fn write_zencoder_discovery_session(root: &Path) {
        let sessions = root.join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(
            root.join("sessions.json"),
            r#"[{"id":"zencoder-discovery"}]"#,
        )
        .unwrap();
        std::fs::write(
            sessions.join("zencoder-discovery.json"),
            r#"{"id":"zencoder-discovery","messages":[]}"#,
        )
        .unwrap();
    }

    fn write_codestudio_discovery_db(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE schema_version (version INTEGER NOT NULL);
            CREATE TABLE sessions (id TEXT PRIMARY KEY);
            CREATE TABLE turns (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT NOT NULL
            );
            "#,
        )
        .unwrap();
    }

    fn write_codestudio_schema_drift_db(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE schema_version (version INTEGER NOT NULL);
            CREATE TABLE sessions (id TEXT PRIMARY KEY);
            CREATE TABLE turns (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL
            );
            "#,
        )
        .unwrap();
    }

    fn assert_source_status(
        home: &Path,
        provider: CaptureProvider,
        expected: ProviderSourceStatus,
    ) {
        let source = discover_provider_sources(home)
            .into_iter()
            .find(|source| source.provider == provider)
            .unwrap();
        assert_eq!(source.status, expected, "{provider:?}");
    }
}
