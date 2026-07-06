#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn sql_reads_existing_store_and_supports_formats_and_input_sources() {
    let temp = tempdir();
    ctx(&temp)
        .args(["setup", "--catalog-only", "--progress", "none"])
        .assert()
        .success();

    let json = json_output(ctx(&temp).args(["sql", "SELECT 1 AS one, 'two' AS two", "--json"]));
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["item_type"], "sql_result");
    assert_eq!(json["read_only"], true);
    assert_eq!(json["share_safe"], false);
    assert_eq!(json["columns"], json!(["one", "two"]));
    assert_eq!(json["rows"], json!([[1, "two"]]));
    assert_eq!(json["returned_rows"], 1);

    let query_file = temp.path().join("query.sql");
    fs::write(&query_file, "SELECT 'a,b' AS value, 2 AS n").unwrap();
    let csv_output = ctx(&temp)
        .arg("sql")
        .arg("--file")
        .arg(&query_file)
        .args(["--format", "csv"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        String::from_utf8(csv_output).unwrap(),
        "value,n\n\"a,b\",2\n"
    );

    let oversized_file_stderr = failure_stderr(
        ctx(&temp)
            .arg("sql")
            .arg("--file")
            .arg(&query_file)
            .args(["--max-sql-bytes", "4"]),
    );
    assert!(
        oversized_file_stderr.contains("exceeds max_sql_bytes (4)"),
        "{oversized_file_stderr}"
    );

    let oversized_stdin_stderr = ctx(&temp)
        .args(["sql", "-", "--max-sql-bytes", "4"])
        .write_stdin("SELECT 1")
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let oversized_stdin_stderr = String::from_utf8(oversized_stdin_stderr).unwrap();
    assert!(
        oversized_stdin_stderr.contains("exceeds max_sql_bytes (4)"),
        "{oversized_stdin_stderr}"
    );

    let raw_output = ctx(&temp)
        .args(["sql", "-", "--format", "raw"])
        .write_stdin("SELECT 'abc' AS value")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(String::from_utf8(raw_output).unwrap(), "abc\n");
}

pub(crate) fn assert_analytics_properties_are_allowlisted(
    properties: &serde_json::Map<String, Value>,
) {
    let allowed = [
        "action",
        "all_sources",
        "analytics_client",
        "available_sources_bucket",
        "background",
        "catalog_only",
        "catalog_source_bytes_bucket",
        "cataloged_sessions_bucket",
        "citation_count_bucket",
        "db_size_bucket",
        "dry_run",
        "edges_imported_bucket",
        "event_results",
        "failed_bucket",
        "failed_sources_bucket",
        "failure_kind",
        "finding_count_bucket",
        "has_event_type_filter",
        "has_file_filter",
        "has_provider_filter",
        "has_query",
        "has_session_filter",
        "has_since_filter",
        "has_workspace_filter",
        "include_current_session",
        "include_subagents",
        "indexed_events_bucket",
        "indexed_items_bucket",
        "indexed_sessions_bucket",
        "indexed_sources_bucket",
        "install_manager",
        "initialized",
        "json_output",
        "limit_bucket",
        "native_sources_bucket",
        "output_format",
        "pending_sessions_bucket",
        "primary_only",
        "progress_mode",
        "provider_filter",
        "provider_lookup",
        "providers_detected_bucket",
        "query_duration_bucket",
        "query_length_bucket",
        "query_term_count_bucket",
        "refresh_duration_bucket",
        "render_duration_bucket",
        "result_count_bucket",
        "resume",
        "search_refresh_mode",
        "search_refresh_source_count_bucket",
        "search_refresh_status",
        "sessions_imported_bucket",
        "skipped_bucket",
        "source_files_bucket",
        "source_mode",
        "target_kind",
        "transcript_mode",
        "window_bucket",
        "writes_out_file",
        "zero_result",
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();

    for key in properties.keys() {
        assert!(
            allowed.contains(key.as_str()),
            "unexpected analytics property {key}: {properties:#?}"
        );
    }
}

#[test]
pub(crate) fn mcp_sql_tool_returns_structured_json_and_rejects_writes() {
    let temp = tempdir();
    ctx(&temp)
        .args(["setup", "--catalog-only", "--progress", "none"])
        .assert()
        .success();

    let responses = mcp_roundtrip(
        &temp,
        &[
            json!({
                "jsonrpc": "2.0",
                "id": "init",
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": { "name": "ctx-test", "version": "0" }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": "sql",
                "method": "tools/call",
                "params": {
                    "name": "sql",
                    "arguments": {
                        "sql": "SELECT COUNT(*) AS sessions FROM ctx_sessions",
                        "max_rows": 5
                    }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": "write",
                "method": "tools/call",
                "params": {
                    "name": "sql",
                    "arguments": {
                        "sql": "CREATE TABLE nope(x INTEGER)"
                    }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": "budget",
                "method": "tools/call",
                "params": {
                    "name": "sql",
                    "arguments": {
                        "sql": format!(
                            "SELECT {}",
                            (0..256).map(|index| format!("1 AS c{index}")).collect::<Vec<_>>().join(", ")
                        ),
                        "max_rows": 10000,
                        "max_columns": 256,
                        "max_value_bytes": 32
                    }
                }
            }),
        ],
    );

    let sql = &responses[1]["result"]["structuredContent"];
    assert_eq!(sql["item_type"], "sql_result");
    assert_eq!(sql["read_only"], true);
    assert_eq!(sql["share_safe"], false);
    assert_eq!(sql["columns"], json!(["sessions"]));
    assert_eq!(sql["rows"], json!([[0]]));

    let write = &responses[2]["result"];
    assert_eq!(write["isError"], true);
    assert!(write["structuredContent"]["error"]
        .as_str()
        .unwrap()
        .contains("SQL query must be read-only"));

    let budget = &responses[3]["result"];
    assert_eq!(budget["isError"], true);
    assert!(budget["structuredContent"]["error"]
        .as_str()
        .unwrap()
        .contains("SQL result preview budget"));
}

#[test]
pub(crate) fn mcp_show_session_caps_transcript_events() {
    let temp = tempdir();
    ctx(&temp)
        .args(["setup", "--catalog-only", "--progress", "none"])
        .assert()
        .success();

    let session_id = "018f45d0-0000-7000-8000-000000010001";
    let conn = Connection::open(temp.path().join("work.sqlite")).unwrap();
    conn.execute(
        r#"
        INSERT INTO sessions
        (
            id, provider, external_session_id, agent_type, is_primary, status, fidelity,
            started_at_ms, created_at_ms, updated_at_ms
        )
        VALUES (?1, 'codex', 'mcp-large-session', 'primary', 1, 'imported', 'imported', 1, 1, 1)
        "#,
        [session_id],
    )
    .unwrap();
    for index in 0..201 {
        let event_id = format!("018f45d0-0000-7000-8000-{index:012x}");
        conn.execute(
            r#"
            INSERT INTO events
            (id, seq, session_id, event_type, role, occurred_at_ms, payload_json)
            VALUES (?1, ?2, ?3, 'message', 'assistant', ?4, ?5)
            "#,
            params![
                event_id,
                index,
                session_id,
                index + 1,
                format!(r#"{{"text":"mcp transcript event {index}"}}"#)
            ],
        )
        .unwrap();
    }
    drop(conn);

    let responses = mcp_roundtrip(
        &temp,
        &[
            json!({
                "jsonrpc": "2.0",
                "id": "init",
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": { "name": "ctx-test", "version": "0" }
                }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": "show",
                "method": "tools/call",
                "params": {
                    "name": "show_session",
                    "arguments": {
                        "ctx_session_id": session_id,
                        "mode": "log"
                    }
                }
            }),
        ],
    );

    let transcript = &responses[1]["result"]["structuredContent"];
    assert_eq!(transcript["truncated"]["events"], true);
    assert_eq!(transcript["truncated"]["max_events"], 200);
    assert_eq!(transcript["events"].as_array().unwrap().len(), 200);
}
