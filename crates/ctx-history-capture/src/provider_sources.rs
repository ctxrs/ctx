use std::{
    collections::HashSet,
    env, fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use ctx_history_core::{CaptureProvider, ProviderRawRetention, ProviderRedactionBoundary};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderSourceKind {
    NativeHistory,
    DetectionOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderImportSupport {
    Native,
    Preview,
    Unsupported,
}

impl ProviderImportSupport {
    pub fn is_importable(self) -> bool {
        matches!(self, Self::Native | Self::Preview)
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

const KILO_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".local", "share", "kilo", "kilo.db"],
    source_format: "kilo_sqlite",
    source_kind: ProviderSourceKind::NativeHistory,
}];

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

const DEXTO_DEFAULTS: &[ProviderDefaultLocation] = &[];

const ANTIGRAVITY_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".gemini", "antigravity-cli", "brain"],
    source_format: "antigravity_cli_transcript_jsonl_tree",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const GEMINI_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".gemini"],
    source_format: "gemini_cli_chat_recording_jsonl",
    source_kind: ProviderSourceKind::NativeHistory,
}];

const CURSOR_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".cursor", "projects"],
    source_format: "cursor_agent_transcript_jsonl_tree",
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
        import_support: ProviderImportSupport::Preview,
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

    let mut sources = spec
        .default_locations
        .iter()
        .map(|location| {
            let path = location
                .path_components
                .iter()
                .fold(home.to_path_buf(), |path, component| path.join(component));
            provider_source_from_location(spec, location, path)
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
        CaptureProvider::Cline => {
            sources.extend(discover_cline_task_json_sources(home, spec));
        }
        CaptureProvider::RooCode => {
            sources.extend(discover_roo_task_json_sources(home, spec));
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
        _ => {}
    }

    sources
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

fn task_json_source(spec: &ProviderSourceSpec, path: PathBuf) -> ProviderSource {
    provider_source_from_parts(
        spec,
        path,
        match spec.provider {
            CaptureProvider::RooCode => "roo_task_directory_json",
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
        CaptureProvider::Kilo => "kilo_sqlite",
        CaptureProvider::Crush => "crush_sqlite",
        CaptureProvider::Goose => "goose_sessions_sqlite",
        CaptureProvider::Antigravity => "antigravity_cli_transcript_jsonl_tree",
        CaptureProvider::Gemini => "gemini_cli_chat_recording_jsonl",
        CaptureProvider::Cursor
            if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") =>
        {
            "cursor_agent_transcript_jsonl"
        }
        CaptureProvider::Cursor => "cursor_agent_transcript_jsonl_tree",
        CaptureProvider::Zed => "zed_threads_sqlite",
        CaptureProvider::CopilotCli => "copilot_cli_session_events_jsonl",
        CaptureProvider::FactoryAiDroid => "factory_ai_droid_sessions_jsonl",
        CaptureProvider::QwenCode if path.is_dir() => "qwen_code_chat_jsonl_tree",
        CaptureProvider::QwenCode => "qwen_code_chat_jsonl",
        CaptureProvider::KimiCodeCli if path.is_dir() => "kimi_code_cli_wire_jsonl_tree",
        CaptureProvider::KimiCodeCli => "kimi_code_cli_wire_jsonl",
        CaptureProvider::AutohandCode => "autohand_code_sessions_jsonl",
        CaptureProvider::OpenClaw => "openclaw_session_jsonl_tree",
        CaptureProvider::Hermes => "hermes_state_sqlite",
        CaptureProvider::NanoClaw => "nanoclaw_project",
        CaptureProvider::AstrBot => "astrbot_data_v4_sqlite",
        CaptureProvider::Shelley => "shelley_sqlite",
        CaptureProvider::Continue => "continue_cli_sessions_json",
        CaptureProvider::OpenHands => "openhands_file_events",
        CaptureProvider::Cline => "cline_task_directory_json",
        CaptureProvider::RooCode => "roo_task_directory_json",
        CaptureProvider::Dexto => "dexto_sqlite",
        CaptureProvider::CodeBuddy => "codebuddy_history_json",
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
        CaptureProvider::Dexto => Some("path exists but no Dexto SQLite database was found"),
        CaptureProvider::CodeBuddy => {
            Some("path exists but no CodeBuddy history sessions were found")
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
        CaptureProvider::Cursor => {
            Some("path exists but the Cursor transcript probe hit its scan budget")
        }
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
        CaptureProvider::Kilo => {
            Some("path exists but the Kilo database could not be read; check permissions")
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
        CaptureProvider::CodeBuddy => Some(
            "path exists but CodeBuddy history JSON files could not be read; check permissions",
        ),
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
        CaptureProvider::Kilo => path_is_file_probe(path),
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
        CaptureProvider::Dexto => path_is_file_probe(path),
        CaptureProvider::Antigravity => has_jsonl_file_under_matching(path, 10_000, |candidate| {
            matches!(
                candidate.file_name().and_then(|name| name.to_str()),
                Some("transcript_full.jsonl" | "transcript.jsonl")
            )
        }),
        CaptureProvider::Gemini => has_gemini_chat_jsonl(path, 10_000),
        CaptureProvider::Cursor => has_jsonl_file_under_matching(path, 10_000, |candidate| {
            path_has_component(candidate, "agent-transcripts")
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
        CaptureProvider::CodeBuddy => has_codebuddy_history_json(path, 10_000),
        CaptureProvider::Shell
        | CaptureProvider::Git
        | CaptureProvider::Jj
        | CaptureProvider::Gh
        | CaptureProvider::Custom
        | CaptureProvider::Unknown => BoundedProbe::NotFound,
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
        assert_eq!(
            astrbot_source.import_support,
            ProviderImportSupport::Preview
        );
        assert!(astrbot_source.import_support.is_importable());
        assert!(!astrbot_source.import_support.is_auto_importable());

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
    fn dexto_discovery_is_explicit_path_only() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("dexto.db");
        std::fs::write(&db, b"sqlite fixture marker").unwrap();

        let discovered =
            discover_provider_sources_for_provider(temp.path(), CaptureProvider::Dexto);
        assert!(discovered.is_empty(), "{discovered:#?}");
        let source = provider_source_for_path(CaptureProvider::Dexto, db.clone());
        assert_eq!(source.path, db);
        assert_eq!(source.status, ProviderSourceStatus::Available);
        assert_eq!(source.source_format, "dexto_sqlite");
        assert_eq!(source.import_support, ProviderImportSupport::Native);
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

    fn write_task_json_discovery_task(root: &Path, task_id: &str, file_name: &str) {
        let task = root.join("tasks").join(task_id);
        std::fs::create_dir_all(&task).unwrap();
        std::fs::write(task.join(file_name), "[]").unwrap();
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
