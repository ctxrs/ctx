use std::path::{Path, PathBuf};

use work_record_core::{CaptureProvider, ProviderRawRetention, ProviderRedactionBoundary};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderSourceKind {
    NativeHistory,
    NormalizedDeveloperInput,
    DetectionOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderImportSupport {
    Native,
    NormalizedDeveloperOnly,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderCatalogSupport {
    Native,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderSourceStatus {
    Available,
    Missing,
    Unsupported,
}

impl ProviderSourceStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
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
    source_kind: ProviderSourceKind::DetectionOnly,
}];

const OPENCODE_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".local", "share", "opencode", "opencode.db"],
    source_format: "opencode_sqlite",
    source_kind: ProviderSourceKind::DetectionOnly,
}];

const ANTIGRAVITY_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".config", "antigravity"],
    source_format: "antigravity_unknown",
    source_kind: ProviderSourceKind::DetectionOnly,
}];

const GEMINI_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".gemini"],
    source_format: "gemini_unknown",
    source_kind: ProviderSourceKind::DetectionOnly,
}];

const CURSOR_DEFAULTS: &[ProviderDefaultLocation] = &[
    ProviderDefaultLocation {
        path_components: &[".cursor"],
        source_format: "cursor_unknown",
        source_kind: ProviderSourceKind::DetectionOnly,
    },
    ProviderDefaultLocation {
        path_components: &[".config", "Cursor"],
        source_format: "cursor_config",
        source_kind: ProviderSourceKind::DetectionOnly,
    },
];

const COPILOT_DEFAULTS: &[ProviderDefaultLocation] = &[
    ProviderDefaultLocation {
        path_components: &[".copilot", "session-state"],
        source_format: "copilot_session_state",
        source_kind: ProviderSourceKind::DetectionOnly,
    },
    ProviderDefaultLocation {
        path_components: &[".copilot", "session-store.db"],
        source_format: "copilot_session_store_sqlite",
        source_kind: ProviderSourceKind::DetectionOnly,
    },
];

const FACTORY_DROID_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".factory"],
    source_format: "factory_ai_droid_unknown",
    source_kind: ProviderSourceKind::DetectionOnly,
}];

const AMP_DEFAULTS: &[ProviderDefaultLocation] = &[ProviderDefaultLocation {
    path_components: &[".config", "amp", "settings.json"],
    source_format: "amp_settings",
    source_kind: ProviderSourceKind::DetectionOnly,
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
        import_support: ProviderImportSupport::Unsupported,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::None,
        redaction_boundary: ProviderRedactionBoundary::ManualReview,
        unsupported_reason: Some(
            "native Claude local-history import is blocked until a read-only .claude/projects parser and sanitized native fixtures ship",
        ),
    },
    ProviderSourceSpec {
        provider: CaptureProvider::OpenCode,
        display_name: "OpenCode",
        default_locations: OPENCODE_DEFAULTS,
        import_support: ProviderImportSupport::Unsupported,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::None,
        redaction_boundary: ProviderRedactionBoundary::ManualReview,
        unsupported_reason: Some(
            "native OpenCode import is blocked until a read-only opencode.db/export parser and sanitized native fixtures ship",
        ),
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Antigravity,
        display_name: "Antigravity",
        default_locations: ANTIGRAVITY_DEFAULTS,
        import_support: ProviderImportSupport::Unsupported,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::None,
        redaction_boundary: ProviderRedactionBoundary::ManualReview,
        unsupported_reason: Some(
            "native Antigravity import is blocked until a stable local transcript path/schema is proven",
        ),
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Gemini,
        display_name: "Gemini",
        default_locations: GEMINI_DEFAULTS,
        import_support: ProviderImportSupport::Unsupported,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::None,
        redaction_boundary: ProviderRedactionBoundary::ManualReview,
        unsupported_reason: Some(
            "native Gemini import is blocked until a stable local session/checkpoint parser and sanitized native fixtures ship",
        ),
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Cursor,
        display_name: "Cursor",
        default_locations: CURSOR_DEFAULTS,
        import_support: ProviderImportSupport::Unsupported,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::None,
        redaction_boundary: ProviderRedactionBoundary::ManualReview,
        unsupported_reason: Some(
            "native Cursor import is blocked until persisted local DB/files and a read-only parser are proven",
        ),
    },
    ProviderSourceSpec {
        provider: CaptureProvider::CopilotCli,
        display_name: "Copilot CLI",
        default_locations: COPILOT_DEFAULTS,
        import_support: ProviderImportSupport::Unsupported,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::None,
        redaction_boundary: ProviderRedactionBoundary::ManualReview,
        unsupported_reason: Some(
            "Copilot CLI local history is detected but unsupported until session-state/session-store schemas, redaction, and read-only parser fixtures ship",
        ),
    },
    ProviderSourceSpec {
        provider: CaptureProvider::FactoryAiDroid,
        display_name: "Factory AI Droid",
        default_locations: FACTORY_DROID_DEFAULTS,
        import_support: ProviderImportSupport::Unsupported,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::None,
        redaction_boundary: ProviderRedactionBoundary::ManualReview,
        unsupported_reason: Some(
            "Factory AI Droid native import is blocked because no stable durable local transcript path/schema is proven",
        ),
    },
    ProviderSourceSpec {
        provider: CaptureProvider::Amp,
        display_name: "Amp",
        default_locations: AMP_DEFAULTS,
        import_support: ProviderImportSupport::Unsupported,
        catalog_support: ProviderCatalogSupport::None,
        raw_retention: ProviderRawRetention::None,
        redaction_boundary: ProviderRedactionBoundary::ManualReview,
        unsupported_reason: Some(
            "Amp native local thread import is blocked because no stable local thread file path/schema is proven",
        ),
    },
];

pub fn provider_source_specs() -> &'static [ProviderSourceSpec] {
    PROVIDER_SPECS
}

pub fn provider_source_spec(provider: CaptureProvider) -> Option<&'static ProviderSourceSpec> {
    PROVIDER_SPECS.iter().find(|spec| spec.provider == provider)
}

pub fn discover_provider_sources(home: &Path) -> Vec<ProviderSource> {
    PROVIDER_SPECS
        .iter()
        .flat_map(|spec| {
            spec.default_locations.iter().map(|location| {
                let path = location
                    .path_components
                    .iter()
                    .fold(home.to_path_buf(), |path, component| path.join(component));
                provider_source_from_location(spec, location, path)
            })
        })
        .collect()
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
        _ => "normalized_provider_jsonl",
    };
    let explicit_import_support = if matches!(spec.import_support, ProviderImportSupport::Native) {
        ProviderImportSupport::Native
    } else if source_format == "normalized_provider_jsonl" {
        ProviderImportSupport::NormalizedDeveloperOnly
    } else {
        spec.import_support
    };
    let source_kind = if matches!(explicit_import_support, ProviderImportSupport::Native) {
        ProviderSourceKind::NativeHistory
    } else {
        ProviderSourceKind::NormalizedDeveloperInput
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
    let exists = path.exists();
    let status = if matches!(spec.import_support, ProviderImportSupport::Unsupported) {
        ProviderSourceStatus::Unsupported
    } else if exists {
        ProviderSourceStatus::Available
    } else {
        ProviderSourceStatus::Missing
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
        unsupported_reason: spec.unsupported_reason,
    }
}
