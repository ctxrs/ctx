mod support;

use std::{fs, path::Path};

use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, CoreRecord, EventIdentityInput,
    McpToolCallAttribution, NativeItemKey, NativeSessionKey, ScannedSourceCounts,
    SessionIdentityInput, SourceAnchor, SourceKey, SourceObservation, TypedKey,
};
use ctx_history_index::{GenerationWriter, VerifiedIndex, WriterOptions};
use rusqlite::Connection;
use serde_json::Value;
use support::*;

const SERVER_CANARY: &str = "zzsrvprivacycanary7e41qphx";
const TOOL_CANARY: &str = "zztoolprivacycanary9b26wkmd";
const BODY_ORACLE: &str = "provider neutral attribution privacy body oracle";

fn private_command<'a>(
    command: &'a mut Command,
    data_root: &Path,
    state: &Path,
    analytics_path: &Path,
) -> &'a mut Command {
    command
        .env("CTX_DATA_ROOT", data_root)
        .env("XDG_STATE_HOME", state)
        .env("LOCALAPPDATA", state)
        .env("CTX_ANALYTICS_ENABLED", "true")
        .env("CTX_ANALYTICS_ENDPOINT", file_url(analytics_path))
        .env("CTX_LOCAL_USAGE_ENABLED", "true")
        .env("CTX_DAEMON_ENABLED", "false")
        .env("CTX_UPGRADE_AUTO", "off")
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|part| part == needle)
}

fn assert_bytes_omit_canaries(label: &str, bytes: &[u8]) {
    for canary in [SERVER_CANARY, TOOL_CANARY] {
        assert!(
            !contains_bytes(bytes, canary.as_bytes()),
            "{label} leaked MCP attribution canary {canary}"
        );
    }
}

fn attribution_source() -> SourceKey {
    SourceKey::derive(
        "codex",
        "codex_session_jsonl_tree",
        "session",
        1,
        SourceAnchor::provider_native(
            "session-file",
            TypedKey::utf8("mcp-attribution-privacy.jsonl").unwrap(),
        )
        .unwrap(),
    )
    .unwrap()
}

fn attribution_certificate(source: &SourceKey) -> CertifiedSource {
    let observation = SourceObservation::new(source.clone(), "regular-file-v1", vec![1]).unwrap();
    CertifiedSource::certify(
        observation.clone(),
        observation,
        "mcp-attribution-privacy-test-v1",
        [1; 32],
        ScannedSourceCounts {
            complete_records: 1,
            retained_records: 1,
            indexed_documents: 1,
            certified_bytes: 1,
            ..ScannedSourceCounts::default()
        },
    )
    .unwrap()
}

fn seed_attributed_core(data_root: &Path) -> uuid::Uuid {
    let source = attribution_source();
    let native_session_key = NativeSessionKey::native_id(
        "session",
        TypedKey::utf8("mcp-attribution-privacy-session").unwrap(),
    )
    .unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source,
        logical_session_kind: "thread",
        native_session_key: &native_session_key,
    })
    .unwrap();
    let native_item_key = NativeItemKey::native_id("tool-result", TypedKey::U64(1)).unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: "tool-result",
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .unwrap();
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        session_id,
        source.clone(),
        1,
        "tool_result",
        "primary",
        true,
        "mcp-attribution-privacy-test-v1",
        BODY_ORACLE,
    )
    .unwrap();
    record.provider_session_id = Some("mcp-attribution-privacy-session".to_owned());
    record.native_event_id = Some(TypedKey::U64(1));
    record.mcp_tool_call = Some(McpToolCallAttribution {
        server: SERVER_CANARY.to_owned(),
        tool: TOOL_CANARY.to_owned(),
    });
    record.validate_contract().unwrap();

    let index_root = data_root.join("search").join("lexical");
    let mut writer = GenerationWriter::open(
        &index_root,
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 32 * 1024 * 1024,
        },
    )
    .unwrap()
    .into_writer()
    .unwrap();
    writer.begin_source(source.clone()).unwrap();
    writer.add_core_record(record).unwrap();
    writer
        .certify_source(attribution_certificate(&source))
        .unwrap();
    writer.commit(|_| true).unwrap();

    let stored = VerifiedIndex::open(&index_root)
        .unwrap()
        .core_record_by_id(event_id.as_uuid())
        .unwrap()
        .expect("seeded Selected Core record must be readable before sink checks");
    assert_eq!(
        stored.mcp_tool_call,
        Some(McpToolCallAttribution {
            server: SERVER_CANARY.to_owned(),
            tool: TOOL_CANARY.to_owned(),
        }),
        "privacy test must not run without real top-level MCP attribution"
    );
    event_id.as_uuid()
}

fn command_output_omits_canaries(label: &str, output: &std::process::Output) {
    assert_bytes_omit_canaries(&format!("{label} stdout"), &output.stdout);
    assert_bytes_omit_canaries(&format!("{label} stderr"), &output.stderr);
}

#[test]
fn mcp_attribution_canaries_stay_out_of_search_analytics_usage_and_diagnostics() {
    let temp = tempdir();
    let data_root = temp.path().join("data");
    let state = temp.path().join("state");
    let analytics_path = temp.path().join("analytics.jsonl");
    let attributed_event_id = seed_attributed_core(&data_root);

    let body_search = private_command(
        ctx(&temp).args(["search", BODY_ORACLE, "--refresh", "off", "--format=json"]),
        &data_root,
        &state,
        &analytics_path,
    )
    .output()
    .unwrap();
    assert!(body_search.status.success());
    command_output_omits_canaries("body search", &body_search);
    let body_packet: Value = serde_json::from_slice(&body_search.stdout).unwrap();
    assert_eq!(body_packet["results"].as_array().unwrap().len(), 1);
    assert_eq!(
        body_packet["results"][0]["ctx_event_id"],
        attributed_event_id.to_string()
    );

    for canary in [SERVER_CANARY, TOOL_CANARY] {
        let search = private_command(
            ctx(&temp).args(["search", canary, "--refresh", "off", "--format=json"]),
            &data_root,
            &state,
            &analytics_path,
        )
        .output()
        .unwrap();
        assert!(search.status.success());
        assert_bytes_omit_canaries("attribution search stderr", &search.stderr);
        let packet: Value = serde_json::from_slice(&search.stdout).unwrap();
        let results = &packet["results"];
        assert_bytes_omit_canaries(
            "attribution search results",
            serde_json::to_string(results).unwrap().as_bytes(),
        );
        assert!(
            results.as_array().unwrap().is_empty(),
            "MCP attribution affected search matching or ranking: {packet:#}"
        );
    }

    let doctor = private_command(
        ctx(&temp).args(["doctor", "--format=json"]),
        &data_root,
        &state,
        &analytics_path,
    )
    .output()
    .unwrap();
    assert!(doctor.status.success());
    assert!(
        !doctor.stdout.is_empty(),
        "doctor diagnostic artifact was not emitted"
    );
    command_output_omits_canaries("doctor diagnostic", &doctor);
    let doctor_packet: Value = serde_json::from_slice(&doctor.stdout).unwrap();
    assert_eq!(doctor_packet["schema_version"], 1);
    assert!(doctor_packet["findings"].is_array());

    let error = private_command(
        ctx(&temp).args(["docs", "show", "missing-attribution-private-topic"]),
        &data_root,
        &state,
        &analytics_path,
    )
    .output()
    .unwrap();
    assert!(!error.status.success());
    assert!(!error.stderr.is_empty(), "error diagnostic was not emitted");
    command_output_omits_canaries("error diagnostic", &error);

    let fixture = provider_history_fixture("codex-sessions");
    let progress = private_command(
        ctx(&temp).args([
            "import",
            "--provider",
            "codex",
            "--path",
            &fixture,
            "--no-daemon",
            "--progress",
            "json",
            "--format=json",
        ]),
        &data_root,
        &state,
        &analytics_path,
    )
    .output()
    .unwrap();
    let progress_stderr = String::from_utf8_lossy(&progress.stderr);
    assert!(
        !progress.status.success(),
        "no-daemon import unexpectedly succeeded"
    );
    assert!(
        progress_stderr.contains("the ctx daemon is unavailable"),
        "expected import error artifact was not emitted: {progress_stderr}"
    );
    assert!(
        progress_stderr.contains(r#""type":"ctx_progress""#),
        "JSON progress artifact was not emitted: {}",
        progress_stderr
    );
    command_output_omits_canaries("progress diagnostics", &progress);

    assert!(analytics_path.is_file(), "analytics sink was not exercised");
    let analytics = fs::read(&analytics_path).unwrap();
    assert!(!analytics.is_empty(), "analytics sink emitted no events");
    assert_bytes_omit_canaries("analytics sink", &analytics);
    let analytics_events = read_analytics_events(&analytics_path);
    assert!(analytics_events.len() >= 6, "{analytics_events:#?}");
    let operations = analytics_events
        .iter()
        .map(|event| analytics_cli_event(event)["operation"].as_str().unwrap())
        .collect::<Vec<_>>();
    for expected in ["search", "doctor", "docs", "import"] {
        assert!(
            operations.contains(&expected),
            "missing analytics event {expected}"
        );
    }

    let usage_path = data_root.join("usage.sqlite");
    assert!(usage_path.is_file(), "local usage sink was not exercised");
    let usage = Connection::open(&usage_path).unwrap();
    let usage_rows: i64 = usage
        .query_row("SELECT COUNT(*) FROM daily_usage", [], |row| row.get(0))
        .unwrap();
    assert!(
        usage_rows >= 4,
        "local usage emitted only {usage_rows} operation rows"
    );
    for operation in ["search", "doctor", "docs", "import"] {
        let calls: i64 = usage
            .query_row(
                "SELECT SUM(calls) FROM daily_usage WHERE operation = ?1",
                [operation],
                |row| row.get(0),
            )
            .unwrap();
        assert!(calls >= 1, "local usage omitted {operation}");
    }
    drop(usage);
    for name in ["usage.sqlite", "usage.sqlite-wal", "usage.sqlite-shm"] {
        if data_root.join(name).exists() {
            assert_bytes_omit_canaries(name, &fs::read(data_root.join(name)).unwrap());
        }
    }

    let stored = VerifiedIndex::open(data_root.join("search/lexical"))
        .unwrap()
        .core_record_by_id(attributed_event_id)
        .unwrap()
        .unwrap();
    assert_eq!(stored.mcp_tool_call.as_ref().unwrap().server, SERVER_CANARY);
    assert_eq!(stored.mcp_tool_call.as_ref().unwrap().tool, TOOL_CANARY);
}
