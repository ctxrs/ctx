use super::*;

#[test]
fn complete_inventory_alone_authorizes_deletion_candidates_and_pages_are_certified() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let first_path = session_path(&projects, "-inventory", "first");
    let second_path = session_path(&projects, "-inventory", "second");
    write_lines(&first_path, &[message("first", "first-1", "one")]);
    write_lines(&second_path, &[message("second", "second-1", "two")]);
    let initial = discover_projects(&projects).unwrap();
    assert!(initial.inventory.complete);
    initial.revalidate_inventory().unwrap();
    let checkpoints = initial
        .sessions
        .iter()
        .map(|source| parse_discard(source, None).checkpoint)
        .collect::<Vec<_>>();

    fs::remove_file(&second_path).unwrap();
    let current = discover_projects(&projects).unwrap();
    let candidates = authoritative_deletion_candidates(&current, &checkpoints).unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(
        candidates[0].lifecycle,
        ClaudeSourceLifecycle::DeletionCandidate
    );
    assert_eq!(candidates[0].session_key.root_session_id, "second");
    assert!(candidates[0].inventory.complete);

    let source = discover_session(&projects, "first");
    let (_, pages, _) = scan_owned(&source, None, ClaudeNativeProfile::CoreOnly);
    assert_eq!(pages.len(), 1);
    assert_eq!(
        pages[0].certificate.certified_prefix_end,
        pages[0].next_safe_frontier.complete_offset
    );
    assert_eq!(
        pages[0].certificate.certified_prefix_chain_sha256,
        pages[0].next_safe_frontier.complete_record_chain_sha256
    );

    append_line(&first_path, &message("first", "first-2", "changed"));
    assert!(matches!(
        current.revalidate_inventory(),
        Err(ClaudeNativePathError::InventoryChanged { .. })
    ));
    assert!(authoritative_deletion_candidates(&current, &checkpoints).is_err());
}

#[test]
fn each_emitted_page_is_certified_and_later_source_mutation_blocks_completion() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-certification", "certification");
    let records = (0..65)
        .map(|index| {
            message(
                "certification",
                &format!("message-{index}"),
                &format!("body-{index}"),
            )
        })
        .collect::<Vec<_>>();
    write_lines(&path, &records);
    let source = discover_session(&projects, "certification");
    let mut scanner =
        ClaudeNativeScanner::new(source, None, ClaudeNativeProfile::CoreOnly).unwrap();
    let first = match scanner.next_page().unwrap().unwrap() {
        ClaudeNativeOwnedPage::Core(page) => *page,
        ClaudeNativeOwnedPage::Pro(_) => unreachable!(),
    };
    assert_eq!(first.logical_units, 64);
    assert_eq!(
        first.certificate.certified_prefix_end,
        first.next_safe_frontier.complete_offset
    );

    append_line(
        &path,
        &message("certification", "late-message", "must invalidate finish"),
    );
    let error = loop {
        match scanner.next_page() {
            Ok(Some(_)) => continue,
            Ok(None) => panic!("mutated Claude source unexpectedly completed"),
            Err(error) => break error,
        }
    };
    assert!(matches!(error, ClaudeNativePathError::SourceChanged { .. }));
}

#[test]
fn more_than_eight_thousand_rows_stream_in_bounded_pages() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-scale", "scale");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut writer = BufWriter::new(File::create(&path).unwrap());
    let body = "conversation-body ".repeat(160);
    for index in 0..9_001 {
        writeln!(
            writer,
            "{}",
            message("scale", &format!("message-{index}"), &body)
        )
        .unwrap();
    }
    writer.flush().unwrap();

    let source = discover_session(&projects, "scale");
    let mut page_count = 0_usize;
    let mut row_count = 0_usize;
    let output = parse_session(&source, None, |page| {
        page_count += 1;
        row_count += page.rows.len();
        assert!(page.rows.len() <= CLAUDE_MAX_PAGE_ROWS);
        assert!(page.estimated_bytes <= CLAUDE_MAX_PAGE_BYTES);
        Ok(())
    })
    .unwrap();
    assert_eq!(output.stats.complete_records, 9_001);
    assert_eq!(row_count, 9_001);
    assert!(page_count >= 3);
    assert_eq!(output.stats.emitted_pages, page_count as u64);
    assert_eq!(output.stats.emitted_rows, row_count as u64);
    assert!(output.stats.peak_page_rows <= CLAUDE_MAX_PAGE_ROWS);
    assert!(output.stats.peak_page_bytes <= CLAUDE_MAX_PAGE_BYTES);
    assert_eq!(output.rejections.total, 0);
}

#[test]
fn top_level_success_and_unknown_results_never_project_content_into_core() {
    use crate::OutputOutcome;

    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-result-policy", "result-policy");
    write_lines(
        &path,
        &[
            json!({
                "sessionId": "result-policy",
                "type": "tool_result",
                "uuid": "success",
                "content": "SUCCESS-RESULT-SECRET",
                "toolUseResult": {"exitCode": 0}
            }),
            json!({
                "sessionId": "result-policy",
                "type": "tool_result",
                "uuid": "unknown",
                "content": "UNKNOWN-RESULT-SECRET"
            }),
            json!({
                "sessionId": "result-policy",
                "type": "tool_result",
                "uuid": "failure",
                "content": "FAILURE-RESULT-SECRET",
                "toolUseResult": {"exitCode": 7, "is_error": false}
            }),
            message("result-policy", "message", "retained conversation"),
        ],
    );
    let source = discover_session(&projects, "result-policy");
    let (core_only, core_pages, _) = scan_owned(&source, None, ClaudeNativeProfile::CoreOnly);
    let (_, combined_core, pro_pages) = scan_owned(&source, None, ClaudeNativeProfile::CoreAndPro);
    assert_core_pages_equal(&core_pages, &combined_core);

    let rows = core_pages
        .iter()
        .flat_map(|page| page.rows.iter())
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows.iter()
            .filter_map(|row| row.body.as_deref())
            .collect::<Vec<_>>(),
        ["retained conversation"]
    );
    let failure = rows
        .iter()
        .find_map(|row| row.sparse_output.as_ref())
        .unwrap();
    assert_eq!(failure.outcome, ClaudeOutputOutcome::Failure);
    assert_eq!(failure.exit_code, Some(7));
    assert!(rows.iter().all(|row| {
        row.body.as_deref() != Some("SUCCESS-RESULT-SECRET")
            && row.body.as_deref() != Some("UNKNOWN-RESULT-SECRET")
            && row.body.as_deref() != Some("FAILURE-RESULT-SECRET")
            && row.tool_call.is_none()
    }));
    assert_eq!(core_only.stats.result_body_bytes_decoded_or_allocated, 0);
    assert_eq!(core_only.stats.result_hashes_created, 0);
    assert_eq!(core_only.stats.result_previews_created, 0);
    assert_eq!(core_only.stats.result_touches_created, 0);
    assert_eq!(core_only.stats.result_fts_rows_created, 0);

    let outputs = pro_pages
        .iter()
        .flat_map(|page| page.outputs.iter())
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), 3);
    assert_eq!(outputs[0].content, b"SUCCESS-RESULT-SECRET");
    assert_eq!(outputs[0].outcome.outcome, OutputOutcome::Success);
    assert_eq!(outputs[1].content, b"UNKNOWN-RESULT-SECRET");
    assert_eq!(outputs[1].outcome.outcome, OutputOutcome::Unknown);
    assert_eq!(outputs[2].content, b"FAILURE-RESULT-SECRET");
    assert_eq!(outputs[2].outcome.outcome, OutputOutcome::Failure);
    assert_eq!(outputs[2].outcome.exit_code, Some(7));
}

#[test]
fn structural_preflight_is_profile_invariant_and_precedes_pro_hydration() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-preflight", "preflight");
    let too_many_blocks = (0..65)
        .map(|index| {
            json!({
                "type": "tool_result",
                "tool_use_id": format!("call-{index}"),
                "content": "BLOCK-MUST-NOT-BE-PRO-HYDRATED"
            })
        })
        .collect::<Vec<_>>();
    write_lines(
        &path,
        &[
            json!({
                "sessionId": "x".repeat(4 * 1024 + 1),
                "type": "tool_result",
                "uuid": "oversize-session",
                "content": "MUST-NOT-BE-PRO-HYDRATED",
                "toolUseResult": {"exitCode": 0}
            }),
            json!({
                "sessionId": "wrong-session",
                "type": "tool_result",
                "uuid": "mismatch",
                "content": "MISMATCH-MUST-NOT-PUBLISH",
                "toolUseResult": {"exitCode": 0}
            }),
            json!({
                "sessionId": "preflight",
                "type": "user",
                "uuid": "too-many-blocks",
                "message": {"content": too_many_blocks},
                "toolUseResult": {"exitCode": 0}
            }),
            message("preflight", "valid", "valid body"),
        ],
    );
    let source = discover_session(&projects, "preflight");
    let (core_only, core_pages, _) = scan_owned(&source, None, ClaudeNativeProfile::CoreOnly);
    let (combined, combined_core, pro_pages) =
        scan_owned(&source, None, ClaudeNativeProfile::CoreAndPro);
    assert_core_pages_equal(&core_pages, &combined_core);
    assert_eq!(core_only.rejections, combined.rejections);
    assert_eq!(core_only.rejections.total, 3);
    assert_eq!(
        core_only
            .rejections
            .samples
            .iter()
            .map(|rejection| rejection.kind)
            .collect::<Vec<_>>(),
        [
            RejectionKind::MalformedJson,
            RejectionKind::SessionIdentityMismatch,
            RejectionKind::MalformedJson,
        ]
    );
    assert!(pro_pages.iter().all(|page| page.outputs.is_empty()));
    assert_eq!(combined.stats.result_body_bytes_decoded_or_allocated, 0);
    assert_eq!(core_only.stats.semantic_record_parses, 4);
    assert_eq!(combined.stats.semantic_record_parses, 4);
}

#[test]
fn same_size_early_rewrite_with_identical_tail_is_rejected_before_delta_pages() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-prefix", "prefix");
    let original = [
        message("prefix", "one", &"A".repeat(40_000)),
        message("prefix", "two", &"B".repeat(40_000)),
        message("prefix", "three", &"C".repeat(40_000)),
    ];
    write_lines(&path, &original);
    let first_source = discover_session(&projects, "prefix");
    let first = parse_discard(&first_source, None);
    let original_len = first_source.fingerprint.len;

    let rewritten = [
        message("prefix", "one", &"Z".repeat(40_000)),
        original[1].clone(),
        original[2].clone(),
    ];
    write_lines(&path, &rewritten);
    assert_eq!(fs::metadata(&path).unwrap().len(), original_len);
    let source = discover_session(&projects, "prefix");
    let mut scanner = ClaudeNativeScanner::new(
        source,
        Some(&first.checkpoint),
        ClaudeNativeProfile::CoreOnly,
    )
    .unwrap();
    let page = match scanner.next_page().unwrap().unwrap() {
        ClaudeNativeOwnedPage::Core(page) => *page,
        ClaudeNativeOwnedPage::Pro(_) => unreachable!(),
    };
    assert_eq!(page.expected_frontier.complete_offset, 0);
    while scanner.next_page().unwrap().is_some() {}
    let output = scanner.finish().unwrap();
    assert_eq!(output.change, ChangeSignal::Rewrite);
    assert_eq!(output.stats.prefix_verification_bytes, original_len);
    assert_eq!(output.stats.prefix_verification_records, 3);
    assert_eq!(output.stats.parsed_source_bytes, original_len);
    assert_eq!(output.stats.semantic_record_parses, 3);
}

#[test]
fn pro_only_revision_upgrade_does_not_stamp_preserved_core_current() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-revisions", "revisions");
    write_lines(
        &path,
        &[
            message("revisions", "message", "body"),
            json!({
                "sessionId": "revisions",
                "type": "tool_result",
                "uuid": "result",
                "content": "output",
                "toolUseResult": {"exitCode": 0}
            }),
        ],
    );
    let source = discover_session(&projects, "revisions");
    let (baseline, _, _) = scan_owned(&source, None, ClaudeNativeProfile::CoreOnly);
    let mut old = baseline.checkpoint;
    old.parser_revision = 3;
    old.policy_revision = 3;
    old.pro_initialized = false;
    old.pro_parser_revision = 0;
    old.pro_policy_revision = 0;

    let (pro_upgrade, core_pages, pro_pages) =
        scan_owned(&source, Some(&old), ClaudeNativeProfile::ProReplayOnly);
    assert!(core_pages.is_empty());
    assert_eq!(
        pro_pages
            .iter()
            .flat_map(|page| page.outputs.iter())
            .count(),
        1
    );
    assert_eq!(pro_upgrade.checkpoint.parser_revision, 3);
    assert_eq!(pro_upgrade.checkpoint.policy_revision, 3);
    assert_eq!(
        pro_upgrade.checkpoint.pro_parser_revision,
        super::super::checkpoint::CLAUDE_NATIVEPATH_PARSER_REVISION
    );
    assert_eq!(
        pro_upgrade.checkpoint.pro_policy_revision,
        super::super::checkpoint::CLAUDE_NATIVEPATH_POLICY_REVISION
    );

    let (core_upgrade, core_pages, _) = scan_owned(
        &source,
        Some(&pro_upgrade.checkpoint),
        ClaudeNativeProfile::CoreOnly,
    );
    assert_eq!(core_upgrade.change, ChangeSignal::Reparse);
    assert!(!core_pages.is_empty());
    assert_eq!(core_upgrade.stats.semantic_record_parses, 2);
    assert!(core_upgrade.checkpoint.core_revisions_match());
    assert!(core_upgrade.checkpoint.pro_revisions_match());
}

#[test]
fn queued_core_sibling_is_revalidated_after_pro_return() {
    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-siblings", "siblings");
    write_lines(
        &path,
        &[json!({
            "sessionId": "siblings",
            "type": "tool_result",
            "uuid": "result",
            "content": "output",
            "toolUseResult": {"exitCode": 0}
        })],
    );
    let source = discover_session(&projects, "siblings");
    let mut scanner =
        ClaudeNativeScanner::new(source, None, ClaudeNativeProfile::CoreAndPro).unwrap();
    assert!(matches!(
        scanner.next_page().unwrap().unwrap(),
        ClaudeNativeOwnedPage::Pro(_)
    ));
    append_line(
        &path,
        &message("siblings", "late", "must invalidate sibling"),
    );
    let error = scanner.next_page().unwrap_err();
    assert!(matches!(error, ClaudeNativePathError::SourceChanged { .. }));
}

#[test]
fn pro_page_identity_binds_every_claim_family() {
    use crate::{
        OutputCommandContext, OutputObservationKind, OutputOutcome, OutputRepositoryContext,
    };

    let temp = tempdir().unwrap();
    let projects = projects_root(temp.path());
    let path = session_path(&projects, "-identity", "identity");
    write_lines(
        &path,
        &[json!({
            "sessionId": "identity",
            "type": "tool_result",
            "uuid": "result",
            "timestamp": "2026-01-01T00:00:00Z",
            "content": "output",
            "toolUseResult": {"exitCode": 0, "durationMs": 5}
        })],
    );
    let source = discover_session(&projects, "identity");
    let (_, _, mut pages) = scan_owned(&source, None, ClaudeNativeProfile::CoreAndPro);
    let page = &mut pages[0];
    let mut prior = super::super::reader::pro_page_identity_for_test(page).unwrap();
    assert_eq!(prior, page.identity);
    macro_rules! binds {
        ($mutation:expr) => {{
            $mutation;
            let next = super::super::reader::pro_page_identity_for_test(page).unwrap();
            assert_ne!(next, prior);
            prior = next;
        }};
    }

    binds!(page.outputs[0].kind = OutputObservationKind::Command);
    binds!(page.outputs[0].coordinate.unit_key.push('x'));
    binds!(page.outputs[0].coordinate.native_sequence += 1);
    binds!(page.outputs[0]
        .coordinate
        .native_record_id
        .as_mut()
        .unwrap()
        .push('x'));
    binds!(page.outputs[0].coordinate.source_record_ordinal = Some(9));
    binds!(page.outputs[0].coordinate.source_record_subrecord_index = Some(9));
    binds!(page.outputs[0].coordinate.byte_start = Some(9));
    binds!(page.outputs[0].coordinate.byte_end_exclusive = Some(99));
    binds!(page.outputs[0].occurred_at_unix_ms = Some(9));
    binds!(page.outputs[0].associations.direct_session_id.push('x'));
    binds!(page.outputs[0].associations.root_session_id.push('x'));
    binds!(page.outputs[0].associations.parent_session_id = Some("parent".to_owned()));
    binds!(page.outputs[0].associations.provider_session_id = Some("provider".to_owned()));
    binds!(page.outputs[0].associations.agent_id = Some("agent".to_owned()));
    binds!(
        page.outputs[0].associations.repository = Some(OutputRepositoryContext {
            repository_id: "repository".to_owned(),
            checkout_id: Some("checkout".to_owned()),
            worktree_id: Some("worktree".to_owned()),
            object_format: Some("sha256".to_owned()),
        })
    );
    binds!(page.outputs[0]
        .associations
        .repository
        .as_mut()
        .unwrap()
        .repository_id
        .push('x'));
    binds!(page.outputs[0]
        .associations
        .repository
        .as_mut()
        .unwrap()
        .checkout_id
        .as_mut()
        .unwrap()
        .push('x'));
    binds!(page.outputs[0]
        .associations
        .repository
        .as_mut()
        .unwrap()
        .worktree_id
        .as_mut()
        .unwrap()
        .push('x'));
    binds!(page.outputs[0]
        .associations
        .repository
        .as_mut()
        .unwrap()
        .object_format
        .as_mut()
        .unwrap()
        .push('x'));
    binds!(page.outputs[0].call_id = Some("call".to_owned()));
    binds!(
        page.outputs[0].command = Some(OutputCommandContext {
            tool_name: "tool".to_owned(),
            command: "command".to_owned(),
            working_directory: Some("cwd".to_owned()),
        })
    );
    binds!(page.outputs[0]
        .command
        .as_mut()
        .unwrap()
        .tool_name
        .push('x'));
    binds!(page.outputs[0].command.as_mut().unwrap().command.push('x'));
    binds!(page.outputs[0]
        .command
        .as_mut()
        .unwrap()
        .working_directory
        .as_mut()
        .unwrap()
        .push('x'));
    binds!(page.outputs[0].outcome.outcome = OutputOutcome::Failure);
    binds!(page.outputs[0].outcome.exit_code = Some(7));
    binds!(page.outputs[0].outcome.duration_ms = Some(99));
    binds!(page.outputs[0].locator.version += 1);
    binds!(page.outputs[0].locator.kind.push('x'));
    binds!(page.outputs[0].locator.payload.push(1));
    binds!(page.outputs[0].content.push(1));
    binds!(page.rejected_outputs += 1);
    binds!(page.logical_units += 1);
    binds!(page.rejections.push(RecordRejection {
        kind: RejectionKind::OversizeProOutput,
        source_record_ordinal: 0,
        locator: ClaudePhysicalLocator {
            path: path.clone(),
            byte_start: 0,
            byte_end_exclusive: 1,
            line_number: 1,
            record_sha256: [0; 32],
        },
        diagnostic: "identity rejection".to_owned(),
    }));
    binds!(page.rejections[0].kind = RejectionKind::TooManyResultSubrecords);
    binds!(page.rejections[0].source_record_ordinal += 1);
    binds!(page.rejections[0].locator.path.push("x"));
    binds!(page.rejections[0].locator.byte_start += 1);
    binds!(page.rejections[0].locator.byte_end_exclusive += 1);
    binds!(page.rejections[0].locator.line_number += 1);
    binds!(page.rejections[0].locator.record_sha256[0] ^= 1);
    binds!(page.rejections[0].diagnostic.push('x'));
    binds!(page.expected_frontier.complete_offset += 1);
    binds!(page.expected_frontier.next_raw_ordinal += 1);
    binds!(page.expected_frontier.complete_record_chain_sha256[0] ^= 1);
    binds!(page.expected_frontier.boundary_proof_len += 1);
    binds!(page.expected_frontier.boundary_proof_sha256[0] ^= 1);
    binds!(page.expected_frontier.native_identity_chain_sha256[0] ^= 1);
    binds!(page.expected_frontier.native_identity_records += 1);
    binds!(
        page.expected_frontier.appendable_boundary = !page.expected_frontier.appendable_boundary
    );
    binds!(page.next_safe_frontier.complete_offset += 1);
    binds!(page.certificate.canonical_route.push("x"));
    binds!(page.certificate.observation_sha256[0] ^= 1);
    binds!(page.certificate.physical_file_id = None);
    binds!(page.certificate.certified_prefix_end += 1);
    binds!(page.certificate.certified_prefix_chain_sha256[0] ^= 1);
    binds!(page.terminal = !page.terminal);
    binds!(page.outputs.pop());
    let _ = prior;
}
