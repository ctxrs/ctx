#![allow(dead_code)]

use std::{
    env,
    ffi::{OsStr, OsString},
    fs, io,
    path::{Path, PathBuf},
    sync::Mutex,
};

use ctx_history_core::CaptureProvider;
use ctx_history_source_discovery::{
    CursorProbeFragment, CursorTranscriptProbeOutcome, ProviderSourceStatus,
    StaticProviderProbeCatalog,
};
use rusqlite::Connection;

pub static ENV_LOCK: Mutex<()> = Mutex::new(());

pub static TEST_PROVIDER_PROBES: StaticProviderProbeCatalog =
    StaticProviderProbeCatalog::new(CursorProbeFragment::new(cursor_transcript_probe));

pub fn tempdir() -> tempfile::TempDir {
    let temp_root = fs::canonicalize(env::temp_dir())
        .expect("system temporary directory should be available for test fixtures");
    tempfile::Builder::new()
        .prefix("ctx-history-source-discovery-qualification-")
        .tempdir_in(temp_root)
        .expect("system temporary directory should support test fixtures")
}

fn capture_repo_root() -> PathBuf {
    let manifest = capture_manifest_dir();
    manifest
        .ancestors()
        .find(|candidate| {
            candidate.join("Cargo.toml").is_file()
                && candidate
                    .join("docs/provider-support-matrix.json")
                    .is_file()
        })
        .unwrap_or_else(|| panic!("locate ctx repository above {}", manifest.display()))
        .to_path_buf()
}

pub fn provider_support_matrix() -> serde_json::Value {
    let path = capture_repo_root().join("docs/provider-support-matrix.json");
    let bytes = fs::read(&path)
        .unwrap_or_else(|error| panic!("read provider support matrix {}: {error}", path.display()));
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("parse provider support matrix {}: {error}", path.display()))
}

fn capture_manifest_dir() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if manifest.is_absolute() {
        return manifest;
    }
    if let Ok(current_dir) = env::current_dir() {
        if let Some(path) = manifest_dir_from(&current_dir, &manifest) {
            return path;
        }
    }
    if let Ok(current_exe) = env::current_exe() {
        for ancestor in current_exe.ancestors() {
            if let Some(path) = manifest_dir_from(ancestor, &manifest) {
                return path;
            }
        }
    }
    manifest
}

fn manifest_dir_from(base: &Path, manifest: &Path) -> Option<PathBuf> {
    let candidate = base.join(manifest);
    if candidate.join("Cargo.toml").is_file() {
        return fs::canonicalize(&candidate).ok().or(Some(candidate));
    }
    None
}

fn cursor_transcript_probe(path: &Path) -> CursorTranscriptProbeOutcome {
    const MAX_DIRECTORY_ENTRIES: usize = 1_024;
    const MAX_TRAVERSAL_ENTRIES: usize = 4_096;

    fn is_valid_transcript(projects: &Path, candidate: &Path) -> bool {
        let Ok(relative) = candidate.strip_prefix(projects) else {
            return false;
        };
        let components = relative.components().collect::<Vec<_>>();
        if components.len() != 4
            || components[1].as_os_str() != "agent-transcripts"
            || candidate
                .extension()
                .and_then(|extension| extension.to_str())
                != Some("jsonl")
        {
            return false;
        }
        let Some(session) = components[2].as_os_str().to_str() else {
            return false;
        };
        !session.trim().is_empty()
            && candidate.file_stem().and_then(|name| name.to_str()) == Some(session)
    }

    fn selected_projects_root(path: &Path) -> PathBuf {
        if path.file_name().and_then(|name| name.to_str()) == Some(".cursor") {
            return path.join("projects");
        }
        path.ancestors()
            .find(|candidate| {
                candidate.file_name().and_then(|name| name.to_str()) == Some("projects")
            })
            .unwrap_or(path)
            .to_path_buf()
    }

    fn scan(
        path: &Path,
        projects: &Path,
        entries: &mut usize,
    ) -> Result<bool, CursorTranscriptProbeOutcome> {
        let metadata = fs::symlink_metadata(path).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                CursorTranscriptProbeOutcome::NotFound
            } else {
                CursorTranscriptProbeOutcome::IoError
            }
        })?;
        if metadata.file_type().is_symlink() {
            return Err(CursorTranscriptProbeOutcome::IoError);
        }
        if metadata.is_file() {
            return Ok(is_valid_transcript(projects, path));
        }
        if !metadata.is_dir() {
            return Ok(false);
        }
        let entries_in_directory = fs::read_dir(path)
            .map_err(|_| CursorTranscriptProbeOutcome::IoError)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| CursorTranscriptProbeOutcome::IoError)?;
        if entries_in_directory.len() > MAX_DIRECTORY_ENTRIES {
            return Err(CursorTranscriptProbeOutcome::BudgetExhausted);
        }
        for entry in entries_in_directory {
            *entries = entries.saturating_add(1);
            if *entries > MAX_TRAVERSAL_ENTRIES {
                return Err(CursorTranscriptProbeOutcome::BudgetExhausted);
            }
            if scan(&entry.path(), projects, entries)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    let projects = selected_projects_root(path);
    let mut entries = 0;
    match scan(&projects, &projects, &mut entries) {
        Ok(true) => CursorTranscriptProbeOutcome::Found,
        Ok(false) => CursorTranscriptProbeOutcome::NotFound,
        Err(outcome) => outcome,
    }
}

pub struct EnvGuard {
    name: &'static str,
    original: Option<OsString>,
}

impl EnvGuard {
    pub fn set(name: &'static str, value: impl AsRef<OsStr>) -> Self {
        let original = env::var_os(name);
        env::set_var(name, value);
        Self { name, original }
    }

    pub fn remove(name: &'static str) -> Self {
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

pub struct CwdGuard {
    original: PathBuf,
}

impl CwdGuard {
    pub fn set(path: &Path) -> Self {
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

pub fn write_pi_discovery_session(root: &Path) {
    let project = root.join("--workspace--");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join("2026-07-03T12-00-00-000Z_pi-discovery.jsonl"),
        "{}\n",
    )
    .unwrap();
}

pub fn write_qwen_discovery_chat(projects: &Path) {
    let chats = projects.join("project/chats");
    fs::create_dir_all(&chats).unwrap();
    fs::write(chats.join("qwen-discovery.jsonl"), "{}\n").unwrap();
}

pub fn write_kimi_discovery_wire(home: &Path) {
    let agent = home.join("sessions/wd_project_abc123/kimi-session/agents/main");
    fs::create_dir_all(&agent).unwrap();
    fs::write(agent.join("wire.jsonl"), "{}\n").unwrap();
}

pub fn write_junie_discovery_session(sessions: &Path, session_id: &str) {
    fs::create_dir_all(sessions.join(session_id)).unwrap();
    fs::write(
        sessions.join("index.jsonl"),
        format!(r#"{{"sessionId":"{session_id}","createdAt":1783339200000}}"#),
    )
    .unwrap();
    fs::write(
        sessions.join(session_id).join("events.jsonl"),
        "{\"kind\":\"UserPromptEvent\",\"prompt\":\"Junie discovery\"}\n",
    )
    .unwrap();
}

pub fn write_mistral_vibe_discovery_session(sessions: &Path) {
    let session = sessions.join("session_20260704_120000_vibe1234");
    fs::create_dir_all(&session).unwrap();
    fs::write(
        session.join("meta.json"),
        r#"{"session_id":"mistral-vibe-discovery","start_time":"2026-07-04T12:00:00Z","end_time":null,"git_commit":null,"git_branch":null,"environment":{"working_directory":"/workspace"},"username":"fixture"}"#,
    )
    .unwrap();
    fs::write(session.join("messages.jsonl"), "{}\n").unwrap();
}

pub fn write_mux_discovery_session(sessions: &Path) {
    let session = sessions.join("mux-discovery");
    fs::create_dir_all(&session).unwrap();
    fs::write(
        session.join("chat.jsonl"),
        r#"{"id":"msg-mux-discovery","role":"user","parts":[{"type":"text","text":"mux discovery"}],"metadata":{"historySequence":0},"workspaceId":"mux-discovery"}"#,
    )
    .unwrap();
}

pub fn shared_provider_history_fixture(name: &str) -> PathBuf {
    capture_repo_root()
        .join("tests/fixtures/provider-history")
        .join(name)
}

pub fn write_task_json_discovery_task(root: &Path, task_id: &str, file_name: &str) {
    let task = root.join("tasks").join(task_id);
    fs::create_dir_all(&task).unwrap();
    fs::write(task.join(file_name), "[]").unwrap();
}

pub fn write_lingma_discovery_db(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
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

pub fn assert_source_status(
    home: &Path,
    provider: CaptureProvider,
    expected: ProviderSourceStatus,
) {
    let source =
        ctx_history_source_discovery::discover_provider_sources(&TEST_PROVIDER_PROBES, home)
            .into_iter()
            .find(|source| source.provider == provider)
            .unwrap();
    assert_eq!(source.status, expected, "{provider:?}");
}
