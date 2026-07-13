mod support;

use support::*;

#[test]
fn codebuddy_cli_jsonl_imports_and_searches_through_public_cli() {
    let temp = tempdir();
    let query = "codebuddy-cli-real-shape-oracle";
    let path = write_native_codebuddy_cli_jsonl_fixture(&temp, query);

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codebuddy",
        "--path",
        &path,
        "--json",
    ]));
    assert_eq!(imported["schema_version"], 2);
    assert_eq!(imported["sources"][0]["provider"], "codebuddy");
    assert_eq!(
        imported["sources"][0]["source_format"],
        "codebuddy_history_json"
    );
    assert_eq!(imported["totals"]["rejected_records"], 0);
    assert_eq!(imported["totals"]["imported_sessions"], 1);
    assert_eq!(imported["totals"]["imported_events"], 2);

    let search = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "codebuddy",
        "--refresh",
        "off",
        "--json",
    ]));
    assert_search_provider_oracle(&search, "codebuddy", query, 1, "message");
}

#[test]
fn nanoclaw_import_preserves_text_timestamp_millis_and_integer_trigger() {
    let temp = tempdir();
    let query = "nanoclaw-real-text-timestamp-oracle";
    let path = write_native_nanoclaw_fixture(&temp, query);

    let central = Connection::open(Path::new(&path).join("data/v2.db")).unwrap();
    central
        .execute_batch(
            "update sessions
             set created_at = '2026-07-10T03:18:34.491Z',
                 last_active = '2026-07-10 03:19:51'",
        )
        .unwrap();
    let inbound = Connection::open(
        Path::new(&path)
            .join("data/v2-sessions/ag-1/session-1")
            .join("inbound.db"),
    )
    .unwrap();
    inbound
        .execute(
            "update messages_in set timestamp = ?1, trigger = 1 where id = 'in-1'",
            ["2026-07-10T03:18:34.491Z"],
        )
        .unwrap();
    let outbound = Connection::open(
        Path::new(&path)
            .join("data/v2-sessions/ag-1/session-1")
            .join("outbound.db"),
    )
    .unwrap();
    outbound
        .execute(
            "update messages_out set timestamp = ?1 where id = 'out-1'",
            ["2026-07-10 03:19:51"],
        )
        .unwrap();

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "nanoclaw",
        "--path",
        &path,
        "--json",
    ]));
    assert_eq!(imported["totals"]["rejected_records"], 0);
    assert_eq!(imported["totals"]["imported_events"], 2);

    let store = Connection::open(temp.path().join("work.sqlite")).unwrap();
    let (occurred_at_ms, payload_json): (i64, String) = store
        .query_row(
            "select occurred_at_ms, payload_json
             from events
             where json_extract(metadata_json, '$.metadata.message_id') = 'in-1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(occurred_at_ms, 1_783_653_514_491);
    let payload: Value = serde_json::from_str(&payload_json).unwrap();
    let body: Value =
        serde_json::from_str(payload["body"]["body"]["json"].as_str().unwrap()).unwrap();
    assert_eq!(body["trigger"], "1");

    let search = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "nanoclaw",
        "--refresh",
        "off",
        "--json",
    ]));
    assert_search_provider_oracle(&search, "nanoclaw", query, 1, "message");
}
