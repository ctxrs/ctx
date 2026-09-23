// Adapted from Sift tests, MIT; see this crate’s NOTICE.
use serde_json::{Value, json};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
include!("tests/fixtures/pi_delivered.rs");

struct Session {
    root: PathBuf,
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
}

impl Session {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "sift-pi-session-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let mut child = crate::test_support::command([
            "compact",
            "--protocol=session-v1",
            "--record-source",
            "pi",
            "--record-tool",
            "bash",
        ])
        .current_dir(&root)
        .env("SIFT_CONFIG_DIR", root.join("config"))
        .env("SIFT_STATE_DIR", root.join("state"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
        let input = child.stdin.take();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        crate::test_support::skip_header(&mut output);
        let mut session = Self {
            root,
            child,
            input,
            output,
        };
        assert_eq!(session.line(), Some(json!({"version":1,"session":1})));
        session
    }

    fn line(&mut self) -> Option<Value> {
        let mut line = String::new();
        (self.output.read_line(&mut line).unwrap() > 0)
            .then(|| serde_json::from_str(&line).unwrap())
    }

    fn request(&mut self, id: u64, text: &str, command: Option<&str>) -> Value {
        let mut envelope = json!({"id":id,"request":{"version":1,"text":text}});
        if let Some(command) = command {
            envelope["command"] = json!(command);
        }
        self.envelope(envelope)
    }

    fn envelope(&mut self, envelope: Value) -> Value {
        let id = envelope["id"].as_u64().unwrap();
        writeln!(self.input.as_mut().unwrap(), "{envelope}").unwrap();
        let response = self.line().unwrap();
        assert_eq!(response["id"], id);
        assert_eq!(self.line(), Some(json!({"version":1,"id":id,"done":true})));
        response
    }

    fn delivered(&mut self, id: u64, text: &str, command: &str) -> Value {
        self.envelope(json!({"id":id,"command":command,"delivered_view":true,
            "request":{"version":1,"text":text,"is_error":true,"complete":false}}))
    }

    fn settings(&self, value: Value) {
        fs::create_dir_all(self.root.join("config")).unwrap();
        fs::write(self.root.join("config/config.json"), value.to_string()).unwrap();
    }

    fn events(&self) -> Vec<Value> {
        fs::read_to_string(self.root.join("state/metrics.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.input.take();
        // Every normal test closes stdin; also reap on an assertion failure.
        if std::thread::panicking() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn recognized_raw_has_exact_counts_and_never_records_or_stores() {
    let mut session = Session::new();
    session.settings(json!({"keep_originals":true}));
    let text = "α repeated diagnostic 🦀\r\n".repeat(50);
    let tokens = tiktoken_rs::o200k_base()
        .unwrap()
        .encode_ordinary(&text)
        .len();
    for (i, command) in [
        "sift proxy -- cat fixture",
        "'/opt/tool space/sift' run --capture --raw -- cat fixture",
        "command /opt/sift.exe run --raw --capture -- cat fixture",
        "sift run --raw --raw cat fixture",
        "sift proxy cat --raw",
    ]
    .iter()
    .enumerate()
    {
        let response = session.request(i as u64 + 1, &text, Some(command));
        assert_eq!(response["text"], text);
        assert_eq!(response["encoding"], "raw");
        assert_eq!(response["input_tokens"], tokens);
        assert_eq!(response["output_tokens"], tokens);
    }
    assert!(!session.root.join("state").exists());
}

#[test]
fn ordinary_unknown_and_missing_commands_keep_generic_compaction_and_metrics() {
    let mut session = Session::new();
    let text = "repeated original output without inferred semantics\n".repeat(60);
    let expected = sift::Compactor::new().unwrap().compact(&text);
    assert_ne!(expected.text, text);
    let commands = [
        None,
        Some("cat fixture"),
        Some("git status"),
        Some("cargo test"),
        Some("sift run -- cat --raw"),
        Some("sift proxy --help"),
        Some("sift run --raw --"),
        Some("printf x; cat fixture"),
        Some("cat 'file\\name'"),
        Some("cat fixture\ntrue"),
        Some("sift proxy cat fixture; true"),
        Some("'unclosed"),
    ];
    for (i, command) in commands.iter().enumerate() {
        let response = session.request(i as u64 + 1, &text, *command);
        assert_eq!(response["text"], expected.text, "{command:?}");
        assert_eq!(response["input_tokens"], expected.input_tokens);
        assert_eq!(response["output_tokens"], expected.output_tokens);
    }
    let events = session.events();
    assert_eq!(events.len(), commands.len());
    for event in events {
        assert_eq!(event["source"], "pi");
        assert_eq!(event["command"], "bash");
        assert_eq!(event["input_bytes"], text.len());
        assert_eq!(event["output_bytes"], expected.text.len());
        assert_eq!(event["input_tokens"], expected.input_tokens);
        assert_eq!(event["output_tokens"], expected.output_tokens);
        assert!(event["exit_code"].is_null());
        assert!(event["original_id"].is_null());
    }
    assert!(!session.root.join("state/originals").exists());
}

#[test]
fn git_context_is_opt_in_and_unterminated_fields_stay_generic() {
    let mut session = Session::new();
    let mut text = String::from("On branch main\nChanges to be committed:\n");
    for i in 0..60 {
        text.push_str(&format!("\tmodified:   src/ordinary_case_{i:03}.rs\n"));
    }
    let expected = sift::Compactor::new().unwrap().compact(&text);
    let response = session.request(1, &text, Some("git status"));
    assert_eq!(response["text"], expected.text);
    assert_eq!(response["input_tokens"], expected.input_tokens);
    assert_eq!(response["output_tokens"], expected.output_tokens);
    assert!(response.get("semantic").is_none());
    let mut proposal = String::from("## main\n");
    for i in 0..60 {
        proposal.push_str(&format!("M  src/ordinary_case_{i:03}.rs\n"));
    }
    let selected = sift::Compactor::new().unwrap().compact(&proposal);
    let response = session.delivered(2, &text, "git status");
    if selected.output_tokens < expected.output_tokens {
        assert_eq!(response["text"], selected.text);
        assert_eq!(response["semantic"], true);
    } else {
        assert_eq!(response["text"], expected.text);
        assert!(response.get("semantic").is_none());
    }
    let partial = text.trim_end_matches('\n');
    let expected = sift::Compactor::new().unwrap().compact(partial);
    let response = session.delivered(3, partial, "git status");
    assert_eq!(response["text"], expected.text);
    assert!(response.get("semantic").is_none());
}

#[test]
fn session_reloads_settings_and_raw_never_records_even_with_originals_enabled() {
    let mut session = Session::new();
    let text = "complete text remains recoverable\n".repeat(50);
    let expected = sift::Compactor::new().unwrap().compact(&text);
    assert_eq!(
        session.request(1, &text, Some("cat fixture"))["text"],
        expected.text
    );
    session.settings(json!({"enabled":false}));
    assert_eq!(session.request(2, &text, Some("cat fixture"))["text"], text);
    session.settings(json!({"exclude_commands":["bash"]}));
    assert_eq!(session.request(3, &text, Some("cat fixture"))["text"], text);
    session.settings(json!({"record_usage":false,"keep_originals":true}));
    assert_eq!(
        session.request(4, &text, Some("cat fixture"))["text"],
        expected.text
    );
    assert_eq!(session.events().len(), 3);
    assert!(!session.root.join("state/originals").exists());
    session.settings(json!({"keep_originals":true}));
    assert_eq!(
        session.request(5, &text, Some("sift proxy -- cat fixture"))["text"],
        text
    );
    assert_eq!(session.events().len(), 3);
    assert!(!session.root.join("state/originals").exists());
    assert_eq!(session.request(6, &text, None)["text"], expected.text);
    let events = session.events();
    assert_eq!(events.len(), 4);
    assert!(events[3]["original_id"].is_string());
}

#[test]
fn changed_invalid_settings_retire_session_without_new_record() {
    let mut session = Session::new();
    session.request(1, "ordinary", Some("cat fixture"));
    assert_eq!(session.events().len(), 1);
    fs::create_dir_all(session.root.join("config")).unwrap();
    fs::write(session.root.join("config/config.json"), "not JSON").unwrap();
    writeln!(
        session.input.as_mut().unwrap(),
        "{}",
        json!({"id":2,
        "command":"sift proxy -- cat fixture", "request":{"version":1,"text":"unchanged"}})
    )
    .unwrap();
    assert!(session.line().unwrap()["error"].is_string());
    assert_eq!(session.line(), None);
    assert!(!session.child.wait().unwrap().success());
    assert_eq!(session.events().len(), 1);
    assert_eq!(
        fs::read_to_string(session.root.join("config/config.json")).unwrap(),
        "not JSON"
    );
}

#[test]
fn combined_utf8_limit_and_invalid_command_metadata_fail_without_recording() {
    for command in [json!(true), json!("🦀".repeat(2 * 1024 * 1024))] {
        let mut session = Session::new();
        let request = json!({"id":1,"command":command,"request":{"version":1,"text":"x"}});
        writeln!(session.input.as_mut().unwrap(), "{request}").unwrap();
        assert!(session.line().unwrap()["error"].is_string());
        assert_eq!(session.line(), None);
        assert!(!session.child.wait().unwrap().success());
        assert!(!session.root.join("state").exists());
    }
}

#[test]
fn generic_protocol_does_not_accept_command_and_session_requires_pi_bash() {
    let mut child = crate::test_support::command(["compact", "--protocol=json-v1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"{\"version\":1,\"text\":\"ordinary\",\"command\":\"sift proxy cat\"}\n")
        .unwrap();
    let result = child
        .wait_with_output()
        .map(crate::test_support::clean)
        .unwrap();
    assert!(!result.status.success());
    assert!(serde_json::from_slice::<Value>(&result.stdout).unwrap()["error"].is_string());
    for (source, tool) in [("omp", "bash"), ("pi", "powershell"), ("opencode", "bash")] {
        let result = crate::test_support::command([
            "compact",
            "--protocol=session-v1",
            "--record-source",
            source,
            "--record-tool",
            tool,
        ])
        .stdin(Stdio::null())
        .output()
        .map(crate::test_support::clean)
        .unwrap();
        assert!(!result.status.success());
        assert!(result.stdout.is_empty());
    }
}

#[test]
fn session_v2_validates_selection_and_fails_open_without_api_key() {
    static NEXT: AtomicU64 = AtomicU64::new(10_000);
    let root = std::env::temp_dir().join(format!(
        "sift-pi-session-v2-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).unwrap();
    // Own this fixture's project even when the test temp root is inside a checkout.
    fs::create_dir(root.join(".git")).unwrap();
    fs::create_dir(root.join("config")).unwrap();
    let project = fs::canonicalize(&root)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    fs::write(
        root.join("config/config.json"),
        json!({"record_usage":false,"semantic_selection":{"mode":"select","allowed_projects":[project]}}).to_string(),
    )
    .unwrap();
    let mut child = crate::test_support::command([
        "compact",
        "--protocol=session-v2",
        "--record-source",
        "pi",
        "--record-tool",
        "pi",
    ])
    .current_dir(&root)
    .env("SIFT_CONFIG_DIR", root.join("config"))
    .env("SIFT_STATE_DIR", root.join("state"))
    .env_remove("TYPESAFE_API_KEY")
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .spawn()
    .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    crate::test_support::skip_header(&mut output);
    let read = |output: &mut BufReader<ChildStdout>| {
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        serde_json::from_str::<Value>(&line).unwrap()
    };
    assert_eq!(read(&mut output), json!({"version":1,"session":2}));
    let text = "alpha repeated output\n".repeat(120);
    let first = text.len() / 3;
    let second = first * 2;
    let selection = json!({"policy":"sift-semantic-v1","task":"find alpha","path":project,"kind":"pi-grep-v1","passages":[
        {"id":"p1","start":0,"end":first,"required":false},
        {"id":"p2","start":first,"end":second,"required":false},
        {"id":"p3","start":second,"end":text.len(),"required":false}
    ]});
    let expected = sift::Compactor::new().unwrap().compact(&text);
    for (id, tool, candidate) in [
        (1, "grep", json!({"policy":"bad"})),
        (2, "grep", selection.clone()),
        (3, "bash", selection),
    ] {
        writeln!(
            input,
            "{}",
            json!({"id":id,"tool":tool,"selection":candidate,"request":{"version":1,"text":text}})
        )
        .unwrap();
        let response = read(&mut output);
        assert_eq!(response["text"], expected.text);
        assert!(response.get("semantic").is_none());
        assert_eq!(read(&mut output), json!({"version":1,"id":id,"done":true}));
    }
    let (cargo, proposal) = cargo_fixture(false, "");
    let ordinary = sift::Compactor::new().unwrap().compact(&cargo);
    let selected = sift::Compactor::new().unwrap().compact(&proposal);
    writeln!(
        input,
        "{}",
        json!({"id":4,"tool":"bash","command":"cargo test","delivered_view":true,
            "request":{"version":1,"text":cargo}})
    )
    .unwrap();
    let response = read(&mut output);
    if selected.output_tokens < ordinary.output_tokens {
        assert_eq!(response["text"], selected.text);
        assert_eq!(response["semantic"], true);
    } else {
        assert_eq!(response["text"], ordinary.text);
        assert!(response.get("semantic").is_none());
    }
    assert_eq!(read(&mut output), json!({"version":1,"id":4,"done":true}));
    drop(input);
    assert!(child.wait().unwrap().success());
    let receipts = fs::read_to_string(root.join("state/semantic.jsonl")).unwrap();
    let receipt: Value = serde_json::from_str(receipts.trim()).unwrap();
    assert_eq!(receipt["status"], "disabled");
    assert_eq!(receipt["disposition"], "fallback");
    assert!(!root.join("state/metrics.jsonl").exists());
    assert!(!root.join("state/originals").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn delivered_selection_marks_semantics_and_keeps_full_original_metrics_and_storage() {
    let mut session = Session::new();
    session.settings(json!({"keep_originals":true}));
    let (text, proposal) = cargo_fixture(
        true,
        "\n[Showing lines 8-80 of 80. Full output: /tmp/synthetic.log]\nCommand exited with code 101",
    );
    let compactor = sift::Compactor::new().unwrap();
    let original = compactor.compact(&text);
    let selected = compactor.compact(&proposal);
    assert!(selected.output_tokens < original.output_tokens);
    let response = session.delivered(1, &text, "cargo test --offline");
    assert_eq!(response["semantic"], true);
    assert_eq!(response["text"], selected.text);
    let encoding: sift::Encoding = serde_json::from_value(response["encoding"].clone()).unwrap();
    assert_eq!(
        sift::restore(encoding, response["text"].as_str().unwrap()).unwrap(),
        proposal
    );
    assert_ne!(proposal, text);
    let counter = tiktoken_rs::o200k_base().unwrap();
    assert_eq!(
        response["input_tokens"],
        counter.encode_ordinary(&text).len()
    );
    assert_eq!(
        response["output_tokens"],
        counter.encode_ordinary(&selected.text).len()
    );
    let events = session.events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["input_tokens"], response["input_tokens"]);
    assert_eq!(events[0]["output_tokens"], response["output_tokens"]);
    assert_eq!(events[0]["input_bytes"], text.len());
    assert_eq!(events[0]["output_bytes"], selected.text.len());
    assert!(events[0]["exit_code"].is_null());
    assert_eq!(events[0]["command"], "bash");
    assert_eq!(events[0]["source"], "pi");
    let saved = session
        .root
        .join("state/originals")
        .join(events[0]["original_id"].as_str().unwrap());
    assert_eq!(fs::read(saved.join("stdout")).unwrap(), text.as_bytes());
    assert_eq!(fs::read(saved.join("stderr")).unwrap(), b"");
}

#[test]
fn delivered_view_is_default_off_and_declines_ordinary_unknown_context() {
    let mut session = Session::new();
    let (text, _) = cargo_fixture(false, "partial suffix");
    let expected = sift::Compactor::new().unwrap().compact(&text);
    let rows = [
        json!({"id":1,"command":"cargo test","request":{"version":1,"text":text}}),
        json!({"id":2,"command":"cargo test","delivered_view":false,"request":{"version":1,"text":text}}),
        json!({"id":3,"delivered_view":true,"request":{"version":1,"text":text}}),
        json!({"id":4,"command":"cargo test; true","delivered_view":true,"request":{"version":1,"text":text}}),
        json!({"id":5,"command":"cat 'file\\name'","delivered_view":true,"request":{"version":1,"text":text}}),
    ];
    for row in rows {
        let response = session.envelope(row);
        assert!(response.get("semantic").is_none());
        assert_eq!(response["text"], expected.text);
        assert_eq!(response["input_tokens"], expected.input_tokens);
        assert_eq!(response["output_tokens"], expected.output_tokens);
    }
}

#[test]
fn delivered_raw_settings_exclusions_and_recording_controls_take_precedence() {
    let mut session = Session::new();
    let (text, _) = cargo_fixture(false, "");
    session.settings(json!({"keep_originals":true}));
    for (i, command) in ["sift proxy -- cargo test", "sift run --raw -- cargo test"]
        .iter()
        .enumerate()
    {
        let response = session.delivered(i as u64 + 1, &text, command);
        assert_eq!(response["text"], text);
        assert_eq!(response["input_tokens"], response["output_tokens"]);
        assert!(response.get("semantic").is_none());
    }
    assert!(!session.root.join("state").exists());
    for (i, settings) in [
        json!({"enabled":false}),
        json!({"exclude_commands":["bash"]}),
    ]
    .into_iter()
    .enumerate()
    {
        session.settings(settings);
        let response = session.delivered(i as u64 + 3, &text, "cargo test");
        assert_eq!(response["text"], text);
        assert!(response.get("semantic").is_none());
    }
    assert_eq!(session.events().len(), 2);
    session.settings(json!({"record_usage":false,"keep_originals":true}));
    assert_eq!(session.delivered(5, &text, "cargo test")["semantic"], true);
    assert_eq!(session.events().len(), 2);
    assert!(!session.root.join("state/originals").exists());
}

#[test]
fn delivered_exact_token_tie_retains_original_generic_selection() {
    let text = "running 1 test\ntest ordinary_configuration_case ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n";
    let proposal = text.replace(
        "test ordinary_configuration_case ... ok\n",
        "[1 passing test lines omitted]\n",
    );
    assert!(proposal.len() < text.len());
    let compactor = sift::Compactor::new().unwrap();
    let original = compactor.compact(text);
    let selected = compactor.compact(&proposal);
    assert_eq!(
        original.output_tokens, selected.output_tokens,
        "authored exact tie fixture"
    );
    let mut session = Session::new();
    let response = session.delivered(1, text, "cargo test");
    assert!(response.get("semantic").is_none());
    assert_eq!(response["text"], original.text);
    assert_eq!(response["output_tokens"], original.output_tokens);
}

#[test]
fn delivered_flag_is_boolean_and_cannot_enter_generic_protocol() {
    for flag in [json!("true"), json!(1), Value::Null] {
        let mut session = Session::new();
        writeln!(
            session.input.as_mut().unwrap(),
            "{}",
            json!({"id":1,"command":"cargo test",
            "delivered_view":flag,"request":{"version":1,"text":"unchanged"}})
        )
        .unwrap();
        assert!(session.line().unwrap()["error"].is_string());
        assert_eq!(session.line(), None);
        assert!(!session.child.wait().unwrap().success());
        assert!(!session.root.join("state").exists());
    }
    let mut child = crate::test_support::command(["compact", "--protocol=json-v1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"{\"version\":1,\"text\":\"plain\",\"delivered_view\":true}\n{\"version\":1,\"text\":\"plain\"}\n").unwrap();
    let output = child
        .wait_with_output()
        .map(crate::test_support::clean)
        .unwrap();
    assert!(!output.status.success());
    let rows: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert!(rows[0]["error"].is_string());
    assert_eq!(rows[1]["text"], "plain");
    assert!(rows[1].get("semantic").is_none());
}

include!("tests/fixtures/tape_delivered.rs");

#[test]
fn tape_session_selects_strict_full_field_minimum_and_preserves_failure_tail() {
    let mut session = Session::new();
    session.settings(json!({"keep_originals":true}));
    let compactor = sift::Compactor::new().unwrap();
    let counter = tiktoken_rs::o200k_base().unwrap();
    for (i, tail) in [
        "",
        "\n\nCommand exited with code 1",
        "\n\nCommand exited with code 1\n\u{001b}[31mopaque partial",
    ]
    .iter()
    .enumerate()
    {
        let (text, proposal) = tape_fixture(tail);
        let generic = compactor.compact(&text);
        let selected = compactor.compact(&proposal);
        assert!(
            selected.output_tokens < generic.output_tokens,
            "authored useful Tape fixture"
        );
        let response = session.delivered(
            i as u64 + 1,
            &text,
            "node ./node_modules/tape/bin/tape -- fixture.js",
        );
        assert_eq!(response["semantic"], true);
        assert_eq!(response["text"], selected.text);
        assert_eq!(
            response["output_tokens"],
            generic.output_tokens.min(selected.output_tokens)
        );
        assert_eq!(
            response["input_tokens"],
            counter.encode_ordinary(&text).len()
        );
        assert_eq!(
            response["output_tokens"],
            counter
                .encode_ordinary(response["text"].as_str().unwrap())
                .len()
        );
        let encoding = serde_json::from_value(response["encoding"].clone()).unwrap();
        assert_eq!(
            sift::restore(encoding, response["text"].as_str().unwrap()).unwrap(),
            proposal
        );
        let events = session.events();
        let event = events.last().unwrap();
        assert_eq!(event["input_bytes"], text.len());
        assert_eq!(event["output_bytes"], selected.text.len());
        assert!(event["exit_code"].is_null());
        let saved = session
            .root
            .join("state/originals")
            .join(event["original_id"].as_str().unwrap());
        assert_eq!(fs::read(saved.join("stdout")).unwrap(), text.as_bytes());
        assert_eq!(fs::read(saved.join("stderr")).unwrap(), b"");
    }
}

#[test]
fn tape_session_raw_settings_and_unsupported_context_keep_existing_precedence() {
    let mut session = Session::new();
    let (text, _) = tape_fixture("\n\nCommand exited with code 1");
    session.settings(json!({"keep_originals":true}));
    for (i, command) in [
        "sift proxy -- node node_modules/tape/bin/tape fixture.js",
        "sift run --raw -- node node_modules/tape/bin/tape fixture.js",
    ]
    .iter()
    .enumerate()
    {
        let response = session.delivered(i as u64 + 1, &text, command);
        assert_eq!(response["text"], text);
        assert!(response.get("semantic").is_none());
    }
    assert!(!session.root.join("state").exists());
    session.settings(json!({"enabled":false}));
    assert_eq!(
        session.delivered(3, &text, "node node_modules/tape/bin/tape fixture.js")["text"],
        text
    );
    session.settings(json!({"exclude_commands":["bash"]}));
    assert_eq!(
        session.delivered(4, &text, "node node_modules/tape/bin/tape fixture.js")["text"],
        text
    );
    session.settings(json!({}));
    let generic = sift::Compactor::new().unwrap().compact(&text);
    for (i, command) in [
        "node other.js",
        "node --test fixture.js",
        "npm test",
        "node node_modules/tape/bin/tape --watch fixture.js",
    ]
    .iter()
    .enumerate()
    {
        let response = session.delivered(i as u64 + 5, &text, command);
        assert_eq!(response["text"], generic.text);
        assert!(response.get("semantic").is_none());
    }
    let response = session.request(9, &text, Some("node node_modules/tape/bin/tape fixture.js"));
    assert_eq!(response["text"], generic.text);
    assert!(response.get("semantic").is_none());
}
