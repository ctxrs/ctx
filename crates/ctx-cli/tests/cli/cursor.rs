#[allow(unused_imports)]
use super::*;

pub(crate) fn write_history_source_plugin(
    temp: &TempDir,
    provider: &str,
    enabled: bool,
    cursor_log: Option<&Path>,
) -> HistorySourcePluginFixture {
    write_history_source_plugin_with_refresh(temp, provider, enabled, None, cursor_log)
}

pub(crate) fn write_history_source_plugin_with_refresh(
    temp: &TempDir,
    provider: &str,
    enabled: bool,
    refresh: Option<&str>,
    cursor_log: Option<&Path>,
) -> HistorySourcePluginFixture {
    write_history_source_plugin_at_with_refresh(
        &temp.path().join("history-plugins"),
        provider,
        enabled,
        refresh,
        cursor_log,
    )
}

pub(crate) fn write_history_source_plugin_at(
    root: &Path,
    provider: &str,
    enabled: bool,
    cursor_log: Option<&Path>,
) -> HistorySourcePluginFixture {
    write_history_source_plugin_at_with_refresh(root, provider, enabled, None, cursor_log)
}

pub(crate) fn write_history_source_plugin_at_with_refresh(
    root: &Path,
    provider: &str,
    enabled: bool,
    refresh: Option<&str>,
    cursor_log: Option<&Path>,
) -> HistorySourcePluginFixture {
    let manifest_dir = root.join(provider);
    fs::create_dir_all(&manifest_dir).unwrap();
    let script = manifest_dir.join("export.py");
    let run_marker = manifest_dir.join("ran");
    let run_marker_json = Value::String(run_marker.display().to_string());
    let cursor_log_py = cursor_log
        .map(|path| {
            serde_json::to_string(&path.display().to_string())
                .expect("cursor log path is JSON-serializable")
        })
        .unwrap_or_else(|| "None".to_owned());
    let script_body = format!(
        r#"#!/usr/bin/env python3
import json
import os
import pathlib
import sys

provider = sys.argv[1]
source_id = os.environ["CTX_HISTORY_SOURCE_ID"]
provider_key = os.environ["CTX_HISTORY_PROVIDER_KEY"]
source_format = os.environ["CTX_HISTORY_SOURCE_FORMAT"]
cursor_stream = os.environ["CTX_HISTORY_CURSOR_STREAM"]
cursor_inline = os.environ.get("CTX_HISTORY_CURSOR")
cursor_file = os.environ.get("CTX_HISTORY_CURSOR_FILE")
pathlib.Path({run_marker_json}).write_text("ran\n")
cursor_log = {cursor_log_py}
cursor_text = cursor_inline
if not cursor_text and cursor_file:
    cursor_text = pathlib.Path(cursor_file).read_text()
if cursor_log and cursor_text:
    file_text = pathlib.Path(cursor_file).read_text() if cursor_file else ""
    with open(cursor_log, "a", encoding="utf-8") as handle:
        handle.write(cursor_text + "\n")
        handle.write("cursor_file=" + file_text + "\n")

cursor_shapes = {{
    "dorkos": {{"files": {{"/tmp/dorkos.jsonl": {{"offset": 128, "size": 128, "mtimeMs": 1}}}}}},
    "disabled-dorkos": {{"files": {{"/tmp/disabled-dorkos.jsonl": {{"offset": 128, "size": 128, "mtimeMs": 1}}}}}},
    "openclaw": {{"backend": "openclaw-file", "transcripts": {{"/tmp/openclaw.jsonl": {{"offset": 256, "size": 256, "lastRecordId": "rec-1"}}}}}},
    "hermes": {{"message_id": 7}},
    "nanoclaw": {{"sessions": {{"sess-1": 42}}}},
}}
next_cursor = cursor_shapes[provider]
if cursor_text:
    if provider == "hermes":
        next_cursor = {{"message_id": 8}}
    elif provider == "nanoclaw":
        next_cursor = {{"sessions": {{"sess-1": 44}}}}
    elif provider == "openclaw":
        next_cursor = {{"backend": "openclaw-file", "transcripts": {{"/tmp/openclaw.jsonl": {{"offset": 512, "size": 512, "lastRecordId": "rec-2"}}}}}}
    else:
        next_cursor = {{"files": {{"/tmp/" + provider + ".jsonl": {{"offset": 256, "size": 256, "mtimeMs": 2}}}}}}

event_index = 1 if cursor_text else 0
phase = "incremental" if cursor_text else "initial"
observed = "2026-07-01T12:00:00Z"
cursor = {{
    "after": {{
        "stream": cursor_stream,
        "cursor": json.dumps(next_cursor, separators=(",", ":")),
        "observed_at": observed,
    }}
}}
if cursor_text:
    cursor["before"] = {{
        "stream": cursor_stream,
        "cursor": cursor_text,
        "observed_at": observed,
    }}

records = [
    {{"record_type": "manifest", "schema_version": "ctx-history-jsonl-v1", "producer": provider + "-fixture"}},
    {{"record_type": "source", "source_id": source_id, "provider_key": provider_key, "source_format": source_format, "observed_at": observed, "cursor": cursor, "metadata": {{"fixture_provider": provider}}}},
    {{"record_type": "session", "source_id": source_id, "session_id": provider + "-session", "started_at": "2026-07-01T11:59:00Z", "cwd": "/workspace/" + provider, "agent_type": "primary", "is_primary": True, "status": "completed"}},
    {{"record_type": "event", "source_id": source_id, "session_id": provider + "-session", "event_index": event_index, "event_id": provider + "-event-" + str(event_index), "native_cursor": phase, "event_type": "message", "role": "assistant", "occurred_at": observed, "payload": {{"text": provider + " plugin " + phase + " marker"}}, "preview": provider + " plugin " + phase + " marker"}},
]
for record in records:
    print(json.dumps(record, separators=(",", ":")))
"#,
        run_marker_json = run_marker_json,
        cursor_log_py = cursor_log_py
    );
    fs::write(&script, script_body).unwrap();
    let mut source_manifest = json!({
        "id": "default",
        "provider_key": provider,
        "source_id": "default",
        "source_format": format!("{provider}-history-v1"),
        "enabled": enabled,
        "command": [python_command(), script.display().to_string(), provider],
        "timeout_seconds": 10
    });
    if let Some(refresh) = refresh {
        source_manifest["refresh"] = json!(refresh);
    }
    let manifest = json!({
        "schema_version": 1,
        "name": provider,
        "display_name": format!("{provider} history"),
        "version": "0.1.0",
        "history_sources": [source_manifest]
    });
    fs::write(
        manifest_dir.join("ctx-history-plugin.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    HistorySourcePluginFixture {
        manifest_dir,
        run_marker,
    }
}

#[test]
pub(crate) fn history_source_plugin_reset_requires_fresh_after_cursor() {
    let temp = tempdir();
    let script = r#"#!/usr/bin/env python3
import json
records = [
  {"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"},
  {"record_type":"source","source_id":"default","provider_key":"nocursor","source_format":"nocursor-history-v1"},
  {"record_type":"session","source_id":"default","session_id":"run","started_at":"2026-07-01T12:00:00Z"},
]
for record in records:
    print(json.dumps(record))
"#;
    let plugin = write_raw_history_source_plugin(&temp, "nocursor", script);

    let stderr = failure_stderr(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
            .args([
                "import",
                "--history-source",
                "nocursor/default",
                "--reset-cursor",
                "--progress",
                "none",
            ]),
    );

    assert!(stderr.contains("source.cursor.after"), "{stderr}");
    let conn = Connection::open(temp.path().join("work.sqlite")).unwrap();
    assert_eq!(
        sqlite_count(&conn, "SELECT COUNT(*) FROM history_records"),
        0
    );
}

#[test]
pub(crate) fn large_history_source_plugin_cursor_uses_cursor_file_without_inline_env() {
    let temp = tempdir();
    let log = temp.path().join("large-cursor.log");
    let log_json = serde_json::to_string(&log.display().to_string()).unwrap();
    let script = format!(
        r#"#!/usr/bin/env python3
import json
import os
import pathlib

cursor_file = os.environ.get("CTX_HISTORY_CURSOR_FILE")
inline = os.environ.get("CTX_HISTORY_CURSOR")
cursor_text = pathlib.Path(cursor_file).read_text() if cursor_file else inline
if cursor_text:
    with open({log_json}, "a", encoding="utf-8") as handle:
        handle.write("inline=" + ("1" if inline else "0") + "\n")
        handle.write("file_len=" + str(len(cursor_text)) + "\n")
next_cursor = "x" * 9000 if not cursor_text else "done"
observed = "2026-07-01T12:00:00Z"
records = [
  {{"record_type":"manifest","schema_version":"ctx-history-jsonl-v1"}},
  {{"record_type":"source","source_id":"default","provider_key":"largecursor","source_format":"largecursor-history-v1","cursor":{{"after":{{"stream":os.environ["CTX_HISTORY_CURSOR_STREAM"],"cursor":next_cursor,"observed_at":observed}}}}}},
  {{"record_type":"session","source_id":"default","session_id":"run","started_at":"2026-07-01T12:00:00Z"}},
  {{"record_type":"event","source_id":"default","session_id":"run","event_index":1 if cursor_text else 0,"event_type":"message","role":"assistant","occurred_at":observed,"preview":"large cursor marker"}},
]
for record in records:
    print(json.dumps(record))
"#
    );
    let plugin = write_raw_history_source_plugin(&temp, "largecursor", &script);

    json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
            .args([
                "import",
                "--history-source",
                "largecursor/default",
                "--json",
                "--progress",
                "none",
            ]),
    );
    json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
            .args([
                "import",
                "--history-source",
                "largecursor/default",
                "--json",
                "--progress",
                "none",
            ]),
    );

    let log = fs::read_to_string(log).unwrap();
    assert!(log.contains("inline=0"), "{log}");
    assert!(log.contains("file_len=9000"), "{log}");
}

#[test]
pub(crate) fn import_history_source_plugin_is_searchable_and_receives_cursor() {
    let temp = tempdir();
    let cursor_log = temp.path().join("cursor-log.txt");
    let plugin = write_history_source_plugin(&temp, "hermes", false, Some(&cursor_log));

    let first = json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
            .args([
                "import",
                "--history-source",
                "hermes/default",
                "--resume",
                "--json",
                "--progress",
                "none",
            ]),
    );
    assert_eq!(first["totals"]["imported_sessions"], 1);
    assert_eq!(first["totals"]["imported_events"], 1);
    assert_eq!(first["sources"][0]["history_source"], "hermes/default");

    let initial = json_output(ctx(&temp).args([
        "search",
        "hermes plugin initial marker",
        "--provider",
        "custom",
        "--refresh",
        "off",
        "--json",
    ]));
    assert!(
        !initial["results"].as_array().unwrap().is_empty(),
        "initial plugin import was not searchable: {initial:#}"
    );
    let initial_by_history_source = json_output(ctx(&temp).args([
        "search",
        "hermes plugin initial marker",
        "--history-source",
        "hermes/default",
        "--refresh",
        "off",
        "--json",
    ]));
    let source_filtered_result = &initial_by_history_source["results"][0];
    assert_eq!(source_filtered_result["provider"], "custom");
    assert_eq!(source_filtered_result["history_source"], "hermes/default");
    assert_eq!(source_filtered_result["history_source_plugin"], "hermes");
    assert_eq!(source_filtered_result["provider_key"], "hermes");
    assert_eq!(source_filtered_result["source_id"], "default");
    assert_eq!(source_filtered_result["source_format"], "hermes-history-v1");

    let second = json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
            .args([
                "import",
                "--history-source",
                "hermes/default",
                "--json",
                "--progress",
                "none",
            ]),
    );
    assert_eq!(second["totals"]["imported_sessions"], 0);
    assert_eq!(second["totals"]["imported_events"], 1);
    assert_eq!(second["resume"], false);
    assert_eq!(second["resume_mode"], "normal_scan");

    let incremental = json_output(ctx(&temp).args([
        "search",
        "hermes plugin incremental marker",
        "--provider",
        "custom",
        "--refresh",
        "off",
        "--json",
    ]));
    assert!(
        !incremental["results"].as_array().unwrap().is_empty(),
        "incremental plugin import was not searchable: {incremental:#}"
    );
    let cursor_log = fs::read_to_string(cursor_log).unwrap();
    assert!(cursor_log.contains(r#""message_id":7"#), "{cursor_log}");
    assert!(cursor_log.contains("cursor_file="), "{cursor_log}");
}

#[test]
pub(crate) fn search_refresh_auto_runs_enabled_auto_history_source_plugins_incrementally() {
    let temp = tempdir();
    let cursor_log = temp.path().join("cursor-log.txt");
    let plugin = write_history_source_plugin_with_refresh(
        &temp,
        "hermes",
        true,
        Some("auto"),
        Some(&cursor_log),
    );

    let initial = json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
            .args([
                "search",
                "hermes plugin initial marker",
                "--provider",
                "custom",
                "--json",
            ]),
    );
    assert_eq!(initial["freshness"]["mode"], "auto");
    assert_eq!(initial["freshness"]["status"], "completed");
    assert_eq!(initial["freshness"]["source_count"], 1);
    assert_eq!(initial["freshness"]["totals"]["imported_sources"], 1);
    assert_eq!(initial["freshness"]["totals"]["imported_sessions"], 1);
    assert_eq!(initial["freshness"]["totals"]["imported_events"], 1);
    assert!(
        !initial["results"].as_array().unwrap().is_empty(),
        "initial plugin refresh was not searchable before query: {initial:#}"
    );
    assert!(plugin.run_marker.exists());

    fs::remove_file(&plugin.run_marker).unwrap();
    let incremental = json_output(
        ctx(&temp)
            .env("CTX_HISTORY_PLUGIN_PATH", &plugin.manifest_dir)
            .args([
                "search",
                "hermes plugin incremental marker",
                "--provider",
                "custom",
                "--json",
            ]),
    );
    assert_eq!(incremental["freshness"]["mode"], "auto");
    assert_eq!(incremental["freshness"]["status"], "completed");
    assert_eq!(incremental["freshness"]["source_count"], 1);
    assert_eq!(incremental["freshness"]["totals"]["imported_sources"], 1);
    assert_eq!(incremental["freshness"]["totals"]["imported_events"], 1);
    assert!(
        !incremental["results"].as_array().unwrap().is_empty(),
        "incremental plugin refresh was not searchable before query: {incremental:#}"
    );
    assert!(plugin.run_marker.exists());

    let cursor_log = fs::read_to_string(cursor_log).unwrap();
    assert!(cursor_log.contains(r#""message_id":7"#), "{cursor_log}");
    assert!(cursor_log.contains("cursor_file="), "{cursor_log}");
}

pub(crate) fn install_default_cursor_fixture(temp: &TempDir, query: &str) {
    let source = PathBuf::from(write_native_cursor_fixture(temp, query));
    copy_dir_all(&source, &temp.path().join(".cursor").join("projects"));
}

pub(crate) fn write_native_cursor_fixture(temp: &TempDir, query: &str) -> String {
    let root = temp
        .path()
        .join("native-cursor/projects/sanitized-workspace/agent-transcripts/cursor-cli-native");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("cursor-cli-native.jsonl"),
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
    temp.path()
        .join("native-cursor/projects")
        .to_str()
        .unwrap()
        .to_owned()
}
