use std::{
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use ctx_history_core::CaptureProvider;
use serde_json::Value;

use ctx_history_source_io::open_provider_source_file;

use super::super::{
    probes::{
        has_deepseek_harness_session_file, has_openhands_current_event_json,
        is_openhands_current_event_json, BoundedProbe,
    },
    provider_source_spec,
    resolvers::unsupported_source,
    selectors::{self, SourcePathError, SourcePathKind},
    ProviderCatalogSupport, ProviderDefaultLocation, ProviderImportSupport, ProviderSource,
    ProviderSourceKind, ProviderSourceSpec, ProviderSourceStatus, StaticProviderProbeCatalog,
};

const CODEX_AMBIGUOUS_JSONL_REASON: &str =
    "Codex JSONL schema is ambiguous; the bounded first-record probe requires either prompt-history fields (session_id, ts, text) or rollout fields (timestamp, type, payload)";
const PI_INVALID_JSONL_REASON: &str = "Pi explicit JSONL file has no valid session header";
const DEEPSEEK_HARNESS_INVALID_SOURCE_REASON: &str =
    "DeepSeek Harness explicit history must be session.jsonl, session.jsonl.zstd, or a session tree containing an exact nested leaf";
const UNSUPPORTED_EXPLICIT_ROOT_REASON: &str =
    "the explicit provider path uses an unsupported, non-local, or unsafe source root";
const PI_HEADER_PROBE_MAX_RECORDS: usize = 64;
const PI_HEADER_PROBE_MAX_BYTES: usize = 8 * 1024 * 1024;

pub fn provider_source_for_path(
    probes: &StaticProviderProbeCatalog,
    provider: CaptureProvider,
    path: PathBuf,
) -> ProviderSource {
    provider_source_for_path_with_optional_data_root(probes, provider, path, None)
}

/// Resolves one explicit provider path with bounded ctx-owned SQLite scratch
/// authority. This is required for portable read-only admission when a source
/// has a WAL or the platform has no retained-handle immutable SQLite VFS.
pub fn provider_source_for_path_with_data_root(
    probes: &StaticProviderProbeCatalog,
    provider: CaptureProvider,
    path: PathBuf,
    data_root: &Path,
) -> ProviderSource {
    provider_source_for_path_with_optional_data_root(probes, provider, path, Some(data_root))
}

fn provider_source_for_path_with_optional_data_root(
    probes: &StaticProviderProbeCatalog,
    provider: CaptureProvider,
    path: PathBuf,
    data_root: Option<&Path>,
) -> ProviderSource {
    let unknown_spec = ProviderSourceSpec {
        provider,
        display_name: "unknown",
        default_locations: &[],
        import_support: ProviderImportSupport::Unsupported,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: Some("provider is not registered for native local-history import"),
    };
    let spec = provider_source_spec(provider).unwrap_or(&unknown_spec);
    if provider == CaptureProvider::Cline {
        if let Some(data_root) = cline_sdk_data_root_for_explicit(&path) {
            let location = ProviderDefaultLocation {
                path_components: &[],
                source_format: "cline_sdk_session_store",
                source_kind: ProviderSourceKind::NativeHistory,
            };
            let probe = super::super::probes::default_location_import_probe(
                probes, None, provider, &location, &data_root,
            );
            return ProviderSource {
                provider,
                exists: true,
                path: data_root,
                source_format: location.source_format,
                source_kind: location.source_kind,
                import_support: spec.import_support,
                catalog_support: spec.catalog_support,
                status: match probe {
                    BoundedProbe::Found => ProviderSourceStatus::Available,
                    BoundedProbe::NotFound => ProviderSourceStatus::Empty,
                    BoundedProbe::BudgetExhausted | BoundedProbe::IoError => {
                        ProviderSourceStatus::Unknown
                    }
                },
                unsupported_reason: None,
                route_provenance: Default::default(),
            };
        }
    }
    let observed = selectors::source_path_kind(&path);
    let (path, openclaw_source_format) = if provider == CaptureProvider::OpenClaw {
        match openclaw_explicit_selection(probes, path, observed.ok(), data_root) {
            Ok(selection) => (selection.path, Some(selection.source_format)),
            Err((path, reason)) => return unsupported_source(spec, path, reason),
        }
    } else {
        (path, None)
    };
    let observed = selectors::source_path_kind(&path);
    let is_directory = observed == Ok(SourcePathKind::Directory);
    if let Some(reason) = exact_current_unsupported_reason(probes, provider, &path, observed.ok()) {
        return unsupported_source(spec, path, reason);
    }
    if provider == CaptureProvider::Pi
        && observed == Ok(SourcePathKind::File)
        && !pi_explicit_jsonl_has_session_header(&path)
    {
        return unsupported_source(spec, path, PI_INVALID_JSONL_REASON);
    }
    if provider == CaptureProvider::DeepSeekHarness
        && matches!(
            observed,
            Ok(SourcePathKind::File | SourcePathKind::Directory)
        )
        && has_deepseek_harness_session_file(&path, 10_000) != BoundedProbe::Found
    {
        return unsupported_source(spec, path, DEEPSEEK_HARNESS_INVALID_SOURCE_REASON);
    }
    let exists = !matches!(observed, Err(SourcePathError::Missing));

    let source_format = match provider {
        CaptureProvider::Codex if is_directory => "codex_session_jsonl_tree",
        CaptureProvider::Codex
            if observed == Ok(SourcePathKind::File)
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(".jsonl.zst")) =>
        {
            "codex_session_jsonl"
        }
        CaptureProvider::Codex if observed == Ok(SourcePathKind::File) => {
            let Some(source_format) = codex_explicit_jsonl_source_format(&path) else {
                return unsupported_source(spec, path, CODEX_AMBIGUOUS_JSONL_REASON);
            };
            source_format
        }
        CaptureProvider::Codex => {
            if path.file_name().and_then(|name| name.to_str()) == Some("history.jsonl") {
                "codex_history_jsonl"
            } else {
                "codex_session_jsonl"
            }
        }
        CaptureProvider::GrokBuild if is_directory => "grok_build_session_updates_jsonl_tree",
        CaptureProvider::GrokBuild => "grok_build_session_updates_jsonl",
        CaptureProvider::DeepSeekHarness if is_directory => "deepseek_harness_session_jsonl_tree",
        CaptureProvider::DeepSeekHarness => "deepseek_harness_session_jsonl",
        CaptureProvider::Pi => "pi_session_jsonl",
        CaptureProvider::Claude => "claude_projects_jsonl_tree",
        CaptureProvider::OpenCode => "opencode_sqlite",
        CaptureProvider::Kilo => "kilo_sqlite",
        CaptureProvider::KiroCli => "kiro_cli_sqlite",
        CaptureProvider::MiMoCode => "mimocode_sqlite",
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
        CaptureProvider::Zed => "zed_threads_sqlite",
        CaptureProvider::CopilotCli => "copilot_cli_session_events_jsonl",
        CaptureProvider::FactoryAiDroid => "factory_ai_droid_sessions_jsonl",
        CaptureProvider::QwenCode if is_directory => "qwen_code_chat_jsonl_tree",
        CaptureProvider::QwenCode => "qwen_code_chat_jsonl",
        CaptureProvider::KimiCodeCli if is_directory => "kimi_code_cli_wire_jsonl_tree",
        CaptureProvider::KimiCodeCli => "kimi_code_cli_wire_jsonl",
        CaptureProvider::Auggie => "auggie_session_json",
        CaptureProvider::Junie if is_directory => "junie_session_events_jsonl_tree",
        CaptureProvider::Junie => "junie_session_events_jsonl",
        CaptureProvider::Firebender => "firebender_chat_history_sqlite",
        CaptureProvider::ForgeCode => "forgecode_sqlite",
        CaptureProvider::DeepAgents => "deepagents_sessions_sqlite",
        CaptureProvider::MistralVibe if is_directory => "mistral_vibe_session_jsonl_tree",
        CaptureProvider::MistralVibe => "mistral_vibe_session_jsonl",
        CaptureProvider::Mux if is_directory => "mux_session_jsonl_tree",
        CaptureProvider::Mux => "mux_session_jsonl",
        CaptureProvider::RovoDev => "rovodev_session_json_tree",
        CaptureProvider::OpenClaw => openclaw_source_format.unwrap_or("unsupported"),
        CaptureProvider::Hermes => "hermes_state_sqlite",
        CaptureProvider::NanoClaw => "nanoclaw_project",
        CaptureProvider::AstrBot => "astrbot_data_v4_sqlite",
        CaptureProvider::Shelley => "shelley_sqlite",
        CaptureProvider::Continue => "continue_cli_sessions_json",
        CaptureProvider::OpenHands if is_openhands_current_cli_path(&path, observed.ok()) => {
            super::super::OPENHANDS_CURRENT_CLI_SOURCE_FORMAT
        }
        CaptureProvider::OpenHands => "openhands_file_events",
        CaptureProvider::Cline => "cline_task_directory_json",
        CaptureProvider::RooCode => "roo_task_directory_json",
        CaptureProvider::Lingma => "lingma_sqlite",
        CaptureProvider::Qoder if is_directory => "qoder_transcript_jsonl_tree",
        CaptureProvider::Qoder => "qoder_transcript_jsonl",
        CaptureProvider::Warp => "warp_sqlite",
        CaptureProvider::CodeBuddy => "codebuddy_history_json",
        CaptureProvider::Fx => "fx_sessions_tree",
        _ => "unsupported",
    };
    let explicit_import_support = spec.import_support;
    let source_kind = if explicit_import_support.is_importable() {
        ProviderSourceKind::NativeHistory
    } else {
        ProviderSourceKind::DetectionOnly
    };
    let exact_fx_status = if provider == CaptureProvider::Fx && observed.is_ok() {
        let location = ProviderDefaultLocation {
            path_components: &[],
            source_format,
            source_kind,
        };
        Some(
            match super::super::probes::default_location_import_probe(
                probes, data_root, provider, &location, &path,
            ) {
                BoundedProbe::Found => ProviderSourceStatus::Available,
                BoundedProbe::NotFound => ProviderSourceStatus::Empty,
                BoundedProbe::BudgetExhausted | BoundedProbe::IoError => {
                    ProviderSourceStatus::Unknown
                }
            },
        )
    } else {
        None
    };

    ProviderSource {
        provider,
        exists,
        path,
        source_format,
        source_kind,
        import_support: explicit_import_support,
        catalog_support: spec.catalog_support,
        status: if matches!(explicit_import_support, ProviderImportSupport::Unsupported)
            || matches!(observed, Err(SourcePathError::Unsupported))
        {
            ProviderSourceStatus::Unsupported
        } else if let Some(status) = exact_fx_status {
            status
        } else if observed.is_ok() {
            ProviderSourceStatus::Available
        } else if matches!(observed, Err(SourcePathError::Missing)) {
            ProviderSourceStatus::Missing
        } else {
            ProviderSourceStatus::Unknown
        },
        unsupported_reason: if matches!(observed, Err(SourcePathError::Unsupported)) {
            Some(UNSUPPORTED_EXPLICIT_ROOT_REASON)
        } else {
            spec.unsupported_reason
        },
        route_provenance: Default::default(),
    }
}

fn cline_sdk_data_root_for_explicit(path: &Path) -> Option<PathBuf> {
    let candidate = if selectors::ordinary_directory(path) {
        match path.file_name().and_then(|name| name.to_str()) {
            Some("sessions" | "db") => path.parent()?.to_path_buf(),
            _ => path.to_path_buf(),
        }
    } else if selectors::ordinary_file(path) {
        match path.file_name().and_then(|name| name.to_str()) {
            Some("sessions.index.json")
                if path
                    .parent()
                    .and_then(Path::file_name)
                    .and_then(|name| name.to_str())
                    == Some("sessions") =>
            {
                path.parent()?.parent()?.to_path_buf()
            }
            Some("sessions.db")
                if path
                    .parent()
                    .and_then(Path::file_name)
                    .and_then(|name| name.to_str())
                    == Some("db") =>
            {
                path.parent()?.parent()?.to_path_buf()
            }
            _ => return None,
        }
    } else {
        return None;
    };
    (selectors::ordinary_directory(&candidate)
        && (is_named_regular_file(&candidate.join("sessions/sessions.index.json"), |name| {
            name == "sessions.index.json"
        }) || is_named_regular_file(&candidate.join("db/sessions.db"), |name| {
            name == "sessions.db"
        })))
    .then_some(candidate)
}

fn codex_explicit_jsonl_source_format(path: &Path) -> Option<&'static str> {
    let value = certified_first_jsonl_value(path)?;
    let object = value.as_object()?;
    let prompt_history = object.get("session_id").and_then(Value::as_str).is_some()
        && object.get("ts").and_then(Value::as_i64).is_some()
        && object.get("text").and_then(Value::as_str).is_some();
    let rollout = object.get("timestamp").and_then(Value::as_str).is_some()
        && object.get("type").and_then(Value::as_str).is_some()
        && object.get("payload").and_then(Value::as_object).is_some();
    match (prompt_history, rollout) {
        (true, false) => Some("codex_history_jsonl"),
        (false, true) => Some("codex_session_jsonl"),
        _ => None,
    }
}

fn pi_explicit_jsonl_has_session_header(path: &Path) -> bool {
    certified_jsonl_value_matching(
        path,
        PI_HEADER_PROBE_MAX_RECORDS,
        PI_HEADER_PROBE_MAX_BYTES,
        |value| {
            value.get("type").and_then(Value::as_str) == Some("session")
                && value
                    .get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| !id.trim().is_empty())
                && value
                    .get("timestamp")
                    .and_then(Value::as_str)
                    .is_some_and(|timestamp| {
                        chrono::DateTime::parse_from_rfc3339(timestamp).is_ok()
                    })
        },
    )
    .is_some()
}

fn certified_first_jsonl_value(path: &Path) -> Option<Value> {
    certified_jsonl_value_matching(
        path,
        1,
        ctx_history_source_io::MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(2),
        |_| true,
    )
}

fn certified_jsonl_value_matching(
    path: &Path,
    max_records: usize,
    max_probe_bytes: usize,
    mut matches: impl FnMut(&Value) -> bool,
) -> Option<Value> {
    let file = open_provider_source_file(path).ok()?;
    let mut reader = BufReader::new(file.file().try_clone().ok()?);
    let mut record = Vec::new();
    let mut probed_bytes = 0_usize;
    for _ in 0..max_records {
        record.clear();
        let mut terminated = false;
        loop {
            let available = reader.fill_buf().ok()?;
            if available.is_empty() {
                break;
            }
            let take = available
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(available.len(), |index| index.saturating_add(1));
            if record.len().saturating_add(take)
                > ctx_history_source_io::MAX_PROVIDER_JSONL_LINE_BYTES
                || probed_bytes.saturating_add(take) > max_probe_bytes
            {
                return None;
            }
            terminated = available.get(take.saturating_sub(1)) == Some(&b'\n');
            record.extend_from_slice(&available[..take]);
            probed_bytes = probed_bytes.saturating_add(take);
            reader.consume(take);
            if terminated {
                break;
            }
        }
        if record.is_empty() {
            break;
        }
        while matches!(record.last(), Some(b'\n') | Some(b'\r')) {
            record.pop();
        }
        if let Ok(value) = serde_json::from_slice::<Value>(&record) {
            if matches(&value) {
                file.revalidate().ok()?;
                return Some(value);
            }
        }
        if !terminated {
            break;
        }
    }
    file.revalidate().ok()?;
    None
}

fn exact_current_unsupported_reason(
    probes: &StaticProviderProbeCatalog,
    provider: CaptureProvider,
    path: &Path,
    kind: Option<SourcePathKind>,
) -> Option<&'static str> {
    if kind == Some(SourcePathKind::Directory)
        && has_supported_explicit_history(probes, provider, path)
    {
        return None;
    }

    match provider {
        CaptureProvider::KiroCli if is_current_kiro_shape(path, kind?) => {
            Some("Kiro ACP/v3 session history is detected but unsupported")
        }
        CaptureProvider::Cline if is_current_cline_sdk_shape(path, kind?) => {
            Some("split Cline SDK roots are unsupported because ctx requires the common data root to bind the SDK catalog safely")
        }
        _ => None,
    }
}

fn has_supported_explicit_history(
    probes: &StaticProviderProbeCatalog,
    provider: CaptureProvider,
    path: &Path,
) -> bool {
    let source_format = match provider {
        CaptureProvider::Qoder => "qoder_transcript_jsonl_tree",
        CaptureProvider::OpenClaw => "openclaw_session_jsonl_tree",
        CaptureProvider::OpenHands => "openhands_file_events",
        CaptureProvider::Mux => "mux_session_jsonl_tree",
        CaptureProvider::Cline => "cline_task_directory_json",
        _ => return false,
    };
    let location = ProviderDefaultLocation {
        path_components: &[],
        source_format,
        source_kind: ProviderSourceKind::NativeHistory,
    };
    matches!(
        super::super::probes::default_location_import_probe(
            probes, None, provider, &location, path
        ),
        BoundedProbe::Found
    )
}

fn is_current_kiro_shape(path: &Path, kind: SourcePathKind) -> bool {
    if kind == SourcePathKind::Directory {
        let cli = if path.file_name().and_then(|name| name.to_str()) == Some("sessions") {
            path.join("cli")
        } else if path.file_name().and_then(|name| name.to_str()) == Some("cli")
            && path.parent().is_some_and(|parent| {
                parent.file_name().and_then(|name| name.to_str()) == Some("sessions")
            })
        {
            path.to_path_buf()
        } else {
            return false;
        };
        return direct_entries(&cli).is_some_and(|entries| {
            entries.iter().any(|entry| {
                let Some(stem) = entry.file_stem().and_then(|stem| stem.to_str()) else {
                    return false;
                };
                match entry.extension().and_then(|extension| extension.to_str()) {
                    Some("json") => is_named_regular_file(
                        &entry.with_file_name(format!("{stem}.jsonl")),
                        |name| name.ends_with(".jsonl"),
                    ),
                    Some("jsonl") => is_named_regular_file(
                        &entry.with_file_name(format!("{stem}.json")),
                        |name| name.ends_with(".json"),
                    ),
                    _ => false,
                }
            })
        });
    }
    if kind != SourcePathKind::File {
        return false;
    }
    let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
        return false;
    };
    let counterpart = match path.extension().and_then(|extension| extension.to_str()) {
        Some("json") => path.with_file_name(format!("{stem}.jsonl")),
        Some("jsonl") => path.with_file_name(format!("{stem}.json")),
        _ => return false,
    };
    path.parent()
        .is_some_and(|parent| parent.file_name().and_then(|name| name.to_str()) == Some("cli"))
        && path.parent().and_then(Path::parent).is_some_and(|parent| {
            parent.file_name().and_then(|name| name.to_str()) == Some("sessions")
        })
        && is_named_regular_file(&counterpart, |_| true)
}

fn contains_openclaw_sqlite(path: &Path, kind: SourcePathKind) -> bool {
    if kind == SourcePathKind::File {
        return is_named_regular_file(path, |name| name == "openclaw-agent.sqlite");
    }
    if kind != SourcePathKind::Directory {
        return false;
    }
    if is_named_regular_file(&path.join("openclaw-agent.sqlite"), |name| {
        name == "openclaw-agent.sqlite"
    }) || is_named_regular_file(&path.join("agent/openclaw-agent.sqlite"), |name| {
        name == "openclaw-agent.sqlite"
    }) {
        return true;
    }
    let agents = if path.file_name().and_then(|name| name.to_str()) == Some("agents") {
        path.to_path_buf()
    } else {
        path.join("agents")
    };
    direct_entries(&agents).is_some_and(|entries| {
        entries.iter().any(|agent| {
            is_named_regular_file(&agent.join("agent/openclaw-agent.sqlite"), |name| {
                name == "openclaw-agent.sqlite"
            })
        })
    })
}

struct OpenClawExplicitSelection {
    path: PathBuf,
    source_format: &'static str,
}

fn openclaw_explicit_selection(
    probes: &StaticProviderProbeCatalog,
    path: PathBuf,
    kind: Option<SourcePathKind>,
    data_root: Option<&Path>,
) -> Result<OpenClawExplicitSelection, (PathBuf, &'static str)> {
    const JSONL: &str = "openclaw_session_jsonl_tree";
    const SQLITE: &str = ctx_history_openclaw_schema::OPENCLAW_AGENT_SQLITE_SOURCE_FORMAT;
    const EXACT_AGENT_REQUIRED: &str =
        "OpenClaw roots containing per-agent SQLite require the exact agent database or agent root with --path";

    if kind == Some(SourcePathKind::File) {
        if is_openclaw_agent_sqlite_leaf(&path) {
            let jsonl = path
                .parent()
                .and_then(Path::parent)
                .map(|agent_root| agent_root.join("sessions"))
                .ok_or_else(|| (path.clone(), EXACT_AGENT_REQUIRED))?;
            return select_openclaw_sqlite(probes, path.clone(), path, jsonl, data_root);
        }
        return Ok(OpenClawExplicitSelection {
            path,
            source_format: JSONL,
        });
    }
    if kind.is_none()
        && path.file_name().and_then(|name| name.to_str()) == Some("openclaw-agent.sqlite")
    {
        return Ok(OpenClawExplicitSelection {
            path,
            source_format: SQLITE,
        });
    }
    if kind != Some(SourcePathKind::Directory) {
        return Ok(OpenClawExplicitSelection {
            path,
            source_format: JSONL,
        });
    }

    let (sqlite, jsonl) = if path.file_name().and_then(|name| name.to_str()) == Some("agent") {
        let Some(agent_root) = path.parent() else {
            return Err((path, EXACT_AGENT_REQUIRED));
        };
        (
            path.join("openclaw-agent.sqlite"),
            agent_root.join("sessions"),
        )
    } else {
        (
            path.join("agent/openclaw-agent.sqlite"),
            path.join("sessions"),
        )
    };
    if is_openclaw_agent_sqlite_leaf(&sqlite) {
        return select_openclaw_sqlite(probes, path, sqlite, jsonl, data_root);
    }
    if contains_openclaw_sqlite(&path, SourcePathKind::Directory) {
        return Err((path, EXACT_AGENT_REQUIRED));
    }
    Ok(OpenClawExplicitSelection {
        path,
        source_format: JSONL,
    })
}

fn select_openclaw_sqlite(
    probes: &StaticProviderProbeCatalog,
    selected_path: PathBuf,
    sqlite: PathBuf,
    jsonl: PathBuf,
    data_root: Option<&Path>,
) -> Result<OpenClawExplicitSelection, (PathBuf, &'static str)> {
    use super::super::probes::{has_openclaw_agent_sqlite_v17, BoundedProbe};

    const JSONL: &str = "openclaw_session_jsonl_tree";
    const SQLITE: &str = ctx_history_openclaw_schema::OPENCLAW_AGENT_SQLITE_SOURCE_FORMAT;
    const INVALID_SQLITE: &str =
        "OpenClaw openclaw-agent.sqlite does not satisfy the bounded current v17 schema and ownership contract";
    const PROBE_UNAVAILABLE: &str =
        "OpenClaw SQLite admission could not complete within the bounded read-only probe";

    let admission = has_openclaw_agent_sqlite_v17(data_root, &sqlite);
    if admission != BoundedProbe::Found
        && has_supported_explicit_history(probes, CaptureProvider::OpenClaw, &jsonl)
    {
        return Ok(OpenClawExplicitSelection {
            path: jsonl,
            source_format: JSONL,
        });
    }
    match admission {
        BoundedProbe::Found => Ok(OpenClawExplicitSelection {
            path: sqlite,
            source_format: SQLITE,
        }),
        BoundedProbe::NotFound => Err((selected_path, INVALID_SQLITE)),
        BoundedProbe::BudgetExhausted | BoundedProbe::IoError => {
            Err((selected_path, PROBE_UNAVAILABLE))
        }
    }
}

fn is_openclaw_agent_sqlite_leaf(path: &Path) -> bool {
    is_named_regular_file(path, |name| name == "openclaw-agent.sqlite")
        && path.parent().is_some_and(|parent| {
            parent.file_name().and_then(|name| name.to_str()) == Some("agent")
        })
        && path.parent().and_then(Path::parent).is_some_and(|agent| {
            agent
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| !name.is_empty())
        })
}

fn is_openhands_current_cli_path(path: &Path, kind: Option<SourcePathKind>) -> bool {
    if path_has_component(path, "v1_conversations") {
        return false;
    }
    if matches!(kind, Some(SourcePathKind::File)) && is_openhands_current_event_json(path) {
        return true;
    }
    if matches!(kind, Some(SourcePathKind::Directory))
        && has_openhands_current_event_json(path, 10_000) == BoundedProbe::Found
    {
        return true;
    }
    openhands_current_textual_alias(path)
}

fn openhands_current_textual_alias(path: &Path) -> bool {
    let components = path.components().collect::<Vec<_>>();
    for index in (0..components.len()).rev() {
        if components[index].as_os_str() != "conversations" {
            continue;
        }
        let tail = &components[index.saturating_add(1)..];
        return match tail {
            [] | [_] => true,
            [_conversation, events] => events.as_os_str() == "events",
            [_conversation, events, event] => {
                events.as_os_str() == "events"
                    && event
                        .as_os_str()
                        .to_str()
                        .is_some_and(|name| name.starts_with("event-") && name.ends_with(".json"))
            }
            _ => false,
        };
    }
    false
}

fn is_current_cline_sdk_shape(path: &Path, kind: SourcePathKind) -> bool {
    if kind == SourcePathKind::File {
        return is_current_cline_sdk_file(path);
    }
    kind == SourcePathKind::Directory
        && direct_entries(path).is_some_and(|entries| {
            entries.iter().any(|entry| {
                is_current_cline_sdk_file(entry)
                    || direct_entries(entry).is_some_and(|children| {
                        children
                            .iter()
                            .any(|child| is_current_cline_sdk_file(child))
                    })
            })
        })
}

fn is_current_cline_sdk_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if !is_named_regular_file(path, |_| true) {
        return false;
    }
    if matches!(name, "sessions.db" | "sessions.index.json") || name.ends_with(".messages.json") {
        return true;
    }
    let Some(id) = name.strip_suffix(".json") else {
        return false;
    };
    !id.is_empty()
        && is_named_regular_file(
            &path.with_file_name(format!("{id}.messages.json")),
            |candidate| candidate.ends_with(".messages.json"),
        )
}

fn direct_entries(path: &Path) -> Option<Vec<PathBuf>> {
    selectors::direct_entries(path).ok()
}

fn is_named_regular_file(path: &Path, matches: impl FnOnce(&str) -> bool) -> bool {
    selectors::ordinary_file(path)
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(matches)
}

fn path_has_component(path: &Path, expected: &str) -> bool {
    path.components()
        .any(|component| component.as_os_str().to_str() == Some(expected))
}
