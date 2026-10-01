use super::*;

#[cfg(unix)]
#[test]
fn copilot_cli_import_skips_symlinked_session_files_checkout() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let query = "copilot-cli-symlinked-files-oracle";
    let path = write_native_copilot_fixture(&temp, query);
    // Copilot CLI stores the session working copy under
    // `<session>/files/`, and checked-out repositories legitimately contain
    // symlinks (for example `CLAUDE.md -> AGENTS.md`). The link can never
    // hold an `events.jsonl` transcript, so it must be skipped without
    // failing the whole copilot_cli source.
    let checkout = Path::new(&path).join("copilot-cli-native/files/checkout");
    fs::create_dir_all(&checkout).unwrap();
    fs::write(checkout.join("AGENTS.md"), b"agents\n").unwrap();
    symlink("AGENTS.md", checkout.join("CLAUDE.md")).unwrap();
    let outside_query = "outsidesymlinktargetoracle9f27c4";
    let outside_session = temp.path().join("outside-copilot-session");
    fs::create_dir_all(&outside_session).unwrap();
    let selected_transcript = Path::new(&path).join("copilot-cli-native/events.jsonl");
    let outside_transcript = fs::read_to_string(&selected_transcript)
        .unwrap()
        .replace(query, outside_query);
    fs::write(outside_session.join("events.jsonl"), outside_transcript).unwrap();
    symlink(&outside_session, checkout.join("linked-session")).unwrap();
    let _daemon = start_isolated_provider_daemon(&temp);

    let first = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "copilot-cli",
        "--path",
        &path,
        "--no-daemon",
        "--format=json",
    ]));
    assert_explicit_source_publication(&first, "copilot_cli", "copilot_cli_session_events_jsonl");
    wait_for_imported_core(&temp, &first);
    assert_eq!(first["totals"]["current_rejected_records"], 0, "{first:#}");
    let (session_count, event_count) = provider_core_counts(&data_root(&temp), "copilot_cli");
    assert!(session_count >= 1);
    assert!(event_count >= 1);

    let search = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "copilot-cli",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert_search_provider_oracle(&search, "copilot_cli", query, 1, "message");
    let outside_search = json_output(ctx(&temp).args([
        "search",
        outside_query,
        "--provider",
        "copilot-cli",
        "--refresh",
        "off",
        "--format=json",
    ]));
    assert!(
        outside_search["results"].as_array().unwrap().is_empty(),
        "outside symlink target leaked into Copilot inventory: {outside_search:#}"
    );

    // A second import exercises the membership fence walk against the same
    // symlinked checkout and must republish cleanly.
    let second = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "copilot-cli",
        "--path",
        &path,
        "--no-daemon",
        "--format=json",
    ]));
    assert_explicit_source_publication(&second, "copilot_cli", "copilot_cli_session_events_jsonl");
    assert_eq!(
        second["totals"]["current_rejected_records"], 0,
        "{second:#}"
    );
}

#[cfg(unix)]
#[test]
fn copilot_cli_reimport_fails_when_transcript_turns_into_symlink() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let query = "copilot-cli-transcript-to-symlink-oracle";
    let path = write_native_copilot_fixture(&temp, query);
    let _daemon = start_isolated_provider_daemon(&temp);

    let first = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "copilot-cli",
        "--path",
        &path,
        "--no-daemon",
        "--format=json",
    ]));
    assert_explicit_source_publication(&first, "copilot_cli", "copilot_cli_session_events_jsonl");
    wait_for_imported_core(&temp, &first);

    // A transcript route that turns into a link after admission drops out of
    // the observed membership route set, so the re-import must fail closed
    // instead of silently keeping stale content.
    let session = Path::new(&path).join("copilot-cli-native");
    let real_transcript = session.join("events.real.jsonl");
    fs::rename(session.join("events.jsonl"), &real_transcript).unwrap();
    symlink("events.real.jsonl", session.join("events.jsonl")).unwrap();

    let second = failure_json_output(ctx(&temp).args([
        "import",
        "--provider",
        "copilot-cli",
        "--path",
        &path,
        "--no-daemon",
        "--format=json",
    ]));
    assert_eq!(second["failure_type"], "source_failure", "{second:#}");
    assert_eq!(
        second["outcome"], "completed_with_source_failures",
        "{second:#}"
    );
    assert_eq!(second["sources"][0]["carried_forward"], true, "{second:#}");
}

#[cfg(unix)]
#[test]
fn copilot_cli_import_still_rejects_symlinked_transcript() {
    use std::os::unix::fs::symlink;

    let temp = tempdir();
    let query = "copilot-cli-symlinked-transcript-oracle";
    let path = write_native_copilot_fixture(&temp, query);
    // A symlink where the transcript itself should be stays fail-closed:
    // skipping is only safe for entries that can never hold a transcript.
    let session = Path::new(&path).join("copilot-cli-native");
    let real_transcript = session.join("events.real.jsonl");
    fs::rename(session.join("events.jsonl"), &real_transcript).unwrap();
    symlink("events.real.jsonl", session.join("events.jsonl")).unwrap();
    let _daemon = start_isolated_provider_daemon(&temp);

    let stderr = failure_stderr(ctx(&temp).args([
        "import",
        "--provider",
        "copilot-cli",
        "--path",
        &path,
        "--no-daemon",
        "--format=json",
    ]));
    assert!(
        stderr.contains("symlinked provider source path components are rejected"),
        "{stderr}"
    );
}

#[test]
fn cursor_divergent_transcript_copies_retain_history_and_repair_to_strict_extension() {
    let temp = tempdir();
    let query = "cursor-conflicting-copies-oracle";
    let path = write_native_cursor_fixture(&temp, query);
    // Model three physical project routes that claim one native session while
    // keeping unrelated history in the same provider root.
    let conflicted = "conflicted-session";
    let mut conflicted_paths = Vec::new();
    for (slug, text) in [
        ("workspace-one", "first copy"),
        ("workspace-two", "second"),
        ("workspace-three", "first copy"),
    ] {
        let session = Path::new(&path)
            .join(slug)
            .join("agent-transcripts")
            .join(conflicted);
        fs::create_dir_all(&session).unwrap();
        let transcript = session.join(format!("{conflicted}.jsonl"));
        fs::write(
            &transcript,
            format!(
                concat!(
                    r#"{{"timestamp":"2026-06-24T12:00:00Z","role":"user","#,
                    r#""message":{{"content":[{{"type":"text","text":"{}"}}]}}}}"#,
                    "\n"
                ),
                text
            ),
        )
        .unwrap();
        conflicted_paths.push(transcript);
    }
    let _daemon = start_isolated_provider_daemon(&temp);

    let receipt = failure_json_output(ctx(&temp).args([
        "import",
        "--provider",
        "cursor",
        "--path",
        &path,
        "--no-daemon",
        "--format=json",
    ]));
    wait_for_imported_core(&temp, &receipt);

    assert_eq!(
        receipt["sources"][0]["source_failure_total"], 1,
        "only the genuinely divergent copy must remain visible as a source diagnostic: {receipt:#}"
    );
    assert_eq!(
        receipt["sources"][0]["carried_forward"], false,
        "the diagnostic belongs to the discarded physical copy, not the imported winner: {receipt:#}"
    );

    // The selected conflicted session and its healthy sibling both publish.
    let (sessions, events) = provider_core_counts(&data_root(&temp), "cursor");
    assert!(sessions >= 2, "{receipt:#}");
    assert!(events >= 2, "{receipt:#}");
    let selected = json_output(ctx(&temp).args([
        "search",
        "first copy",
        "--provider",
        "cursor",
        "--refresh",
        "off",
        "--limit",
        "1",
        "--format=json",
    ]));
    assert_search_provider_oracle(&selected, "cursor", "first copy", 1, "message");
    let search = json_output(ctx(&temp).args([
        "search",
        query,
        "--provider",
        "cursor",
        "--refresh",
        "off",
        "--limit",
        "1",
        "--format=json",
    ]));
    assert_search_provider_oracle(&search, "cursor", query, 1, "message");

    // An unchanged warm refresh remains a no-op while preserving the physical
    // copy diagnostic. It must never claim that the accepted winner failed or
    // was carried forward.
    let replay = failure_json_output(ctx(&temp).args([
        "import",
        "--provider",
        "cursor",
        "--path",
        &path,
        "--no-daemon",
        "--format=json",
    ]));
    wait_for_imported_core(&temp, &replay);
    assert_eq!(
        replay["sources"][0]["source_failure_total"], 1,
        "{replay:#}"
    );
    assert_eq!(replay["sources"][0]["carried_forward"], false, "{replay:#}");
    assert_eq!(
        replay["sources"][0]["generation_changed"], false,
        "{replay:#}"
    );

    // The second copy converges into a strict extension. The longer history
    // wins, the divergence diagnostic clears, and the added event is visible.
    fs::write(
        &conflicted_paths[1],
        concat!(
            r#"{"timestamp":"2026-06-24T12:00:00Z","role":"user","message":{"content":[{"type":"text","text":"first copy"}]}}"#,
            "\n",
            r#"{"timestamp":"2026-06-24T12:01:00Z","role":"assistant","message":{"content":[{"type":"text","text":"strict extension"}]}}"#,
            "\n"
        ),
    )
    .unwrap();
    let repaired = json_output(ctx(&temp).args([
        "import",
        "--provider",
        "cursor",
        "--path",
        &path,
        "--no-daemon",
        "--format=json",
    ]));
    wait_for_imported_core(&temp, &repaired);
    assert_eq!(
        repaired["sources"][0]["source_failure_total"], 0,
        "a unique strict extension must repair the divergence: {repaired:#}"
    );
    let extension = json_output(ctx(&temp).args([
        "search",
        "strict extension",
        "--provider",
        "cursor",
        "--refresh",
        "off",
        "--limit",
        "1",
        "--format=json",
    ]));
    assert_search_provider_oracle(&extension, "cursor", "strict extension", 1, "message");
}
