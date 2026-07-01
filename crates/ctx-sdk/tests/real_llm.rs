use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use ctx_sdk::{
    CaptureProvider, CtxClient, EventRole, EventType, ImportOptions, SearchOptions,
    ShowSessionOptions, TranscriptMode,
};
use rusqlite::Connection;
use serde_json::{json, Value};
use uuid::Uuid;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/provider-history")
        .join(name)
}

#[test]
fn real_llm_history_fixtures_import_and_search_through_sdk() {
    let temp = tempfile::tempdir().unwrap();
    let client = CtxClient::builder()
        .data_root(temp.path())
        .home_dir(temp.path())
        .build()
        .unwrap();

    let codex = client
        .import_path(
            CaptureProvider::Codex,
            fixture("codex-sessions"),
            ImportOptions::default(),
        )
        .unwrap();
    let pi = client
        .import_path(
            CaptureProvider::Pi,
            fixture("pi-session.jsonl"),
            ImportOptions::default(),
        )
        .unwrap();

    assert_eq!(codex.totals.failed_sources, 0);
    assert_eq!(pi.totals.failed_sources, 0);
    assert!(codex.totals.imported_sessions > 0);
    assert!(pi.totals.imported_sessions > 0);

    let codex_packet = client
        .search(
            "onboarding",
            SearchOptions::default()
                .provider(CaptureProvider::Codex)
                .events()
                .limit(5),
        )
        .unwrap();
    let codex_hit = codex_packet
        .results
        .iter()
        .find(|result| result.provider == Some(CaptureProvider::Codex))
        .expect("Codex fixture should produce a provider-scoped event hit");
    assert!(codex_hit.provider_session_id.is_some());
    assert!(
        codex_hit.raw_source_path.is_some(),
        "SDK search should preserve source citations for LLM history"
    );
    let codex_session_id = codex_hit
        .session_id
        .expect("Codex hit should include session id");

    let codex_event = client
        .show_event(
            codex_hit
                .event_id
                .expect("event search should include event id"),
            Default::default(),
        )
        .unwrap();
    assert_eq!(codex_event.event.event_type, EventType::Message);
    assert!(matches!(
        codex_event.event.role,
        Some(EventRole::User | EventRole::Assistant | EventRole::System)
    ));
    let codex_location = client.locate_session(codex_session_id).unwrap();
    assert_eq!(
        codex_hit.provider_session_id,
        codex_location.session.external_session_id
    );

    let pi_packet = client
        .search(
            "provider metadata",
            SearchOptions::default()
                .provider(CaptureProvider::Pi)
                .events()
                .limit(5),
        )
        .unwrap();
    let pi_hit = pi_packet
        .results
        .iter()
        .find(|result| result.provider == Some(CaptureProvider::Pi))
        .expect("Pi fixture should produce a provider-scoped event hit");
    assert!(pi_hit.provider_session_id.is_some());

    let pi_transcript = client
        .show_session(
            pi_hit.session_id.expect("Pi hit should include session id"),
            ShowSessionOptions {
                mode: TranscriptMode::Full,
            },
        )
        .unwrap();
    assert!(
        pi_transcript
            .events
            .iter()
            .any(|event| event.role == Some(EventRole::Assistant)),
        "SDK should expose assistant LLM messages as typed events"
    );
    assert_eq!(
        pi_hit.provider_session_id,
        pi_transcript.session.external_session_id
    );
}

#[test]
fn native_provider_histories_import_and_search_through_sdk() {
    let temp = tempfile::tempdir().unwrap();
    let client = CtxClient::with_data_root(temp.path().join("ctx-data"));

    for case in native_provider_cases(&temp) {
        let report = client
            .import_path(case.provider, &case.path, ImportOptions::default())
            .unwrap_or_else(|error| panic!("{} import failed: {error}", case.name));
        assert_eq!(
            report.totals.failed_sources, 0,
            "{} should import without failed sources: {:?}",
            case.name, report
        );
        assert!(
            report.totals.imported_sessions >= 1,
            "{} should import at least one session",
            case.name
        );
        assert!(
            report.totals.imported_events >= 1,
            "{} should import at least one event",
            case.name
        );

        let packet = client
            .search(
                &case.query,
                SearchOptions::default()
                    .provider(case.provider)
                    .events()
                    .limit(5),
            )
            .unwrap();
        assert!(
            packet
                .results
                .iter()
                .any(|result| result.provider == Some(case.provider)),
            "{} should produce a provider-scoped search result for `{}`",
            case.name,
            case.query
        );
    }
}

struct NativeProviderCase {
    name: &'static str,
    provider: CaptureProvider,
    path: PathBuf,
    query: String,
}

fn native_provider_cases(temp: &tempfile::TempDir) -> Vec<NativeProviderCase> {
    vec![
        NativeProviderCase {
            name: "claude",
            provider: CaptureProvider::Claude,
            path: write_native_claude_fixture(temp, "claude-sdk-coverage").into(),
            query: "claude-sdk-coverage".to_owned(),
        },
        NativeProviderCase {
            name: "opencode",
            provider: CaptureProvider::OpenCode,
            path: write_native_opencode_fixture(temp, "opencode-sdk-coverage").into(),
            query: "opencode-sdk-coverage".to_owned(),
        },
        NativeProviderCase {
            name: "antigravity",
            provider: CaptureProvider::Antigravity,
            path: fixture("antigravity/v1/brain/agy-success"),
            query: "tiny README".to_owned(),
        },
        NativeProviderCase {
            name: "gemini",
            provider: CaptureProvider::Gemini,
            path: write_native_gemini_fixture(temp, "gemini-sdk-coverage").into(),
            query: "gemini-sdk-coverage".to_owned(),
        },
        NativeProviderCase {
            name: "cursor",
            provider: CaptureProvider::Cursor,
            path: write_native_cursor_fixture(temp, "cursor-sdk-coverage").into(),
            query: "cursor-sdk-coverage".to_owned(),
        },
        NativeProviderCase {
            name: "copilot-cli",
            provider: CaptureProvider::CopilotCli,
            path: write_native_copilot_fixture(temp, "copilot-sdk-coverage").into(),
            query: "copilot-sdk-coverage".to_owned(),
        },
        NativeProviderCase {
            name: "factory-ai-droid",
            provider: CaptureProvider::FactoryAiDroid,
            path: write_native_factory_droid_fixture(temp, "droid-sdk-coverage").into(),
            query: "droid-sdk-coverage".to_owned(),
        },
    ]
}

#[test]
#[ignore = "requires CTX_SDK_LIVE_OPENAI=1, OPENAI_API_KEY, CTX_SDK_LIVE_OPENAI_MODEL, curl, and network access"]
fn live_openai_response_imports_and_searches_without_cli() {
    if env::var("CTX_SDK_LIVE_OPENAI").ok().as_deref() != Some("1") {
        eprintln!("skipping live LLM test; set CTX_SDK_LIVE_OPENAI=1 to opt in");
        return;
    }

    let api_key = env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY is required");
    let model =
        env::var("CTX_SDK_LIVE_OPENAI_MODEL").expect("CTX_SDK_LIVE_OPENAI_MODEL is required");
    let base_url =
        env::var("OPENAI_BASE_URL").unwrap_or_else(|_| "https://api.openai.com/v1".to_owned());
    let nonce = format!("ctx-sdk-live-llm-{}", Uuid::new_v4());

    let assistant_text = live_model_response_text(&base_url, &api_key, &model, &nonce);
    assert!(
        assistant_text.contains(&nonce),
        "live model response should echo nonce `{nonce}`, got: {assistant_text}"
    );

    let temp = tempfile::tempdir().unwrap();
    let history = write_live_codex_history(temp.path(), &model, &nonce, &assistant_text).unwrap();
    let client = CtxClient::with_data_root(temp.path().join("ctx-data"));

    let report = client
        .import_path(CaptureProvider::Codex, history, ImportOptions::default())
        .unwrap();
    assert_eq!(report.totals.failed_sources, 0);
    assert_eq!(report.totals.imported_sessions, 1);

    let packet = client
        .search(
            &nonce,
            SearchOptions::default()
                .provider(CaptureProvider::Codex)
                .events()
                .limit(5),
        )
        .unwrap();
    let hit = packet
        .results
        .first()
        .expect("live LLM text should be searchable after SDK import");
    let transcript = client
        .show_session(
            hit.session_id
                .expect("live LLM hit should include session id"),
            ShowSessionOptions {
                mode: TranscriptMode::Full,
            },
        )
        .unwrap();

    assert!(
        transcript
            .events
            .iter()
            .any(|event| event.role == Some(EventRole::Assistant)
                && event.payload.to_string().contains(&nonce)),
        "typed transcript should include the live assistant response"
    );
}

fn live_model_response_text(base_url: &str, api_key: &str, model: &str, nonce: &str) -> String {
    match env::var("CTX_SDK_LIVE_OPENAI_API").as_deref() {
        Ok("chat") | Ok("chat_completions") => {
            openai_chat_completion_text(base_url, api_key, model, nonce)
        }
        _ => openai_response_text(base_url, api_key, model, nonce),
    }
}

fn openai_response_text(base_url: &str, api_key: &str, model: &str, nonce: &str) -> String {
    let request = json!({
        "model": model,
        "input": exact_echo_prompt(nonce),
    });
    let response = post_json(
        &format!("{}/responses", base_url.trim_end_matches('/')),
        api_key,
        &request,
        "OpenAI live request",
    );
    extract_response_text(&response).expect("OpenAI response should contain text output")
}

fn openai_chat_completion_text(base_url: &str, api_key: &str, model: &str, nonce: &str) -> String {
    let request = json!({
        "model": model,
        "messages": [{"role": "user", "content": exact_echo_prompt(nonce)}],
        "temperature": 0,
        "max_tokens": 512,
        "stream": false,
    });
    let response = post_json(
        &format!("{}/chat/completions", base_url.trim_end_matches('/')),
        api_key,
        &request,
        "OpenAI-compatible chat request",
    );
    extract_response_text(&response).expect("chat completion response should contain text output")
}

fn post_json(url: &str, api_key: &str, request: &Value, label: &str) -> Value {
    let mut body = tempfile::NamedTempFile::new().expect("request body tempfile");
    write!(body, "{request}").expect("write request body");

    let mut child = Command::new("curl")
        .arg("--config")
        .arg("-")
        .arg("--data-binary")
        .arg(format!("@{}", body.path().display()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to execute curl for live LLM test");

    {
        let mut stdin = child.stdin.take().expect("curl stdin");
        write!(
            stdin,
            "silent\nshow-error\nfail-with-body\nmax-time = 60\nrequest = \"POST\"\nurl = \"{}\"\nheader = \"Authorization: Bearer {}\"\nheader = \"Content-Type: application/json\"\n",
            url, api_key
        )
        .expect("write curl config");
    }

    let output = child.wait_with_output().expect("curl should finish");
    if !output.status.success() {
        panic!(
            "{label} failed with status {:?}: stderr={} body={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
    }

    serde_json::from_slice(&output.stdout).expect("live LLM response should be JSON")
}

fn exact_echo_prompt(nonce: &str) -> String {
    format!("Reply with exactly this text and no extra words: {nonce}")
}

fn extract_response_text(value: &Value) -> Option<String> {
    if let Some(text) = value.get("output_text").and_then(Value::as_str) {
        return Some(text.to_owned());
    }
    if let Some(text) = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
    {
        return Some(text.to_owned());
    }
    if let Some(text) = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("reasoning"))
        .and_then(Value::as_str)
    {
        return Some(text.to_owned());
    }
    let mut parts = Vec::new();
    collect_text_fields(value, &mut parts);
    let text = parts.join("\n").trim().to_owned();
    (!text.is_empty()).then_some(text)
}

fn collect_text_fields(value: &Value, parts: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            if let Some(text) = map.get("text").and_then(Value::as_str) {
                parts.push(text.to_owned());
            }
            for child in map.values() {
                collect_text_fields(child, parts);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_text_fields(item, parts);
            }
        }
        _ => {}
    }
}

fn write_live_codex_history(
    root: &Path,
    model: &str,
    nonce: &str,
    assistant_text: &str,
) -> io::Result<PathBuf> {
    let path = root.join("codex-live").join("2026").join("07").join("01");
    fs::create_dir_all(&path)?;
    let file = path.join("live.jsonl");
    let session_id = format!("live-openai-{nonce}");
    let lines = [
        json!({
            "timestamp": "2026-07-01T00:00:00.000Z",
            "type": "session_meta",
            "payload": {
                "id": session_id,
                "timestamp": "2026-07-01T00:00:00.000Z",
                "cwd": "/tmp/ctx-sdk-live-openai",
                "originator": "ctx-sdk-live-test",
                "cli_version": "live-test",
                "source": "cli",
                "model_provider": "openai",
                "model": model,
            }
        }),
        json!({
            "timestamp": "2026-07-01T00:00:01.000Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": format!("Echo {nonce}")}]
            }
        }),
        json!({
            "timestamp": "2026-07-01T00:00:02.000Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": assistant_text}]
            }
        }),
    ];
    let body = lines
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&file, format!("{body}\n"))?;
    Ok(path)
}

fn write_native_claude_fixture(temp: &tempfile::TempDir, query: &str) -> PathBuf {
    let root = temp.path().join("native-claude/projects/-workspace");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("claude-sdk-native.jsonl"),
        format!(
            "{}\n{}\n",
            json!({
                "sessionId": "claude-sdk-native",
                "timestamp": "2026-06-24T12:00:00Z",
                "cwd": "/workspace",
                "version": "test",
                "type": "user",
                "message": {"role": "user", "content": [{"type": "text", "text": query}]},
                "uuid": "claude-sdk-native-user"
            }),
            json!({
                "sessionId": "claude-sdk-native",
                "timestamp": "2026-06-24T12:00:01Z",
                "cwd": "/workspace",
                "version": "test",
                "type": "assistant",
                "message": {"role": "assistant", "content": [{"type": "text", "text": "native import ok"}]},
                "uuid": "claude-sdk-native-assistant"
            })
        ),
    )
    .unwrap();
    temp.path().join("native-claude/projects")
}

fn write_native_opencode_fixture(temp: &tempfile::TempDir, query: &str) -> PathBuf {
    let path = temp.path().join("native-opencode.db");
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
            time_created integer not null,
            time_updated integer not null,
            data text not null
        );",
    )
    .unwrap();
    conn.execute(
        "insert into session (
            id, project_id, parent_id, slug, directory, title, version, permission,
            time_created, time_updated
        ) values (?1, 'project-1', null, 'native', '/workspace', 'native', '0.8.0',
            'default', 1782259200000, 1782259200000)",
        ["opencode-sdk-native"],
    )
    .unwrap();
    conn.execute(
        "insert into message values (?1, ?2, 1782259200000, 1782259200000, ?3)",
        [
            "opencode-sdk-native-user",
            "opencode-sdk-native",
            &format!(r#"{{"role":"user","time":{{"created":1782259200000}},"text":"{query}"}}"#),
        ],
    )
    .unwrap();
    path
}

fn write_native_gemini_fixture(temp: &tempfile::TempDir, query: &str) -> PathBuf {
    let chats = temp.path().join("native-gemini/tmp/project/chats");
    fs::create_dir_all(&chats).unwrap();
    fs::write(
        chats.join("session-native.jsonl"),
        format!(
            "{}\n{}\n",
            json!({
                "sessionId": "gemini-sdk-native",
                "startTime": "2026-06-24T12:00:00Z",
                "kind": "main",
                "directories": ["/workspace"]
            }),
            json!({
                "id": "gemini-sdk-native-user",
                "timestamp": "2026-06-24T12:00:01Z",
                "type": "user",
                "content": query
            })
        ),
    )
    .unwrap();
    temp.path().join("native-gemini")
}

fn write_native_cursor_fixture(temp: &tempfile::TempDir, query: &str) -> PathBuf {
    let root = temp
        .path()
        .join("native-cursor/projects/sanitized-workspace/agent-transcripts/cursor-sdk-native");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("cursor-sdk-native.jsonl"),
        format!(
            "{}\n{}\n",
            json!({
                "timestamp": "2026-06-24T12:00:00Z",
                "role": "user",
                "message": {"role": "user", "content": [{"type": "text", "text": query}]}
            }),
            json!({
                "timestamp": "2026-06-24T12:00:01Z",
                "role": "assistant",
                "message": {"role": "assistant", "content": [{"type": "text", "text": "native import ok"}]}
            })
        ),
    )
    .unwrap();
    temp.path().join("native-cursor/projects")
}

fn write_native_copilot_fixture(temp: &tempfile::TempDir, query: &str) -> PathBuf {
    let root = temp
        .path()
        .join("native-copilot/session-state/copilot-sdk-native");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("events.jsonl"),
        format!(
            "{}\n{}\n",
            json!({
                "id": "copilot-sdk-native-start",
                "timestamp": "2026-06-24T12:00:00Z",
                "type": "session.start",
                "data": {
                    "sessionId": "copilot-sdk-native",
                    "startTime": "2026-06-24T12:00:00Z",
                    "selectedModel": "gpt-5-mini",
                    "context": {"cwd": "/workspace"}
                }
            }),
            json!({
                "id": "copilot-sdk-native-user",
                "timestamp": "2026-06-24T12:00:01Z",
                "type": "user.message",
                "data": {"content": query}
            })
        ),
    )
    .unwrap();
    temp.path().join("native-copilot/session-state")
}

fn write_native_factory_droid_fixture(temp: &tempfile::TempDir, query: &str) -> PathBuf {
    let root = temp.path().join("native-droid/sessions/project");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("droid-sdk-native.jsonl"),
        format!(
            "{}\n{}\n",
            json!({
                "type": "session_start",
                "sessionId": "droid-sdk-native",
                "timestamp": "2026-06-24T12:00:00Z",
                "cwd": "/workspace",
                "model": "factory/droid"
            }),
            json!({
                "type": "message",
                "id": "droid-sdk-native-user",
                "timestamp": "2026-06-24T12:00:01Z",
                "role": "user",
                "content": [{"type": "text", "text": query}]
            })
        ),
    )
    .unwrap();
    temp.path().join("native-droid/sessions")
}
