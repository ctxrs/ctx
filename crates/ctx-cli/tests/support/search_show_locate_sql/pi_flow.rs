#[test]
fn pi_cli_import_search_flow() {
    let temp = tempdir();
    let fixture = temp
        .path()
        .join(".pi/agent/sessions/--workspace--/pi-session.jsonl");
    fs::create_dir_all(fixture.parent().unwrap()).unwrap();
    fs::copy(provider_history_fixture("pi-session.jsonl"), &fixture).unwrap();
    let _daemon = start_source_refresh_daemon(&temp);

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "pi",
        "--no-daemon",
        "--format=json",
    ]));
    assert_authoritative_provider_publication(&imported);
    assert_eq!(imported["totals"]["current_rejected_records"], 0);
    let first_generation = imported["sources"][0]["published_generation"]
        .as_str()
        .expect("Pi provider import must publish a source-backed generation");

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
        "--resume",
        "--no-daemon",
        "--format=json",
    ]));
    assert_eq!(second["resume"], true);
    assert_eq!(second["resume_mode"], "idempotent_rescan");
    assert_authoritative_provider_publication(&second);
    assert_eq!(second["totals"]["current_rejected_records"], 0);
    assert_eq!(
        second["sources"][0]["published_generation"], first_generation,
        "{second:#}"
    );

    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM ctx_sessions WHERE provider = 'pi'"
        ),
        1
    );
    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM ctx_events WHERE provider = 'pi'"
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
    assert_eq!(
        source_backed_count(
            &temp,
            "SELECT COUNT(*) FROM pragma_table_info('ctx_events') WHERE name = 'payload_json'",
        ),
        0,
        "the metadata-only relational projection must expose no payload column"
    );
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
