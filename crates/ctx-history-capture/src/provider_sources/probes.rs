use std::{
    fs::{self, File},
    io::{BufReader, ErrorKind, Read},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use ctx_history_core::platform_security::create_private_directory_all;
use ctx_history_core::CaptureProvider;
use rusqlite::{limits::Limit as SqliteLimit, Connection, OpenFlags};
use serde_json::Value;
use tempfile::TempDir;
use url::Url;

use crate::common::io::{
    ensure_regular_provider_transcript_file, provider_metadata_is_link_like,
    read_provider_jsonl_line_or_skip_oversized, ProviderJsonlLineRead,
};
use crate::provider::{
    provider_safe_path_segment,
    providers::cursor::{discover_cursor_transcripts, CursorDiscoveryIssueKind},
    sqlite::sqlite_component_change_token,
};
use crate::MAX_PROVIDER_SQLITE_VALUE_BYTES;

use super::{
    observe_ordinary_file, open_ordinary_file_without_following, selectors::sort_paths,
    types::ProviderDefaultLocation, OrdinaryFileObservation,
};

const SQLITE_PROBE_MAX_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;
const SQLITE_PROBE_DEADLINE: Duration = Duration::from_millis(500);
const SQLITE_PROBE_PROGRESS_OPS: i32 = 1_000;
const SQLITE_PROBE_MAX_PROGRESS_CALLS: usize = 1_000;

#[cfg(test)]
std::thread_local! {
    static DEFAULT_LOCATION_PROBE_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(super) fn default_location_import_probe(
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
        CaptureProvider::Cursor => has_cursor_agent_transcript(path),
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
        CaptureProvider::Trae => has_trae_state_vscdb_chat_history(data_root, path, 10_000),
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

fn has_cursor_agent_transcript(path: &Path) -> BoundedProbe {
    let input = if path.is_dir() && path.join("projects").is_dir() {
        path.join("projects")
    } else {
        path.to_path_buf()
    };
    let inventory = discover_cursor_transcripts(&input);
    if !inventory.completed {
        if inventory
            .issues
            .iter()
            .any(|issue| issue.kind == CursorDiscoveryIssueKind::LimitExceeded)
        {
            return BoundedProbe::BudgetExhausted;
        }
        if inventory.issues.iter().any(|issue| {
            matches!(
                issue.kind,
                CursorDiscoveryIssueKind::Io | CursorDiscoveryIssueKind::Symlink
            )
        }) {
            return BoundedProbe::IoError;
        }
        return BoundedProbe::NotFound;
    }
    if inventory.transcripts.is_empty() {
        BoundedProbe::NotFound
    } else {
        BoundedProbe::Found
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

pub(super) fn has_trae_state_vscdb_chat_history(
    data_root: Option<&Path>,
    root: &Path,
    max_entries: usize,
) -> BoundedProbe {
    match fs::symlink_metadata(root) {
        Ok(metadata) if provider_metadata_is_link_like(&metadata) => {
            return BoundedProbe::NotFound;
        }
        Ok(metadata) if metadata.is_file() => {
            if root.file_name().and_then(|name| name.to_str()) != Some("state.vscdb") {
                return BoundedProbe::NotFound;
            }
            return has_trae_state_vscdb_chat_keys(data_root, root);
        }
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return BoundedProbe::NotFound,
        Err(err) if err.kind() == ErrorKind::NotFound => return BoundedProbe::NotFound,
        Err(_) => return BoundedProbe::IoError,
    }

    let direct = root.join("state.vscdb");
    if path_is_file_probe(&direct) == BoundedProbe::Found {
        return has_trae_state_vscdb_chat_keys(data_root, &direct);
    }

    let entries = match sorted_probe_entries(root, max_entries) {
        Ok(entries) => entries,
        Err(outcome) => return outcome,
    };
    let mut visited = 0usize;
    let mut saw_io_error = false;
    for path in entries {
        visited = visited.saturating_add(1);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => {
                saw_io_error = true;
                continue;
            }
        };
        if provider_metadata_is_link_like(&metadata) || !metadata.file_type().is_dir() {
            continue;
        }
        let candidate = path.join("state.vscdb");
        if path_is_file_probe(&candidate) != BoundedProbe::Found {
            continue;
        }
        match has_trae_state_vscdb_chat_keys(data_root, &candidate) {
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

fn has_trae_state_vscdb_chat_keys(data_root: Option<&Path>, path: &Path) -> BoundedProbe {
    match path_is_file_probe(path) {
        BoundedProbe::Found => {}
        other => return other,
    }
    sqlite_structural_probe(data_root, path, SqliteProbeLimits::default(), |conn| {
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

#[derive(Clone, Copy)]
struct SqliteProbeLimits {
    max_total_bytes: u64,
    deadline: Duration,
    max_progress_calls: usize,
}

impl Default for SqliteProbeLimits {
    fn default() -> Self {
        Self {
            max_total_bytes: SQLITE_PROBE_MAX_TOTAL_BYTES,
            deadline: SQLITE_PROBE_DEADLINE,
            max_progress_calls: SQLITE_PROBE_MAX_PROGRESS_CALLS,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct SqliteProbeComponent {
    path: PathBuf,
    len: u64,
    observation: OrdinaryFileObservation,
    change_token: [u8; 32],
}

enum SqliteProbeDatabase {
    Immutable(Connection),
    Snapshot {
        connection: Connection,
        _directory: TempDir,
    },
}

impl SqliteProbeDatabase {
    fn connection(&self) -> &Connection {
        match self {
            Self::Immutable(connection) | Self::Snapshot { connection, .. } => connection,
        }
    }
}

fn sqlite_structural_probe(
    data_root: Option<&Path>,
    path: &Path,
    limits: SqliteProbeLimits,
    query: impl FnOnce(&Connection) -> rusqlite::Result<bool>,
) -> BoundedProbe {
    let initial_components = match sqlite_probe_components(path, limits.max_total_bytes) {
        Ok(components) => components,
        Err(outcome) => return outcome,
    };
    let database = match open_sqlite_probe_database(data_root, path, &initial_components) {
        Ok(database) => database,
        Err(_) => return BoundedProbe::IoError,
    };
    let connection = database.connection();
    let exhausted = Arc::new(AtomicBool::new(false));
    let deadline = Instant::now() + limits.deadline;
    let progress_exhausted = Arc::clone(&exhausted);
    let mut progress_calls = 0usize;
    connection.progress_handler(
        SQLITE_PROBE_PROGRESS_OPS,
        Some(move || {
            progress_calls = progress_calls.saturating_add(1);
            let stop = progress_calls > limits.max_progress_calls || Instant::now() >= deadline;
            if stop {
                progress_exhausted.store(true, Ordering::Relaxed);
            }
            stop
        }),
    );
    let query_result =
        configure_sqlite_probe(connection, limits.deadline).and_then(|()| query(connection));
    connection.progress_handler(0, None::<fn() -> bool>);
    drop(database);
    let stable = sqlite_probe_components(path, limits.max_total_bytes)
        .is_ok_and(|closing| closing == initial_components);
    if !stable {
        return BoundedProbe::IoError;
    }
    if exhausted.load(Ordering::Relaxed) {
        return BoundedProbe::BudgetExhausted;
    }
    match query_result {
        Ok(true) => BoundedProbe::Found,
        Ok(false) => BoundedProbe::NotFound,
        Err(_) => BoundedProbe::IoError,
    }
}

fn configure_sqlite_probe(connection: &Connection, deadline: Duration) -> rusqlite::Result<()> {
    let value_limit = i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).unwrap_or(i32::MAX);
    connection.set_limit(SqliteLimit::SQLITE_LIMIT_LENGTH, value_limit);
    connection.set_limit(SqliteLimit::SQLITE_LIMIT_SQL_LENGTH, 64 * 1024);
    connection.set_limit(SqliteLimit::SQLITE_LIMIT_COLUMN, 256);
    connection.set_limit(SqliteLimit::SQLITE_LIMIT_EXPR_DEPTH, 100);
    connection.set_limit(SqliteLimit::SQLITE_LIMIT_COMPOUND_SELECT, 16);
    connection.set_limit(SqliteLimit::SQLITE_LIMIT_VDBE_OP, 100_000);
    connection.set_limit(SqliteLimit::SQLITE_LIMIT_ATTACHED, 0);
    connection.set_limit(SqliteLimit::SQLITE_LIMIT_WORKER_THREADS, 0);
    connection.busy_timeout(deadline)?;
    connection.pragma_update(None, "query_only", true)?;
    connection.pragma_update(None, "trusted_schema", false)
}

fn sqlite_probe_components(
    path: &Path,
    max_total_bytes: u64,
) -> std::result::Result<Vec<SqliteProbeComponent>, BoundedProbe> {
    let mut paths = vec![path.to_path_buf()];
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut sidecar = path.as_os_str().to_os_string();
        sidecar.push(suffix);
        paths.push(PathBuf::from(sidecar));
    }
    let mut components = Vec::new();
    let mut total = 0u64;
    for (index, component) in paths.into_iter().enumerate() {
        let metadata = match fs::symlink_metadata(&component) {
            Ok(metadata) => metadata,
            Err(error) if index > 0 && error.kind() == ErrorKind::NotFound => continue,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Err(BoundedProbe::NotFound)
            }
            Err(_) => return Err(BoundedProbe::IoError),
        };
        if provider_metadata_is_link_like(&metadata)
            || !metadata.file_type().is_file()
            || ensure_regular_provider_transcript_file(&component).is_err()
        {
            return Err(BoundedProbe::IoError);
        }
        total = total
            .checked_add(metadata.len())
            .ok_or(BoundedProbe::BudgetExhausted)?;
        if total > max_total_bytes {
            return Err(BoundedProbe::BudgetExhausted);
        }
        let observation = observe_ordinary_file(&component).map_err(|_| BoundedProbe::IoError)?;
        if observation.len() != metadata.len() {
            return Err(BoundedProbe::IoError);
        }
        let change_token = sqlite_component_change_token(&component, &observation)
            .map_err(|_| BoundedProbe::IoError)?;
        components.push(SqliteProbeComponent {
            path: component,
            len: metadata.len(),
            observation,
            change_token,
        });
    }
    Ok(components)
}

fn open_sqlite_probe_database(
    data_root: Option<&Path>,
    path: &Path,
    components: &[SqliteProbeComponent],
) -> crate::Result<SqliteProbeDatabase> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    if components.len() == 1 {
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        };
        let mut uri = Url::from_file_path(&absolute).map_err(|()| {
            crate::CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "provider SQLite probe path cannot be represented as a file URI",
            }
        })?;
        uri.query_pairs_mut()
            .append_pair("mode", "ro")
            .append_pair("immutable", "1");
        let connection = Connection::open_with_flags(
            uri.as_str(),
            flags | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        return Ok(SqliteProbeDatabase::Immutable(connection));
    }

    let Some(data_root) = data_root else {
        return Err(crate::CaptureError::SourceChangedDuringCapture);
    };
    let staging_root = data_root.join("tmp").join("provider-sqlite");
    create_private_directory_all(&staging_root)?;
    let directory = tempfile::Builder::new()
        .prefix("ctx-provider-probe-sqlite-")
        .tempdir_in(staging_root)?;
    for component in components {
        copy_sqlite_probe_component(component, directory.path())?;
    }
    let snapshot_path = directory.path().join(path.file_name().ok_or_else(|| {
        crate::CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "provider SQLite probe path has no file name",
        }
    })?);
    // The snapshot directory and every component are ctx-created with create_new.
    // NOFOLLOW remains required for the live immutable path above, but omitting it
    // here lets SQLite's macOS VFS open the copied WAL sidecars read-only.
    let connection = Connection::open_with_flags(&snapshot_path, flags)?;
    Ok(SqliteProbeDatabase::Snapshot {
        connection,
        _directory: directory,
    })
}

fn copy_sqlite_probe_component(
    component: &SqliteProbeComponent,
    destination: &Path,
) -> crate::Result<()> {
    let mut source = open_ordinary_file_without_following(&component.path)?;
    let metadata = source.metadata()?;
    if provider_metadata_is_link_like(&metadata)
        || !metadata.file_type().is_file()
        || metadata.len() != component.len
    {
        return Err(crate::CaptureError::SourceChangedDuringCapture);
    }
    let file_name = component.path.file_name().ok_or_else(|| {
        crate::CaptureError::InvalidProviderTranscriptPath {
            path: component.path.clone(),
            reason: "provider SQLite probe component has no file name",
        }
    })?;
    let mut output = File::options()
        .write(true)
        .create_new(true)
        .open(destination.join(file_name))?;
    let copied = std::io::copy(
        &mut source.by_ref().take(component.len.saturating_add(1)),
        &mut output,
    )?;
    if copied != component.len {
        return Err(crate::CaptureError::SourceChangedDuringCapture);
    }
    Ok(())
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
    if crate::common::io::ensure_provider_path_parents_are_not_symlinks(path).is_err() {
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
