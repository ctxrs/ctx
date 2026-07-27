use std::{
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use ctx_history_core::CaptureProvider;
use serde_json::Value;

use super::super::{
    open_ordinary_file_without_following,
    probes::{default_location_import_probe, BoundedProbe},
    provider_source_spec,
    resolvers::unsupported_source,
    selectors, ProviderCatalogSupport, ProviderDefaultLocation, ProviderImportSupport,
    ProviderSource, ProviderSourceKind, ProviderSourceSpec, ProviderSourceStatus,
};

const CODEX_AMBIGUOUS_JSONL_REASON: &str =
    "Codex JSONL schema is ambiguous; the bounded first-record probe requires either prompt-history fields (session_id, ts, text) or rollout fields (timestamp, type, payload)";

pub fn provider_source_for_path(provider: CaptureProvider, path: PathBuf) -> ProviderSource {
    let unknown_spec = ProviderSourceSpec {
        provider,
        display_name: "unknown",
        default_locations: &[],
        import_support: ProviderImportSupport::Unsupported,
        catalog_support: ProviderCatalogSupport::None,
        unsupported_reason: Some("provider is not registered for native local-history import"),
    };
    let spec = provider_source_spec(provider).unwrap_or(&unknown_spec);
    if let Some(reason) = exact_current_unsupported_reason(provider, &path) {
        return unsupported_source(spec, path, reason);
    }
    let exists = path.exists();

    let source_format = match provider {
        CaptureProvider::Codex if path.is_dir() => "codex_session_jsonl_tree",
        CaptureProvider::Codex if exists => {
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
        CaptureProvider::Auggie => "auggie_session_json",
        CaptureProvider::Junie if path.is_dir() => "junie_session_events_jsonl_tree",
        CaptureProvider::Junie => "junie_session_events_jsonl",
        CaptureProvider::Firebender => "firebender_chat_history_sqlite",
        CaptureProvider::ForgeCode => "forgecode_sqlite",
        CaptureProvider::DeepAgents => "deepagents_sessions_sqlite",
        CaptureProvider::MistralVibe if path.is_dir() => "mistral_vibe_session_jsonl_tree",
        CaptureProvider::MistralVibe => "mistral_vibe_session_jsonl",
        CaptureProvider::Mux if path.is_dir() => "mux_session_jsonl_tree",
        CaptureProvider::Mux => "mux_session_jsonl",
        CaptureProvider::RovoDev => "rovodev_session_json_tree",
        CaptureProvider::OpenClaw => "openclaw_session_jsonl_tree",
        CaptureProvider::Hermes => "hermes_state_sqlite",
        CaptureProvider::NanoClaw => "nanoclaw_project",
        CaptureProvider::AstrBot => "astrbot_data_v4_sqlite",
        CaptureProvider::Shelley => "shelley_sqlite",
        CaptureProvider::Continue => "continue_cli_sessions_json",
        CaptureProvider::OpenHands => "openhands_file_events",
        CaptureProvider::Cline => "cline_task_directory_json",
        CaptureProvider::RooCode => "roo_task_directory_json",
        CaptureProvider::Lingma => "lingma_sqlite",
        CaptureProvider::Trae => "trae_state_vscdb",
        CaptureProvider::Qoder if path.is_dir() => "qoder_transcript_jsonl_tree",
        CaptureProvider::Qoder => "qoder_transcript_jsonl",
        CaptureProvider::Warp => "warp_sqlite",
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
        unsupported_reason: spec.unsupported_reason,
    }
}

fn codex_explicit_jsonl_source_format(path: &Path) -> Option<&'static str> {
    let file = open_ordinary_file_without_following(path).ok()?;
    let mut reader = BufReader::new(file);
    let mut record = Vec::new();
    loop {
        let available = reader.fill_buf().ok()?;
        if available.is_empty() {
            break;
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index.saturating_add(1));
        if record.len().saturating_add(take) > crate::MAX_PROVIDER_JSONL_LINE_BYTES {
            return None;
        }
        let terminated = available.get(take.saturating_sub(1)) == Some(&b'\n');
        record.extend_from_slice(&available[..take]);
        reader.consume(take);
        if terminated {
            break;
        }
    }
    while matches!(record.last(), Some(b'\n') | Some(b'\r')) {
        record.pop();
    }

    let value = serde_json::from_slice::<Value>(&record).ok()?;
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

fn exact_current_unsupported_reason(
    provider: CaptureProvider,
    path: &Path,
) -> Option<&'static str> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if metadata.file_type().is_symlink() {
        return None;
    }
    if metadata.file_type().is_dir() && has_supported_explicit_history(provider, path) {
        return None;
    }

    match provider {
        CaptureProvider::Codex if is_named_regular_file(path, |name| name.ends_with(".jsonl.zst")) => {
            Some("Codex compressed .jsonl.zst history is detected but unsupported")
        }
        CaptureProvider::KiroCli if is_current_kiro_shape(path, &metadata) => {
            Some("Kiro ACP/v3 session history is detected but unsupported")
        }
        CaptureProvider::Qoder if is_qoder_direct_sdk_shape(path, &metadata) => {
            Some("Qoder direct SDK JSONL history without a transcript directory is detected but unsupported")
        }
        CaptureProvider::OpenClaw if contains_openclaw_sqlite(path, &metadata) => {
            Some("OpenClaw openclaw-agent.sqlite history is detected but unsupported")
        }
        CaptureProvider::OpenHands if is_openhands_cli_events_shape(path, &metadata) => {
            Some("OpenHands CLI events/event-*.json history is detected but unsupported")
        }
        CaptureProvider::Mux if contains_mux_archive(path, &metadata) => {
            Some("Mux chat-archive.jsonl history is detected but unsupported")
        }
        CaptureProvider::Cline if is_current_cline_sdk_shape(path, &metadata) => {
            Some("current Cline SDK session history is detected but unsupported")
        }
        _ => None,
    }
}

fn has_supported_explicit_history(provider: CaptureProvider, path: &Path) -> bool {
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
        default_location_import_probe(provider, &location, path),
        BoundedProbe::Found
    )
}

fn is_current_kiro_shape(path: &Path, metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_dir() {
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
    if !metadata.file_type().is_file() {
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

fn is_qoder_direct_sdk_shape(path: &Path, metadata: &fs::Metadata) -> bool {
    if path_has_component(path, "transcript") {
        return false;
    }
    if metadata.file_type().is_file() {
        return path.extension().and_then(|extension| extension.to_str()) == Some("jsonl")
            && path
                .parent()
                .and_then(Path::parent)
                .is_some_and(|projects| {
                    projects.file_name().and_then(|name| name.to_str()) == Some("projects")
                });
    }
    if !metadata.file_type().is_dir() {
        return false;
    }
    if path
        .parent()
        .is_some_and(|parent| parent.file_name().and_then(|name| name.to_str()) == Some("projects"))
    {
        return direct_entries(path).is_some_and(|entries| {
            entries
                .iter()
                .any(|entry| is_named_regular_file(entry, |name| name.ends_with(".jsonl")))
        });
    }
    path.file_name().and_then(|name| name.to_str()) == Some("projects")
        && direct_entries(path).is_some_and(|buckets| {
            buckets.iter().any(|bucket| {
                direct_entries(bucket).is_some_and(|entries| {
                    entries
                        .iter()
                        .any(|entry| is_named_regular_file(entry, |name| name.ends_with(".jsonl")))
                })
            })
        })
}

fn contains_openclaw_sqlite(path: &Path, metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_file() {
        return is_named_regular_file(path, |name| name == "openclaw-agent.sqlite");
    }
    if !metadata.file_type().is_dir() {
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

fn is_openhands_cli_events_shape(path: &Path, metadata: &fs::Metadata) -> bool {
    if path_has_component(path, "v1_conversations") {
        return false;
    }
    if metadata.file_type().is_file() {
        return is_openhands_cli_event_file(path);
    }
    if !metadata.file_type().is_dir() {
        return false;
    }
    let events = if path.file_name().and_then(|name| name.to_str()) == Some("events") {
        path.to_path_buf()
    } else {
        path.join("events")
    };
    direct_entries(&events).is_some_and(|entries| {
        entries
            .iter()
            .any(|entry| is_openhands_cli_event_file(entry))
    })
}

fn is_openhands_cli_event_file(path: &Path) -> bool {
    path.parent()
        .is_some_and(|parent| parent.file_name().and_then(|name| name.to_str()) == Some("events"))
        && is_named_regular_file(path, |name| {
            name.starts_with("event-") && name.ends_with(".json")
        })
}

fn contains_mux_archive(path: &Path, metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_file() {
        return is_named_regular_file(path, |name| name == "chat-archive.jsonl");
    }
    metadata.file_type().is_dir()
        && (is_named_regular_file(&path.join("chat-archive.jsonl"), |name| {
            name == "chat-archive.jsonl"
        }) || direct_entries(path).is_some_and(|entries| {
            entries.iter().any(|entry| {
                is_named_regular_file(&entry.join("chat-archive.jsonl"), |name| {
                    name == "chat-archive.jsonl"
                })
            })
        }))
}

fn is_current_cline_sdk_shape(path: &Path, metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_file() {
        return is_current_cline_sdk_file(path);
    }
    metadata.file_type().is_dir()
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
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    metadata.file_type().is_file()
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(matches)
}

fn path_has_component(path: &Path, expected: &str) -> bool {
    path.components()
        .any(|component| component.as_os_str().to_str() == Some(expected))
}
