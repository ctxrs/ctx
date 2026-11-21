mod support;

use chrono::{Duration as ChronoDuration, SecondsFormat, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, AgentScope as CoreAgentScope, CertifiedSource,
    CoreActivity, CoreRecord, EventIdentityInput, LiteralFactKind, NativeItemKey, NativeSessionKey,
    ProviderDeclaredFact, ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceKey,
    SourceObservation, TypedKey, CORE_ACTIVITY_REVISION,
};
use ctx_history_index::{GenerationWriter, WriterOptions};
use support::*;

fn lexical_root(temp: &TempDir) -> PathBuf {
    let root = data_root(temp).join("search/lexical");
    fs::create_dir_all(&root).unwrap();
    root
}

fn source_for(provider: &str, format: &str, name: &str) -> SourceKey {
    SourceKey::derive(
        provider,
        format,
        "session",
        1,
        SourceAnchor::provider_native("session-file", TypedKey::utf8(name).unwrap()).unwrap(),
    )
    .unwrap()
}

fn certify(source: &SourceKey, revision: u8, documents: u64) -> CertifiedSource {
    let observation =
        SourceObservation::new(source.clone(), "regular-file-v1", vec![revision]).unwrap();
    CertifiedSource::certify(
        observation.clone(),
        observation,
        "test-parser-v1",
        [revision; 32],
        ScannedSourceCounts {
            complete_records: documents,
            retained_records: documents,
            indexed_documents: documents,
            certified_bytes: documents * 16,
            ..ScannedSourceCounts::default()
        },
    )
    .unwrap()
}

fn record_for(
    source: &SourceKey,
    native_session: &str,
    sequence: u64,
    occurred_at_unix_ms: Option<i64>,
    body: &str,
) -> CoreRecord {
    let session_key =
        NativeSessionKey::native_id("session", TypedKey::utf8(native_session).unwrap()).unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "thread",
        native_session_key: &session_key,
    })
    .unwrap();
    let item_key = NativeItemKey::native_id(
        "message",
        TypedKey::utf8(format!("event-{sequence}")).unwrap(),
    )
    .unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &item_key,
        subrecord_selector: None,
    })
    .unwrap();
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        source.clone(),
        sequence,
        "message",
        "sessions-cli-test-v1",
        body,
    )
    .unwrap();
    record.provider_session_id = Some(native_session.to_owned());
    record.native_event_id = Some(TypedKey::U64(sequence));
    record.occurred_at_unix_ms = occurred_at_unix_ms;
    record.role = Some("user".to_owned());
    record.agent_scope = Some(CoreAgentScope::Primary);
    record
}

fn add_fact(record: &mut CoreRecord, kind: LiteralFactKind, value: &str) {
    let activity = record.content.activity.get_or_insert_with(|| CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id: None,
        invocation: None,
        result: None,
        facts: Vec::new(),
    });
    activity.facts.push(ProviderDeclaredFact {
        kind,
        value: value.to_owned(),
    });
}

#[test]
fn sessions_missing_index_reports_clean_error_in_text_and_json() {
    let temp = tempdir();
    ctx(&temp)
        .args(["sessions"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("History sessions are not ready"))
        .stderr(predicate::str::contains("ctx setup"));

    ctx(&temp)
        .args(["sessions", "--format", "json"])
        .assert()
        .failure()
        .stdout(predicate::str::is_empty());
}

#[test]
fn sessions_empty_generation_returns_empty_json_and_text_notice() {
    let temp = tempdir();
    let writer = GenerationWriter::open(lexical_root(&temp), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.commit(|_| true).unwrap();

    let json = json_output(ctx(&temp).args(["sessions", "--format", "json"]));
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["payload_type"], "session_list");
    assert!(json["generation_id"]
        .as_str()
        .is_some_and(|id| !id.is_empty()));
    assert_eq!(json["truncated"], false);
    assert_eq!(json["sessions"].as_array().unwrap().len(), 0);

    ctx(&temp)
        .args(["sessions"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No sessions found."));
}

#[test]
fn sessions_filters_ordering_limit_and_text_json_contracts() {
    let temp = tempdir();
    let now = Utc::now();
    let recent_ms = (now - ChronoDuration::minutes(30)).timestamp_millis();
    let mid_ms = (now - ChronoDuration::hours(6)).timestamp_millis();
    let old_ms = (now - ChronoDuration::days(10)).timestamp_millis();

    let codex_source = source_for("codex", "codex_session_jsonl", "codex.jsonl");
    let claude_source = source_for("claude", "claude_code_jsonl", "claude.jsonl");
    let custom_source = source_for("custom", "ctx_history_jsonl_v2", "custom.jsonl");

    // 1. Recent Codex primary session with old start + recent last activity (3 events).
    let mut codex_1 = record_for(&codex_source, "codex-main", 1, Some(old_ms), "start");
    add_fact(&mut codex_1, LiteralFactKind::SessionCwd, "/work/ctx-core");
    add_fact(&mut codex_1, LiteralFactKind::Project, "ctx-core");
    add_fact(&mut codex_1, LiteralFactKind::Branch, "main");
    let codex_2 = record_for(&codex_source, "codex-main", 2, Some(mid_ms), "middle");
    let codex_3 = record_for(&codex_source, "codex-main", 3, Some(recent_ms), "latest");

    // 2. Recent Codex subagent child of codex-main (newest overall, 1 event).
    let subagent_ms = (now - ChronoDuration::minutes(10)).timestamp_millis();
    let mut codex_sub = record_for(&codex_source, "codex-sub", 1, Some(subagent_ms), "child");
    codex_sub.agent_scope = Some(CoreAgentScope::Subagent);
    codex_sub.parent_session_id = Some(codex_1.session_id);
    codex_sub.root_session_id = Some(codex_1.session_id);
    add_fact(
        &mut codex_sub,
        LiteralFactKind::SessionCwd,
        "/work/ctx-core",
    );

    // 3. Mid-age Claude primary session in a different workspace (2 events).
    let mut claude_1 = record_for(&claude_source, "claude-app", 1, Some(mid_ms - 5_000), "hi");
    add_fact(&mut claude_1, LiteralFactKind::Workspace, "/work/web-app");
    let mut claude_2 = record_for(&claude_source, "claude-app", 2, Some(mid_ms), "done");
    add_fact(&mut claude_2, LiteralFactKind::Branch, "feat/ui");

    // 4. Old unscoped custom root session (agent_scope = None, must survive --no-subagents).
    let mut custom_1 = record_for(&custom_source, "custom-old", 1, Some(old_ms), "archive");
    custom_1.agent_scope = None;
    custom_1.native_event_id = Some(TypedKey::Composite(vec![
        TypedKey::utf8("amp").unwrap(),
        TypedKey::utf8("threads").unwrap(),
        TypedKey::utf8("event_id:custom-1").unwrap(),
    ]));
    add_fact(&mut custom_1, LiteralFactKind::ToolWorkdir, "/work/legacy");

    let mut writer = GenerationWriter::open(lexical_root(&temp), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(codex_source.clone()).unwrap();
    for rec in [codex_1.clone(), codex_2, codex_3, codex_sub.clone()] {
        writer.add_core_record(rec).unwrap();
    }
    writer.certify_source(certify(&codex_source, 1, 4)).unwrap();

    writer.begin_source(claude_source.clone()).unwrap();
    for rec in [claude_1.clone(), claude_2] {
        writer.add_core_record(rec).unwrap();
    }
    writer
        .certify_source(certify(&claude_source, 1, 2))
        .unwrap();

    writer.begin_source(custom_source.clone()).unwrap();
    writer.add_core_record(custom_1.clone()).unwrap();
    writer
        .certify_source(certify(&custom_source, 1, 1))
        .unwrap();

    writer.commit(|_| true).unwrap();

    // Unfiltered JSON: newest activity first (codex-sub -> codex-main -> claude-app -> custom-old).
    let all = json_output(ctx(&temp).args(["sessions", "--format", "json"]));
    assert_eq!(all["schema_version"], 1);
    assert_eq!(all["payload_type"], "session_list");
    assert_eq!(all["truncated"], false);
    let rows = all["sessions"].as_array().unwrap();
    assert_eq!(rows.len(), 4);
    assert_eq!(
        rows[0]["ctx_session_id"],
        codex_sub.session_id.as_uuid().to_string()
    );
    assert_eq!(
        rows[0]["parent_ctx_session_id"],
        codex_1.session_id.as_uuid().to_string()
    );
    assert_eq!(
        rows[0]["root_ctx_session_id"],
        codex_1.session_id.as_uuid().to_string()
    );
    assert_eq!(rows[0]["events"], 1);

    assert_eq!(
        rows[1]["ctx_session_id"],
        codex_1.session_id.as_uuid().to_string()
    );
    assert_eq!(rows[1]["provider"], "codex");
    assert_eq!(rows[1]["provider_session_id"], "codex-main");
    assert_eq!(rows[1]["cwd"], "/work/ctx-core");
    assert_eq!(rows[1]["project"], "ctx-core");
    assert_eq!(rows[1]["branch"], "main");
    assert_eq!(rows[1]["events"], 3);
    assert!(rows[1]["started_at"].as_str().unwrap().ends_with('Z'));
    assert!(rows[1]["last_activity_at"].as_str().unwrap().ends_with('Z'));
    assert!(rows[1].get("parent_ctx_session_id").is_none());

    assert_eq!(
        rows[2]["ctx_session_id"],
        claude_1.session_id.as_uuid().to_string()
    );
    assert_eq!(rows[2]["provider"], "claude");
    assert_eq!(rows[2]["cwd"], "/work/web-app");
    assert_eq!(rows[2]["branch"], "feat/ui");
    assert_eq!(rows[2]["events"], 2);

    assert_eq!(
        rows[3]["ctx_session_id"],
        custom_1.session_id.as_uuid().to_string()
    );
    assert_eq!(rows[3]["provider"], "custom");
    assert_eq!(rows[3]["provider_key"], "amp");
    assert_eq!(rows[3]["source_id"], "threads");
    assert_eq!(rows[3]["cwd"], "/work/legacy");

    // --no-subagents drops codex-sub while keeping codex-main, claude-app, and unscoped custom-old.
    let no_sub = json_output(ctx(&temp).args(["sessions", "--no-subagents", "--format", "json"]));
    let ids: Vec<&str> = no_sub["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["ctx_session_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec![
            codex_1.session_id.as_uuid().to_string(),
            claude_1.session_id.as_uuid().to_string(),
            custom_1.session_id.as_uuid().to_string(),
        ]
    );

    // --since 2h selects only codex-sub and codex-main, and codex-main retains whole-session events=3!
    let since_2h = json_output(ctx(&temp).args([
        "sessions",
        "--since",
        "2h",
        "--no-subagents",
        "--format",
        "json",
    ]));
    let since_rows = since_2h["sessions"].as_array().unwrap();
    assert_eq!(since_rows.len(), 1);
    assert_eq!(
        since_rows[0]["ctx_session_id"],
        codex_1.session_id.as_uuid().to_string()
    );
    assert_eq!(since_rows[0]["events"], 3);
    assert_eq!(since_rows[0]["cwd"], "/work/ctx-core");

    // --since with explicit RFC3339 timestamp and YYYY-MM-DD date also work.
    let cutoff_rfc3339 =
        (now - ChronoDuration::hours(2)).to_rfc3339_opts(SecondsFormat::Secs, true);
    let since_rfc = json_output(ctx(&temp).args([
        "sessions",
        "--since",
        &cutoff_rfc3339,
        "--no-subagents",
        "--format",
        "json",
    ]));
    assert_eq!(since_rfc["sessions"].as_array().unwrap().len(), 1);

    let cutoff_date = (now - ChronoDuration::days(2))
        .format("%Y-%m-%d")
        .to_string();
    let since_date = json_output(ctx(&temp).args([
        "sessions",
        "--since",
        &cutoff_date,
        "--no-subagents",
        "--format",
        "json",
    ]));
    assert_eq!(since_date["sessions"].as_array().unwrap().len(), 2);

    // --provider + --workspace filters (including trailing slash from shell tab-completion).
    let claude_web = json_output(ctx(&temp).args([
        "sessions",
        "--provider",
        "claude",
        "--workspace",
        "WEB-APP/",
        "--format",
        "json",
    ]));
    assert_eq!(claude_web["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(
        claude_web["sessions"][0]["ctx_session_id"],
        claude_1.session_id.as_uuid().to_string()
    );

    // --limit + truncated boundary.
    let limited = json_output(ctx(&temp).args(["sessions", "--limit", "2", "--format", "json"]));
    assert_eq!(limited["truncated"], true);
    assert_eq!(limited["sessions"].as_array().unwrap().len(), 2);

    // Human text mode renders compact session IDs, metadata, and truncation footer.
    let text_out = ctx(&temp)
        .args(["sessions", "--limit", "2"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(text_out).unwrap();
    assert!(text.contains("2 sessions"), "{text}");
    assert!(text.contains("Codex"), "{text}");
    assert!(text.contains("/work/ctx-core"), "{text}");
    assert!(text.contains("More sessions available."), "{text}");

    // Extract the compact session ID prefix for card #2 (codex-main) and verify
    // `ctx show session <compact_ref>` resolves that exact session cleanly.
    let card_two_line = text
        .lines()
        .find(|line| line.starts_with("2. "))
        .unwrap_or_else(|| panic!("missing card #2 line in:\n{text}"));
    let compact_ref = card_two_line
        .strip_prefix("2. ")
        .and_then(|rest| rest.split(" · ").next())
        .unwrap_or_else(|| panic!("missing compact ref in line: {card_two_line:?}"));
    assert!(
        (8..32).contains(&compact_ref.len()) && !compact_ref.contains('-'),
        "expected compact hex prefix (8..32 chars, no dashes), got {compact_ref:?}"
    );
    assert!(
        codex_1
            .session_id
            .as_uuid()
            .simple()
            .to_string()
            .starts_with(compact_ref),
        "compact ref {compact_ref:?} does not match {}",
        codex_1.session_id.as_uuid()
    );
    let shown = json_output(ctx(&temp).args(["show", "session", compact_ref, "--format", "json"]));
    assert_eq!(
        shown["ctx_session_id"],
        codex_1.session_id.as_uuid().to_string()
    );

    // Invalid flags rejected cleanly.
    ctx(&temp)
        .args(["sessions", "--limit", "0"])
        .assert()
        .failure()
        .code(2);
    ctx(&temp)
        .args(["sessions", "--limit", "1001"])
        .assert()
        .failure()
        .code(2);
    ctx(&temp)
        .args(["sessions", "--since", "not-a-window"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid --since"));
    ctx(&temp)
        .args(["sessions", "--json"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn sessions_broken_pipe_in_text_and_json_exits_cleanly_without_panic() {
    use std::io::Read as _;
    use std::process::Stdio;

    let temp = tempdir();
    let src = source_for("codex", "codex_session_jsonl", "pipe-sessions.jsonl");
    let now_ms = Utc::now().timestamp_millis();
    let long_cwd = format!("/work/{}", "segment_".repeat(40));

    let mut writer = GenerationWriter::open(lexical_root(&temp), WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(src.clone()).unwrap();
    // 250 sessions with long CWDs exceed the OS pipe buffer (~64 KiB) in both text and JSON modes.
    for index in 0..250_u64 {
        let mut rec = record_for(
            &src,
            &format!("pipe-session-{index:04}"),
            1,
            Some(now_ms - (index as i64) * 1_000),
            "pipe output test",
        );
        add_fact(
            &mut rec,
            LiteralFactKind::SessionCwd,
            &format!("{long_cwd}/repo-{index:04}"),
        );
        add_fact(&mut rec, LiteralFactKind::Project, "pipe-project");
        add_fact(&mut rec, LiteralFactKind::Branch, "main");
        writer.add_core_record(rec).unwrap();
    }
    writer.certify_source(certify(&src, 1, 250)).unwrap();
    writer.commit(|_| true).unwrap();

    let ctx_bin = assert_cmd::Command::cargo_bin("ctx")
        .expect("resolve compiled ctx executable")
        .get_program()
        .to_os_string();
    let data = data_root(&temp);

    for (label, args) in [
        (
            "text",
            vec!["sessions", "--limit", "250", "--format", "text"],
        ),
        (
            "json",
            vec!["sessions", "--limit", "250", "--format", "json"],
        ),
    ] {
        for read_prefix in [false, true] {
            let mut child = std::process::Command::new(&ctx_bin)
                .args(["--data-root"])
                .arg(&data)
                .args(&args)
                .env("HOME", temp.path())
                .env("USERPROFILE", temp.path())
                .env("CTX_ANALYTICS_ENABLED", "false")
                .env("CTX_UPGRADE_AUTO", "off")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();

            let mut stdout = child.stdout.take().unwrap();
            if read_prefix {
                let mut first = [0u8; 16];
                let _ = stdout.read(&mut first);
            }
            drop(stdout);

            let output = child.wait_with_output().unwrap();
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                !stderr.contains("panicked"),
                "CLI panicked on broken pipe ({label}, read_prefix={read_prefix}): {stderr}"
            );
            assert!(
                !stderr.contains("failed printing to"),
                "CLI emitted write panic on broken pipe ({label}, read_prefix={read_prefix}): {stderr}"
            );
            assert!(
                !stderr.contains("Broken pipe"),
                "CLI emitted broken pipe error on stderr ({label}, read_prefix={read_prefix}): {stderr}"
            );
            assert!(
                output.status.success(),
                "expected clean exit on broken pipe ({label}, read_prefix={read_prefix}), got {:?}; stderr: {stderr}",
                output.status
            );
        }
    }
}
