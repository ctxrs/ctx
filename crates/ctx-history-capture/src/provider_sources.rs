use std::{
    collections::HashSet,
    env,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use ctx_history_core::{CaptureProvider, ProviderRawRetention, ProviderRedactionBoundary};

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

const PI_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".pi", "sessions.jsonl"],
    source_format: "pi_session_jsonl",
    source_kind: ProviderSourceKind::NativeHistory,
}];

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
        _ => {}
    }

    sources
}

fn env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
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
        CaptureProvider::Antigravity => "antigravity_cli_transcript_jsonl_tree",
        CaptureProvider::Gemini => "gemini_cli_chat_recording_jsonl",
        CaptureProvider::Cursor
            if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") =>
        {
            "cursor_agent_transcript_jsonl"
        }
        CaptureProvider::Cursor => "cursor_agent_transcript_jsonl_tree",
        CaptureProvider::CopilotCli => "copilot_cli_session_events_jsonl",
        CaptureProvider::FactoryAiDroid => "factory_ai_droid_sessions_jsonl",
        CaptureProvider::OpenClaw => "openclaw_session_jsonl_tree",
        CaptureProvider::Hermes => "hermes_state_sqlite",
        CaptureProvider::NanoClaw => "nanoclaw_project",
        CaptureProvider::AstrBot => "astrbot_data_v4_sqlite",
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
        CaptureProvider::Pi => Some("path exists but no Pi session JSONL file was found"),
        CaptureProvider::Claude => {
            Some("path exists but no Claude project JSONL transcripts were found")
        }
        CaptureProvider::OpenCode => Some("path exists but no OpenCode SQLite database was found"),
        CaptureProvider::Antigravity => {
            Some("path exists but no Antigravity transcript JSONL files were found")
        }
        CaptureProvider::Gemini => Some(
            "path exists but no Gemini CLI chat JSONL transcripts were found under tmp/*/chats",
        ),
        CaptureProvider::Cursor => {
            Some("path exists but no Cursor agent JSONL transcripts were found")
        }
        CaptureProvider::CopilotCli => {
            Some("path exists but no Copilot CLI session event JSONL files were found")
        }
        CaptureProvider::FactoryAiDroid => {
            Some("path exists but no Factory AI Droid session JSONL files were found")
        }
        CaptureProvider::OpenClaw => {
            Some("path exists but no OpenClaw agent session JSONL files were found")
        }
        CaptureProvider::Hermes => Some("path exists but no Hermes state.db file was found"),
        CaptureProvider::NanoClaw => {
            Some("path exists but no NanoClaw data/v2.db and data/v2-sessions store was found")
        }
        CaptureProvider::AstrBot => Some("path exists but no AstrBot data/data_v4.db was found"),
        _ => None,
    }
}

fn unknown_source_reason(provider: CaptureProvider) -> Option<&'static str> {
    match provider {
        CaptureProvider::Codex => {
            Some("path exists but the Codex session transcript probe hit its scan budget")
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
        CaptureProvider::CopilotCli => {
            Some("path exists but the Copilot CLI transcript probe hit its scan budget")
        }
        CaptureProvider::FactoryAiDroid => {
            Some("path exists but the Factory AI Droid transcript probe hit its scan budget")
        }
        CaptureProvider::OpenClaw => {
            Some("path exists but the OpenClaw transcript probe hit its scan budget")
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
            Some("path exists but the Pi session file could not be read; check permissions")
        }
        CaptureProvider::Claude => {
            Some("path exists but Claude project transcripts could not be read; check permissions")
        }
        CaptureProvider::OpenCode => {
            Some("path exists but the OpenCode database could not be read; check permissions")
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
        CaptureProvider::CopilotCli => {
            Some("path exists but Copilot CLI session events could not be read; check permissions")
        }
        CaptureProvider::FactoryAiDroid => {
            Some("path exists but Factory AI Droid sessions could not be read; check permissions")
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
        CaptureProvider::Pi => path_is_file_probe(path),
        CaptureProvider::OpenCode => path_is_file_probe(path),
        CaptureProvider::Claude => has_jsonl_file_under_matching(path, 10_000, |_| true),
        CaptureProvider::OpenClaw => has_openclaw_session_jsonl(path, 10_000),
        CaptureProvider::Hermes => path_is_file_probe(path),
        CaptureProvider::NanoClaw => has_nanoclaw_project(path),
        CaptureProvider::AstrBot => path_is_file_probe(path),
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
        CaptureProvider::CopilotCli => has_jsonl_file_under_matching(path, 10_000, |candidate| {
            candidate.file_name().and_then(|name| name.to_str()) == Some("events.jsonl")
        }),
        CaptureProvider::FactoryAiDroid => has_jsonl_file_under_matching(path, 10_000, |_| true),
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
    match path_metadata_probe(root) {
        PathProbe::File => {
            return if root.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
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
                && path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
                && matches_path(&path)
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
