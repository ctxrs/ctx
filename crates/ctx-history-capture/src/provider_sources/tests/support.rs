use std::{
    env,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    sync::Mutex,
};

use ctx_history_core::CaptureProvider;
use rusqlite::Connection;

use crate::test_support_paths::capture_repo_root;

use super::super::{discover_provider_sources, ProviderSourceStatus};

pub(super) static ENV_LOCK: Mutex<()> = Mutex::new(());

pub(super) fn tempdir() -> tempfile::TempDir {
    let temp_root = std::fs::canonicalize(std::env::temp_dir())
        .expect("system temporary directory should be canonicalizable");
    tempfile::Builder::new()
        .prefix("ctx-provider-sources-")
        .tempdir_in(temp_root)
        .unwrap()
}

pub(super) struct EnvGuard {
    name: &'static str,
    original: Option<OsString>,
}

impl EnvGuard {
    pub(super) fn set(name: &'static str, value: impl AsRef<OsStr>) -> Self {
        let original = env::var_os(name);
        env::set_var(name, value);
        Self { name, original }
    }

    pub(super) fn remove(name: &'static str) -> Self {
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

pub(super) struct CwdGuard {
    original: PathBuf,
}

impl CwdGuard {
    pub(super) fn set(path: &Path) -> Self {
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

pub(super) fn write_pi_discovery_session(root: &Path) {
    let project = root.join("--workspace--");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("2026-07-03T12-00-00-000Z_pi-discovery.jsonl"),
        "{}\n",
    )
    .unwrap();
}

pub(super) fn write_qwen_discovery_chat(projects: &Path) {
    let chats = projects.join("project/chats");
    std::fs::create_dir_all(&chats).unwrap();
    std::fs::write(chats.join("qwen-discovery.jsonl"), "{}\n").unwrap();
}

pub(super) fn write_kimi_discovery_wire(home: &Path) {
    let agent = home.join("sessions/wd_project_abc123/kimi-session/agents/main");
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::write(agent.join("wire.jsonl"), "{}\n").unwrap();
}
pub(super) fn write_junie_discovery_session(sessions: &Path, session_id: &str) {
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
pub(super) fn write_mistral_vibe_discovery_session(sessions: &Path) {
    let session = sessions.join("session_20260704_120000_vibe1234");
    std::fs::create_dir_all(&session).unwrap();
    std::fs::write(
        session.join("meta.json"),
        r#"{"session_id":"mistral-vibe-discovery","start_time":"2026-07-04T12:00:00Z","end_time":null,"git_commit":null,"git_branch":null,"environment":{"working_directory":"/workspace"},"username":"fixture"}"#,
    )
    .unwrap();
    std::fs::write(session.join("messages.jsonl"), "{}\n").unwrap();
}

pub(super) fn write_mux_discovery_session(sessions: &Path) {
    let session = sessions.join("mux-discovery");
    std::fs::create_dir_all(&session).unwrap();
    std::fs::write(
        session.join("chat.jsonl"),
        r#"{"id":"msg-mux-discovery","role":"user","parts":[{"type":"text","text":"mux discovery"}],"metadata":{"historySequence":0},"workspaceId":"mux-discovery"}"#,
    )
    .unwrap();
}

pub(super) fn shared_provider_history_fixture(name: &str) -> PathBuf {
    capture_repo_root()
        .join("tests/fixtures/provider-history")
        .join(name)
}

pub(super) fn write_task_json_discovery_task(root: &Path, task_id: &str, file_name: &str) {
    let task = root.join("tasks").join(task_id);
    std::fs::create_dir_all(&task).unwrap();
    std::fs::write(task.join(file_name), "[]").unwrap();
}

pub(super) fn write_lingma_discovery_db(path: &Path) {
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

pub(super) fn write_trae_discovery_db(path: &Path) {
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

pub(super) fn write_trae_non_chat_state_db(path: &Path) {
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

pub(super) fn assert_source_status(
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
