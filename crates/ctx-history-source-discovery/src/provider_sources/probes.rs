use std::{
    fs,
    io::{BufReader, ErrorKind},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use ctx_history_core::CaptureProvider;
use ctx_history_source_sqlite::{SqliteReadFinalizationError, MAX_PROVIDER_SQLITE_VALUE_BYTES};
use rusqlite::{limits::Limit as SqliteLimit, Connection};
use serde_json::Value;

use ctx_history_source_io::{
    open_provider_source_path, provider_metadata_is_link_like, provider_safe_path_segment,
    read_provider_jsonl_line_or_skip_oversized, OpenedProviderSourcePath, ProviderJsonlLineRead,
    ProviderSourceDirectory, ProviderSourceRoot, SourceIoError,
};

#[cfg(test)]
use super::SqliteSourceDirectoryAuthority;
use super::{
    open_ordinary_file_without_following, open_root_handle_sqlite_source_snapshot_with_limits,
    retain_sqlite_source_directory_authority,
    selectors::{sort_paths, MAX_DIRECT_DIRECTORY_ENTRIES},
    types::ProviderDefaultLocation,
    CursorTranscriptProbeOutcome, SqliteSourceAccessError, SqliteSourceReadSnapshot,
    SqliteSourceSnapshotLimits, StaticProviderProbeCatalog,
};

const SQLITE_PROBE_MAX_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;
const SQLITE_PROBE_DEADLINE: Duration = Duration::from_millis(500);
const SQLITE_PROBE_PROGRESS_OPS: i32 = 1_000;
const SQLITE_PROBE_MAX_PROGRESS_CALLS: usize = 1_000;

#[cfg(test)]
std::thread_local! {
    static DEFAULT_LOCATION_PROBE_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static FAIL_NEXT_SQLITE_PROBE_CONNECTION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub(super) fn default_location_import_probe(
    probes: &StaticProviderProbeCatalog,
    data_root: Option<&Path>,
    provider: CaptureProvider,
    location: &ProviderDefaultLocation,
    path: &Path,
) -> BoundedProbe {
    #[cfg(test)]
    DEFAULT_LOCATION_PROBE_CALLS.with(|calls| calls.set(calls.get().saturating_add(1)));
    match provider {
        CaptureProvider::Codex if location.source_format == "codex_history_jsonl" => {
            path_is_file_probe(path)
        }
        CaptureProvider::Codex => has_file_under_matching(path, 10_000, |candidate| {
            candidate
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".jsonl") || name.ends_with(".jsonl.zst"))
        }),
        CaptureProvider::GrokBuild => has_jsonl_file_under_matching(path, 10_000, |candidate| {
            candidate.file_name().and_then(|name| name.to_str()) == Some("updates.jsonl")
        }),
        CaptureProvider::DeepSeekHarness => has_deepseek_harness_session_file(path, 10_000),
        CaptureProvider::Pi => has_jsonl_file_under_matching(path, 10_000, |_| true),
        CaptureProvider::OpenCode => path_is_file_probe(path),
        CaptureProvider::Kilo => path_is_file_probe(path),
        CaptureProvider::MiMoCode => path_is_file_probe(path),
        CaptureProvider::KiroCli => path_is_file_probe(path),
        CaptureProvider::Crush => path_is_file_probe(path),
        CaptureProvider::Goose => path_is_file_probe(path),
        CaptureProvider::Claude => has_jsonl_file_under_matching(path, 10_000, |_| true),
        CaptureProvider::OpenClaw
            if location.source_format
                == ctx_history_openclaw_schema::OPENCLAW_AGENT_SQLITE_SOURCE_FORMAT =>
        {
            has_openclaw_agent_sqlite_v17(data_root, path)
        }
        CaptureProvider::OpenClaw => has_openclaw_session_jsonl(path, 10_000),
        CaptureProvider::Hermes => path_is_file_probe(path),
        CaptureProvider::NanoClaw => has_nanoclaw_project(path),
        CaptureProvider::AstrBot => path_is_file_probe(path),
        CaptureProvider::Shelley => path_is_file_probe(path),
        CaptureProvider::Continue => has_json_file_under_matching(path, 10_000, |candidate| {
            candidate.file_name().and_then(|name| name.to_str()) != Some("sessions.json")
        }),
        CaptureProvider::OpenHands => has_openhands_event_json(path, 10_000),
        CaptureProvider::Antigravity => has_jsonl_file_under_matching(path, 10_000, |candidate| {
            matches!(
                candidate.file_name().and_then(|name| name.to_str()),
                Some("transcript_full.jsonl" | "transcript.jsonl")
            )
        }),
        CaptureProvider::Gemini | CaptureProvider::Tabnine => has_gemini_chat_jsonl(path, 10_000),
        CaptureProvider::Cursor => has_cursor_agent_transcript(probes, path),
        CaptureProvider::Qoder => has_file_under_matching(path, 10_000, |candidate| {
            qoder_jsonl_path_is_supported(path, candidate)
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
        CaptureProvider::Auggie => has_auggie_session_json(path),
        CaptureProvider::Junie => has_junie_session_events(path, 10_000),
        CaptureProvider::Firebender => has_firebender_chat_sessions_table(data_root, path),
        CaptureProvider::ForgeCode => has_forgecode_conversations_table(data_root, path),
        CaptureProvider::DeepAgents => has_deepagents_checkpoint_tables(data_root, path),
        CaptureProvider::MistralVibe => has_jsonl_file_under_matching(path, 10_000, |candidate| {
            candidate.file_name().and_then(|name| name.to_str()) == Some("messages.jsonl")
                && candidate.parent().is_some_and(|parent| {
                    path_is_file_probe(&parent.join("meta.json")) == BoundedProbe::Found
                })
        }),
        CaptureProvider::Mux => has_mux_session_files(path, 10_000),
        CaptureProvider::RovoDev => has_json_file_under_matching(path, 10_000, |candidate| {
            candidate.file_name().and_then(|name| name.to_str()) == Some("session_context.json")
        }),
        CaptureProvider::Cline if location.source_format == "cline_sdk_session_store" => {
            has_cline_sdk_catalog(path)
        }
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
        CaptureProvider::Lingma => has_lingma_chat_record_table(data_root, path),
        CaptureProvider::Warp => path_is_file_probe(path),
        CaptureProvider::CodeBuddy => has_codebuddy_history_json(path, 10_000),
        CaptureProvider::Shell
        | CaptureProvider::Git
        | CaptureProvider::Jj
        | CaptureProvider::Gh
        | CaptureProvider::Custom
        | CaptureProvider::Unknown => BoundedProbe::NotFound,
    }
}

fn has_auggie_session_json(root: &Path) -> BoundedProbe {
    let opened = match open_provider_source_path(root) {
        Ok(opened) => opened,
        Err(_) => return BoundedProbe::IoError,
    };
    let OpenedProviderSourcePath::Directory(directory) = opened else {
        return BoundedProbe::NotFound;
    };

    match directory.open_child(std::ffi::OsStr::new("sessions")) {
        Ok(OpenedProviderSourcePath::Directory(sessions)) => {
            has_direct_auggie_session_json(&sessions)
        }
        Ok(OpenedProviderSourcePath::File(_)) => BoundedProbe::IoError,
        Err(SourceIoError::Io(error)) if error.kind() == ErrorKind::NotFound => {
            let outcome = has_direct_auggie_session_json(&directory);
            if matches!(outcome, BoundedProbe::Found | BoundedProbe::NotFound) {
                match directory.open_child(std::ffi::OsStr::new("sessions")) {
                    Err(SourceIoError::Io(error)) if error.kind() == ErrorKind::NotFound => outcome,
                    _ => BoundedProbe::IoError,
                }
            } else {
                outcome
            }
        }
        Err(_) => BoundedProbe::IoError,
    }
}

fn has_direct_auggie_session_json(directory: &ProviderSourceDirectory) -> BoundedProbe {
    let names = match bounded_auggie_directory_entries(directory) {
        Ok(names) => names,
        Err(outcome) => return outcome,
    };
    let authority = directory.authority_root();
    let mut opened_entries = Vec::with_capacity(names.len());
    let mut found = false;
    for name in &names {
        let child = match directory.open_child(name) {
            Ok(child) => child,
            Err(_) => return BoundedProbe::IoError,
        };
        let is_file = matches!(&child, OpenedProviderSourcePath::File(_));
        found |=
            is_file && Path::new(name).extension().and_then(|ext| ext.to_str()) == Some("json");
        opened_entries.push((name.clone(), child.authority_fingerprint(), is_file));
    }
    if directory.revalidate().is_err() || authority.revalidate().is_err() {
        return BoundedProbe::IoError;
    }

    let closing_names = match bounded_auggie_directory_entries(directory) {
        Ok(names) => names,
        Err(outcome) => return outcome,
    };
    if closing_names != names {
        return BoundedProbe::IoError;
    }
    for (name, fingerprint, is_file) in opened_entries {
        let child = match directory.open_child(&name) {
            Ok(child) => child,
            Err(_) => return BoundedProbe::IoError,
        };
        if child.authority_fingerprint() != fingerprint
            || matches!(&child, OpenedProviderSourcePath::File(_)) != is_file
        {
            return BoundedProbe::IoError;
        }
    }
    if directory.revalidate().is_err() || authority.revalidate().is_err() {
        return BoundedProbe::IoError;
    }
    BoundedProbe::from_bool(found)
}

fn bounded_auggie_directory_entries(
    directory: &ProviderSourceDirectory,
) -> std::result::Result<Vec<std::ffi::OsString>, BoundedProbe> {
    let names = match directory.entries(MAX_DIRECT_DIRECTORY_ENTRIES.saturating_add(1)) {
        Ok(names) => names,
        Err(SourceIoError::InvalidProviderTranscriptPath { .. }) => {
            return Err(BoundedProbe::BudgetExhausted);
        }
        Err(_) => return Err(BoundedProbe::IoError),
    };
    if names.len() > MAX_DIRECT_DIRECTORY_ENTRIES {
        return Err(BoundedProbe::BudgetExhausted);
    }
    Ok(names)
}

fn has_cline_sdk_catalog(root: &Path) -> BoundedProbe {
    let index = path_is_file_probe(&root.join("sessions/sessions.index.json"));
    let database = path_is_file_probe(&root.join("db/sessions.db"));
    if matches!(index, BoundedProbe::Found) || matches!(database, BoundedProbe::Found) {
        BoundedProbe::Found
    } else if matches!(index, BoundedProbe::IoError) || matches!(database, BoundedProbe::IoError) {
        BoundedProbe::IoError
    } else {
        BoundedProbe::NotFound
    }
}

pub(super) fn has_deepseek_harness_session_file(root: &Path, max_entries: usize) -> BoundedProbe {
    has_file_under_matching(root, max_entries, |candidate| {
        let supported_leaf = matches!(
            candidate.file_name().and_then(|name| name.to_str()),
            Some("session.jsonl.zstd" | "session.jsonl")
        );
        supported_leaf
            && (candidate == root
                || candidate
                    .strip_prefix(root)
                    .is_ok_and(|relative| relative.components().count() == 3))
    })
}

fn has_cursor_agent_transcript(probes: &StaticProviderProbeCatalog, path: &Path) -> BoundedProbe {
    match (probes.cursor.probe)(path) {
        CursorTranscriptProbeOutcome::Found => BoundedProbe::Found,
        CursorTranscriptProbeOutcome::NotFound => BoundedProbe::NotFound,
        CursorTranscriptProbeOutcome::BudgetExhausted => BoundedProbe::BudgetExhausted,
        CursorTranscriptProbeOutcome::IoError => BoundedProbe::IoError,
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

fn has_firebender_chat_sessions_table(data_root: Option<&Path>, path: &Path) -> BoundedProbe {
    let db_path = match fs::symlink_metadata(path) {
        Ok(metadata) if provider_metadata_is_link_like(&metadata) => {
            return BoundedProbe::NotFound;
        }
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
    sqlite_structural_probe(data_root, &db_path, SqliteProbeLimits::default(), |conn| {
        firebender_supported_chat_sessions_shape(conn)
    })
}

fn firebender_supported_chat_sessions_shape(conn: &Connection) -> rusqlite::Result<bool> {
    let has_schema_info = conn.query_row(
        "select exists(select 1 from sqlite_schema where type = 'table' and name = 'schema_info')",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    let has_subagents = conn.query_row(
        "select exists(select 1 from sqlite_schema where type = 'table' and name = 'subagent_conversations')",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    let mut statement = conn.prepare("pragma table_info(chat_sessions)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let chat_sessions_supported = [
        "id",
        "name",
        "created_at",
        "updated_at",
        "deleted_at",
        "messages_json",
        "metadata_json",
    ]
    .iter()
    .all(|required| columns.iter().any(|column| column == required));
    if has_schema_info && has_subagents && chat_sessions_supported {
        Ok(true)
    } else {
        Err(rusqlite::Error::InvalidQuery)
    }
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
    match fs::symlink_metadata(&index_path) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => return BoundedProbe::NotFound,
        Err(err) if err.kind() == ErrorKind::NotFound => return BoundedProbe::NotFound,
        Err(_) => return BoundedProbe::IoError,
    }

    let file = match open_ordinary_file_without_following(&index_path) {
        Ok(file) => file,
        Err(_) => return BoundedProbe::IoError,
    };
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut visited = 0usize;
    loop {
        match read_provider_jsonl_line_or_skip_oversized(&mut reader, &mut line) {
            Ok(ProviderJsonlLineRead::Eof) => break,
            Ok(ProviderJsonlLineRead::Line { .. }) => {}
            Ok(ProviderJsonlLineRead::Oversized { .. }) => {
                visited = visited.saturating_add(1);
                if visited > max_entries {
                    return BoundedProbe::BudgetExhausted;
                }
                continue;
            }
            Err(_) => return BoundedProbe::IoError,
        }
        visited = visited.saturating_add(1);
        if visited > max_entries {
            return BoundedProbe::BudgetExhausted;
        }
        let Ok(line) = std::str::from_utf8(&line) else {
            continue;
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(session_id) = value.get("sessionId").and_then(Value::as_str) else {
            continue;
        };
        if !provider_safe_path_segment(session_id) {
            continue;
        }
        match path_is_file_probe(&root.join(session_id).join("events.jsonl")) {
            BoundedProbe::Found => return BoundedProbe::Found,
            BoundedProbe::IoError => return BoundedProbe::IoError,
            BoundedProbe::NotFound | BoundedProbe::BudgetExhausted => {}
        }
    }
    let entries = match sorted_probe_entries(root, max_entries.saturating_sub(visited)) {
        Ok(entries) => entries,
        Err(outcome) => return outcome,
    };
    for path in entries {
        visited = visited.saturating_add(1);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if provider_metadata_is_link_like(&metadata) || !metadata.file_type().is_dir() {
            continue;
        }
        let Some(session_id) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !provider_safe_path_segment(session_id) {
            continue;
        }
        match path_is_file_probe(&path.join("events.jsonl")) {
            BoundedProbe::Found => return BoundedProbe::Found,
            BoundedProbe::IoError => return BoundedProbe::IoError,
            BoundedProbe::NotFound | BoundedProbe::BudgetExhausted => {}
        }
    }
    BoundedProbe::NotFound
}

fn has_forgecode_conversations_table(data_root: Option<&Path>, path: &Path) -> BoundedProbe {
    match path_is_file_probe(path) {
        BoundedProbe::Found => {}
        other => return other,
    }
    sqlite_structural_probe(data_root, path, SqliteProbeLimits::default(), |conn| {
        conn.query_row(
            "select exists(select 1 from sqlite_schema \
             where type = 'table' and name = 'conversations')",
            [],
            |row| row.get::<_, bool>(0),
        )
    })
}

fn has_lingma_chat_record_table(data_root: Option<&Path>, path: &Path) -> BoundedProbe {
    match path_is_file_probe(path) {
        BoundedProbe::Found => {}
        other => return other,
    }
    sqlite_structural_probe(data_root, path, SqliteProbeLimits::default(), |conn| {
        conn.query_row(
            "select count(*) from pragma_table_info('chat_record') \
             where name in ('session_id', 'request_id', 'chat_prompt', 'summary', \
                            'error_result', 'gmt_create', 'extra')",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|count| count >= 7)
    })
}

fn has_deepagents_checkpoint_tables(data_root: Option<&Path>, path: &Path) -> BoundedProbe {
    match path_is_file_probe(path) {
        BoundedProbe::Found => {}
        other => return other,
    }
    sqlite_structural_probe(data_root, path, SqliteProbeLimits::default(), |conn| {
        conn.query_row(
            "select count(*) = 2 from sqlite_schema \
             where type = 'table' and name in ('checkpoints', 'writes')",
            [],
            |row| row.get::<_, bool>(0),
        )
    })
}

mod sqlite_probe;

#[cfg(test)]
use sqlite_probe::{
    configure_sqlite_probe, execute_sqlite_structural_probe,
    fail_next_sqlite_probe_connection_for_test, SqliteProbePrimaryError,
};
use sqlite_probe::{sqlite_structural_probe, SqliteProbeLimits};

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

pub(super) fn has_openclaw_agent_sqlite_v17(data_root: Option<&Path>, path: &Path) -> BoundedProbe {
    let Some(agent_id) = path
        .parent()
        .filter(|parent| parent.file_name().and_then(|name| name.to_str()) == Some("agent"))
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .filter(|agent_id| !agent_id.is_empty())
    else {
        return BoundedProbe::NotFound;
    };
    sqlite_structural_probe(
        data_root,
        path,
        SqliteProbeLimits::default(),
        |connection| ctx_history_openclaw_schema::matches_openclaw_agent_v17(connection, agent_id),
    )
}

fn has_mux_session_files(root: &Path, max_entries: usize) -> BoundedProbe {
    match has_jsonl_file_under_matching(root, max_entries, |candidate| {
        matches!(
            candidate.file_name().and_then(|name| name.to_str()),
            Some("chat.jsonl" | "chat-archive.jsonl")
        )
    }) {
        BoundedProbe::Found => BoundedProbe::Found,
        BoundedProbe::IoError => BoundedProbe::IoError,
        _ => has_json_file_under_matching(root, max_entries, |candidate| {
            candidate.file_name().and_then(|name| name.to_str()) == Some("partial.json")
        }),
    }
}

fn has_openhands_event_json(root: &Path, max_entries: usize) -> BoundedProbe {
    match has_openhands_v1_event_json(root, max_entries) {
        BoundedProbe::Found => BoundedProbe::Found,
        BoundedProbe::IoError => BoundedProbe::IoError,
        BoundedProbe::BudgetExhausted => BoundedProbe::BudgetExhausted,
        BoundedProbe::NotFound => has_openhands_current_event_json(root, max_entries),
    }
}

pub(super) fn has_openhands_current_event_json(root: &Path, max_entries: usize) -> BoundedProbe {
    has_json_file_under_matching(root, max_entries, is_openhands_current_event_json)
}

pub(super) fn has_openhands_v1_event_json(root: &Path, max_entries: usize) -> BoundedProbe {
    has_json_file_under_matching(root, max_entries, |path| {
        path_has_component(path, "v1_conversations")
    })
}

fn qoder_jsonl_path_is_supported(root: &Path, path: &Path) -> bool {
    if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
        return false;
    }

    // An explicitly selected Qoder file retains its released path admission.
    // Directory sources instead name the projects transcript tree itself, so
    // classify its two native layouts relative to that selected authority.
    if root == path {
        return path_has_component(path, "transcript")
            || path
                .parent()
                .and_then(Path::parent)
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                == Some("projects");
    }

    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    let mut components = relative.components();
    match (
        components.next(),
        components.next(),
        components.next(),
        components.next(),
    ) {
        (
            Some(std::path::Component::Normal(_project)),
            Some(std::path::Component::Normal(_session)),
            None,
            None,
        ) => true,
        (
            Some(std::path::Component::Normal(_project)),
            Some(std::path::Component::Normal(transcript)),
            Some(std::path::Component::Normal(_session)),
            None,
        ) => transcript == "transcript",
        _ => false,
    }
}

pub(super) fn is_openhands_current_event_json(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("event-") && name.ends_with(".json"))
        && path.parent().is_some_and(|parent| {
            parent.file_name().and_then(|name| name.to_str()) == Some("events")
        })
        && path
            .parent()
            .and_then(Path::parent)
            .is_some_and(|conversation| {
                conversation
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| !name.is_empty())
            })
}

fn has_codebuddy_history_json(root: &Path, max_entries: usize) -> BoundedProbe {
    let projects = root.join("projects");
    match path_is_dir_probe(&projects) {
        BoundedProbe::Found => {
            match has_jsonl_file_under_matching(&projects, max_entries, |_| true) {
                BoundedProbe::Found => return BoundedProbe::Found,
                BoundedProbe::IoError => return BoundedProbe::IoError,
                BoundedProbe::BudgetExhausted => return BoundedProbe::BudgetExhausted,
                BoundedProbe::NotFound => {}
            }
        }
        BoundedProbe::IoError => return BoundedProbe::IoError,
        BoundedProbe::NotFound | BoundedProbe::BudgetExhausted => {}
    }
    match has_json_file_under_matching(root, max_entries, |path| {
        path.file_name().and_then(|name| name.to_str()) == Some("index.json")
            && path_has_component(path, "history")
    }) {
        BoundedProbe::Found => BoundedProbe::Found,
        BoundedProbe::IoError => BoundedProbe::IoError,
        BoundedProbe::BudgetExhausted => BoundedProbe::BudgetExhausted,
        BoundedProbe::NotFound => has_jsonl_file_under_matching(root, max_entries, |path| {
            path_has_component(path, "projects")
        }),
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
pub(super) enum BoundedProbe {
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
    if ctx_history_source_io::ensure_provider_path_parents_are_not_symlinks(path).is_err() {
        return PathProbe::IoError;
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if provider_metadata_is_link_like(&metadata) => PathProbe::Other,
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
    has_file_under_matching(root, max_entries, |path| {
        path.extension().and_then(|ext| ext.to_str()) == Some(extension) && matches_path(path)
    })
}

fn has_file_under_matching(
    root: &Path,
    max_entries: usize,
    matches_path: impl Fn(&Path) -> bool,
) -> BoundedProbe {
    match path_metadata_probe(root) {
        PathProbe::File => return BoundedProbe::from_bool(matches_path(root)),
        PathProbe::Dir => {}
        PathProbe::Missing | PathProbe::Other => return BoundedProbe::NotFound,
        PathProbe::IoError => return BoundedProbe::IoError,
    }

    let mut visited = 0usize;
    let mut stack = vec![(root.to_path_buf(), true)];
    while let Some((dir, is_root)) = stack.pop() {
        let entries = match sorted_probe_entries(&dir, max_entries.saturating_sub(visited)) {
            Ok(entries) => entries,
            Err(BoundedProbe::BudgetExhausted) => return BoundedProbe::BudgetExhausted,
            Err(_) if is_root => return BoundedProbe::IoError,
            Err(_) => continue,
        };
        let mut child_directories = Vec::new();
        for path in entries {
            visited = visited.saturating_add(1);
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };
            if provider_metadata_is_link_like(&metadata) {
                continue;
            }
            if metadata.file_type().is_dir() {
                child_directories.push(path);
            } else if metadata.file_type().is_file() && matches_path(&path) {
                return BoundedProbe::Found;
            }
        }
        for child in child_directories.into_iter().rev() {
            stack.push((child, false));
        }
    }
    BoundedProbe::NotFound
}

fn has_task_json_file_under_matching(
    root: &Path,
    max_entries: usize,
    matches_name: impl Fn(&str) -> bool,
) -> BoundedProbe {
    has_file_under_matching(root, max_entries, |path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(&matches_name)
    })
}

fn sorted_probe_entries(
    directory: &Path,
    remaining: usize,
) -> std::result::Result<Vec<PathBuf>, BoundedProbe> {
    let entries = fs::read_dir(directory).map_err(|_| BoundedProbe::IoError)?;
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|_| BoundedProbe::IoError)?;
        if paths.len() >= remaining {
            return Err(BoundedProbe::BudgetExhausted);
        }
        paths.push(entry.path());
    }
    sort_paths(&mut paths);
    Ok(paths)
}

fn path_has_component(path: &Path, expected: &str) -> bool {
    path.components()
        .any(|component| component.as_os_str().to_str() == Some(expected))
}

#[cfg(test)]
pub(super) fn reset_default_location_probe_calls() {
    DEFAULT_LOCATION_PROBE_CALLS.with(|calls| calls.set(0));
}

#[cfg(test)]
pub(super) fn default_location_probe_calls() -> usize {
    DEFAULT_LOCATION_PROBE_CALLS.with(std::cell::Cell::get)
}

#[cfg(test)]
#[path = "probes_tests.rs"]
mod tests;
