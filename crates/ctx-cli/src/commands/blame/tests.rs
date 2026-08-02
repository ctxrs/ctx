use std::{
    io::Write as _,
    sync::{Arc, Mutex},
};

use clap::Parser as _;
use ctx_pro_host_protocol::{
    BlameResult, CommitBlameMatch, CommitFactType, CommitPredicate, FactConfidence, FactState,
    ResolvedBlameTarget, ResourceKind, ResourceRef,
};

use super::*;

fn sink_ui() -> crate::ui::Ui {
    let stdout_context = crate::ui::RenderContext::for_test(crate::ui::TestContext::pipe(
        crate::ui::StreamKind::Stdout,
    ));
    let stderr_context = crate::ui::RenderContext::for_test(crate::ui::TestContext::pipe(
        crate::ui::StreamKind::Stderr,
    ));
    crate::ui::Ui::with_writers(io::sink(), stdout_context, io::sink(), stderr_context)
}

#[derive(Clone, Default)]
struct SharedWriter {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl SharedWriter {
    fn text(&self) -> String {
        String::from_utf8(self.bytes.lock().unwrap().clone()).unwrap()
    }
}

impl io::Write for SharedWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes
            .lock()
            .map_err(|_| io::Error::other("shared blame writer was poisoned"))?
            .extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn blame_routes_recognized_human_errors_to_pro_recovery_without_changing_machine_errors() {
    for (raw, title, action) in [
        (
            "authentication_denied: private identity detail at /private/auth",
            "ctx Pro sign-in was denied",
            Some("ctx pro"),
        ),
        (
            "pro_not_installed: no helper at /private/helper",
            "ctx Pro is not set up",
            Some("ctx pro"),
        ),
        (
            "entitlement_expired: private grant detail at /private/grant",
            "ctx Pro is locked",
            Some("ctx pro manage"),
        ),
        (
            "key_store_unavailable: private vault detail at /private/vault",
            "The secure key store is unavailable",
            Some("ctx pro"),
        ),
        (
            "key_store_unavailable: interrupted Pro deletion must be completed at /private/vault",
            "A previous ctx Pro deletion is incomplete",
            Some("ctx pro uninstall --delete-data"),
        ),
        (
            "protocol_mismatch: private helper detail at /private/helper",
            "The ctx Pro helper needs repair",
            Some("ctx pro"),
        ),
        (
            "invalid_response: malformed helper frame at /private/helper",
            "ctx Pro returned an invalid response",
            Some("ctx pro"),
        ),
        (
            "resource_not_found: private graph detail at /private/graph",
            crate::pro::RESOURCE_NOT_FOUND_DIAGNOSTIC,
            None,
        ),
    ] {
        let captured = SharedWriter::default();
        let stderr = captured.clone();
        let stdout_context = crate::ui::RenderContext::for_test(crate::ui::TestContext::pipe(
            crate::ui::StreamKind::Stdout,
        ));
        let stderr_context = crate::ui::RenderContext::for_test(
            crate::ui::TestContext::tty(crate::ui::StreamKind::Stderr, 80)
                .color(crate::ui::ColorMode::Never),
        );
        let mut ui =
            crate::ui::Ui::with_writers(io::sink(), stdout_context, stderr, stderr_context);

        present_blame_result::<()>(Err(anyhow!(raw)), false, &mut ui).unwrap_err();
        ui.flush().unwrap();
        let rendered = captured.text();
        let code = raw.split(':').next().unwrap();

        assert!(
            rendered.starts_with(&format!("✗ {title}")),
            "{raw}: {rendered}"
        );
        assert!(
            !rendered.starts_with(&format!("✗ {code}:")),
            "{raw}: {rendered}"
        );
        assert!(!rendered.contains("/private"), "{raw}: {rendered}");
        match action {
            Some(action) => assert!(
                rendered.contains(&format!("Next\n  {action}\n")),
                "{raw}: {rendered}"
            ),
            None => assert!(!rendered.contains("\nNext\n"), "{raw}: {rendered}"),
        }
    }

    for (raw, expected) in [
            (
                "authentication_denied: WorkOS sign-in was denied",
                "authentication_denied: WorkOS sign-in was denied",
            ),
            (
                "pro_not_installed: private helper detail",
                "pro_not_installed: ctx Pro is not set up; run `ctx pro`",
            ),
            (
                "entitlement_expired: private grant detail",
                "entitlement_expired: ctx Pro is locked; run `ctx pro manage` to restore access",
            ),
            (
                "protocol_mismatch: private helper detail",
                "protocol_mismatch: the Pro helper needs repair; run `ctx pro`",
            ),
            (
                "key_store_unavailable: private vault detail",
                "key_store_unavailable: unlock or repair the already selected secure key store, then run `ctx pro`; a fresh installation can select the owner-private local vault only when the native store is genuinely unavailable, and ctx never downgrades existing state",
            ),
            (
                "key_store_unavailable: interrupted Pro deletion must be completed",
                "key_store_unavailable: unlock or repair the already selected secure key store, then run `ctx pro`; a fresh installation can select the owner-private local vault only when the native store is genuinely unavailable, and ctx never downgrades existing state",
            ),
            (
                "cancelled: uninstall confirmation was not provided",
                "cancelled: uninstall confirmation was not provided",
            ),
            (
                "invalid_request: qualification helpers are unsupported on this platform",
                "invalid_request: qualification helpers are unsupported on this platform",
            ),
            (
                "invalid_response: malformed helper frame at /private/helper",
                "invalid_response: malformed helper frame at /private/helper",
            ),
        ] {
            let captured = SharedWriter::default();
            let stderr = captured.clone();
            let context = crate::ui::RenderContext::for_test(crate::ui::TestContext::pipe(
                crate::ui::StreamKind::Stderr,
            ));
            let mut ui = crate::ui::Ui::with_writers(io::sink(), context, stderr, context);
            let error = present_blame_result::<()>(Err(anyhow!(raw)), true, &mut ui).unwrap_err();
            ui.flush().unwrap();
            assert_eq!(error.to_string(), expected);
            assert!(captured.text().is_empty());
        }
}

#[test]
fn line_range_parser_accepts_points_and_inclusive_ranges() {
    assert_eq!(parse_line_range("42"), Ok(LineRange { start: 42, end: 42 }));
    assert_eq!(
        parse_line_range("42:60"),
        Ok(LineRange { start: 42, end: 60 })
    );
    for invalid in ["0", "0:1", "4:3", "1:2:3", "-1", "x"] {
        assert!(parse_line_range(invalid).is_err(), "{invalid}");
    }
}

#[test]
fn shorthand_classifier_is_deterministic_and_conservative() {
    for target in [
        "42",
        "https://github.com/ctxrs/ctx/pull/42",
        "https://gitlab.example.com/group/project/-/merge_requests/42",
        "https://codeberg.org/ctxrs/ctx/pulls/42",
    ] {
        assert_eq!(
            classify_target(target),
            Some(BlameTargetType::Pr),
            "{target}"
        );
    }
    for target in ["abc1234", "0123456789abcdef0123456789abcdef01234567"] {
        assert_eq!(
            classify_target(target),
            Some(BlameTargetType::Commit),
            "{target}"
        );
    }
    for target in ["src/lib.rs", r"src\lib.rs", "README.md", ".gitignore"] {
        assert_eq!(
            classify_target(target),
            Some(BlameTargetType::File),
            "{target}"
        );
    }
    for target in [
        "main",
        "README",
        "abc",
        "feature",
        "https://example.com/not-a-pr",
    ] {
        assert_eq!(classify_target(target), None, "{target}");
    }
}

#[test]
fn shorthand_parser_builds_the_existing_typed_queries() {
    let parse = |arguments: &[&str]| {
        let cli =
            crate::Cli::try_parse_from(std::iter::once("ctx").chain(arguments.iter().copied()))
                .unwrap();
        let crate::cli::CommandRoot::Blame(args) = cli.command else {
            panic!("expected blame command");
        };
        args.into_query().unwrap().0
    };

    assert_eq!(
        parse(&[
            "blame",
            "src/lib.rs",
            "--lines",
            "4:8",
            "--repository",
            "forge:github.com/ctxrs/ctx",
        ]),
        BlameTarget::File {
            path: "src/lib.rs".to_owned(),
            repository: Some("forge:github.com/ctxrs/ctx".to_owned()),
            lines: Some(LineRange { start: 4, end: 8 }),
        }
    );
    assert_eq!(
        parse(&["blame", "0123456789abcdef"]),
        BlameTarget::Commit {
            oid: "0123456789abcdef".to_owned(),
            repository: None,
        }
    );
    assert_eq!(
        parse(&["blame", "42", "--repository", "forge:github.com/ctxrs/ctx",]),
        BlameTarget::PullRequest {
            selector: "42".to_owned(),
            repository: Some("forge:github.com/ctxrs/ctx".to_owned()),
        }
    );
}

#[test]
fn explicit_type_is_authoritative_and_invalid_type_fails_in_clap() {
    let cli =
        crate::Cli::try_parse_from(["ctx", "blame", "0123456789abcdef", "--type", "file"]).unwrap();
    let crate::cli::CommandRoot::Blame(args) = cli.command else {
        panic!("expected blame command");
    };
    assert!(matches!(
        args.into_query().unwrap().0,
        BlameTarget::File { path, .. } if path == "0123456789abcdef"
    ));

    let error = crate::Cli::try_parse_from(["ctx", "blame", "src/lib.rs", "--type", "unknown"])
        .unwrap_err()
        .to_string();
    assert!(error.contains("invalid value 'unknown'"), "{error}");
    assert!(error.contains("file, commit, pr"), "{error}");
}

#[test]
fn ambiguous_shorthand_and_non_file_lines_are_typed_actionable_errors() {
    for arguments in [
        &["ctx", "blame", "main"][..],
        &["ctx", "blame", "abc1234", "--lines", "4:8"][..],
    ] {
        let cli = crate::Cli::try_parse_from(arguments).unwrap();
        let crate::cli::CommandRoot::Blame(args) = cli.command else {
            panic!("expected blame command");
        };
        let error = args.into_query().unwrap_err().to_string();
        assert!(error.starts_with("invalid_request:"), "{error}");
        assert!(error.contains("--type"), "{error}");
    }
}

#[test]
fn explicit_blame_subcommands_keep_their_original_queries() {
    for (arguments, expected) in [
        (
            &["ctx", "blame", "file", "src/lib.rs"][..],
            BlameTarget::File {
                path: "src/lib.rs".to_owned(),
                repository: None,
                lines: None,
            },
        ),
        (
            &["ctx", "blame", "commit", "abc1234"][..],
            BlameTarget::Commit {
                oid: "abc1234".to_owned(),
                repository: None,
            },
        ),
        (
            &[
                "ctx",
                "blame",
                "pr",
                "42",
                "--repository",
                "forge:github.com/ctxrs/ctx",
            ][..],
            BlameTarget::PullRequest {
                selector: "42".to_owned(),
                repository: Some("forge:github.com/ctxrs/ctx".to_owned()),
            },
        ),
    ] {
        let cli = crate::Cli::try_parse_from(arguments).unwrap();
        let crate::cli::CommandRoot::Blame(args) = cli.command else {
            panic!("expected blame command");
        };
        assert_eq!(args.into_query().unwrap().0, expected, "{arguments:?}");
    }
}

#[test]
fn evidence_preview_is_explicit_and_available_on_universal_and_nested_targets() {
    for arguments in [
        &["ctx", "blame", "src/lib.rs", "--evidence-preview"][..],
        &[
            "ctx",
            "blame",
            "abc1234",
            "--type",
            "commit",
            "--evidence-preview",
        ],
        &[
            "ctx",
            "blame",
            "42",
            "--type",
            "pr",
            "--repository",
            "forge:github.com/ctxrs/ctx",
            "--evidence-preview",
        ],
        &["ctx", "blame", "file", "src/lib.rs", "--evidence-preview"],
        &["ctx", "blame", "commit", "abc1234", "--evidence-preview"],
        &[
            "ctx",
            "blame",
            "pr",
            "42",
            "--repository",
            "forge:github.com/ctxrs/ctx",
            "--evidence-preview",
        ],
    ] {
        let cli = crate::Cli::try_parse_from(arguments).unwrap();
        let crate::cli::CommandRoot::Blame(args) = cli.command else {
            panic!("expected blame command");
        };
        let query = args.into_query().unwrap();
        assert!(query.4, "{arguments:?}");
    }

    let cli = crate::Cli::try_parse_from(["ctx", "blame", "file", "src/lib.rs"]).unwrap();
    let crate::cli::CommandRoot::Blame(args) = cli.command else {
        panic!("expected blame command");
    };
    assert!(!args.into_query().unwrap().4);
}

#[test]
fn json_preview_conflict_stops_before_pro_or_evidence_access() {
    for arguments in [
        &[
            "ctx",
            "blame",
            "src/lib.rs",
            "--evidence-preview",
            "--format",
            "json",
        ][..],
        &[
            "ctx",
            "blame",
            "file",
            "src/lib.rs",
            "--evidence-preview",
            "--format=json",
        ],
        &[
            "ctx",
            "blame",
            "commit",
            "abc1234",
            "--evidence-preview",
            "--format=json",
        ],
        &[
            "ctx",
            "blame",
            "pr",
            "42",
            "--repository",
            "forge:github.com/ctxrs/ctx",
            "--evidence-preview",
            "--format=json",
        ],
    ] {
        let cli = crate::Cli::try_parse_from(arguments).unwrap();
        let mut usage = crate::local_usage::CliUsage::from_command(&cli.command);
        let crate::cli::CommandRoot::Blame(args) = cli.command else {
            panic!("expected blame command");
        };
        let pro_calls = std::cell::Cell::new(0usize);
        let evidence_reads = std::cell::Cell::new(0usize);
        let mut ui = sink_ui();
        let error = run_with(
            args,
            PathBuf::from("/unused"),
            &mut usage,
            &mut ui,
            |_, _, _, _| {
                pro_calls.set(pro_calls.get() + 1);
                panic!("JSON conflict reached Pro")
            },
            |_, _| {
                evidence_reads.set(evidence_reads.get() + 1);
                panic!("JSON conflict reached evidence hydration")
            },
        )
        .unwrap_err();
        assert_eq!(pro_calls.get(), 0, "{arguments:?}");
        assert_eq!(evidence_reads.get(), 0, "{arguments:?}");
        assert_eq!(
                error.to_string(),
                "invalid_request: --evidence-preview is only available for human output; remove it or use --format text"
            );
        assert!(!error.to_string().contains("/unused"));
    }
}

#[test]
fn output_failure_does_not_retain_blame_result_or_citation_counts() {
    let resource = |id: &str, kind| ResourceRef {
        id: id.to_owned(),
        kind,
        display: id.to_owned(),
    };
    let commit = resource("commit:abc1234", ResourceKind::Commit);
    let result = BlameResult {
        target: ResolvedBlameTarget::Commit {
            commit: commit.clone(),
            repository: resource("repository:ctx", ResourceKind::Repository),
        },
        git_snapshot: None,
        matches: vec![ctx_pro_host_protocol::BlameMatch::Commit(
            CommitBlameMatch {
                fact_id: "fact:1".to_owned(),
                fact_type: CommitFactType::Produced,
                predicate: CommitPredicate::ProducedBy,
                subject: commit,
                object: Some(resource("session:1", ResourceKind::Session)),
                fact_occurred_at_ms: None,
                confidence: FactConfidence::Explicit,
                state: FactState::Asserted,
                direct_actor: None,
                owning_root: None,
                evidence_numbers: Vec::new(),
            },
        )],
        evidence: Vec::new(),
        next: None,
    };
    let cli = crate::Cli::try_parse_from(["ctx", "blame", "commit", "abc1234"]).unwrap();
    let mut usage = crate::local_usage::CliUsage::from_command(&cli.command);
    let mut ui = sink_ui();

    let error = emit_blame_result(&result, true, &mut usage, &mut ui, |_, _, _| {
        Err(anyhow!("simulated output failure"))
    })
    .unwrap_err();
    assert_eq!(error.to_string(), "simulated output failure");

    let completed = usage.completed(true, std::time::Duration::ZERO).unwrap();
    assert_eq!(
        completed.result_metadata_for_test(),
        (crate::local_usage::ValueClass::NotApplicable, 0, 0)
    );
}

#[test]
fn successful_blame_observes_structured_results_and_empty_pages() {
    let resource = |id: &str, kind| ResourceRef {
        id: id.to_owned(),
        kind,
        display: id.to_owned(),
    };
    let commit = resource("commit:abc1234", ResourceKind::Commit);
    let mut result = BlameResult {
        target: ResolvedBlameTarget::Commit {
            commit: commit.clone(),
            repository: resource("repository:ctx", ResourceKind::Repository),
        },
        git_snapshot: None,
        matches: vec![ctx_pro_host_protocol::BlameMatch::Commit(
            CommitBlameMatch {
                fact_id: "fact:1".to_owned(),
                fact_type: CommitFactType::Produced,
                predicate: CommitPredicate::ProducedBy,
                subject: commit,
                object: Some(resource("session:1", ResourceKind::Session)),
                fact_occurred_at_ms: None,
                confidence: FactConfidence::Explicit,
                state: FactState::Asserted,
                direct_actor: None,
                owning_root: None,
                evidence_numbers: Vec::new(),
            },
        )],
        evidence: Vec::new(),
        next: None,
    };
    let cli = crate::Cli::try_parse_from(["ctx", "blame", "commit", "abc1234"]).unwrap();
    let mut usage = crate::local_usage::CliUsage::from_command(&cli.command);
    let mut ui = sink_ui();

    emit_blame_result(&result, true, &mut usage, &mut ui, |result, _, _| {
        blame_json_output_bytes(result)
    })
    .unwrap();
    let completed = usage.completed(true, std::time::Duration::ZERO).unwrap();
    assert_eq!(
        completed.result_metadata_for_test(),
        (crate::local_usage::ValueClass::ResultBearing, 1, 0)
    );
    assert_eq!(
        completed.delivered_output_bytes_for_test(),
        blame_json_output_bytes(&result).unwrap() as u64
    );
    assert!(blame_json_output_bytes(&result).unwrap() > serde_json::to_vec(&result).unwrap().len());

    result.matches.clear();
    let mut usage = crate::local_usage::CliUsage::from_command(&cli.command);
    let mut expected_ui = sink_ui();
    let expected_bytes = print_blame_result(&result, false, &mut expected_ui).unwrap();
    let mut ui = sink_ui();
    emit_blame_result(&result, false, &mut usage, &mut ui, print_blame_result).unwrap();
    let completed = usage.completed(true, std::time::Duration::ZERO).unwrap();
    assert_eq!(
        completed.result_metadata_for_test(),
        (crate::local_usage::ValueClass::Empty, 0, 0)
    );
    assert_eq!(
        completed.delivered_output_bytes_for_test(),
        expected_bytes as u64
    );
}

#[test]
fn human_byte_accounting_is_plain_and_invariant_across_color_modes() {
    let resource = |id: &str, kind| ResourceRef {
        id: id.to_owned(),
        kind,
        display: id.to_owned(),
    };
    let result = BlameResult {
        target: ResolvedBlameTarget::Commit {
            commit: resource("commit:abc1234", ResourceKind::Commit),
            repository: resource("repository:ctx", ResourceKind::Repository),
        },
        git_snapshot: None,
        matches: Vec::new(),
        evidence: Vec::new(),
        next: None,
    };
    let cli = crate::Cli::try_parse_from(["ctx", "blame", "commit", "abc1234"]).unwrap();
    let mut observations = Vec::new();

    for color in [crate::ui::ColorMode::Never, crate::ui::ColorMode::Always] {
        let writer = SharedWriter::default();
        let captured = writer.clone();
        let stdout_context = crate::ui::RenderContext::for_test(
            crate::ui::TestContext::tty(crate::ui::StreamKind::Stdout, 48).color(color),
        );
        let stderr_context = crate::ui::RenderContext::for_test(
            crate::ui::TestContext::pipe(crate::ui::StreamKind::Stderr)
                .color(crate::ui::ColorMode::Never),
        );
        let mut ui =
            crate::ui::Ui::with_writers(writer, stdout_context, io::sink(), stderr_context);
        let mut usage = crate::local_usage::CliUsage::from_command(&cli.command);
        emit_blame_result(&result, false, &mut usage, &mut ui, print_blame_result).unwrap();
        ui.flush().unwrap();
        let completed = usage.completed(true, std::time::Duration::ZERO).unwrap();
        observations.push((captured.text(), completed.delivered_output_bytes_for_test()));
    }

    let mut stripped = anstream::StripStream::new(Vec::new());
    stripped.write_all(observations[1].0.as_bytes()).unwrap();
    let stripped = String::from_utf8(stripped.into_inner()).unwrap();
    assert_eq!(observations[0].0, stripped);
    assert_eq!(observations[0].1, observations[1].1);
    assert_eq!(observations[0].1, observations[0].0.len() as u64);
    assert!(observations[1].0.contains("\u{1b}["));
}

#[test]
fn opted_in_preview_bytes_are_included_in_local_usage_accounting() {
    let resource = |id: &str, kind| ResourceRef {
        id: id.to_owned(),
        kind,
        display: id.to_owned(),
    };
    let result = BlameResult {
        target: ResolvedBlameTarget::Commit {
            commit: resource("commit:abc1234", ResourceKind::Commit),
            repository: resource("repository:ctx", ResourceKind::Repository),
        },
        git_snapshot: None,
        matches: Vec::new(),
        evidence: Vec::new(),
        next: None,
    };
    let previews = crate::pro::evidence_preview::EvidencePreviewModel {
        previews: Vec::new(),
    };
    let cli =
        crate::Cli::try_parse_from(["ctx", "blame", "commit", "abc1234", "--evidence-preview"])
            .unwrap();
    let mut expected_ui = sink_ui();
    let expected =
        print_blame_result_with_evidence_preview(&result, false, &previews, &mut expected_ui)
            .unwrap();
    let mut default_ui = sink_ui();
    let default = print_blame_result(&result, false, &mut default_ui).unwrap();
    assert!(expected > default);

    let mut usage = crate::local_usage::CliUsage::from_command(&cli.command);
    let mut ui = sink_ui();
    emit_blame_result(&result, false, &mut usage, &mut ui, |result, json, ui| {
        print_blame_result_with_evidence_preview(result, json, &previews, ui)
    })
    .unwrap();
    let completed = usage.completed(true, std::time::Duration::ZERO).unwrap();
    assert_eq!(completed.delivered_output_bytes_for_test(), expected as u64);
}

#[test]
fn referral_cta_requires_nonempty_interactive_human_success() {
    let resource = |id: &str, kind| ResourceRef {
        id: id.to_owned(),
        kind,
        display: id.to_owned(),
    };
    let commit = resource("commit:abc1234", ResourceKind::Commit);
    let mut result = BlameResult {
        target: ResolvedBlameTarget::Commit {
            commit: commit.clone(),
            repository: resource("repository:ctx", ResourceKind::Repository),
        },
        git_snapshot: None,
        matches: vec![ctx_pro_host_protocol::BlameMatch::Commit(
            CommitBlameMatch {
                fact_id: "fact:1".to_owned(),
                fact_type: CommitFactType::Produced,
                predicate: CommitPredicate::ProducedBy,
                subject: commit,
                object: None,
                fact_occurred_at_ms: None,
                confidence: FactConfidence::Explicit,
                state: FactState::Asserted,
                direct_actor: None,
                owning_root: None,
                evidence_numbers: Vec::new(),
            },
        )],
        evidence: Vec::new(),
        next: None,
    };

    assert!(referral_cta_eligible(&result, false, true));
    assert!(!referral_cta_eligible(&result, true, true));
    assert!(!referral_cta_eligible(&result, false, false));
    result.matches.clear();
    assert!(!referral_cta_eligible(&result, false, true));
}

#[test]
fn semantically_invalid_pr_keeps_the_cli_blame_target_not_applicable() {
    let cli = crate::Cli::try_parse_from([
        "ctx",
        "blame",
        "pr",
        "0",
        "--repository",
        "forge:github.com/ctxrs/ctx",
    ])
    .unwrap();
    let mut usage = crate::local_usage::CliUsage::from_command(&cli.command);
    let crate::cli::CommandRoot::Blame(args) = cli.command else {
        panic!("expected blame command");
    };
    let mut ui = sink_ui();

    let error = run(args, PathBuf::from("/unused"), &mut usage, &mut ui).unwrap_err();
    assert!(error.to_string().contains("invalid_request"));
    let completed = usage.completed(false, std::time::Duration::ZERO).unwrap();
    assert_eq!(
        completed.target_type_for_test(),
        crate::local_usage::TargetType::NotApplicable
    );
}
