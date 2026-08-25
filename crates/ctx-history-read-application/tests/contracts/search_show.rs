#[path = "../support/mod.rs"]
mod support;

use std::{
    io::Read,
    process::{Child, Command as StdCommand, Stdio},
};

use support::{daemon_test_root as tempdir, *};

struct SourceRefreshDaemon {
    child: Option<Child>,
}

impl Drop for SourceRefreshDaemon {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn start_source_refresh_daemon(temp: &TempDir) -> SourceRefreshDaemon {
    start_source_refresh_daemon_with_env(temp, &[])
}

fn start_source_refresh_daemon_with_env(
    temp: &TempDir,
    extra_env: &[(&str, &Path)],
) -> SourceRefreshDaemon {
    fs::create_dir_all(data_root(temp)).unwrap();
    fs::write(
        data_root(temp).join("config.toml"),
        "[daemon]\nenabled = true\nmode = \"full\"\n\n[search]\nsemantic = false\n",
    )
    .unwrap();
    let binary = copied_ctx_binary(temp);
    let prepared = ctx_from_binary(temp, &binary);
    let mut command = StdCommand::new(prepared.get_program());
    for (name, value) in prepared.get_envs() {
        match value {
            Some(value) => {
                command.env(name, value);
            }
            None => {
                command.env_remove(name);
            }
        }
    }
    command
        .current_dir(temp.path())
        .args(["daemon", "run", "--force", "--loop-interval-seconds", "600"])
        .env("CTX_DAEMON_MODE", "full")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    for (name, value) in extra_env {
        command.env(name, value);
    }
    let spawn_deadline = Instant::now() + Duration::from_secs(1);
    let child = loop {
        match command.spawn() {
            Ok(child) => break child,
            Err(error) if error.raw_os_error() == Some(26) && Instant::now() < spawn_deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("start isolated source-refresh daemon: {error}"),
        }
    };
    let mut daemon = SourceRefreshDaemon { child: Some(child) };
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(exit) = daemon.child.as_mut().unwrap().try_wait().unwrap() {
            let mut stderr = String::new();
            daemon
                .child
                .as_mut()
                .unwrap()
                .stderr
                .as_mut()
                .unwrap()
                .read_to_string(&mut stderr)
                .unwrap();
            panic!("source-refresh daemon exited before becoming ready ({exit}): {stderr}");
        }
        let status = ctx(temp)
            .args(["daemon", "status", "--format=json"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| serde_json::from_slice::<Value>(&output.stdout).ok());
        if status.as_ref().is_some_and(|status| {
            status["daemon"]["running"] == true
                && status["daemon"]["core_refresh_endpoint"]["available"] == true
        }) {
            return daemon;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for source-refresh daemon readiness: {status:#?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn assert_source_backed_search(search: &Value, provider: &str, query: &str) {
    assert_eq!(search["schema_version"], 2, "{search:#}");
    assert_eq!(search["query"], query, "{search:#}");
    assert_eq!(search["filters"]["provider"], provider, "{search:#}");
    assert_eq!(search["retrieval"]["index"], "core", "{search:#}");
    let results = search["results"].as_array().unwrap();
    assert_eq!(results.len(), 1, "{search:#}");
    assert_eq!(results[0]["provider"], provider, "{search:#}");
    assert!(results[0]["ctx_event_id"].is_string(), "{search:#}");
    assert!(results[0]["ctx_session_id"].is_string(), "{search:#}");
    assert!(
        results[0]["snippet"]
            .as_str()
            .is_some_and(|snippet| snippet.contains(query)),
        "{search:#}"
    );
}

#[test]
fn codex_ctx_retrieval_echoes_are_hidden_from_search_but_remain_directly_retrievable() {
    const PROVIDER_SESSION_ID: &str = "ctx-retrieval-self-echo-e2e";
    const EXCLUDED_DIRECT_INVOCATION: &str = "zzctxexactdirectinvocatione2ecanary";
    const EXCLUDED_DIRECT_PAYLOAD: &str = "zzctxexactdirectpayloade2ecanary";
    const EXCLUDED_MCP_PAYLOAD: &str = "zzctxexactmcppayloade2ecanary";

    let temp = tempdir();
    let fixture = provider_history_fixture("codex-retrieval-self-echo");
    let (_daemon, imported) = import_codex_fixture_through_daemon(&temp, &fixture);
    assert_eq!(imported["outcome"], "success", "{imported:#}");

    let lexical_event_search = |query: &str| {
        json_output(ctx(&temp).args([
            "search",
            query,
            "--provider",
            "codex",
            "--events",
            "--include-current-session",
            "--backend",
            "lexical",
            "--refresh",
            "off",
            "--format=json",
        ]))
    };

    for excluded in [
        EXCLUDED_DIRECT_INVOCATION,
        EXCLUDED_DIRECT_PAYLOAD,
        EXCLUDED_MCP_PAYLOAD,
    ] {
        let search = lexical_event_search(excluded);
        assert_eq!(search["retrieval"]["index"], "core", "{search:#}");
        assert_eq!(
            search["results"].as_array().map(Vec::len),
            Some(0),
            "ctx retrieval echo remained searchable for {excluded}: {search:#}"
        );
    }

    for (case, searchable) in [
        ("failed", "zzctxfailedresulte2ecanary"),
        ("diagnostic-bearing", "zzctxdiagnosticresulte2ecanary"),
        ("mixed", "zzctxmixedresulte2ecanary"),
        ("ambiguous", "zzctxambiguousresulte2ecanary"),
        ("malformed", "zzctxmalformedresulte2ecanary"),
        ("non-ctx", "zznonctxresulte2ecanary"),
    ] {
        let search = lexical_event_search(searchable);
        let results = search["results"].as_array().unwrap();
        assert!(
            results.iter().any(|result| {
                result["provider"] == "codex"
                    && result["snippet"]
                        .as_str()
                        .is_some_and(|snippet| snippet.contains(searchable))
            }),
            "{case} content did not fail open into lexical search: {search:#}"
        );
    }

    let listed = json_output(ctx(&temp).args([
        "list",
        "events",
        "--provider",
        "codex",
        "--provider-session",
        PROVIDER_SESSION_ID,
        "--content",
        "full",
        "--limit",
        "100",
        "--format=json",
    ]));
    let listed_events = listed["events"].as_array().unwrap();
    assert_eq!(listed_events.len(), 16, "{listed:#}");

    let exact_records = [
        ("direct-exact", "tool_call", EXCLUDED_DIRECT_INVOCATION),
        ("direct-exact", "command_output", EXCLUDED_DIRECT_PAYLOAD),
        ("mcp-ctx-exact", "tool_output", EXCLUDED_MCP_PAYLOAD),
    ]
    .map(|(native_event_id, event_type, oracle)| {
        let event = listed_events
            .iter()
            .find(|event| {
                event["event_type"] == event_type
                    && event["text"]
                        .as_str()
                        .is_some_and(|text| text.contains(oracle))
            })
            .unwrap_or_else(|| {
                panic!("missing listed {native_event_id}/{event_type} record: {listed:#}")
            });
        assert_eq!(event["content"]["complete"], true, "{event:#}");
        assert!(
            serde_json::to_string(&event["native_event_id"])
                .unwrap()
                .contains(native_event_id),
            "{event:#}"
        );
        event
    });

    assert_eq!(
        exact_records[2]["activity"]["invocation"]["server"], "ctx",
        "{:#}",
        exact_records[2]
    );
    assert_eq!(
        exact_records[2]["activity"]["invocation"]["tool"], "search",
        "{:#}",
        exact_records[2]
    );

    for record in exact_records {
        let event_id = record["ctx_event_id"].as_str().unwrap();
        let shown = json_output(ctx(&temp).args(["show", "event", event_id, "--format=json"]));
        assert_eq!(shown["event"]["ctx_event_id"], event_id, "{shown:#}");
        assert_eq!(shown["event"]["text"], record["text"], "{shown:#}");
        assert_eq!(shown["event"]["content"]["complete"], true, "{shown:#}");
    }

    let native_session = json_output(ctx(&temp).args([
        "show",
        "session",
        "--provider",
        "codex",
        "--provider-session",
        PROVIDER_SESSION_ID,
        "--mode",
        "log",
        "--max-events",
        "100",
        "--format=json",
    ]));
    assert_eq!(
        native_session["provider_session_id"], PROVIDER_SESSION_ID,
        "{native_session:#}"
    );
    let native_session_text = serde_json::to_string(&native_session["events"]).unwrap();
    for retained in [
        EXCLUDED_DIRECT_INVOCATION,
        EXCLUDED_DIRECT_PAYLOAD,
        EXCLUDED_MCP_PAYLOAD,
    ] {
        assert!(native_session_text.contains(retained), "{native_session:#}");
    }
}

#[test]
fn search_keeps_huge_matching_grapheme_hits_in_json_and_human_output() {
    const SNIPPET_MAX_BYTES: usize = 16 * 1024;
    const SESSION_ID: &str = "019c0000-0000-7000-8000-000000000001";

    let temp = tempdir();
    let oversized_cluster = format!("x{}", "\u{301}".repeat(SNIPPET_MAX_BYTES));
    let fixture = write_codex_message_fixture(
        &temp.path().join("huge-grapheme-fixture"),
        SESSION_ID,
        &oversized_cluster,
    );
    let _daemon = start_mcp_source_refresh_daemon(&temp);
    json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        fixture.to_str().unwrap(),
        "--format=json",
        "--progress=none",
        "--no-daemon",
    ]));

    let search_json = || {
        json_output(ctx(&temp).args([
            "search",
            "x",
            "--provider",
            "codex",
            "--events",
            "--include-current-session",
            "--refresh=off",
            "--format=json",
        ]))
    };
    let first = search_json();
    let second = search_json();
    let first_result = &first["results"][0];
    let second_result = &second["results"][0];
    assert_eq!(
        first["results"].as_array().map(Vec::len),
        Some(1),
        "{first:#}"
    );
    assert!(
        first_result["snippet"]
            .as_str()
            .is_some_and(|snippet| snippet.ends_with("/workspace/huge-grapheme")),
        "{first:#}"
    );
    assert_eq!(first_result["snippet_truncated"], true, "{first:#}");
    assert!(
        first_result["snippet"].as_str().unwrap().len() <= SNIPPET_MAX_BYTES,
        "{first:#}"
    );
    assert_eq!(first_result["snippet"], second_result["snippet"]);
    assert_eq!(
        first_result["snippet_truncated"],
        second_result["snippet_truncated"]
    );
    assert_eq!(first_result["ctx_event_id"], second_result["ctx_event_id"]);

    let render_human = || {
        let output = ctx(&temp)
            .args([
                "search",
                "x",
                "--provider",
                "codex",
                "--events",
                "--include-current-session",
                "--refresh=off",
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        String::from_utf8(output).unwrap()
    };
    let first_human = render_human();
    let second_human = render_human();
    assert_eq!(first_human, second_human);
    assert!(first_human.contains("1 result"), "{first_human}");
    assert!(
        first_human.contains("/workspace/huge-grapheme"),
        "{first_human}"
    );
    assert!(!first_human.contains('\u{301}'), "{first_human}");
}

#[test]
fn search_result_window_is_truthful_in_json_and_human_output() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions");
    let (_daemon, imported) = import_codex_fixture_through_daemon(&temp, &fixture);
    assert!(imported["sources"][0]["published_generation"].is_string());

    let limited = json_output(ctx(&temp).args([
        "search",
        "search",
        "--provider",
        "codex",
        "--limit",
        "1",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_eq!(limited["results"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        limited["result_window"],
        json!({
            "limit": 1,
            "returned": 1,
            "more_available": true,
        })
    );
    assert!(limited.get("pagination").is_none(), "{limited:#}");
    assert!(limited["truncation"]["candidate_pool"].is_number());
    assert!(limited["truncation"]["candidate_pool_truncated"].is_boolean());
    assert_eq!(limited["diversification"]["status"], "applied");
    assert_eq!(limited["diversification"]["top_n"], 1);
    assert!(limited["diversification"]["changed_final_top_n"].is_boolean());
    assert_eq!(limited["truncation"]["lexical"]["work_complete"], true);
    assert!(limited["truncation"]["lexical"]["candidate_set_exhaustive"].is_boolean());
    assert!(limited["result_window"].get("candidate_pool").is_none());

    let limited_human = ctx(&temp)
        .args([
            "search",
            "search",
            "--provider",
            "codex",
            "--limit",
            "1",
            "--refresh",
            "off",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let limited_human = String::from_utf8(limited_human).unwrap();
    assert_eq!(limited_human.matches("More results available.").count(), 1);
    assert!(
        limited_human.ends_with("More results available.\n"),
        "{limited_human}"
    );

    let complete = json_output(ctx(&temp).args([
        "search",
        "search",
        "--provider",
        "codex",
        "--limit",
        "200",
        "--refresh",
        "off",
        "--format=json",
    ]));
    let returned = complete["results"].as_array().map(Vec::len).unwrap();
    assert!(returned > 1, "{complete:#}");
    assert_eq!(
        complete["result_window"],
        json!({
            "limit": 200,
            "returned": returned,
            "more_available": false,
        })
    );
    assert_eq!(complete["diversification"]["status"], "applied");

    let complete_human = ctx(&temp)
        .args([
            "search",
            "search",
            "--provider",
            "codex",
            "--limit",
            "200",
            "--refresh",
            "off",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let complete_human = String::from_utf8(complete_human).unwrap();
    assert!(!complete_human.contains("More results available."));
}

fn measured_json_output(command: &mut Command) -> (Value, usize) {
    let output = command.assert().success().get_output().clone();
    let output_bytes = output.stdout.len();
    let value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("invalid JSON output: {error}: {output:#?}"));
    (value, output_bytes)
}

#[path = "../support/search_show/import_paths.rs"]
mod import_paths;

#[test]
fn search_excludes_the_exact_active_tree_across_codex_session_sources() {
    let temp = tempdir();
    let fixture = temp.path().join("active-tree-search-fixture");
    copy_dir_all(
        Path::new(&provider_history_fixture("codex-sessions")),
        &fixture,
    );
    let child_fixture = fixture.join("2026/06/23/codex-session-child.jsonl");
    let grandchild_fixture = fixture.join("2026/06/23/codex-session-grandchild.jsonl");
    let grandchild_records = fs::read_to_string(child_fixture)
        .unwrap()
        .replace("codex-session-child", "codex-session-grandchild")
        .replace("codex-session-root", "codex-session-child")
        .replace("\"depth\":1", "\"depth\":2");
    fs::write(grandchild_fixture, grandchild_records).unwrap();
    write_codex_message_fixture(
        &fixture,
        "codex-independent-root",
        "Archived local history search remains independently searchable.",
    );
    json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        fixture.to_str().unwrap(),
        "--format=json",
        "--progress",
        "none",
    ]));

    let excluded = json_output(
        ctx(&temp)
            .env("CODEX_THREAD_ID", "codex-independent-root")
            .args([
                "search",
                "independently",
                "--provider",
                "codex",
                "--refresh",
                "off",
                "--format=json",
            ]),
    );
    assert_eq!(excluded["results"].as_array().unwrap().len(), 0);
    assert!(excluded["filters"]["include_current_session"].is_null());

    let excluded_tree = json_output(
        ctx(&temp)
            .env("CODEX_THREAD_ID", "codex-session-root")
            .args([
                "search",
                "local history search",
                "--provider",
                "codex",
                "--refresh",
                "off",
                "--format=json",
            ]),
    );
    let excluded_tree_sessions = excluded_tree["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|result| result["provider_session_id"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        excluded_tree_sessions,
        BTreeSet::from(["codex-independent-root"]),
        "cross-source active tree was not excluded: {excluded_tree:#}"
    );

    let excluded_resumed_tree = json_output(
        ctx(&temp)
            .env("CODEX_THREAD_ID", "codex-session-child")
            .args([
                "search",
                "local history search",
                "--provider",
                "codex",
                "--refresh",
                "off",
                "--format=json",
            ]),
    );
    let excluded_resumed_sessions = excluded_resumed_tree["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|result| result["provider_session_id"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        excluded_resumed_sessions,
        BTreeSet::from(["codex-independent-root"]),
        "cross-source resumed child tree was not excluded: {excluded_resumed_tree:#}"
    );

    let child_session = json_output(ctx(&temp).args([
        "show",
        "session",
        "--provider",
        "codex",
        "--provider-session",
        "codex-session-child",
        "--format=json",
    ]));
    let child_session_id = child_session["ctx_session_id"].as_str().unwrap();
    let explicit_resumed_child = json_output(
        ctx(&temp)
            .env("CODEX_THREAD_ID", "codex-session-child")
            .args([
                "search",
                "subagent",
                "--session",
                child_session_id,
                "--refresh",
                "off",
                "--format=json",
            ]),
    );
    assert_eq!(
        explicit_resumed_child["filters"]["session"],
        child_session_id
    );
    assert!(
        explicit_resumed_child["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|result| result["ctx_session_id"] == child_session_id),
        "explicit child selection escaped its exact session: {explicit_resumed_child:#}"
    );
    assert!(
        !explicit_resumed_child["results"].as_array().unwrap().is_empty(),
        "explicit child selection inherited the default primary-only suppression: {explicit_resumed_child:#}"
    );

    let explicit_primary_child = json_output(
        ctx(&temp)
            .env("CODEX_THREAD_ID", "codex-session-child")
            .args([
                "search",
                "subagent",
                "--session",
                child_session_id,
                "--primary-only",
                "--refresh",
                "off",
                "--format=json",
            ]),
    );
    assert!(
        explicit_primary_child["results"]
            .as_array()
            .unwrap()
            .is_empty(),
        "explicit --primary-only did not retain authority: {explicit_primary_child:#}"
    );

    let root_session = json_output(ctx(&temp).args([
        "show",
        "session",
        "--provider",
        "codex",
        "--provider-session",
        "codex-session-root",
        "--format=json",
    ]));
    let root_session_id = root_session["ctx_session_id"].as_str().unwrap();
    let explicit_root = json_output(
        ctx(&temp)
            .env("CODEX_THREAD_ID", "codex-session-child")
            .args([
                "search",
                "local history search",
                "--session",
                root_session_id,
                "--refresh",
                "off",
                "--format=json",
            ]),
    );
    assert!(
        !explicit_root["results"].as_array().unwrap().is_empty()
            && explicit_root["results"]
                .as_array()
                .unwrap()
                .iter()
                .all(|result| result["ctx_session_id"] == root_session_id),
        "explicit root selection included descendants or another root: {explicit_root:#}"
    );

    let child_session_prefix = &child_session_id[..8];
    let manually_excluded = json_output(ctx(&temp).args([
        "search",
        "local history search",
        "--provider",
        "codex",
        "--events",
        "--exclude-session",
        root_session_id,
        "--exclude-session",
        child_session_prefix,
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_eq!(
        manually_excluded["filters"]["exclude_session"],
        json!([root_session_id, child_session_prefix])
    );
    assert!(
        manually_excluded["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|result| result["ctx_session_id"] != root_session_id
                && result["ctx_session_id"] != child_session_id),
        "repeatable manual exclusions leaked an excluded session: {manually_excluded:#}"
    );

    let included = json_output(
        ctx(&temp)
            .env("CODEX_THREAD_ID", "codex-session-root")
            .args([
                "search",
                "onboarding",
                "--provider",
                "codex",
                "--refresh",
                "off",
                "--include-current-session",
                "--format=json",
            ]),
    );
    assert_search_provider_oracle(&included, "codex", "onboarding", 1, "message");
    assert_eq!(included["filters"]["include_current_session"], true);

    let included_tree = json_output(
        ctx(&temp)
            .env("CODEX_THREAD_ID", "codex-session-root")
            .args([
                "search",
                "local history search",
                "--provider",
                "codex",
                "--refresh",
                "off",
                "--include-current-session",
                "--format=json",
            ]),
    );
    let included_provider_sessions = included_tree["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|result| result["provider_session_id"].as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        included_provider_sessions,
        BTreeSet::from([
            "codex-independent-root",
            "codex-session-child",
            "codex-session-grandchild",
            "codex-session-root",
        ])
    );
}

#[test]
fn show_does_not_initialize_core_storage() {
    let temp = tempdir();
    let stderr = failure_stderr(ctx(&temp).args(["show", "event", "deadbeef"]));
    assert!(stderr.contains("the Core index does not exist; retry with daemon refresh enabled"));
    assert!(!temp.path().join("work.sqlite").exists());
    assert!(!data_root(&temp).join("relational.sqlite").exists());
}

include!("../support/search_show/search_flows.rs");

#[test]
fn search_backend_defaults_and_supported_semantic_config_are_reported() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions");
    json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &fixture,
        "--format=json",
        "--progress",
        "none",
    ]));

    let default_search = json_output(ctx(&temp).args([
        "search",
        "semantic-only-missing-sidecar",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_eq!(default_search["retrieval"]["requested_mode"], "lexical");
    assert_eq!(default_search["retrieval"]["effective_mode"], "lexical");
    assert!(default_search["retrieval"]["semantic_fallback_code"].is_null());

    let hybrid = json_output(ctx(&temp).args([
        "search",
        "onboarding",
        "--backend",
        "hybrid",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_eq!(hybrid["retrieval"]["requested_mode"], "hybrid");
    assert_eq!(hybrid["retrieval"]["effective_mode"], "lexical");
    assert_eq!(
        hybrid["retrieval"]["semantic_fallback_code"],
        "semantic_disabled"
    );

    let disabled_strict_semantic = ctx(&temp)
        .args([
            "search",
            "onboarding",
            "--backend",
            "semantic",
            "--refresh",
            "off",
            "--format=json",
        ])
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(
        disabled_strict_semantic.stdout.is_empty(),
        "{disabled_strict_semantic:#?}"
    );
    let disabled_strict_semantic: Value =
        serde_json::from_slice(&disabled_strict_semantic.stderr).unwrap();
    assert_eq!(
        disabled_strict_semantic["error_code"], "semantic_disabled",
        "{disabled_strict_semantic:#}"
    );
    assert_eq!(disabled_strict_semantic["retryable"], false);
    assert!(disabled_strict_semantic["detail"]
        .as_str()
        .is_some_and(|detail| detail.contains("semantic search is disabled")));

    fs::write(
        data_root(&temp).join("config.toml"),
        "[daemon]\nenabled = true\n\n[search]\nsemantic = true\n",
    )
    .unwrap();

    let supported_hybrid = json_output(ctx(&temp).args([
        "search",
        "onboarding",
        "--backend",
        "hybrid",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_eq!(supported_hybrid["retrieval"]["requested_mode"], "hybrid");
    assert_eq!(supported_hybrid["retrieval"]["effective_mode"], "lexical");
    assert_eq!(
        supported_hybrid["retrieval"]["semantic_fallback_code"],
        "semantic_store_missing"
    );

    let missing_index_strict_semantic = ctx(&temp)
        .args([
            "search",
            "onboarding",
            "--backend",
            "semantic",
            "--refresh",
            "off",
            "--format=json",
        ])
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(
        missing_index_strict_semantic.stdout.is_empty(),
        "{missing_index_strict_semantic:#?}"
    );
    let missing_index_strict_semantic: Value =
        serde_json::from_slice(&missing_index_strict_semantic.stderr).unwrap();
    assert!(
        matches!(
            missing_index_strict_semantic["error_code"].as_str(),
            Some("semantic_store_missing" | "semantic_generation_not_acknowledged")
        ),
        "{missing_index_strict_semantic:#}"
    );
    assert_eq!(missing_index_strict_semantic["retryable"], true);
    assert!(missing_index_strict_semantic["detail"]
        .as_str()
        .is_some_and(|detail| detail.contains("flat-F32")));

    let explicit_lexical = json_output(ctx(&temp).args([
        "search",
        "onboarding",
        "--backend",
        "lexical",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_eq!(explicit_lexical["retrieval"]["requested_mode"], "lexical");
    assert_eq!(explicit_lexical["retrieval"]["effective_mode"], "lexical");

    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(status["semantic"]["status"], "pending");
    assert!(
        matches!(
            status["semantic"]["reason"].as_str(),
            Some(
                "flat_f32_projection_missing"
                    | "projection_control_missing"
                    | "generation_not_acknowledged"
            )
        ),
        "{status:#}"
    );
    assert_eq!(status["semantic"]["enabled"], true);
    assert_eq!(status["semantic"]["config_source"], "config");
}

#[test]
fn doctor_reports_missing_store_without_creating_it() {
    let temp = tempdir();

    let doctor = json_output(ctx(&temp).args(["doctor", "--format=json"]));

    assert_eq!(doctor["schema_version"], 1);
    assert_eq!(doctor["ok"], false);
    assert_eq!(doctor["daemon"]["enabled"], true);
    assert!(doctor["source_epoch"]["lexical"]["status"].is_string());
    assert!(doctor["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|finding| { finding.as_str().unwrap().starts_with("lexical is ") }));
    assert!(
        !temp.path().join("work.sqlite").exists(),
        "doctor should not create the ctx store"
    );
}

#[test]
fn codex_cli_resume_is_idempotent_rescan_and_defaults_to_all_agents() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions");
    let _daemon = start_source_refresh_daemon(&temp);

    let first = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &fixture,
        "--no-daemon",
        "--format=json",
    ]));
    assert_explicit_source_publication(&first, "codex", "codex_session_jsonl_tree");
    let first_generation = first["sources"][0]["published_generation"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(first["resume"], false);
    assert_eq!(first["resume_mode"], "normal_scan");
    wait_for_test_lexical_projection(&temp, &first_generation);
    assert_eq!(provider_core_counts(&data_root(&temp), "codex"), (2, 8));

    let all_agents_default =
        json_output(ctx(&temp).args(["search", "subagent", "--refresh", "off", "--format=json"]));
    assert!(all_agents_default["filters"]
        .get("include_subagents")
        .is_none());
    let all_agents_default_text = serde_json::to_string(&all_agents_default).unwrap();
    assert!(
        all_agents_default_text.contains("codex-session-child"),
        "{all_agents_default_text}"
    );

    let family_shaped = json_output(ctx(&temp).args([
        "search",
        "local history search",
        "--limit",
        "1",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_eq!(
        family_shaped["results"][0]["provider_session_id"],
        "codex-session-child"
    );
    assert_eq!(family_shaped["results"][0]["agent_scope"], "subagent");
    assert_eq!(family_shaped["result_window"]["more_available"], true);
    assert_eq!(family_shaped["diversification"]["status"], "applied");

    let default_events = json_output(ctx(&temp).args([
        "search",
        "subagent",
        "--events",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert!(default_events["filters"].get("include_subagents").is_none());
    assert_eq!(
        default_events["diversification"]["status"],
        "not_applicable"
    );
    let default_events_text = serde_json::to_string(&default_events).unwrap();
    assert!(
        default_events_text.contains("codex-session-child"),
        "{default_events_text}"
    );

    let child_session = json_output(ctx(&temp).args([
        "show",
        "session",
        "--provider",
        "codex",
        "--provider-session",
        "codex-session-child",
        "--format",
        "json",
    ]));
    assert_eq!(child_session["provider_session_id"], "codex-session-child");
    let child_session_id = child_session["ctx_session_id"].as_str().unwrap();
    let explicit_child_session = json_output(ctx(&temp).args([
        "search",
        "subagent",
        "--session",
        child_session_id,
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_eq!(
        explicit_child_session["filters"]["session"],
        child_session_id
    );
    assert!(serde_json::to_string(&explicit_child_session)
        .unwrap()
        .contains("codex-session-child"));
    assert_eq!(
        explicit_child_session["diversification"]["status"],
        "not_applicable"
    );
    for result in explicit_child_session["results"].as_array().unwrap() {
        assert_eq!(result["result_scope"], "event");
        assert!(result.get("more_matches_in_session").is_none());
        assert!(result.get("copied_lineage").is_none());
    }

    let primary_only = json_output(ctx(&temp).args([
        "search",
        "subagent",
        "--primary-only",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert!(primary_only["filters"].get("include_subagents").is_none());
    assert_eq!(primary_only["filters"]["primary_only"], true);
    assert!(
        primary_only["results"].as_array().unwrap().len()
            <= all_agents_default["results"].as_array().unwrap().len()
    );
    assert!(!serde_json::to_string(&primary_only)
        .unwrap()
        .contains("codex-session-child"));

    let second = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &fixture,
        "--resume",
        "--no-daemon",
        "--format=json",
    ]));
    assert_explicit_source_publication(&second, "codex", "codex_session_jsonl_tree");
    assert_eq!(second["resume"], true);
    assert_eq!(second["resume_mode"], "idempotent_rescan");
    assert_eq!(second["totals"]["current_rejected_records"], 0);
    assert_noop_publication(&second);
    assert!(!data_root(&temp).join("relational.sqlite").exists());
}

#[test]
fn search_rejects_unbounded_limit() {
    let temp = tempdir();
    ctx(&temp)
        .args(["search", "anything", "--limit", "201"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid value"));
}

#[test]
fn codex_cli_default_import_uses_catalog_state_for_incremental_catch_up() {
    let temp = tempdir();
    let _daemon = start_source_refresh_daemon(&temp);
    let fixture = provider_history_fixture("codex-sessions");

    let first = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &fixture,
        "--no-daemon",
        "--format=json",
    ]));
    assert_explicit_source_publication(&first, "codex", "codex_session_jsonl_tree");
    assert_eq!(first["resume"], false);
    assert_eq!(first["resume_mode"], "normal_scan");
    assert_eq!(first["totals"]["current_rejected_records"], 0);
    let first_generation = first["sources"][0]["published_generation"]
        .as_str()
        .unwrap()
        .to_owned();

    let status_deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        let status = json_output(ctx(&temp).args(["status", "--format=json"]));
        if status["indexed_sessions"] == 2
            && status["indexed_events"] == 8
            && status["indexed_sources"] == 2
            && status["lexical"]["status"] == "ready"
        {
            break status;
        }
        assert!(
            Instant::now() < status_deadline,
            "timed out waiting for imported catalog state: {status:#}"
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(status["indexed_sessions"], 2);
    assert_eq!(status["indexed_events"], 8);
    assert_eq!(status["indexed_sources"], 2);
    assert_eq!(status["lexical"]["status"], "ready");
    assert!(!data_root(&temp).join("relational.sqlite").exists());

    let second = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &fixture,
        "--no-daemon",
        "--format=json",
    ]));
    assert_explicit_source_publication(&second, "codex", "codex_session_jsonl_tree");
    assert_eq!(second["resume"], false);
    assert_eq!(second["resume_mode"], "normal_scan");
    assert_eq!(second["totals"]["current_rejected_records"], 0);
    assert_noop_publication(&second);
    assert_eq!(
        second["sources"][0]["published_generation"], first_generation,
        "{second:#}"
    );
    assert!(!data_root(&temp).join("relational.sqlite").exists());
}

#[test]
fn codex_cli_provider_oracle_covers_retrieval_and_claimed_fidelity() {
    let temp = tempdir();
    let _daemon = start_source_refresh_daemon(&temp);
    let fixture = temp.path().join("combined-codex-sessions");
    copy_dir_all(
        Path::new(&provider_history_fixture("codex-sessions")),
        &fixture,
    );
    copy_dir_all(
        Path::new(&provider_history_fixture("codex-rich-sessions")),
        &fixture,
    );

    let imported = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        fixture.to_str().unwrap(),
        "--no-daemon",
        "--format=json",
    ]));
    assert_explicit_source_publication(&imported, "codex", "codex_session_jsonl_tree");
    let imported_generation = imported["sources"][0]["published_generation"]
        .as_str()
        .unwrap();
    wait_for_test_lexical_projection(&temp, imported_generation);

    let query = "setup flow";
    let search = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "codex",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_source_backed_search(&search, "codex", query);

    let records = provider_core_records(&data_root(&temp), "codex");
    assert_eq!(provider_core_counts(&data_root(&temp), "codex"), (3, 16));
    assert_eq!(
        records
            .iter()
            .filter(
                |record| record.event_type == "message" && record.role.as_deref() == Some("user")
            )
            .count(),
        3
    );
    assert_eq!(
        records
            .iter()
            .filter(|record| record.event_type == "message"
                && record.role.as_deref() == Some("assistant"))
            .count(),
        2
    );
    assert_eq!(
        records
            .iter()
            .filter(|record| record.event_type == "tool_call")
            .count(),
        4
    );
    assert_eq!(
        records
            .iter()
            .filter(|record| record.event_type == "tool_output")
            .count(),
        2
    );
    assert_eq!(
        records
            .iter()
            .filter(|record| record.event_type == "command_output")
            .count(),
        2
    );
    for record in &records {
        let value = serde_json::to_value(record).unwrap();
        for removed in [
            "repository_candidate_evidence",
            "repository_bindings",
            "repository_abstentions",
            "repository_file_invocation_evidence",
            "repository_file_observations",
            "repository_vcs_observations",
            "repository_outcome_observations",
        ] {
            assert!(
                value.get(removed).is_none(),
                "removed repository projection {removed} leaked into Core: {value:#}"
            );
        }
    }
    assert!(
        !temp.path().join("work.sqlite").exists(),
        "Codex acceptance must use the Core generation"
    );
    assert!(!data_root(&temp).join("relational.sqlite").exists());
}

include!("../support/search_show/pi_flow.rs");
include!("../support/search_show/search_filters.rs");
