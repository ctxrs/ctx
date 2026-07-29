#[test]
fn pi_cli_import_search_flow() {
    let temp = tempdir();
    let _daemon = start_source_refresh_daemon(&temp);
    let fixture = provider_history_fixture("pi-session.jsonl");

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "pi",
        "--path",
        &fixture,
        "--no-daemon",
        "--format=json",
    ]));
    assert_eq!(imported["schema_version"], 2);
    assert_eq!(
        imported["sources"][0]["status"], "published",
        "{imported:#}"
    );
    assert_eq!(imported["sources"][0]["provider"], "pi");
    assert_eq!(imported["sources"][0]["source_format"], "pi_session_jsonl");
    assert_eq!(imported["totals"]["imported_sessions"], 0);
    assert_eq!(imported["totals"]["imported_events"], 0);
    let first_generation = imported["sources"][0]["published_generation"]
        .as_str()
        .expect("Pi explicit import must publish a source-backed generation");

    let search = json_output(ctx(&temp).args([
        "search",
        "provider metadata",
        "--provider",
        "pi",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_source_backed_search(&search, "pi", "provider metadata");

    let second = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "pi",
        "--path",
        &fixture,
        "--resume",
        "--no-daemon",
        "--format=json",
    ]));
    assert_eq!(second["resume"], true);
    assert_eq!(second["resume_mode"], "idempotent_rescan");
    assert_eq!(second["totals"]["imported_sessions"], 0);
    assert_eq!(second["totals"]["imported_events"], 0);
    assert_eq!(second["totals"]["skipped"], 0);
    assert_eq!(second["sources"][0]["catalog_changed"], false, "{second:#}");
    assert_eq!(
        second["sources"][0]["published_generation"], first_generation,
        "{second:#}"
    );

    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM ctx_sessions WHERE provider = 'pi' AND fidelity = 'imported'"
        ),
        1
    );
    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM ctx_events WHERE provider = 'pi' AND fidelity = 'imported'"
        ),
        4
    );
    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM ctx_events WHERE provider = 'pi' AND event_type = 'message' AND role = 'user'"
        ),
        1
    );
    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM ctx_events WHERE provider = 'pi' AND event_type = 'message' AND role = 'assistant'"
        ),
        1
    );
    for event_type in ["tool_output", "command_output"] {
        assert_eq!(
            source_backed_count(
                &temp,
                &format!(
                    "SELECT COUNT(*) FROM ctx_events \
                     WHERE provider = 'pi' AND event_type = '{event_type}'"
                ),
            ),
            0,
            "successful Pi output created a Core {event_type} row"
        );
    }
    for forbidden_output in ["tests passed", "ok token=fixture-secret"] {
        assert_eq!(
            source_backed_count(
                &temp,
                &format!(
                    "SELECT COUNT(*) FROM ctx_events \
                     WHERE provider = 'pi' AND payload_json LIKE '%{forbidden_output}%'"
                ),
            ),
            0,
            "successful Pi output body leaked into Core rows: {forbidden_output}"
        );
    }
    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM ctx_projection_metadata WHERE status = 'ready'"
        ),
        1
    );
    assert!(
        !temp.path().join("work.sqlite").exists(),
        "Pi acceptance must use the source-backed generation and relational projection"
    );
}
