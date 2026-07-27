use super::*;

#[test]
fn malformed_records_advance_order_and_incomplete_tail_resumes_at_boundary() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-project", "session");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let tail = message("session", "tail", "completed tail").to_string();
    let split = tail.len() - 4;
    let mut file = File::create(&path).unwrap();
    writeln!(file, "{}", message("session", "first", "before malformed")).unwrap();
    writeln!(file, "{{malformed").unwrap();
    writeln!(
        file,
        "{}",
        json!({
            "sessionId": "session",
            "type": "user",
            "message": {"content": [{"type": "tool_result", "content": "discarded"}]}
        })
    )
    .unwrap();
    file.write_all(&tail.as_bytes()[..split]).unwrap();
    file.flush().unwrap();

    let (first, first_rows, _) = parse_collect(&discover_session(&projects, "session"), None);
    assert_eq!(first_rows.len(), 1);
    assert_eq!(first_rows[0].identity.source_record_ordinal, 0);
    assert_eq!(first.rejections.total, 1);
    assert_eq!(
        first.rejections.samples[0].kind,
        RejectionKind::MalformedJson
    );
    assert_eq!(first.rejections.samples[0].source_record_ordinal, 1);
    assert_eq!(first.stats.native_result_records, 1);
    assert_eq!(first.checkpoint.next_raw_ordinal, 3);
    assert!(!first.checkpoint.terminal);
    assert!(first.incomplete_tail.is_some());
    let checkpoint_bytes = serde_json::to_vec(&first.checkpoint).unwrap();
    let checkpoint: ParseCheckpoint = serde_json::from_slice(&checkpoint_bytes).unwrap();
    assert_eq!(checkpoint, first.checkpoint);

    let unchanged = parse_discard(&discover_session(&projects, "session"), Some(&checkpoint));
    assert_eq!(unchanged.change, ChangeSignal::Unchanged);
    assert!(unchanged.stats.metadata_only_noop);
    assert_eq!(
        unchanged.stats.source_bytes_read,
        checkpoint.complete_offset
    );
    assert_eq!(
        unchanged.stats.prefix_verification_bytes,
        checkpoint.complete_offset
    );
    assert_eq!(unchanged.stats.prefix_verification_records, 3);
    assert_eq!(unchanged.stats.semantic_record_parses, 0);
    assert!(unchanged.incomplete_tail.is_some());

    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(&tail.as_bytes()[split..]).unwrap();
    file.write_all(b"\n").unwrap();
    file.flush().unwrap();
    let (second, second_rows, _) =
        parse_collect(&discover_session(&projects, "session"), Some(&checkpoint));
    assert_eq!(second.change, ChangeSignal::Append);
    assert_eq!(second_rows.len(), 1);
    assert_eq!(second_rows[0].identity.source_record_ordinal, 3);
    assert_eq!(second.checkpoint.next_raw_ordinal, 4);
    assert!(second.checkpoint.terminal);
    assert!(second.incomplete_tail.is_none());
}

#[test]
fn core_advance_does_not_bless_a_stale_nonterminal_pro_lane() {
    use crate::OutputOutcome;

    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-core-first", "core-first");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let result_body = "CORE_FIRST_FUTURE_OUTPUT";
    let tail = json!({
        "sessionId": "core-first",
        "type": "user",
        "uuid": "future-result",
        "timestamp": "2026-07-25T00:00:01Z",
        "message": {"content": [{
            "type": "future_provider_output",
            "tool_use_id": "future-call",
            "content": result_body
        }]},
        "toolUseResult": {"exitCode": 17, "is_error": false}
    })
    .to_string();
    let mut file = File::create(&path).unwrap();
    writeln!(
        file,
        "{}",
        message("core-first", "prefix", "committed prefix")
    )
    .unwrap();
    file.write_all(tail.as_bytes()).unwrap();
    file.flush().unwrap();

    let initial_source = discover_session(&projects, "core-first");
    let (initial, _, _) = scan_owned(&initial_source, None, ClaudeNativeProfile::CoreAndPro);
    assert!(!initial.checkpoint.terminal);
    assert!(!initial.checkpoint.pro_terminal);
    assert_eq!(
        initial.checkpoint.core_frontier(),
        initial.checkpoint.pro_frontier()
    );

    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"\n").unwrap();
    file.flush().unwrap();
    let completed_source = discover_session(&projects, "core-first");
    let current_observation = completed_source.fingerprint.observation_sha256();

    let (core_advanced, core_pages, pro_pages) = scan_owned(
        &completed_source,
        Some(&initial.checkpoint),
        ClaudeNativeProfile::CoreOnly,
    );
    assert!(pro_pages.is_empty());
    assert_eq!(core_advanced.change, ChangeSignal::Append);
    assert!(!core_advanced.stats.metadata_only_noop);
    assert_eq!(core_advanced.stats.semantic_record_parses, 1);
    assert_eq!(
        core_advanced.stats.prefix_verification_bytes,
        initial.checkpoint.complete_offset
    );
    let sparse = core_pages
        .iter()
        .flat_map(|page| page.rows.iter())
        .find_map(|row| row.sparse_output.as_ref())
        .unwrap();
    assert_eq!(sparse.outcome, ClaudeOutputOutcome::Failure);
    assert_eq!(sparse.exit_code, Some(17));
    assert!(core_advanced.checkpoint.terminal);
    assert!(!core_advanced.checkpoint.pro_terminal);
    assert_eq!(
        core_advanced.checkpoint.pro_frontier(),
        initial.checkpoint.pro_frontier()
    );
    assert_eq!(
        core_advanced.checkpoint.pro_observation_sha256,
        initial.checkpoint.pro_observation_sha256
    );
    assert_eq!(
        core_advanced.checkpoint.observation_sha256,
        current_observation
    );
    assert_ne!(
        core_advanced.checkpoint.observation_sha256,
        core_advanced.checkpoint.pro_observation_sha256
    );
    assert!(core_advanced.checkpoint.core_observation_binding_matches());
    assert!(core_advanced.checkpoint.pro_observation_binding_matches());

    let (pro_replayed, replay_core, replay_pro) = scan_owned(
        &completed_source,
        Some(&core_advanced.checkpoint),
        ClaudeNativeProfile::ProReplayOnly,
    );
    assert!(replay_core.is_empty());
    assert_eq!(pro_replayed.change, ChangeSignal::Append);
    assert!(!pro_replayed.stats.metadata_only_noop);
    assert_eq!(pro_replayed.stats.semantic_record_parses, 1);
    assert_eq!(
        pro_replayed.stats.prefix_verification_bytes,
        initial.checkpoint.pro_complete_offset
    );
    let outputs = replay_pro
        .iter()
        .flat_map(|page| page.outputs.iter())
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].content, result_body.as_bytes());
    assert_eq!(outputs[0].outcome.outcome, OutputOutcome::Failure);
    assert_eq!(
        pro_replayed.checkpoint.core_frontier(),
        pro_replayed.checkpoint.pro_frontier()
    );
    assert!(pro_replayed.checkpoint.terminal);
    assert!(pro_replayed.checkpoint.pro_terminal);
    assert_eq!(
        pro_replayed.checkpoint.pro_observation_sha256,
        current_observation
    );
}

#[test]
fn pro_advance_does_not_bless_a_stale_nonterminal_core_lane() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-pro-first", "pro-first");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let result_body = "PRO_FIRST_FUTURE_OUTPUT";
    let tail = json!({
        "sessionId": "pro-first",
        "type": "user",
        "uuid": "future-result",
        "timestamp": "2026-07-25T00:00:01Z",
        "message": {"content": [{
            "type": "future_provider_result",
            "tool_use_id": "future-call",
            "content": result_body
        }]},
        "toolUseResult": {"timedOut": true, "durationMs": 91}
    })
    .to_string();
    let mut file = File::create(&path).unwrap();
    writeln!(
        file,
        "{}",
        message("pro-first", "prefix", "committed prefix")
    )
    .unwrap();
    file.write_all(tail.as_bytes()).unwrap();
    file.flush().unwrap();

    let initial_source = discover_session(&projects, "pro-first");
    let (initial, _, _) = scan_owned(&initial_source, None, ClaudeNativeProfile::CoreAndPro);
    assert!(!initial.checkpoint.terminal);
    assert!(!initial.checkpoint.pro_terminal);

    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"\n").unwrap();
    file.flush().unwrap();
    let completed_source = discover_session(&projects, "pro-first");
    let current_observation = completed_source.fingerprint.observation_sha256();

    let (pro_advanced, core_pages, pro_pages) = scan_owned(
        &completed_source,
        Some(&initial.checkpoint),
        ClaudeNativeProfile::ProReplayOnly,
    );
    assert!(core_pages.is_empty());
    assert_eq!(pro_advanced.change, ChangeSignal::Append);
    assert!(!pro_advanced.stats.metadata_only_noop);
    assert_eq!(pro_advanced.stats.semantic_record_parses, 1);
    assert_eq!(
        pro_advanced.stats.prefix_verification_bytes,
        initial.checkpoint.pro_complete_offset
    );
    let outputs = pro_pages
        .iter()
        .flat_map(|page| page.outputs.iter())
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].content, result_body.as_bytes());
    assert!(pro_advanced.checkpoint.pro_terminal);
    assert!(!pro_advanced.checkpoint.terminal);
    assert_eq!(
        pro_advanced.checkpoint.core_frontier(),
        initial.checkpoint.core_frontier()
    );
    assert_eq!(
        pro_advanced.checkpoint.observation_sha256,
        initial.checkpoint.observation_sha256
    );
    assert_eq!(
        pro_advanced.checkpoint.pro_observation_sha256,
        current_observation
    );
    assert_ne!(
        pro_advanced.checkpoint.observation_sha256,
        pro_advanced.checkpoint.pro_observation_sha256
    );

    let (core_replayed, replay_core, replay_pro) = scan_owned(
        &completed_source,
        Some(&pro_advanced.checkpoint),
        ClaudeNativeProfile::CoreOnly,
    );
    assert!(replay_pro.is_empty());
    assert_eq!(core_replayed.change, ChangeSignal::Append);
    assert!(!core_replayed.stats.metadata_only_noop);
    assert_eq!(core_replayed.stats.semantic_record_parses, 1);
    assert_eq!(
        core_replayed.stats.prefix_verification_bytes,
        initial.checkpoint.complete_offset
    );
    let sparse = replay_core
        .iter()
        .flat_map(|page| page.rows.iter())
        .find_map(|row| row.sparse_output.as_ref())
        .unwrap();
    assert_eq!(sparse.outcome, ClaudeOutputOutcome::Timeout);
    assert_eq!(sparse.duration_ms, Some(91));
    assert_eq!(
        core_replayed.checkpoint.core_frontier(),
        core_replayed.checkpoint.pro_frontier()
    );
    assert!(core_replayed.checkpoint.terminal);
    assert!(core_replayed.checkpoint.pro_terminal);
    assert_eq!(
        core_replayed.checkpoint.observation_sha256,
        current_observation
    );
}

#[test]
fn corrupt_current_observation_cannot_bless_a_stale_nonterminal_lane() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-corrupt-lane", "corrupt-lane");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let result_body = "CORRUPT_STALE_LANE_OUTPUT";
    let tail = json!({
        "sessionId": "corrupt-lane",
        "type": "user",
        "uuid": "future-result",
        "message": {"content": [{
            "type": "future_provider_output",
            "content": result_body
        }]},
        "toolUseResult": {"exitCode": 0}
    })
    .to_string();
    let mut file = File::create(&path).unwrap();
    writeln!(
        file,
        "{}",
        message("corrupt-lane", "prefix", "committed prefix")
    )
    .unwrap();
    file.write_all(tail.as_bytes()).unwrap();
    file.flush().unwrap();

    let initial_source = discover_session(&projects, "corrupt-lane");
    let (initial, _, _) = scan_owned(&initial_source, None, ClaudeNativeProfile::CoreAndPro);
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"\n").unwrap();
    file.flush().unwrap();
    let completed_source = discover_session(&projects, "corrupt-lane");
    let (core_advanced, _, _) = scan_owned(
        &completed_source,
        Some(&initial.checkpoint),
        ClaudeNativeProfile::CoreOnly,
    );

    let mut corrupt = core_advanced.checkpoint;
    corrupt.pro_observed_file_len = completed_source.fingerprint.len;
    corrupt.pro_observation_sha256 = completed_source.fingerprint.observation_sha256();
    assert!(!corrupt.pro_terminal);
    assert!(!corrupt.pro_observation_binding_matches());

    let (reparsed, core_pages, pro_pages) = scan_owned(
        &completed_source,
        Some(&corrupt),
        ClaudeNativeProfile::ProReplayOnly,
    );
    assert!(core_pages.is_empty());
    assert_eq!(reparsed.change, ChangeSignal::Reparse);
    assert!(!reparsed.stats.metadata_only_noop);
    assert_eq!(reparsed.stats.prefix_verification_bytes, 0);
    assert_eq!(reparsed.stats.semantic_record_parses, 2);
    let outputs = pro_pages
        .iter()
        .flat_map(|page| page.outputs.iter())
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].content, result_body.as_bytes());
    assert!(reparsed.checkpoint.pro_observation_binding_matches());
}

#[test]
fn event_identity_and_order_include_excluded_and_rejected_physical_records() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-project", "session");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = File::create(&path).unwrap();
    writeln!(
        file,
        "{}",
        json!({
            "sessionId": "session",
            "type": "user",
            "message": {"content": [{"type": "tool_result", "content": "output"}]}
        })
    )
    .unwrap();
    writeln!(file, "{{malformed").unwrap();
    writeln!(
        file,
        "{}",
        json!({
            "sessionId": "session",
            "type": "assistant",
            "uuid": "mixed",
            "message": {
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "safe text"},
                    {"type": "tool_use", "id": "call-a", "name": "Read", "input": {"path": "a"}},
                    {"type": "tool_use", "id": "call-b", "name": "Edit", "input": {"path": "b"}}
                ]
            }
        })
    )
    .unwrap();
    file.flush().unwrap();

    let (output, rows, _) = parse_collect(&discover_session(&projects, "session"), None);
    assert_eq!(
        rows.iter()
            .map(|row| (
                row.identity.source_record_ordinal,
                row.identity.source_subrecord_index
            ))
            .collect::<Vec<_>>(),
        [(2, 0), (2, 1), (2, 2)]
    );
    assert!(rows
        .windows(2)
        .all(|rows| rows[0].native_order < rows[1].native_order));
    assert_eq!(output.checkpoint.next_raw_ordinal, 3);
}

#[test]
fn rejection_samples_are_bounded_while_the_aggregate_is_exact() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-project", "session");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut writer = BufWriter::new(File::create(&path).unwrap());
    for index in 0..100 {
        writeln!(writer, "{{malformed-{index}").unwrap();
    }
    writer.flush().unwrap();

    let output = parse_discard(&discover_session(&projects, "session"), None);
    assert_eq!(output.rejections.total, 100);
    assert_eq!(
        output.rejections.samples.len(),
        CLAUDE_MAX_REJECTION_SAMPLES
    );
    assert_eq!(output.stats.malformed_records, 100);
}

#[test]
fn c0_baseline_shape_retains_seventeen_rows_and_subagent_identity() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let primary = session_path(&projects, "-project", "root-session");
    let subagent = projects.join("-project/root-session/subagents/agent-review.jsonl");
    let primary_records = (0..10)
        .map(|index| c0_record(index, "root-session"))
        .collect::<Vec<_>>();
    let subagent_records = (10..20)
        .map(|index| c0_record(index, "root-session"))
        .collect::<Vec<_>>();
    write_lines(&primary, &primary_records);
    write_lines(&subagent, &subagent_records);

    let discovery = discover_projects(&projects).unwrap();
    let mut retained = 0_u64;
    let mut results = 0_u64;
    let mut subagent_key = None;
    for source in &discovery.sessions {
        let output = parse_session(source, None, |page| {
            retained += page.rows.len() as u64;
            Ok(())
        })
        .unwrap();
        results += output.stats.native_result_records;
        if source.layout == SessionLayout::Subagent {
            subagent_key = Some(output.session.key);
        }
    }
    assert_eq!(retained, 17);
    assert_eq!(results, 3);
    assert_eq!(
        subagent_key.unwrap().agent_id.as_deref(),
        Some("agent-review")
    );
}

fn c0_record(index: usize, session: &str) -> Value {
    let kind = index % 6;
    let content = match kind {
        0 | 1 | 4 | 5 => json!([{
            "type": "text",
            "text": format!("conversation-{index}")
        }]),
        2 => json!([{
            "type": "tool_use",
            "id": format!("call-{index}"),
            "name": "Bash",
            "input": {"command": "printf nativepath"}
        }]),
        3 => json!([{
            "type": "tool_result",
            "tool_use_id": format!("call-{}", index - 1),
            "content": format!("output-{index}")
        }]),
        _ => unreachable!(),
    };
    json!({
        "sessionId": session,
        "type": if kind == 0 || kind == 3 { "user" } else if kind == 4 { "system" } else { "assistant" },
        "uuid": format!("record-{index}"),
        "message": {"content": content}
    })
}

#[test]
fn append_rewrite_truncation_replacement_relocation_and_copy_are_distinguished() {
    assert_eq!(mutation_signal(Mutation::Append), ChangeSignal::Append);
    assert_eq!(mutation_signal(Mutation::Rewrite), ChangeSignal::Rewrite);
    assert_eq!(
        mutation_signal(Mutation::Truncate),
        ChangeSignal::Truncation
    );
    assert_eq!(
        mutation_signal(Mutation::ReplaceShort),
        ChangeSignal::Replacement
    );
    assert_eq!(
        mutation_signal(Mutation::Relocate),
        ChangeSignal::Relocation
    );
    assert_eq!(mutation_signal(Mutation::Copy), ChangeSignal::LiveCopy);
    assert_eq!(
        mutation_signal(Mutation::ConflictingCopy),
        ChangeSignal::ConflictingLiveCopy
    );
    assert_eq!(
        mutation_output(Mutation::Append).lifecycle,
        ClaudeSourceLifecycle::Append
    );
    assert_eq!(
        mutation_output(Mutation::Rewrite).lifecycle,
        ClaudeSourceLifecycle::Rewrite
    );
    assert_eq!(
        mutation_output(Mutation::Truncate).lifecycle,
        ClaudeSourceLifecycle::Rewind
    );
    assert_eq!(
        mutation_output(Mutation::ReplaceShort).lifecycle,
        ClaudeSourceLifecycle::Replacement
    );
    assert_eq!(
        mutation_output(Mutation::Relocate).lifecycle,
        ClaudeSourceLifecycle::Move
    );
    assert_eq!(
        mutation_output(Mutation::Copy).lifecycle,
        ClaudeSourceLifecycle::Copy
    );
    assert_eq!(
        mutation_output(Mutation::ConflictingCopy).lifecycle,
        ClaudeSourceLifecycle::Ambiguous
    );
}

#[derive(Clone, Copy)]
enum Mutation {
    Append,
    Rewrite,
    Truncate,
    ReplaceShort,
    Relocate,
    Copy,
    ConflictingCopy,
}

fn mutation_signal(mutation: Mutation) -> ChangeSignal {
    mutation_output(mutation).change
}

fn mutation_output(mutation: Mutation) -> ParseOutput {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-project-a", "session");
    write_lines(
        &path,
        &[
            message("session", "one", &"1".repeat(2_048)),
            message("session", "two", &"2".repeat(2_048)),
        ],
    );
    let first_source = discover_session(&projects, "session");
    let first = parse_discard(&first_source, None);
    assert_eq!(first.lifecycle, ClaudeSourceLifecycle::New);

    let current_path = match mutation {
        Mutation::Append => {
            append_line(&path, &message("session", "three", "333333"));
            path.clone()
        }
        Mutation::Rewrite => {
            write_lines(
                &path,
                &[
                    message("session", "one", &"A".repeat(4_096)),
                    message("session", "two", &"B".repeat(4_096)),
                ],
            );
            path.clone()
        }
        Mutation::Truncate => {
            OpenOptions::new()
                .write(true)
                .open(&path)
                .unwrap()
                .set_len(8)
                .unwrap();
            path.clone()
        }
        Mutation::ReplaceShort => {
            let replacement = path.with_extension("replacement");
            write_lines(&replacement, &[message("session", "replacement", "short")]);
            fs::rename(&replacement, &path).unwrap();
            path.clone()
        }
        Mutation::Relocate => {
            let relocated = session_path(&projects, "-project-b", "session");
            fs::create_dir_all(relocated.parent().unwrap()).unwrap();
            fs::rename(&path, &relocated).unwrap();
            relocated
        }
        Mutation::Copy => {
            let copy = session_path(&projects, "-project-b", "session");
            fs::create_dir_all(copy.parent().unwrap()).unwrap();
            fs::copy(&path, &copy).unwrap();
            copy
        }
        Mutation::ConflictingCopy => {
            let copy = session_path(&projects, "-project-b", "session");
            write_lines(
                &copy,
                &[message("session", "different-copy", "not the same source")],
            );
            copy
        }
    };
    let current = discover_projects(&projects)
        .unwrap()
        .sessions
        .into_iter()
        .find(|source| source.path == current_path)
        .unwrap();
    parse_discard(&current, Some(&first.checkpoint))
}

#[test]
fn no_op_and_append_verify_the_full_prefix_while_parsing_only_delta() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-project", "session");
    write_lines(
        &path,
        &[
            message("session", "one", &"1".repeat(40_000)),
            message("session", "two", &"2".repeat(40_000)),
        ],
    );
    let first_source = discover_session(&projects, "session");
    let first_len = first_source.fingerprint.len;
    let first = parse_discard(&first_source, None);
    assert_eq!(first.stats.parsed_source_bytes, first_len);
    assert_eq!(first.stats.source_bytes_read, first_len);
    assert_eq!(first.stats.prefix_verification_bytes, 0);
    assert_eq!(first.stats.prefix_verification_records, 0);

    let unchanged = parse_discard(
        &discover_session(&projects, "session"),
        Some(&first.checkpoint),
    );
    assert_eq!(unchanged.change, ChangeSignal::Unchanged);
    assert_eq!(unchanged.lifecycle, ClaudeSourceLifecycle::Replay);
    assert!(unchanged.stats.metadata_only_noop);
    assert_eq!(unchanged.stats.source_bytes_read, first_len);
    assert_eq!(unchanged.stats.parsed_source_bytes, 0);
    assert_eq!(unchanged.stats.prefix_verification_bytes, first_len);
    assert_eq!(unchanged.stats.prefix_verification_records, 2);
    assert_eq!(unchanged.stats.semantic_record_parses, 0);

    let before_append = fs::metadata(&path).unwrap().len();
    append_line(&path, &message("session", "three", "append-tail"));
    let after_append = fs::metadata(&path).unwrap().len();
    let appended = parse_discard(
        &discover_session(&projects, "session"),
        Some(&first.checkpoint),
    );
    assert_eq!(appended.change, ChangeSignal::Append);
    assert_eq!(appended.lifecycle, ClaudeSourceLifecycle::Append);
    assert_eq!(
        appended.stats.parsed_source_bytes,
        after_append - before_append
    );
    assert_eq!(
        appended.stats.prefix_verification_bytes,
        first.checkpoint.complete_offset
    );
    assert_eq!(appended.stats.prefix_verification_records, 2);
    assert_eq!(appended.stats.semantic_record_parses, 1);
    assert_eq!(
        appended.stats.source_bytes_read,
        appended.stats.parsed_source_bytes + appended.stats.prefix_verification_bytes
    );

    write_lines(
        &path,
        &[
            message("session", "rewrite-one", &"A".repeat(80_000)),
            message("session", "rewrite-two", &"B".repeat(80_000)),
        ],
    );
    let rewritten_len = fs::metadata(&path).unwrap().len();
    let rewritten = parse_discard(
        &discover_session(&projects, "session"),
        Some(&appended.checkpoint),
    );
    assert_eq!(rewritten.change, ChangeSignal::Rewrite);
    assert_eq!(rewritten.lifecycle, ClaudeSourceLifecycle::Rewrite);
    assert_eq!(rewritten.stats.parsed_source_bytes, rewritten_len);
    assert!(rewritten.stats.prefix_verification_bytes > 64 * 1024);
    assert_eq!(
        rewritten.stats.source_bytes_read,
        rewritten.stats.parsed_source_bytes + rewritten.stats.prefix_verification_bytes
    );
}
