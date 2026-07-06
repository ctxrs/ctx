pub(super) use crate::provider::adapter::ProviderCaptureAdapter;
pub(super) use crate::provider::codex::session::{
    should_parse_codex_session_line, should_skip_codex_tool_output_line,
};
pub(super) use crate::provider::custom_history_jsonl::{
    custom_history_internal_session_id, custom_history_jsonl_v1_cursor_stream,
};
pub(super) use crate::provider::file_touches::provider_file_touches_from_raw_value;
pub(super) use crate::provider::importer::{
    import_normalized_provider_captures, provider_command_run_from_event, provider_cursor_stream,
    provider_event_import_identity, provider_event_seq, provider_event_uuid,
    provider_file_touch_uuid, provider_scoped_source_uuid, provider_session_uuid,
    provider_source_event_import_identity, provider_source_event_seq, provider_source_event_uuid,
    provider_source_uuid, provider_sync_metadata, timestamps, ProviderCommandRunInput,
};
pub(super) use crate::provider::native::ShelleyMessageRow;
pub(super) use crate::provider::providers::{
    lingma::normalize_lingma_sqlite,
    opencode::{normalize_opencode_sqlite, OPENCODE_SQLITE_DIALECT},
    pi::{pi_provider_event_identity_index, PiSessionHeader},
    shelley::{shelley_event_index, shelley_value_text},
    trae::{TRAE_CN_INPUT_HISTORY_KEY, TRAE_STATE_VSCDB_SOURCE_FORMAT},
};
pub(super) use crate::{
    catalog_codex_session_tree, compute_payload_hash, discover_provider_sources_for_provider,
    fixture_envelope, import_antigravity_cli_history, import_astrbot_sqlite, import_auggie_history,
    import_claude_projects_jsonl_tree, import_cline_task_json_history, import_codebuddy_history,
    import_codex_history_jsonl, import_codex_session_jsonl, import_codex_session_paths,
    import_codex_session_tree, import_copilot_cli_session_events, import_crush_sqlite,
    import_custom_history_jsonl_v1, import_custom_history_jsonl_v1_reader,
    import_deepagents_sqlite, import_factory_ai_droid_sessions, import_firebender_sqlite,
    import_forgecode_sqlite, import_gemini_cli_history, import_goose_sessions_sqlite,
    import_hermes_sqlite, import_junie_history, import_kilo_sqlite, import_kimi_code_cli_history,
    import_kiro_sqlite, import_lingma_sqlite, import_mistral_vibe_history, import_mux_history,
    import_openclaw_history, import_opencode_sqlite, import_pi_session_jsonl,
    import_provider_fixture_jsonl, import_qoder_history, import_qwen_code_history,
    import_roo_task_json_history, import_rovodev_history, import_shelley_sqlite, import_spool,
    import_tabnine_cli_history, import_trae_history, import_warp_sqlite,
    import_windsurf_cascade_hook_transcripts, import_zed_threads_sqlite, provider_source_for_path,
    read_jsonl, spool_counts, stable_capture_uuid, AntigravityCliImportOptions,
    AstrBotSqliteImportOptions, AuggieImportOptions, CaptureError, CatalogSummary,
    ClaudeProjectsImportOptions, ClineTaskJsonImportOptions, CodeBuddyImportOptions,
    CodexEventImportMode, CodexHistoryImportOptions, CodexSessionCatalogOptions,
    CodexSessionImportOptions, CodexSessionJsonlAdapter, CodexToolOutputMode,
    CopilotCliImportOptions, CrushSqliteImportOptions, CustomHistoryJsonlV1ImportOptions,
    DeepAgentsSqliteImportOptions, FactoryAiDroidImportOptions, FirebenderSqliteImportOptions,
    FixtureOptions, ForgeCodeSqliteImportOptions, GeminiCliImportOptions,
    GooseSessionsSqliteImportOptions, HermesSqliteImportOptions, JunieImportOptions,
    KiloSqliteImportOptions, KimiCodeCliImportOptions, KiroSqliteImportOptions,
    LingmaSqliteImportOptions, MistralVibeImportOptions, MuxImportOptions,
    NormalizedProviderImportOptions, OpenClawImportOptions, OpenCodeSqliteImportOptions,
    PiSessionImportOptions, ProviderAdapterContext, ProviderFileTouchedEnvelope,
    ProviderFixtureImportOptions, ProviderImportSummary, ProviderImportSupport,
    ProviderNormalizationResult, ProviderSourceStatus, QoderImportOptions, QwenCodeImportOptions,
    RooTaskJsonImportOptions, RovoDevImportOptions, ShelleySqliteImportOptions, SpoolWriter,
    TabnineCliImportOptions, TraeImportOptions, WarpSqliteImportOptions,
    WindsurfCascadeHookImportOptions, ZedThreadsSqliteImportOptions, ANTIGRAVITY_CLI_SOURCE_FORMAT,
    ASTRBOT_SQLITE_SOURCE_FORMAT, AUGGIE_SESSION_JSON_SOURCE_FORMAT, CLAUDE_PROJECTS_SOURCE_FORMAT,
    CODEBUDDY_SOURCE_FORMAT, COPILOT_CLI_SOURCE_FORMAT, CRUSH_SQLITE_SOURCE_FORMAT,
    CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT, DEEPAGENTS_SQLITE_SOURCE_FORMAT,
    FACTORY_DROID_SOURCE_FORMAT, FIREBENDER_SQLITE_SOURCE_FORMAT, FORGECODE_SQLITE_SOURCE_FORMAT,
    GEMINI_CLI_SOURCE_FORMAT, JUNIE_SESSION_EVENTS_SOURCE_FORMAT, KILO_SQLITE_SOURCE_FORMAT,
    KIRO_SQLITE_SOURCE_FORMAT, LINGMA_SQLITE_SOURCE_FORMAT, MAX_OPENCLAW_SESSION_INDEX_BYTES,
    MAX_PROVIDER_JSONL_LINE_BYTES, MAX_PROVIDER_SQLITE_VALUE_BYTES, OPENCODE_SQLITE_SOURCE_FORMAT,
    PROVIDER_MAX_TEXT_CHARS, SHELLEY_SQLITE_SOURCE_FORMAT, ZED_THREADS_SQLITE_SOURCE_FORMAT,
};
pub(super) use chrono::{DateTime, Utc};
pub(super) use ctx_history_core::{
    new_id, AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    Confidence, Event, EventRole, EventType, Fidelity, FileChangeKind, FileTouched,
    ProviderCaptureEnvelope, ProviderEventEnvelope, ProviderRawRetention,
    ProviderRedactionBoundary, ProviderSessionEnvelope, ProviderSourceEnvelope,
    ProviderSourceTrust, RedactionState, Session, SessionStatus,
    PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
};
pub(super) use ctx_history_store::Store;
pub(super) use rusqlite::Connection;
pub(super) use serde_json::{json, Value};
pub(super) use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};
pub(super) use tempfile::TempDir;
pub(super) use uuid::Uuid;

pub(super) fn tempdir() -> TempDir {
    tempfile::Builder::new()
        .prefix("ctx-history-capture-")
        .tempdir()
        .unwrap()
}

pub(super) fn fixture_options(dedupe_key: &str, title: &str) -> FixtureOptions {
    FixtureOptions {
        title: title.to_owned(),
        body: "captured body".to_owned(),
        tags: vec!["capture-test".to_owned()],
        dedupe_key: Some(dedupe_key.to_owned()),
        machine_id: Some("test-machine".to_owned()),
        cwd: Some(PathBuf::from("/tmp/work")),
        occurred_at: DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
    }
}

pub(super) fn provider_fixture(name: &str) -> PathBuf {
    materialized_fixture("provider", name)
}

pub(super) fn provider_history_fixture(name: &str) -> PathBuf {
    materialized_fixture("provider-history", name)
}

pub(super) fn custom_history_fixture(name: &str) -> PathBuf {
    materialized_fixture("custom-history-jsonl", name)
}

pub(super) fn write_oversized_jsonl_line(path: &Path) {
    fs::write(path, vec![b'x'; MAX_PROVIDER_JSONL_LINE_BYTES + 1]).unwrap();
}

pub(super) fn jsonl_line(value: Value) -> String {
    serde_json::to_string(&value).unwrap() + "\n"
}

pub(super) fn test_provider_event(event_type: EventType) -> ProviderEventEnvelope {
    ProviderEventEnvelope {
        provider_event_index: 0,
        provider_event_hash: Some("event-hash".to_owned()),
        cursor: None,
        event_type,
        role: Some(EventRole::Tool),
        occurred_at: "2026-07-03T12:00:00Z".parse().unwrap(),
        fidelity: Fidelity::Imported,
        redaction_state: RedactionState::LocalPreview,
        idempotency_key: None,
        artifacts: Vec::new(),
        payload: json!({}),
        metadata: json!({}),
    }
}

pub(super) fn materialized_fixture(category: &str, name: &str) -> PathBuf {
    let source = match category {
        "provider" => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/provider")
            .join(name),
        "provider-history" => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/provider-history")
            .join(name),
        "custom-history-jsonl" => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/custom-history-jsonl")
            .join(name),
        _ => panic!("unknown fixture category {category}"),
    };
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/test-data/materialized-fixtures");
    fs::create_dir_all(&root).unwrap();
    let unique = format!(
        "{}-{}-{}-{}",
        category,
        name.replace(['/', '\\', '.'], "_"),
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let target = root.join(unique);
    if source.is_dir() {
        copy_dir_all(&source, &target);
    } else {
        fs::copy(&source, &target).unwrap();
    }
    target
}

pub(super) fn copy_dir_all(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let entry_path = entry.path();
        let target = to.join(entry.file_name());
        if entry_path.is_dir() {
            copy_dir_all(&entry_path, &target);
        } else {
            fs::copy(entry_path, target).unwrap();
        }
    }
}

pub(super) fn synthetic_codex_session_tree(root: &Path, sessions: usize) -> u64 {
    (0..sessions)
        .map(|index| write_synthetic_codex_session(root, index, "baseline"))
        .sum()
}

pub(super) fn write_synthetic_codex_session(root: &Path, index: usize, marker: &str) -> u64 {
    let shard = format!("{:02}", index / 1000);
    let dir = root.join("2026").join("06").join("26").join(shard);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("synthetic-session-{index:06}.jsonl"));
    let seconds = index % 86_400;
    let timestamp = format!(
        "2026-06-26T{:02}:{:02}:{:02}.000Z",
        seconds / 3600,
        (seconds / 60) % 60,
        seconds % 60
    );
    let session_id = format!("synthetic-codex-session-{index:06}");
    let meta = json!({
        "timestamp": timestamp,
        "type": "session_meta",
        "payload": {
            "id": session_id,
            "timestamp": timestamp,
            "cwd": "/repo/ctx",
            "originator": "codex-cli",
            "cli_version": "0.2.0-test",
            "source": "cli",
            "model_provider": "openai"
        }
    });
    let message = json!({
        "timestamp": timestamp,
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": format!("incremental import synthetic corpus {index:06} {marker}")
            }]
        }
    });
    let body = format!("{meta}\n{message}\n");
    fs::write(&path, body.as_bytes()).unwrap();
    body.len() as u64
}

#[derive(Debug)]
pub(super) struct IncrementalCatchUpSummary {
    pub(super) catalog: CatalogSummary,
    pub(super) import: ProviderImportSummary,
    pub(super) pending_sessions: usize,
}

pub(super) fn incremental_codex_catch_up(
    root: &Path,
    store: &mut Store,
    observed_at: DateTime<Utc>,
) -> IncrementalCatchUpSummary {
    let source_root = root.display().to_string();
    let catalog = catalog_codex_session_tree(
        root,
        store,
        CodexSessionCatalogOptions {
            source_root: Some(root.to_path_buf()),
            cataloged_at: observed_at,
            allow_partial_failures: false,
            ..CodexSessionCatalogOptions::default()
        },
    )
    .unwrap();
    let pending = store
        .list_pending_catalog_sessions(CaptureProvider::Codex, &source_root)
        .unwrap();
    let pending_sessions = pending.len();
    if pending.is_empty() {
        return IncrementalCatchUpSummary {
            catalog,
            import: ProviderImportSummary::default(),
            pending_sessions,
        };
    }

    let paths = pending
        .iter()
        .map(|session| PathBuf::from(&session.source_path))
        .collect::<Vec<_>>();
    let import = import_codex_session_paths(
        paths,
        store,
        CodexSessionImportOptions {
            source_path: Some(root.to_path_buf()),
            imported_at: observed_at,
            allow_partial_failures: false,
            ..CodexSessionImportOptions::default()
        },
    )
    .unwrap();
    let indexed_at_ms = observed_at.timestamp_millis();
    for session in pending {
        store
            .mark_catalog_source_indexed(
                CaptureProvider::Codex,
                ctx_history_store::CatalogSourceIndexUpdate {
                    source_root: &session.source_root,
                    source_path: &session.source_path,
                    file_size_bytes: session.file_size_bytes,
                    file_modified_at_ms: session.file_modified_at_ms,
                    file_sha256: None,
                    event_count: Some(1),
                    indexed_at_ms,
                },
            )
            .unwrap();
    }

    IncrementalCatchUpSummary {
        catalog,
        import,
        pending_sessions,
    }
}

#[derive(Debug)]
pub(super) struct TimingStats {
    pub(super) min_ms: f64,
    pub(super) p50_ms: f64,
    pub(super) p95_ms: f64,
    pub(super) max_ms: f64,
}

impl TimingStats {
    pub(super) fn to_json(&self) -> Value {
        json!({
            "min_ms": rounded(self.min_ms),
            "p50_ms": rounded(self.p50_ms),
            "p95_ms": rounded(self.p95_ms),
            "max_ms": rounded(self.max_ms),
        })
    }
}

pub(super) fn timing_stats(samples: &[f64]) -> TimingStats {
    assert!(!samples.is_empty(), "timing samples must not be empty");
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    TimingStats {
        min_ms: sorted[0],
        p50_ms: percentile(&sorted, 0.50),
        p95_ms: percentile(&sorted, 0.95),
        max_ms: *sorted.last().unwrap(),
    }
}

pub(super) fn percentile(sorted: &[f64], percentile: f64) -> f64 {
    let index = ((sorted.len() - 1) as f64 * percentile).ceil() as usize;
    sorted[index.min(sorted.len() - 1)]
}

pub(super) fn elapsed_ms(duration: std::time::Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

pub(super) fn rounded(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

pub(super) fn env_flag(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| {
        let value = value.to_string_lossy();
        !matches!(value.as_ref(), "" | "0" | "false" | "False" | "FALSE")
    })
}

pub(super) fn env_usize(name: &str) -> Option<usize> {
    std::env::var(name).ok()?.parse().ok()
}

pub(super) fn env_f64(name: &str) -> Option<f64> {
    std::env::var(name).ok()?.parse().ok()
}

pub(super) fn incremental_perf_file_count() -> usize {
    env_usize("CTX_CODEX_INCREMENTAL_PERF_FILES").unwrap_or_else(|| {
        if env_flag("CTX_CODEX_INCREMENTAL_PERF_SLOW") {
            32_000
        } else {
            5_000
        }
    })
}

pub(super) fn incremental_perf_repeats() -> usize {
    env_usize("CTX_CODEX_INCREMENTAL_PERF_REPEATS")
        .unwrap_or(5)
        .max(1)
}

pub(super) fn incremental_perf_noop_p95_threshold_ms(file_count: usize) -> f64 {
    env_f64("CTX_CODEX_INCREMENTAL_PERF_NOOP_P95_MS").unwrap_or({
        if file_count >= 30_000 {
            1_000.0
        } else {
            500.0
        }
    })
}

pub(super) fn incremental_perf_noop_us_per_file_threshold() -> f64 {
    env_f64("CTX_CODEX_INCREMENTAL_PERF_NOOP_US_PER_FILE").unwrap_or(50.0)
}

pub(super) fn fixed_import_options(path: PathBuf) -> ProviderFixtureImportOptions {
    ProviderFixtureImportOptions {
        machine_id: "test-machine".into(),
        source_path: Some(path),
        imported_at: DateTime::parse_from_rfc3339("2026-06-23T15:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
        history_record_id: None,
        expected_provider: None,
        allow_partial_failures: false,
        ..ProviderFixtureImportOptions::default()
    }
}

pub(super) fn write_minimal_provider_fixture(
    temp: &TempDir,
    provider: CaptureProvider,
    external_session_id: &str,
) -> PathBuf {
    let provider_name = provider.as_str();
    let path = temp.path().join(format!("{provider_name}.jsonl"));
    let line = json!({
        "provider": provider_name,
        "session": {
            "provider_session_id": external_session_id,
            "agent_type": "primary",
            "role_hint": "primary",
            "is_primary": true,
            "status": "imported",
            "started_at": "2026-06-23T17:00:00Z",
            "cwd": "/workspace/example",
            "metadata": {"source": "temp-fixture", "provider": provider_name}
        },
        "event": {
            "provider_event_index": 0,
            "cursor": format!("{provider_name}-cursor-0"),
            "event_type": "message",
            "role": "user",
            "occurred_at": "2026-06-23T17:00:01Z",
            "payload": {"text": format!("{provider_name} provider fixture smoke")},
            "metadata": {"source": "temp-fixture"}
        }
    });
    fs::write(&path, format!("{line}\n")).unwrap();
    path
}

pub(super) fn write_unimportable_jsonl_siblings(root: &Path, prefix: &str) {
    fs::write(root.join(format!("{prefix}-empty.jsonl")), "").unwrap();
    fs::write(
        root.join(format!("{prefix}-malformed.jsonl")),
        "{\"not valid\"\n",
    )
    .unwrap();
    fs::write(
        root.join(format!("{prefix}-headerless.jsonl")),
        "{\"type\":\"message\",\"content\":\"missing session header\"}\n",
    )
    .unwrap();
}

pub(super) fn write_unimportable_copilot_siblings(root: &Path) {
    for (session, content) in [
        ("copilot-empty", ""),
        ("copilot-malformed", "{\"not valid\"\n"),
        (
            "copilot-headerless",
            "{\"type\":\"user.message\",\"data\":{\"content\":\"missing session header\"}}\n",
        ),
    ] {
        let path = root.join(session);
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("events.jsonl"), content).unwrap();
    }
}

pub(super) fn assert_provider_failures_include_headerless_and_malformed(
    summary: &ProviderImportSummary,
) {
    assert!(summary.failures.iter().any(|failure| failure
        .error
        .contains("no importable native JSONL session header")));
    assert!(summary
        .failures
        .iter()
        .any(|failure| failure.error.contains("malformed JSONL")));
}

pub(super) fn write_claude_smoke_fixture(temp: &TempDir) -> PathBuf {
    let root = temp.path().join("claude/projects/-workspace");
    let subagents = root.join("claude-native-parent/subagents");
    fs::create_dir_all(&subagents).unwrap();
    fs::write(
            root.join("claude-native-parent.jsonl"),
            concat!(
                "{\"sessionId\":\"claude-native-parent\",\"timestamp\":\"2026-06-24T12:00:00Z\",\"cwd\":\"/workspace\",\"version\":\"test\",\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"Run a smoke tool.\"}]},\"uuid\":\"claude-parent-1\"}\n",
                "{\"sessionId\":\"claude-native-parent\",\"timestamp\":\"2026-06-24T12:00:01Z\",\"cwd\":\"/workspace\",\"version\":\"test\",\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"tool_use\",\"id\":\"tool-1\",\"name\":\"Bash\",\"input\":{\"command\":\"true\"}}]},\"uuid\":\"claude-parent-2\"}\n",
                "{\"sessionId\":\"claude-native-parent\",\"timestamp\":\"2026-06-24T12:00:02Z\",\"cwd\":\"/workspace\",\"version\":\"test\",\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"tool-1\",\"content\":\"ok\"}]},\"uuid\":\"claude-parent-3\"}\n",
            ),
        )
        .unwrap();
    fs::write(
            subagents.join("agent-scout.jsonl"),
            concat!(
                "{\"sessionId\":\"claude-native-parent\",\"timestamp\":\"2026-06-24T12:00:03Z\",\"cwd\":\"/workspace\",\"version\":\"test\",\"isSidechain\":true,\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"inspect\"},\"uuid\":\"claude-child-1\"}\n",
                "{\"sessionId\":\"claude-native-parent\",\"timestamp\":\"2026-06-24T12:00:04Z\",\"cwd\":\"/workspace\",\"version\":\"test\",\"isSidechain\":true,\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":\"done\"},\"uuid\":\"claude-child-2\"}\n",
            ),
        )
        .unwrap();
    temp.path().join("claude/projects")
}

pub(super) fn write_opencode_smoke_db(temp: &TempDir, malformed: bool) -> PathBuf {
    let path = temp.path().join(if malformed {
        "opencode-malformed.db"
    } else {
        "opencode.db"
    });
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
            "create table session (
                id text primary key, parent_id text, title text not null, directory text not null,
                model text, agent text, time_created integer not null, time_updated integer not null,
                tokens_input integer not null, tokens_output integer not null,
                tokens_reasoning integer not null, tokens_cache_read integer not null,
                tokens_cache_write integer not null
            );
            create table session_message (
                id text primary key, session_id text not null, type text not null, seq integer not null,
                time_created integer not null, time_updated integer not null, data text not null
            );",
        )
        .unwrap();
    conn.execute(
            "insert into session values (?1, null, 'root', '/workspace', '{\"id\":\"test\"}', 'build', 1782259200000, 1782259200000, 1, 1, 0, 0, 0)",
            ["opencode-root"],
        )
        .unwrap();
    conn.execute(
            "insert into session values (?1, ?2, 'child', '/workspace', '{\"id\":\"test\"}', 'scout', 1782259201000, 1782259201000, 1, 1, 0, 0, 0)",
            ["opencode-child", "opencode-root"],
        )
        .unwrap();
    conn.execute(
        "insert into session_message values (?1, ?2, 'user', 1, 1782259200000, 1782259200000, ?3)",
        [
            "msg-user",
            "opencode-root",
            "{\"time\":{\"created\":1782259200000},\"text\":\"inspect\"}",
        ],
    )
    .unwrap();
    conn.execute(
            "insert into session_message values (?1, ?2, 'assistant', 2, 1782259201000, 1782259201000, ?3)",
            ["msg-assistant", "opencode-root", "{\"time\":{\"created\":1782259201000},\"content\":[{\"type\":\"tool\",\"name\":\"bash\"}]}"],
        )
        .unwrap();
    let child_data = if malformed {
        "{\"time\":{\"created\":1782259202000},\"text\":"
    } else {
        "{\"time\":{\"created\":1782259202000},\"text\":\"child done\"}"
    };
    conn.execute(
            "insert into session_message values (?1, ?2, 'assistant', 1, 1782259202000, 1782259202000, ?3)",
            ["msg-child", "opencode-child", child_data],
        )
        .unwrap();
    path
}

pub(super) fn write_hermes_smoke_db(temp: &TempDir) -> PathBuf {
    let path = temp.path().join("hermes-state.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table sessions (
                id text primary key,
                source text not null,
                started_at real not null
            );
            create table messages (
                id integer primary key autoincrement,
                session_id text not null,
                role text not null,
                content text,
                timestamp real not null,
                active integer not null default 1,
                compacted integer not null default 0
            );",
    )
    .unwrap();
    conn.execute(
        "insert into sessions values (?1, 'acp', 1782259200.0)",
        ["hermes-root"],
    )
    .unwrap();
    conn.execute(
            "insert into messages (session_id, role, content, timestamp) values (?1, 'user', 'bad timestamp', 1782259201.0)",
            ["hermes-root"],
        )
        .unwrap();
    conn.execute(
            "insert into messages (session_id, role, content, timestamp) values (?1, 'assistant', 'good timestamp', 1782259202.0)",
            ["hermes-root"],
        )
        .unwrap();
    path
}

pub(super) fn write_opencode_session_message_without_seq_db(temp: &TempDir) -> PathBuf {
    let path = temp.path().join("opencode-no-seq.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table session (
                id text primary key, title text not null, directory text not null,
                time_created integer not null, time_updated integer not null
            );
            create table session_message (
                id text primary key, session_id text not null, type text not null,
                time_created integer not null, time_updated integer not null, data text not null
            );",
    )
    .unwrap();
    conn.execute(
        "insert into session values (?1, 'no seq', '/workspace', 1782259200000, 1782259200000)",
        ["opencode-no-seq"],
    )
    .unwrap();
    conn.execute(
        "insert into session_message values (?1, ?2, 'user', 1782259200000, 1782259200000, ?3)",
        [
            "msg-no-seq-user",
            "opencode-no-seq",
            "{\"time\":{\"created\":1782259200000},\"text\":\"first no seq\"}",
        ],
    )
    .unwrap();
    conn.execute(
            "insert into session_message values (?1, ?2, 'assistant', 1782259201000, 1782259201000, ?3)",
            [
                "msg-no-seq-assistant",
                "opencode-no-seq",
                "{\"time\":{\"created\":1782259201000},\"text\":\"second no seq\"}",
            ],
        )
        .unwrap();
    path
}

pub(super) fn write_opencode_current_schema_db(temp: &TempDir, with_message: bool) -> PathBuf {
    let path = temp.path().join(if with_message {
        "opencode-current-message.db"
    } else {
        "opencode-current-empty.db"
    });
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table session (
                id text primary key,
                project_id text not null,
                parent_id text,
                slug text not null,
                directory text not null,
                title text not null,
                version text not null,
                share_url text,
                summary_additions integer,
                summary_deletions integer,
                summary_files integer,
                summary_diffs text,
                revert text,
                permission text,
                time_created integer not null,
                time_updated integer not null,
                time_compacting integer,
                time_archived integer,
                workspace_id text
            );
            create table session_entry (
                id text primary key,
                session_id text not null,
                type text not null,
                time_created integer not null,
                time_updated integer not null,
                data text not null
            );
            create table message (
                id text primary key,
                session_id text not null,
                time_created integer not null,
                time_updated integer not null,
                data text not null
            );
            create table part (
                id text primary key,
                message_id text not null,
                session_id text not null,
                type text not null,
                time_created integer not null,
                time_updated integer not null,
                data text not null
            );",
    )
    .unwrap();

    if with_message {
        conn.execute(
            "insert into session (
                    id, project_id, parent_id, slug, directory, title, version, permission,
                    time_created, time_updated
                ) values (?1, 'project-1', null, 'current-root', '/workspace', 'current root',
                    '0.8.0', 'default', 1782259200000, 1782259200000)",
            ["current-root"],
        )
        .unwrap();
        conn.execute(
                "insert into message values (?1, ?2, 1782259200000, 1782259200000, ?3)",
                [
                    "current-message-1",
                    "current-root",
                    "{\"role\":\"user\",\"time\":{\"created\":1782259200000},\"text\":\"legacy hello\"}",
                ],
            )
            .unwrap();
    }

    path
}

pub(super) fn write_opencode_future_incomplete_schema_db(temp: &TempDir) -> PathBuf {
    let path = temp.path().join("opencode-future-incomplete.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table session (
                id text primary key,
                project_id text not null,
                slug text not null,
                directory text not null,
                title text not null,
                version text not null,
                time_created integer not null,
                time_updated integer not null
            );
            create table message (
                id text primary key,
                session_id text not null,
                time_created integer not null,
                time_updated integer not null
            );",
    )
    .unwrap();
    conn.execute(
        "insert into session (
                id, project_id, slug, directory, title, version, time_created, time_updated
            ) values ('future-root', 'project-1', 'future-root', '/workspace', 'future root',
                '0.9.0', 1782259200000, 1782259200000)",
        [],
    )
    .unwrap();
    conn.execute(
        "insert into message values ('future-message-1', 'future-root', 1782259200000,
                1782259200000)",
        [],
    )
    .unwrap();
    path
}

pub(super) fn write_shelley_smoke_db(temp: &TempDir) -> PathBuf {
    let path = temp.path().join("shelley.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table conversations (
                conversation_id text primary key,
                slug text,
                user_initiated boolean not null default true,
                created_at datetime not null default current_timestamp,
                updated_at datetime not null default current_timestamp,
                cwd text,
                archived boolean not null default false,
                parent_conversation_id text,
                model text,
                conversation_options text not null default '{}',
                current_generation integer not null default 1,
                agent_working boolean not null default false,
                tags text not null default '[]',
                is_draft boolean not null default false,
                draft text not null default '',
                queued_messages text not null default '[]'
            );
            create table messages (
                message_id text primary key,
                conversation_id text not null,
                sequence_id integer not null,
                type text not null,
                llm_data text,
                user_data text,
                usage_data text,
                created_at datetime not null default current_timestamp,
                display_data text,
                excluded_from_context boolean not null default false,
                generation integer not null default 1,
                llm_api_url text,
                model_name text,
                forked_from_message_id text
            );",
    )
    .unwrap();
    conn.execute(
            "insert into conversations values (
                'shelley-root', 'root-slug', 1, '2026-06-24 12:00:00',
                '2026-06-24 12:05:00', '/workspace/shelley', 0, null,
                'claude-opus-4-7', ?1, 2, 0, ?2, 0, '', ?3
            )",
            [
                r#"{"thinking_level":"high","subagent_backend":"shelley"}"#,
                r#"["native","ctx"]"#,
                r#"[{"id":"queued-1","llm":{"Content":[{"Type":2,"Text":"queued oracle"}]},"created_at":"2026-06-24T12:00:04Z","model":"claude-opus-4-7"}]"#,
            ],
        )
        .unwrap();
    conn.execute(
        "insert into conversations values (
                'shelley-child', 'child-slug', 0, '2026-06-24 12:01:00',
                '2026-06-24 12:02:00', '/workspace/shelley', 0, 'shelley-root',
                'claude-sonnet-4-5', '{}', 1, 0, '[]', 0, '', '[]'
            )",
        [],
    )
    .unwrap();
    conn.execute(
        "insert into conversations values (
                'shelley-draft', 'old-draft', 1, '2026-06-24 11:00:00',
                '2026-06-24 11:01:00', '/workspace/archive', 1, null,
                null, '{}', 1, 0, '[]', 1, 'draft body', '[]'
            )",
        [],
    )
    .unwrap();
    conn.execute(
        "insert into messages (
                message_id, conversation_id, sequence_id, type, user_data, created_at
            ) values ('msg-user', 'shelley-root', 1, 'user', ?1, '2026-06-24 12:00:01')",
        [json!({
            "Content": [
                {"Type": 2, "Text": "please run shelley search oracle"}
            ]
        })
        .to_string()],
    )
    .unwrap();
    conn.execute(
            "insert into messages (
                message_id, conversation_id, sequence_id, type, llm_data, usage_data,
                created_at, generation, llm_api_url, model_name
            ) values (
                'msg-agent', 'shelley-root', 2, 'agent', ?1, ?2,
                '2026-06-24 12:00:02', 2, 'https://api.anthropic.com/v1/messages',
                'claude-opus-4-7'
            )",
            [
                json!({
                    "Role": 1,
                    "Content": [
                        {"Type": 3, "Thinking": "thinking through the search"},
                        {"Type": 2, "Text": "I will inspect the source."},
                        {"Type": 5, "ID": "toolu_1", "ToolName": "bash", "ToolInput": {"command": "rg shelley"}}
                    ],
                    "EndOfTurn": false
                })
                .to_string(),
                json!({
                    "input_tokens": 100,
                    "cache_read_input_tokens": 25,
                    "output_tokens": 40,
                    "cost_usd": 0.0123,
                    "model": "claude-opus-4-7",
                    "url": "https://api.anthropic.com/v1/messages"
                })
                .to_string(),
            ],
        )
        .unwrap();
    conn.execute(
            "insert into messages (
                message_id, conversation_id, sequence_id, type, user_data, display_data,
                created_at, forked_from_message_id
            ) values (
                'msg-tool-result', 'shelley-root', 3, 'user', ?1, ?2,
                '2026-06-24 12:00:03', 'source-msg-tool-result'
            )",
            [
                json!({
                    "Role": 0,
                    "Content": [
                        {"Type": 6, "ToolUseID": "toolu_1", "ToolResult": [{"Type": 2, "Text": "tool output oracle"}]}
                    ]
                })
                .to_string(),
                json!({"stdout": "tool output oracle", "exit_code": 0}).to_string(),
            ],
        )
        .unwrap();
    conn.execute(
        "insert into messages (
                message_id, conversation_id, sequence_id, type, llm_data, created_at
            ) values ('msg-child', 'shelley-child', 1, 'agent', ?1, '2026-06-24 12:01:01')",
        [json!({
            "Content": [
                {"Type": 2, "Text": "subagent result from Shelley"}
            ]
        })
        .to_string()],
    )
    .unwrap();
    path
}

pub(super) fn write_shelley_adversarial_db(temp: &TempDir) -> PathBuf {
    let path = temp.path().join("shelley-adversarial.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table conversations (
                conversation_id text primary key,
                slug text,
                user_initiated boolean not null default true,
                created_at datetime not null default current_timestamp,
                updated_at datetime not null default current_timestamp,
                cwd text,
                archived boolean not null default false,
                parent_conversation_id text,
                model text,
                conversation_options text not null default '{}',
                current_generation integer not null default 1,
                agent_working boolean not null default false,
                tags text not null default '[]',
                is_draft boolean not null default false,
                draft text not null default '',
                queued_messages text not null default '[]'
            );
            create table messages (
                message_id text primary key,
                conversation_id text not null,
                sequence_id integer not null,
                type text not null,
                llm_data text,
                user_data text,
                usage_data text,
                created_at datetime not null default current_timestamp,
                display_data text,
                excluded_from_context boolean not null default false,
                generation integer not null default 1,
                llm_api_url text,
                model_name text,
                forked_from_message_id text
            );",
    )
    .unwrap();
    conn.execute(
        "insert into conversations values (
                'shelley-adversarial', 'adversarial', 1, '2026-06-24 12:00:00',
                '2026-06-24 12:05:00', '/workspace/shelley', 0, null,
                'claude-opus-4-7', '{}', 1, 0, '[]', 0, '', '[]'
            )",
        [],
    )
    .unwrap();
    for (message_id, sequence_id, message_type, text) in [
        ("msg-dup-a", 1, "user", "duplicate sequence first"),
        ("msg-dup-b", 1, "user", "duplicate sequence second"),
        ("msg-git", 2, "gitinfo", "commit abc touched shelley.rs"),
        ("msg-warning", 3, "warning", "warning message for Shelley"),
    ] {
        conn.execute(
            "insert into messages (
                    message_id, conversation_id, sequence_id, type, user_data, created_at
                ) values (?1, 'shelley-adversarial', ?2, ?3, ?4, '2026-06-24 12:00:01')",
            rusqlite::params![
                message_id,
                sequence_id,
                message_type,
                json!({"Content": [{"Type": 2, "Text": text}]}).to_string(),
            ],
        )
        .unwrap();
    }
    conn.execute(
        "insert into messages (
                message_id, conversation_id, sequence_id, type, llm_data, created_at
            ) values ('msg-large', 'shelley-adversarial', 4, 'agent', ?1, '2026-06-24 12:00:04')",
        [json!({
            "Content": [
                {"Type": 2, "Text": "x".repeat(PROVIDER_MAX_TEXT_CHARS + 200)}
            ]
        })
        .to_string()],
    )
    .unwrap();
    path
}

pub(super) fn write_shelley_malformed_db(temp: &TempDir) -> PathBuf {
    let path = temp.path().join("shelley-malformed.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table conversations (conversation_id text primary key);
             create table messages (
                message_id text primary key,
                conversation_id text not null
             );",
    )
    .unwrap();
    path
}

pub(super) fn write_gemini_smoke_fixture(temp: &TempDir) -> PathBuf {
    let chats = temp.path().join("gemini/.gemini/tmp/project/chats");
    let child_dir = chats.join("gemini-root");
    fs::create_dir_all(&child_dir).unwrap();
    fs::write(
            chats.join("session-root.jsonl"),
            concat!(
                "{\"sessionId\":\"gemini-root\",\"startTime\":\"2026-06-24T12:00:00Z\",\"kind\":\"main\",\"directories\":[\"/workspace\"]}\n",
                "{\"id\":\"gemini-user\",\"timestamp\":\"2026-06-24T12:00:01Z\",\"type\":\"user\",\"content\":\"hi\"}\n",
                "{\"id\":\"gemini-tool\",\"timestamp\":\"2026-06-24T12:00:02Z\",\"type\":\"gemini\",\"toolCalls\":[{\"id\":\"call-1\",\"name\":\"run_subagent\"}]}\n",
            ),
        )
        .unwrap();
    fs::write(
            child_dir.join("gemini-child.jsonl"),
            concat!(
                "{\"sessionId\":\"gemini-child\",\"startTime\":\"2026-06-24T12:00:03Z\",\"kind\":\"subagent\",\"directories\":[\"/workspace\"]}\n",
                "{\"id\":\"gemini-child-user\",\"timestamp\":\"2026-06-24T12:00:04Z\",\"type\":\"user\",\"content\":\"inspect\"}\n",
            ),
        )
        .unwrap();
    temp.path().join("gemini/.gemini")
}

pub(super) fn write_droid_smoke_fixture(temp: &TempDir) -> PathBuf {
    let root = temp.path().join("droid/sessions/project");
    fs::create_dir_all(&root).unwrap();
    fs::write(
            root.join("droid-root.jsonl"),
            concat!(
                "{\"type\":\"session_start\",\"sessionId\":\"droid-root\",\"timestamp\":\"2026-06-24T12:00:00Z\",\"cwd\":\"/workspace\",\"model\":\"factory/droid\"}\n",
                "{\"type\":\"message\",\"id\":\"droid-user\",\"timestamp\":\"2026-06-24T12:00:01Z\",\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"delegate\"}]}\n",
                "{\"type\":\"message\",\"id\":\"droid-tool\",\"timestamp\":\"2026-06-24T12:00:02Z\",\"role\":\"assistant\",\"content\":[{\"type\":\"tool_use\",\"id\":\"tool-1\",\"name\":\"droid_worker\"}]}\n",
            ),
        )
        .unwrap();
    fs::write(
            root.join("droid-child.jsonl"),
            concat!(
                "{\"type\":\"session_start\",\"sessionId\":\"droid-child\",\"timestamp\":\"2026-06-24T12:00:03Z\",\"cwd\":\"/workspace\",\"model\":\"factory/droid\",\"parent\":\"droid-root\",\"decompSessionType\":\"worker\"}\n",
                "{\"type\":\"message\",\"id\":\"droid-child-user\",\"timestamp\":\"2026-06-24T12:00:04Z\",\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"inspect\"}]}\n",
            ),
        )
        .unwrap();
    temp.path().join("droid/sessions")
}

pub(super) fn write_copilot_smoke_fixture(temp: &TempDir) -> PathBuf {
    let root = temp.path().join("copilot/session-state/copilot-root");
    fs::create_dir_all(&root).unwrap();
    fs::write(
            root.join("events.jsonl"),
            concat!(
                "{\"id\":\"copilot-1\",\"timestamp\":\"2026-06-24T12:00:00Z\",\"type\":\"session.start\",\"data\":{\"sessionId\":\"copilot-root\",\"startTime\":\"2026-06-24T12:00:00Z\",\"selectedModel\":\"gpt-5-mini\",\"context\":{\"cwd\":\"/workspace\"}}}\n",
                "{\"id\":\"copilot-2\",\"timestamp\":\"2026-06-24T12:00:01Z\",\"type\":\"user.message\",\"data\":{\"content\":\"status\"}}\n",
                "{\"id\":\"copilot-3\",\"timestamp\":\"2026-06-24T12:00:02Z\",\"type\":\"assistant.message\",\"data\":{\"content\":\"running\",\"toolRequests\":[{\"toolCallId\":\"tool-1\",\"name\":\"bash\"}]}}\n",
                "{\"id\":\"copilot-4\",\"timestamp\":\"2026-06-24T12:00:03Z\",\"type\":\"tool.execution_start\",\"data\":{\"toolCallId\":\"tool-1\",\"toolName\":\"bash\"}}\n",
                "{\"id\":\"copilot-5\",\"timestamp\":\"2026-06-24T12:00:04Z\",\"type\":\"tool.execution_complete\",\"data\":{\"toolCallId\":\"tool-1\",\"success\":true,\"result\":{\"content\":\"ok\"}}}\n",
            ),
        )
        .unwrap();
    temp.path().join("copilot/session-state")
}

pub(super) fn write_qwen_smoke_fixture(temp: &TempDir) -> PathBuf {
    let chats = temp.path().join("qwen/.qwen/projects/workspace/chats");
    fs::create_dir_all(&chats).unwrap();
    fs::write(
            chats.join("qwen-smoke.jsonl"),
            concat!(
                "{\"uuid\":\"qwen-1\",\"parentUuid\":null,\"sessionId\":\"qwen-smoke\",\"timestamp\":\"2026-07-04T12:00:00Z\",\"type\":\"user\",\"cwd\":\"/workspace/qwen\",\"version\":\"test\",\"gitBranch\":\"main\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"qwen jsonl oracle prompt\"}]},\"model\":\"qwen3-coder\"}\n",
                "{\"uuid\":\"qwen-2\",\"parentUuid\":\"qwen-1\",\"sessionId\":\"qwen-smoke\",\"timestamp\":\"2026-07-04T12:00:01Z\",\"type\":\"assistant\",\"cwd\":\"/workspace/qwen\",\"version\":\"test\",\"gitBranch\":\"main\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"qwen jsonl oracle answer\"},{\"type\":\"tool_use\",\"id\":\"tool-1\",\"name\":\"Write\",\"input\":{\"path\":\"src/qwen_oracle.txt\",\"content\":\"proof\"}}]},\"usageMetadata\":{\"inputTokens\":5,\"outputTokens\":7},\"model\":\"qwen3-coder\"}\n",
                "{\"uuid\":\"qwen-3\",\"parentUuid\":\"qwen-2\",\"sessionId\":\"qwen-smoke\",\"timestamp\":\"2026-07-04T12:00:02Z\",\"type\":\"tool_result\",\"cwd\":\"/workspace/qwen\",\"version\":\"test\",\"gitBranch\":\"main\",\"message\":{\"role\":\"tool\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"tool-1\",\"content\":\"wrote src/qwen_oracle.txt\"}]},\"toolCallResult\":{\"tool\":\"Write\",\"path\":\"src/qwen_oracle.txt\",\"output\":\"ok\"},\"model\":\"qwen3-coder\"}\n",
            ),
        )
        .unwrap();
    temp.path().join("qwen/.qwen/projects")
}

pub(super) fn write_kimi_smoke_fixture(temp: &TempDir) -> PathBuf {
    let home = temp.path().join("kimi/.kimi-code");
    let session = home.join("sessions/wd_demo_abc123/kimi-smoke");
    let main = session.join("agents/main");
    let child = session.join("agents/agent-1");
    fs::create_dir_all(&main).unwrap();
    fs::create_dir_all(&child).unwrap();
    fs::write(
        home.join("session_index.jsonl"),
        format!(
            "{}\n",
            json!({
                "sessionId": "kimi-smoke",
                "sessionDir": session.display().to_string(),
                "workDir": "/workspace/kimi"
            })
        ),
    )
    .unwrap();
    fs::write(
            session.join("state.json"),
            json!({
                "createdAt": "2026-07-04T13:00:00Z",
                "updatedAt": "2026-07-04T13:00:05Z",
                "title": "Kimi JSONL oracle",
                "lastPrompt": "kimi jsonl oracle prompt",
                "agents": {
                    "main": {"homedir": "/fixture/agents/main", "type": "main", "parentAgentId": null},
                    "agent-1": {"homedir": "/fixture/agents/agent-1", "type": "coder", "parentAgentId": "main"}
                }
            })
            .to_string(),
        )
        .unwrap();
    fs::write(
            main.join("wire.jsonl"),
            concat!(
                "{\"type\":\"metadata\",\"protocol_version\":\"1.4\",\"created_at\":1783170000000}\n",
                "{\"type\":\"turn.prompt\",\"time\":1783170001000,\"input\":[{\"type\":\"text\",\"text\":\"kimi jsonl oracle prompt\"}],\"origin\":{\"kind\":\"user\"}}\n",
                "{\"type\":\"context.append_message\",\"time\":1783170002000,\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"kimi jsonl oracle answer\"}]}}\n",
                "{\"type\":\"context.append_loop_event\",\"time\":1783170003000,\"event\":{\"type\":\"tool.call\",\"toolName\":\"Write\",\"input\":{\"path\":\"src/kimi_oracle.txt\",\"content\":\"proof\"}}}\n",
                "{\"type\":\"context.append_loop_event\",\"time\":1783170004000,\"event\":{\"type\":\"tool.result\",\"toolName\":\"Write\",\"output\":\"wrote src/kimi_oracle.txt\"}}\n",
                "{\"type\":\"usage.record\",\"time\":1783170005000,\"model\":\"kimi-k2\",\"usage\":{\"input_tokens\":11,\"output_tokens\":13}}\n",
            ),
        )
        .unwrap();
    fs::write(
            child.join("wire.jsonl"),
            concat!(
                "{\"type\":\"metadata\",\"protocol_version\":\"1.4\",\"created_at\":1783170006000}\n",
                "{\"type\":\"turn.prompt\",\"time\":1783170007000,\"input\":[{\"type\":\"text\",\"text\":\"child inspect\"}]}\n",
                "{\"type\":\"context.append_message\",\"time\":1783170008000,\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"child done\"}]}}\n",
            ),
        )
        .unwrap();
    home
}

pub(super) fn assert_provider_source_collision_is_distinct(
    first_source_format: &str,
    first_source_path: &str,
    second_source_format: &str,
    second_source_path: &str,
) {
    let temp = tempdir();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let provider = CaptureProvider::Claude;
    let provider_session_id = "shared-provider-session";
    let occurred_at = DateTime::parse_from_rfc3339("2026-06-23T17:00:01Z")
        .unwrap()
        .with_timezone(&Utc);
    let first_source_id = provider_scoped_source_uuid(
        provider,
        provider_session_id,
        first_source_format,
        Some(first_source_path),
    );
    let second_source_id = provider_scoped_source_uuid(
        provider,
        provider_session_id,
        second_source_format,
        Some(second_source_path),
    );
    assert_ne!(first_source_id, second_source_id);

    let normalization = ProviderNormalizationResult {
        summary: ProviderImportSummary::default(),
        captures: vec![
            (
                1,
                provider_collision_capture(
                    provider,
                    provider_session_id,
                    first_source_format,
                    first_source_path,
                    occurred_at,
                ),
            ),
            (
                2,
                provider_collision_capture(
                    provider,
                    provider_session_id,
                    second_source_format,
                    second_source_path,
                    occurred_at,
                ),
            ),
        ],
        files_touched: vec![
            (
                1,
                provider_collision_file_touch(
                    provider,
                    provider_session_id,
                    first_source_format,
                    first_source_path,
                    occurred_at,
                ),
            ),
            (
                2,
                provider_collision_file_touch(
                    provider,
                    provider_session_id,
                    second_source_format,
                    second_source_path,
                    occurred_at,
                ),
            ),
        ],
    };

    let summary = import_normalized_provider_captures(
        &mut store,
        normalization,
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(summary.imported_events, 2);
    assert_eq!(store.capture_source_count().unwrap(), 2);

    let first_source = store.get_capture_source(first_source_id).unwrap();
    let second_source = store.get_capture_source(second_source_id).unwrap();
    assert_eq!(
        first_source.descriptor.raw_source_path.as_deref(),
        Some(first_source_path)
    );
    assert_eq!(
        first_source.sync.metadata["source_format"].as_str(),
        Some(first_source_format)
    );
    assert_eq!(
        second_source.descriptor.raw_source_path.as_deref(),
        Some(second_source_path)
    );
    assert_eq!(
        second_source.sync.metadata["source_format"].as_str(),
        Some(second_source_format)
    );

    let session_id = provider_session_uuid(provider, provider_session_id);
    let event_source_ids = store
        .events_for_session(session_id)
        .unwrap()
        .into_iter()
        .map(|event| event.capture_source_id.unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        event_source_ids,
        BTreeSet::from([first_source_id, second_source_id])
    );

    let archive = store.export_archive().unwrap();
    assert_eq!(archive.files_touched.len(), 2);
    let touched_source_ids = archive
        .files_touched
        .iter()
        .map(|file| file.source_id.unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        touched_source_ids,
        BTreeSet::from([first_source_id, second_source_id])
    );
    for file in archive.files_touched {
        let source_id = file.source_id.unwrap();
        assert_eq!(
            file.event_id,
            Some(provider_source_event_uuid(source_id, 0))
        );
    }
}

pub(super) fn provider_collision_capture(
    provider: CaptureProvider,
    provider_session_id: &str,
    source_format: &str,
    raw_source_path: &str,
    occurred_at: DateTime<Utc>,
) -> ProviderCaptureEnvelope {
    ProviderCaptureEnvelope {
        schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
        provider,
        source: ProviderSourceEnvelope {
            source_format: source_format.to_owned(),
            machine_id: "test-machine".to_owned(),
            observed_at: occurred_at,
            raw_source_path: Some(raw_source_path.to_owned()),
            raw_retention: ProviderRawRetention::PathReference,
            redaction_boundary: ProviderRedactionBoundary::BeforeExport,
            trust: ProviderSourceTrust::ProviderExport,
            fidelity: Fidelity::Imported,
            cursor: None,
            idempotency_key: Some(format!(
                "provider-source:{}:{}:{}",
                provider.as_str(),
                source_format,
                provider_session_id
            )),
            metadata: json!({}),
        },
        session: ProviderSessionEnvelope {
            provider_session_id: provider_session_id.to_owned(),
            parent_provider_session_id: None,
            root_provider_session_id: None,
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".to_owned()),
            is_primary: true,
            status: SessionStatus::Imported,
            started_at: occurred_at,
            ended_at: None,
            cwd: Some("/workspace/example".to_owned()),
            fidelity: Fidelity::Imported,
            idempotency_key: Some(format!(
                "provider-session:{}:{}",
                provider.as_str(),
                provider_session_id
            )),
            artifacts: Vec::new(),
            metadata: json!({}),
        },
        event: Some(ProviderEventEnvelope {
            provider_event_index: 0,
            provider_event_hash: None,
            cursor: None,
            event_type: EventType::Message,
            role: Some(EventRole::User),
            occurred_at,
            fidelity: Fidelity::Imported,
            redaction_state: RedactionState::LocalPreview,
            idempotency_key: Some(format!(
                "provider-event:{}:{}:0",
                provider.as_str(),
                provider_session_id
            )),
            artifacts: Vec::new(),
            payload: json!({"text": "same provider event payload"}),
            metadata: json!({}),
        }),
    }
}

pub(super) fn provider_collision_file_touch(
    provider: CaptureProvider,
    provider_session_id: &str,
    source_format: &str,
    raw_source_path: &str,
    occurred_at: DateTime<Utc>,
) -> ProviderFileTouchedEnvelope {
    ProviderFileTouchedEnvelope {
        provider,
        provider_session_id: provider_session_id.to_owned(),
        provider_touch_index: 0,
        provider_event_index: Some(0),
        raw_source_path: Some(raw_source_path.to_owned()),
        path: "src/lib.rs".to_owned(),
        change_kind: Some(FileChangeKind::Modified),
        old_path: None,
        line_count_delta: Some(1),
        confidence: Confidence::Explicit,
        occurred_at,
        source_format: source_format.to_owned(),
        metadata: json!({}),
    }
}
