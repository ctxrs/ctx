use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
};
use tempfile::TempDir;

pub(crate) struct HistorySourcePluginFixture {
    pub(crate) manifest_dir: PathBuf,
    pub(crate) run_marker: PathBuf,
}

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
    #[allow(clippy::uninlined_format_args)]
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

pub(crate) fn python_command() -> String {
    std::env::var("PYTHON").unwrap_or_else(|_| "python3".to_owned())
}

pub(crate) fn write_raw_history_source_plugin(
    temp: &TempDir,
    provider: &str,
    script_body: &str,
) -> HistorySourcePluginFixture {
    write_raw_history_source_plugin_with_options(temp, provider, script_body, false, None)
}

pub(crate) fn write_raw_history_source_plugin_with_options(
    temp: &TempDir,
    provider: &str,
    script_body: &str,
    enabled: bool,
    refresh: Option<&str>,
) -> HistorySourcePluginFixture {
    write_raw_history_source_plugin_with_options_and_timeout(
        temp,
        provider,
        script_body,
        enabled,
        refresh,
        10,
    )
}

pub(crate) fn write_raw_history_source_plugin_with_options_and_timeout(
    temp: &TempDir,
    provider: &str,
    script_body: &str,
    enabled: bool,
    refresh: Option<&str>,
    timeout_seconds: u64,
) -> HistorySourcePluginFixture {
    let manifest_dir = temp.path().join("history-plugins").join(provider);
    fs::create_dir_all(&manifest_dir).unwrap();
    let script = manifest_dir.join("export.py");
    let run_marker = manifest_dir.join("ran");
    fs::write(&script, script_body).unwrap();
    let mut source_manifest = json!({
        "id": "default",
        "provider_key": provider,
        "source_id": "default",
        "source_format": format!("{provider}-history-v1"),
        "enabled": enabled,
        "command": [python_command(), script.display().to_string()],
        "timeout_seconds": timeout_seconds
    });
    if let Some(refresh) = refresh {
        source_manifest["refresh"] = json!(refresh);
    }
    let manifest = json!({
        "schema_version": 1,
        "name": provider,
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
