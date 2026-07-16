use std::{
    fs::{self, File},
    io::{BufReader, ErrorKind},
    path::Path,
};

use ctx_history_core::CaptureProvider;
use serde_json::Value;

use crate::common::io::{read_provider_jsonl_line_or_skip_oversized, ProviderJsonlLineRead};
use crate::provider::provider_safe_path_segment;
use crate::provider::sqlite::probe_sqlite_readonly_source;

use super::types::ProviderDefaultLocation;

pub(super) fn default_location_import_probe(
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
        CaptureProvider::MiMoCode => path_is_file_probe(path),
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
        CaptureProvider::Auggie => has_json_file_under_matching(path, 10_000, |candidate| {
            candidate.extension().and_then(|ext| ext.to_str()) == Some("json")
        }),
        CaptureProvider::Junie => has_junie_session_events(path, 10_000),
        CaptureProvider::Firebender => has_firebender_chat_sessions_table(path),
        CaptureProvider::ForgeCode => has_forgecode_conversations_table(path),
        CaptureProvider::DeepAgents => has_deepagents_checkpoint_tables(path),
        CaptureProvider::MistralVibe => has_jsonl_file_under_matching(path, 10_000, |candidate| {
            candidate.file_name().and_then(|name| name.to_str()) == Some("messages.jsonl")
                && candidate
                    .parent()
                    .is_some_and(|parent| parent.join("meta.json").is_file())
        }),
        CaptureProvider::Mux => has_mux_session_files(path, 10_000),
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
        CaptureProvider::Lingma => has_lingma_chat_record_table(path),
        CaptureProvider::Trae => has_trae_state_vscdb_chat_history(path, 10_000),
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

fn has_gemini_chat_jsonl(root: &Path, max_entries: usize) -> BoundedProbe {
    let tmp = root.join("tmp");
    match path_is_dir_probe(&tmp) {
        BoundedProbe::Found => {}
        BoundedProbe::IoError => return BoundedProbe::IoError,
        _ => return BoundedProbe::NotFound,
    }
    has_jsonl_file_under_matching(&tmp, max_entries, |path| path_has_component(path, "chats"))
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
    match probe_sqlite_readonly_source(&db_path, |conn| {
        conn.query_row(
            "select count(*) from sqlite_schema where type = 'table' and name = 'chat_sessions'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|count| count > 0)
    }) {
        Ok(true) => BoundedProbe::Found,
        Ok(false) => BoundedProbe::NotFound,
        Err(_) => BoundedProbe::IoError,
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

    let file = match File::open(&index_path) {
        Ok(file) => file,
        Err(_) => return BoundedProbe::IoError,
    };
    let mut reader = BufReader::new(crate::disk_io_pacing::PacedReader::new(file));
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
    let entries = match fs::read_dir(root) {
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
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };
        if !file_type.is_dir() {
            continue;
        }
        let Some(session_id) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !provider_safe_path_segment(&session_id) {
            continue;
        }
        match path_is_file_probe(&entry.path().join("events.jsonl")) {
            BoundedProbe::Found => return BoundedProbe::Found,
            BoundedProbe::IoError => return BoundedProbe::IoError,
            BoundedProbe::NotFound | BoundedProbe::BudgetExhausted => {}
        }
    }
    BoundedProbe::NotFound
}

fn has_forgecode_conversations_table(path: &Path) -> BoundedProbe {
    match path_is_file_probe(path) {
        BoundedProbe::Found => {}
        other => return other,
    }
    match probe_sqlite_readonly_source(path, |conn| {
        conn.query_row(
            "select count(*) from sqlite_schema where type = 'table' and name = 'conversations'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|count| count > 0)
    }) {
        Ok(true) => BoundedProbe::Found,
        Ok(false) => BoundedProbe::NotFound,
        Err(_) => BoundedProbe::IoError,
    }
}

fn has_lingma_chat_record_table(path: &Path) -> BoundedProbe {
    match path_is_file_probe(path) {
        BoundedProbe::Found => {}
        other => return other,
    }
    match probe_sqlite_readonly_source(path, |conn| {
        conn.query_row(
            "select count(*) from pragma_table_info('chat_record') \
             where name in ('session_id', 'request_id', 'chat_prompt', 'summary', \
                            'error_result', 'gmt_create', 'extra')",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|count| count >= 7)
    }) {
        Ok(true) => BoundedProbe::Found,
        Ok(false) => BoundedProbe::NotFound,
        Err(_) => BoundedProbe::IoError,
    }
}

pub(super) fn has_trae_state_vscdb_chat_history(root: &Path, max_entries: usize) -> BoundedProbe {
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
    match probe_sqlite_readonly_source(path, |conn| {
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
    match probe_sqlite_readonly_source(path, |conn| {
        conn.query_row(
            "select count(*) from sqlite_schema \
             where type = 'table' and name in ('checkpoints', 'writes')",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|count| count == 2)
    }) {
        Ok(true) => BoundedProbe::Found,
        Ok(false) => BoundedProbe::NotFound,
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
                    .is_some_and(&matches_name),
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
                    .is_some_and(&matches_name)
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
mod sqlite_probe_tests {
    use std::{
        fs::{self, OpenOptions},
        io::Write,
        path::{Path, PathBuf},
    };

    use rusqlite::Connection;

    use super::{has_firebender_chat_sessions_table, BoundedProbe};
    use crate::provider::{
        sqlite::{
            install_sqlite_snapshot_test_hook, take_sqlite_snapshot_test_metrics,
            SqliteSnapshotTestMetrics,
        },
        sqlite_observation::SQLITE_GENERATION_MAX_ATTEMPTS,
    };

    #[test]
    fn shm_only_probe_does_not_copy_the_database() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("chat_history.db");
        Connection::open(&db).unwrap().close().unwrap();
        let shm = sidecar(&db, "-shm");
        fs::write(&shm, b"volatile coordination state").unwrap();
        let before = fs::read(&shm).unwrap();

        take_sqlite_snapshot_test_metrics();
        assert_eq!(
            has_firebender_chat_sessions_table(&db),
            BoundedProbe::NotFound
        );
        assert_eq!(
            take_sqlite_snapshot_test_metrics(),
            SqliteSnapshotTestMetrics::default()
        );
        assert_eq!(fs::read(shm).unwrap(), before);
    }

    #[test]
    fn wal_only_schema_predicate_uses_a_read_only_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let (db, _writer) = wal_only_firebender_db(temp.path());
        let wal = sidecar(&db, "-wal");
        let shm = sidecar(&db, "-shm");
        let source_before = [
            (db.clone(), fs::read(&db).unwrap()),
            (wal.clone(), fs::read(&wal).unwrap()),
            (shm.clone(), fs::read(&shm).unwrap()),
        ];

        take_sqlite_snapshot_test_metrics();
        assert_eq!(has_firebender_chat_sessions_table(&db), BoundedProbe::Found);
        assert_eq!(
            take_sqlite_snapshot_test_metrics(),
            SqliteSnapshotTestMetrics {
                attempts: 1,
                copied_files: 2,
            }
        );
        for (path, bytes) in source_before {
            assert_eq!(fs::read(path).unwrap(), bytes);
        }
    }

    #[test]
    fn sqlite_probe_snapshot_retries_are_bounded_under_churn() {
        let temp = tempfile::tempdir().unwrap();
        let (db, _writer) = wal_only_firebender_db(temp.path());
        let wal = sidecar(&db, "-wal");
        let _hook = install_sqlite_snapshot_test_hook(move |_, _| {
            OpenOptions::new()
                .append(true)
                .open(&wal)
                .unwrap()
                .write_all(&[0])
                .unwrap();
        });

        take_sqlite_snapshot_test_metrics();
        assert_eq!(
            has_firebender_chat_sessions_table(&db),
            BoundedProbe::IoError
        );
        assert_eq!(
            take_sqlite_snapshot_test_metrics(),
            SqliteSnapshotTestMetrics {
                attempts: SQLITE_GENERATION_MAX_ATTEMPTS,
                copied_files: SQLITE_GENERATION_MAX_ATTEMPTS * 2,
            }
        );
    }

    fn wal_only_firebender_db(root: &Path) -> (PathBuf, Connection) {
        let db = root.join("chat_history.db");
        Connection::open(&db).unwrap().close().unwrap();
        let writer = Connection::open(&db).unwrap();
        assert_eq!(
            writer
                .query_row("PRAGMA journal_mode=WAL", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "wal"
        );
        writer
            .execute_batch("PRAGMA wal_autocheckpoint=0; CREATE TABLE chat_sessions (id TEXT);")
            .unwrap();
        assert!(sidecar(&db, "-wal").is_file());
        (db, writer)
    }

    fn sidecar(path: &Path, suffix: &str) -> PathBuf {
        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push(suffix);
        PathBuf::from(sidecar)
    }
}
