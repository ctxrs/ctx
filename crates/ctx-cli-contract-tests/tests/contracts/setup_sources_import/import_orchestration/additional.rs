use super::*;

#[test]
fn explicit_import_reports_warm_carried_route_failure() {
    let temp = tempdir();
    let _daemon = start_full_source_refresh_daemon(&temp);
    let fixture = temp.path().join("warm-explicit-failure.jsonl");
    write_valid_explicit_custom_source(&fixture, "warm carried explicit route oracle");

    let first = json_output(ctx(&temp).args([
        "import",
        "--input-format",
        "ctx-history-jsonl-v2",
        "--path",
        fixture.to_str().unwrap(),
        "--no-daemon",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(first["outcome"], "success", "{first:#}");
    assert_eq!(provider_core_counts(&data_root(&temp), "custom"), (1, 1));
    let first_generation = published_generation(&first);

    fs::write(&fixture, b"").unwrap();
    let carried = json_output(ctx(&temp).args([
        "import",
        "--input-format",
        "ctx-history-jsonl-v2",
        "--path",
        fixture.to_str().unwrap(),
        "--no-daemon",
        "--format=json",
        "--progress",
        "none",
    ]));

    assert_eq!(
        carried["outcome"], "completed_with_source_failures",
        "{carried:#}"
    );
    assert_eq!(carried["failure_scope"], "source", "{carried:#}");
    assert_eq!(carried["failure_type"], "source_failure", "{carried:#}");
    assert!(
        carried["totals"].get("imported_sources").is_none(),
        "{carried:#}"
    );
    assert_eq!(carried["totals"]["failed_sources"], 1, "{carried:#}");
    assert_eq!(
        carried["totals"]["current_indexed_documents"], 1,
        "{carried:#}"
    );
    let source = &carried["sources"][0];
    assert_eq!(source["status"], "failure", "{source:#}");
    assert_eq!(source["source_failure_total"], 1, "{source:#}");
    assert_eq!(source["source_failure_class"], "unreadable", "{source:#}");
    assert_eq!(source["carried_forward"], true, "{source:#}");
    assert_eq!(source["successful_routes"], 0, "{source:#}");
    assert_eq!(published_generation(&carried), first_generation);
    assert!(source["source_identity"].is_string(), "{source:#}");
    assert_eq!(provider_core_counts(&data_root(&temp), "custom"), (1, 1));
}

#[test]
fn all_invalid_custom_source_fails_with_unrelated_history_then_refreshes_after_fix() {
    let temp = tempdir();
    let _daemon = start_full_source_refresh_daemon(&temp);
    let fixture = temp.path().join("custom-retry.jsonl");
    let records = |event_index: &str| {
        r#"{"record_type":"manifest","schema_version":"ctx-history-jsonl-v2"}
{"record_type":"source","source_id":"retry-source","provider_key":"retry-agent","source_format":"retry-jsonl","cursor":{"after":{"stream":"retry-agent:retry-source","cursor":"1","observed_at":"2026-07-13T12:00:00Z"}}}
{"record_type":"session","source_id":"retry-source","provider_session_id":"retry-session","started_at":"2026-07-13T12:00:00Z","agent_scope":"primary","status":"completed"}
{"record_type":"event","source_id":"retry-source","provider_session_id":"retry-session","event_index":EVENT_INDEX,"event_id":"retry-event","event_type":"message","role":"user","occurred_at":"2026-07-13T12:00:01Z","payload":{"text":"retry oracle"},"preview":"retry oracle"}
"#
        .replace("EVENT_INDEX", event_index)
    };
    let retained = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &provider_history_fixture("codex-sessions"),
        "--no-daemon",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(retained["outcome"], "success", "{retained:#}");
    let retained_generation = published_generation(&retained);
    let retained_documents = retained["totals"]["current_indexed_documents"]
        .as_u64()
        .filter(|count| *count > 0)
        .expect("retained Codex fixture documents");
    fs::write(&fixture, records(r#""invalid""#)).unwrap();

    let failure = failure_stderr(ctx(&temp).args([
        "import",
        "--input-format",
        "ctx-history-jsonl-v2",
        "--path",
        fixture.to_str().unwrap(),
        "--no-daemon",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert!(
        failure.contains("No usable history was imported"),
        "{failure}"
    );
    assert_eq!(
        provider_core_counts(&data_root(&temp), "codex").1,
        retained_documents as usize
    );
    assert_eq!(provider_core_counts(&data_root(&temp), "custom"), (0, 0));

    fs::write(&fixture, records("0")).unwrap();
    let retry = json_output(ctx(&temp).args([
        "import",
        "--input-format",
        "ctx-history-jsonl-v2",
        "--path",
        fixture.to_str().unwrap(),
        "--no-daemon",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(retry["outcome"], "success", "{retry:#}");
    let generation = published_generation(&retry);
    assert_ne!(generation, retained_generation);
    let status = wait_for_core_generation(&temp, &generation);
    assert_eq!(
        status["lexical"]["indexed_documents"],
        retained_documents + 1,
        "{status:#}"
    );
    assert_eq!(provider_core_counts(&data_root(&temp), "custom"), (1, 1));
    assert!(!data_root(&temp).join("relational.sqlite").exists());
    let search = json_output(ctx(&temp).args([
        "search",
        "retry oracle",
        "--provider",
        "custom",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_eq!(search["retrieval"]["generation_id"], generation);
    assert_eq!(search["results"].as_array().unwrap().len(), 1, "{search:#}");
}

#[test]
fn import_custom_history_format_is_not_a_native_provider_importer() {
    let temp = tempdir();
    let stderr = failure_stderr(ctx(&temp).args(["import", "--provider", "custom"]));
    assert!(stderr.contains("invalid value 'custom'"), "{stderr}");

    let fixture = custom_history_fixture("basic.jsonl");
    let stderr = failure_stderr(ctx(&temp).args([
        "import",
        "--input-format",
        "ctx-history-jsonl-v2",
        "--path",
        &fixture,
        "--all",
    ]));
    assert!(stderr.contains("--input-format"), "{stderr}");
    assert!(stderr.contains("--all"), "{stderr}");
}

#[test]
fn import_all_discovers_and_imports_providers_together() {
    let temp = tempdir();
    copy_dir_all(
        Path::new(&provider_history_fixture("codex-sessions")),
        &temp.path().join(".codex").join("sessions"),
    );
    let pi_home = temp.path().join(".pi/agent/sessions/--workspace-example--");
    fs::create_dir_all(&pi_home).unwrap();
    write_pi_session_jsonl(
        &pi_home.join("2026-06-24T12-00-00-000Z_pi-session-docs-1.jsonl"),
        "pi-session-docs-1",
        "Inspect the provider metadata rows.",
    );

    let output = ctx(&temp)
        .args(["import", "--all", "--format=json", "--progress", "json"])
        .assert()
        .success()
        .get_output()
        .clone();

    let stdout: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(stdout["schema_version"], 2);
    assert!(
        stdout["totals"]["current_indexed_documents"]
            .as_u64()
            .is_some_and(|count| count >= 3),
        "{stdout:#}"
    );
    assert!(
        stdout["totals"]["current_source_count"]
            .as_u64()
            .is_some_and(|count| count >= 2),
        "{stdout:#}"
    );
    assert_eq!(
        stdout["sources"][0]["source_format"],
        "provider_authoritative_all"
    );

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains(r#""type":"ctx_progress""#), "{stderr}");
    assert!(stderr.contains(r#""phase":"published""#), "{stderr}");
}

#[test]
fn selected_provider_and_exact_path_ignore_unrelated_discoverable_sources() {
    for exact_path in [false, true] {
        let temp = tempdir();
        let daemon = start_full_source_refresh_daemon(&temp);
        wait_for_initial_source_refresh(&temp);
        write_codex_setup_session(&temp);
        install_default_pi_fixture(&temp, "unselected pi import scope oracle");
        let opencode_dir = temp.path().join(".local/share/opencode");
        fs::create_dir_all(&opencode_dir).unwrap();
        fs::write(opencode_dir.join("opencode.db"), b"not sqlite").unwrap();
        let codex_path = temp.path().join(".codex/sessions");
        let discovered = json_output(ctx(&temp).args(["sources", "--format=json"]));
        for provider in ["codex", "pi", "opencode"] {
            let source = discovered["sources"]
                .as_array()
                .unwrap()
                .iter()
                .find(|source| source["provider"] == provider)
                .unwrap_or_else(|| {
                    panic!("missing discoverable {provider} source: {discovered:#}")
                });
            assert_eq!(source["status"], "available", "{provider}: {discovered:#}");
            assert_eq!(source["importable"], true, "{provider}: {discovered:#}");
        }
        for provider in ["codex", "pi", "opencode"] {
            assert_eq!(
                provider_core_counts(&data_root(&temp), provider),
                (0, 0),
                "the daemon's initial generation must predate the selected-import fixtures"
            );
        }

        let mut command = ctx(&temp);
        command.args(["import", "--provider", "codex"]);
        if exact_path {
            command.args(["--path", codex_path.to_str().unwrap()]);
        }
        let imported = json_output(command.args(["--format=json", "--progress", "none"]));
        let source = if exact_path {
            let source =
                assert_explicit_source_publication(&imported, "codex", "codex_session_jsonl_tree");
            assert_eq!(source["path"], codex_path.to_str().unwrap(), "{imported:#}");
            assert!(source["route_identity"].is_string(), "{imported:#}");
            source
        } else {
            assert_authoritative_provider_publication(&imported)
        };

        assert_eq!(source["scanned_routes"], 1, "{imported:#}");
        assert_eq!(source["successful_routes"], 1, "{imported:#}");
        assert_eq!(source["source_failure_total"], 0, "{imported:#}");
        assert_eq!(
            imported["totals"]["current_sources_with_rejections"], 0,
            "{imported:#}"
        );

        assert!(!published_generation(&imported).is_empty(), "{imported:#}");
        drop(daemon);
    }
}

#[test]
fn import_all_without_sources_does_not_report_missing_explicit_path() {
    let temp = tempdir();
    let report = json_output(ctx(&temp).args(["import", "--all", "--format=json"]));
    assert_eq!(report["outcome"], "success", "{report:#}");
    assert!(
        matches!(
            report["totals"]["change"].as_str(),
            Some("changed" | "no_op")
        ),
        "{report:#}"
    );
    assert_eq!(report["totals"]["current_source_count"], 0, "{report:#}");
    assert_eq!(
        report["totals"]["current_indexed_documents"], 0,
        "{report:#}"
    );
}

#[test]
fn import_all_discovers_sources_when_home_unset_and_userprofile_set() {
    let temp = daemon_test_root();
    copy_dir_all(
        Path::new(&provider_history_fixture("codex-sessions")),
        &temp.path().join(".codex").join("sessions"),
    );
    // Establish the exact retained daemon while the fixture-owned HOME is
    // intact. The import below exercises USERPROFILE discovery without
    // authorizing a detached child to escape through the installed-user home.
    let _daemon = start_full_source_refresh_daemon(&temp);

    let imported = json_output(
        ctx(&temp)
            .env_remove("HOME")
            .env("USERPROFILE", temp.path())
            .env("CTX_DAEMON_AUTOSTART_OFF", "1")
            .args(["import", "--all", "--format=json", "--progress", "none"]),
    );
    assert!(imported["totals"]["current_source_count"]
        .as_u64()
        .is_some_and(|count| count >= 1));
    assert_eq!(imported["totals"]["failed_sources"], 0);
    assert_eq!(
        imported["sources"][0]["source_format"],
        "provider_authoritative_all"
    );

    let discovered = json_output(
        ctx(&temp)
            .env_remove("HOME")
            .env("USERPROFILE", temp.path())
            .env("CTX_DAEMON_AUTOSTART_OFF", "1")
            .args(["sources", "--format=json"]),
    );
    let codex = discovered["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| {
            source["provider"] == "codex" && source["source_format"] == "codex_session_jsonl_tree"
        })
        .unwrap_or_else(|| panic!("missing USERPROFILE Codex route: {discovered:#}"));
    assert!(
        Path::new(codex["path"].as_str().unwrap()).starts_with(temp.path()),
        "USERPROFILE discovery escaped the fixture root: {codex:#}"
    );
}

#[test]
fn import_all_skips_empty_gemini_source() {
    let temp = tempdir();
    copy_dir_all(
        Path::new(&provider_history_fixture("codex-sessions")),
        &temp.path().join(".codex").join("sessions"),
    );
    fs::create_dir_all(temp.path().join(".gemini")).unwrap();

    let sources = json_output(ctx(&temp).args(["sources", "--format=json"]));
    let gemini = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["provider"] == "gemini")
        .unwrap();
    assert_eq!(gemini["status"], "empty");
    assert_eq!(gemini["native_import"], true);
    assert_eq!(gemini["importable"], false);
    let concise = success_stdout(ctx(&temp).arg("sources"));
    assert!(!concise.contains("~/.gemini"), "{concise}");

    let imported =
        json_output(ctx(&temp).args(["import", "--all", "--format=json", "--progress", "none"]));
    assert!(imported["totals"]["current_source_count"]
        .as_u64()
        .is_some_and(|count| count >= 1));
    assert_eq!(imported["totals"]["failed_sources"], 0);
}

#[test]
fn import_all_publishes_valid_routes_and_reports_one_invalid_route() {
    let temp = daemon_test_root();
    copy_dir_all(
        Path::new(&provider_history_fixture("codex-sessions")),
        &temp.path().join(".codex").join("sessions"),
    );
    let opencode_dir = temp.path().join(".local/share/opencode");
    fs::create_dir_all(&opencode_dir).unwrap();
    fs::write(opencode_dir.join("opencode.db"), b"not sqlite").unwrap();

    let imported =
        json_output(ctx(&temp).args(["import", "--all", "--format=json", "--progress", "none"]));
    assert_eq!(imported["outcome"], "completed_with_source_failures");
    assert_eq!(imported["failure_scope"], "source");
    assert!(imported["totals"]["current_source_count"]
        .as_u64()
        .is_some_and(|count| count >= 1));
    assert!(imported["totals"]["current_indexed_documents"]
        .as_u64()
        .is_some_and(|count| count >= 1));
    let publication = &imported["sources"][0];
    assert_eq!(
        publication["daemon_request_metadata"]["operation"],
        "import"
    );
    assert!(publication["successful_routes"]
        .as_u64()
        .is_some_and(|routes| routes > 0));
    let failed = imported["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["provider"] == "opencode")
        .expect("typed opencode route failure");
    assert_eq!(failed["status"], "failure");
    assert_eq!(failed["source_failure_class"], "unreadable");
    assert_eq!(failed["carried_forward"], false);
}

#[test]
fn warm_import_all_advances_with_changed_codex_and_failed_opencode() {
    let temp = daemon_test_root();
    let opencode_query = "opencode warm carry forward oracle";
    let new_codex_query = "codex warm success boundary oracle";
    write_codex_setup_session(&temp);
    install_provider_default_fixture(
        &temp,
        temp.path(),
        "open_code",
        opencode_query,
        "OpenCode warm carry forward response",
    );

    let cold =
        json_output(ctx(&temp).args(["import", "--all", "--format=json", "--progress", "none"]));
    assert_eq!(cold["outcome"], "success", "{cold:#}");
    assert_eq!(cold["totals"]["failed_sources"], 0, "{cold:#}");
    let cold_generation = published_generation(&cold);
    let cold_opencode_counts = provider_core_counts(&data_root(&temp), "opencode");
    assert!(cold_opencode_counts.0 > 0, "{cold:#}");
    assert!(cold_opencode_counts.1 > 0, "{cold:#}");
    assert!(provider_core_counts(&data_root(&temp), "codex").1 > 0);

    let codex_session = temp
        .path()
        .join(".codex/sessions/2026/06/24/codex-session-setup.jsonl");
    let mut codex = fs::OpenOptions::new()
        .append(true)
        .open(codex_session)
        .unwrap();
    writeln!(
        codex,
        "{}",
        json!({
            "timestamp": "2026-06-24T10:00:02.000Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": new_codex_query}]
            }
        })
    )
    .unwrap();
    drop(codex);
    fs::write(
        temp.path().join(".local/share/opencode/opencode.db"),
        b"not sqlite",
    )
    .unwrap();

    let warm =
        json_output(ctx(&temp).args(["import", "--all", "--format=json", "--progress", "none"]));
    assert_eq!(
        warm["outcome"], "completed_with_source_failures",
        "{warm:#}"
    );
    assert_eq!(warm["failure_scope"], "source", "{warm:#}");
    assert_eq!(warm["failure_type"], "source_failure", "{warm:#}");
    assert_eq!(warm["totals"]["failed_sources"], 1, "{warm:#}");

    let sources = warm["sources"].as_array().unwrap();
    let failed = sources
        .iter()
        .find(|source| source["provider"] == "opencode")
        .expect("typed OpenCode source failure");
    assert_eq!(failed["status"], "failure", "{warm:#}");
    assert_eq!(failed["source_failure_class"], "unreadable", "{warm:#}");
    assert_eq!(failed["carried_forward"], true, "{warm:#}");

    let publication = sources
        .iter()
        .find(|source| source["source_format"] == "provider_authoritative_all")
        .expect("successful import-all publication");
    let warm_generation = publication["published_generation"]
        .as_str()
        .expect("warm import should publish a generation")
        .to_owned();
    assert_ne!(warm_generation, cold_generation, "{warm:#}");
    wait_for_core_generation(&temp, &warm_generation);
    assert_eq!(
        provider_core_counts(&data_root(&temp), "opencode"),
        cold_opencode_counts
    );

    for (provider, query) in [("codex", new_codex_query), ("opencode", opencode_query)] {
        let search = json_output(ctx(&temp).args([
            "search",
            query,
            "--provider",
            provider,
            "--refresh",
            "off",
            "--format=json",
        ]));
        assert_eq!(
            search["retrieval"]["generation_id"], warm_generation,
            "{search:#}"
        );
        assert!(
            search["results"].as_array().is_some_and(|results| {
                results.iter().any(|result| {
                    result["provider"] == provider
                        && result["snippet"]
                            .as_str()
                            .is_some_and(|snippet| snippet.contains(query))
                })
            }),
            "{search:#}"
        );
    }
}

#[test]
fn failed_import_attempt_does_not_count_as_indexed_history() {
    let temp = tempdir();
    let opencode_dir = temp.path().join(".local/share/opencode");
    fs::create_dir_all(&opencode_dir).unwrap();
    fs::write(opencode_dir.join("opencode.db"), b"not sqlite").unwrap();

    ctx(&temp)
        .args(["import", "--all", "--format=json", "--progress", "none"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("source-backed scan failed for opencode")
                .and(predicate::str::contains("file is not a database")),
        );

    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert!(
        matches!(
            status["lexical"]["status"].as_str(),
            Some("unavailable" | "pending")
        ),
        "{status:#}"
    );
    assert!(
        matches!(
            status["history_epoch"]["status"].as_str(),
            Some("unavailable" | "pending")
        ),
        "{status:#}"
    );
    assert_ne!(status["lexical"]["status"], "ready", "{status:#}");
    assert_ne!(status["history_epoch"]["status"], "ready", "{status:#}");
    assert_eq!(status["initialized"], false, "{status:#}");
    assert!(
        status
            .get("indexed_events")
            .and_then(serde_json::Value::as_u64)
            .is_none_or(|count| count == 0),
        "{status:#}"
    );
}

#[derive(Debug, PartialEq, Eq)]
struct ProviderCoreSnapshot {
    generation: String,
    sessions: Vec<String>,
    events: Vec<String>,
    sources: Vec<String>,
}

fn provider_core_snapshot(temp: &TempDir, provider: &str) -> ProviderCoreSnapshot {
    let status = json_output(ctx(temp).args(["status", "--format=json"]));
    let generation = status["lexical"]["generation_id"]
        .as_str()
        .expect("published lexical Core generation")
        .to_owned();
    wait_for_core_generation(temp, &generation);
    let records = provider_core_records(&data_root(temp), provider);
    let mut sessions = records
        .iter()
        .map(|record| {
            format!(
                "{}:{}",
                record.session_id,
                record.provider_session_id.as_deref().unwrap_or_default()
            )
        })
        .collect::<Vec<_>>();
    sessions.sort();
    sessions.dedup();
    let mut events = records
        .iter()
        .map(|record| {
            format!(
                "{}:{}:{}",
                record.event_id, record.session_id, record.event_type
            )
        })
        .collect::<Vec<_>>();
    events.sort();
    let manifest = generation_manifest(temp, &generation);
    let mut sources = manifest["sources"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|source| source["observation"]["source"]["provider"] == provider)
        .map(serde_json::Value::to_string)
        .collect::<Vec<_>>();
    sources.sort();
    ProviderCoreSnapshot {
        generation,
        sessions,
        events,
        sources,
    }
}

fn ready_setup(temp: &TempDir) -> Value {
    json_output(ctx(temp).args(["setup", "--wait", "--format=json", "--progress", "none"]))
}

fn generation_manifest(temp: &TempDir, generation: &str) -> Value {
    let path = data_root(temp)
        .join("search/lexical/ctx-generations")
        .join(format!("{generation}.json"));
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn manifest_providers(temp: &TempDir, generation: &str) -> Vec<String> {
    let manifest = generation_manifest(temp, generation);
    let mut providers = manifest["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|source| {
            source["observation"]["source"]["provider"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect::<Vec<_>>();
    providers.sort();
    providers
}

fn assert_searchable_and_showable(temp: &TempDir, provider: &str, query: &str) -> (String, String) {
    let search = json_output(ctx(temp).args([
        "search",
        query,
        "--provider",
        provider,
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_eq!(search["retrieval"]["index"], "core", "{search:#}");
    assert_eq!(search["filters"]["provider"], provider, "{search:#}");
    assert_eq!(search["results"].as_array().unwrap().len(), 1, "{search:#}");
    let result = &search["results"][0];
    assert_eq!(result["provider"], provider, "{result:#}");
    assert!(
        result["snippet"]
            .as_str()
            .is_some_and(|snippet| snippet.contains(query)),
        "{result:#}"
    );
    let event_id = result["ctx_event_id"].as_str().unwrap().to_owned();
    let session_id = result["ctx_session_id"].as_str().unwrap().to_owned();

    let shown_event = json_output(ctx(temp).args([
        "show", "event", &event_id, "--window", "1", "--format", "json",
    ]));
    assert_eq!(shown_event["payload_type"], "event_window");
    assert_eq!(shown_event["ctx_event_id"], event_id);
    assert_eq!(shown_event["ctx_session_id"], session_id);
    assert_eq!(shown_event["event"]["content"]["policy_status"], "selected");

    let shown_session =
        json_output(ctx(temp).args(["show", "session", &session_id, "--format", "json"]));
    assert_eq!(shown_session["payload_type"], "session_transcript");
    assert_eq!(shown_session["ctx_session_id"], session_id);
    assert_eq!(shown_session["provider"], provider);
    (session_id, event_id)
}

#[test]
fn fresh_setup_publishes_provider_sources_to_core() {
    let temp = tempdir();
    write_codex_setup_session(&temp);
    let _daemon = start_full_source_refresh_daemon(&temp);

    let setup = ready_setup(&temp);

    assert_eq!(setup["schema_version"], 2, "{setup:#}");
    assert_eq!(setup["mode"], "ready", "{setup:#}");
    assert_eq!(setup["history_epoch"]["status"], "ready", "{setup:#}");
    assert_eq!(setup["lexical"]["status"], "ready", "{setup:#}");
    assert_eq!(setup["refresh_request"]["status"], "published", "{setup:#}");
    assert_eq!(setup["refresh_request"]["source_count"], 1, "{setup:#}");
    let generation = setup["lexical"]["generation_id"].as_str().unwrap();
    assert_eq!(
        manifest_providers(&temp, generation),
        vec!["codex".to_owned()]
    );
    let status = wait_for_core_generation(&temp, generation);
    assert_eq!(status["lexical"]["status"], "ready", "{status:#}");
    assert_eq!(provider_core_counts(&data_root(&temp), "codex"), (1, 1));
    assert!(data_root(&temp)
        .join("search/lexical/active-generation.json")
        .is_file());
    assert!(!data_root(&temp).join("relational.sqlite").exists());

    let projection = provider_core_snapshot(&temp, "codex");
    assert_eq!(projection.generation, generation);
    assert_eq!(projection.sessions.len(), 1, "{projection:#?}");
    assert_eq!(projection.events.len(), 1, "{projection:#?}");
    assert_eq!(projection.sources.len(), 1, "{projection:#?}");
    assert_searchable_and_showable(&temp, "codex", "setup should import");
}

#[test]
fn mixed_setup_publishes_each_provider_once() {
    let temp = tempdir();
    write_codex_setup_session(&temp);
    let claude_query = "mixed setup claude source authority";
    install_default_claude_fixture(&temp, claude_query);
    let _daemon = start_full_source_refresh_daemon(&temp);

    let setup = ready_setup(&temp);
    assert_eq!(setup["schema_version"], 2, "{setup:#}");
    assert_eq!(setup["mode"], "ready", "{setup:#}");
    assert_eq!(setup["refresh_request"]["status"], "published", "{setup:#}");
    assert_eq!(setup["refresh_request"]["source_count"], 2, "{setup:#}");
    let generation = setup["lexical"]["generation_id"].as_str().unwrap();
    assert_eq!(
        manifest_providers(&temp, generation),
        vec!["claude".to_owned(), "codex".to_owned()]
    );
    let status = wait_for_core_generation(&temp, generation);
    assert_eq!(status["lexical"]["status"], "ready", "{status:#}");
    assert!(!data_root(&temp).join("relational.sqlite").exists());

    let codex = provider_core_snapshot(&temp, "codex");
    let claude = provider_core_snapshot(&temp, "claude");
    assert_eq!(codex.sessions.len(), 1, "{codex:#?}");
    assert_eq!(claude.sessions.len(), 1, "{claude:#?}");
    assert_eq!(codex.sources.len(), 1, "{codex:#?}");
    assert_eq!(claude.sources.len(), 1, "{claude:#?}");
    assert_eq!(codex.generation, claude.generation);
    assert_eq!(codex.generation, generation);

    assert_searchable_and_showable(&temp, "codex", "setup should import");
    assert_searchable_and_showable(&temp, "claude", claude_query);
}

#[test]
fn setup_adds_provider_without_changing_unchanged_source_ids() {
    let temp = tempdir();
    let pi_query = "pi authority retained across provider addition";
    install_default_pi_fixture(&temp, pi_query);
    let _daemon = start_full_source_refresh_daemon(&temp);
    ready_setup(&temp);
    let pi_before = provider_core_snapshot(&temp, "pi");
    let pi_ids_before = assert_searchable_and_showable(&temp, "pi", pi_query);

    write_codex_setup_session(&temp);
    let setup = ready_setup(&temp);
    let generation = setup["lexical"]["generation_id"].as_str().unwrap();
    assert_eq!(
        manifest_providers(&temp, generation),
        vec!["codex".to_owned(), "pi".to_owned()]
    );
    let pi_after = provider_core_snapshot(&temp, "pi");
    assert_ne!(pi_after.generation, pi_before.generation);
    assert_eq!(pi_after.sessions, pi_before.sessions);
    assert_eq!(pi_after.events, pi_before.events);
    assert_eq!(pi_after.sources, pi_before.sources);
    assert_eq!(
        assert_searchable_and_showable(&temp, "pi", pi_query),
        pi_ids_before
    );
    assert_searchable_and_showable(&temp, "codex", "setup should import");
}

#[test]
fn repeated_setup_and_import_preserve_generation_and_public_ids() {
    let temp = tempdir();
    write_codex_setup_session(&temp);
    let _daemon = start_full_source_refresh_daemon(&temp);

    let first = ready_setup(&temp);
    let first_generation = first["lexical"]["generation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let projection = provider_core_snapshot(&temp, "codex");
    let ids = assert_searchable_and_showable(&temp, "codex", "setup should import");
    assert!(!data_root(&temp).join("relational.sqlite").exists());

    let second = ready_setup(&temp);
    assert_eq!(
        second["lexical"]["generation_id"], first_generation,
        "{second:#}"
    );
    assert_eq!(
        second["refresh_request"]["published_generation"], first_generation,
        "{second:#}"
    );
    assert_eq!(provider_core_snapshot(&temp, "codex"), projection);

    let imported = json_output(ctx(&temp).args([
        "import",
        "--all",
        "--no-daemon",
        "--format=json",
        "--progress",
        "none",
    ]));
    assert_eq!(imported["sources"].as_array().unwrap().len(), 1);
    assert_eq!(
        imported["sources"][0]["source_format"],
        "provider_authoritative_all"
    );
    assert_eq!(
        imported["sources"][0]["published_generation"],
        first_generation
    );
    assert_eq!(provider_core_snapshot(&temp, "codex"), projection);
    assert!(!data_root(&temp).join("relational.sqlite").exists());
    assert_eq!(
        assert_searchable_and_showable(&temp, "codex", "setup should import"),
        ids
    );
}
