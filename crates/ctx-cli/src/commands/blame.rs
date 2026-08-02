use std::{
    io::{self, IsTerminal as _},
    path::PathBuf,
    time::Instant,
};

use anyhow::{anyhow, Result};
use clap::{Args, Subcommand, ValueEnum};
use ctx_pro_host_protocol::{BlameTarget, LineRange, MAX_BLAME_RESULTS};

use crate::{
    analytics::{
        send_pro_operation, Outcome, ProBlameTargetV1, ProBlameTelemetryV1, ProFailureBucketV1,
        ProHostOperationV1, ProSurfaceV1,
    },
    output::JsonOutputFormat,
    pro::{print_blame_result, print_blame_result_with_evidence_preview, DEFAULT_BLAME_LIMIT},
};

mod evidence_hydration;

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
pub(crate) struct BlameArgs {
    #[command(subcommand)]
    pub(crate) explicit_target: Option<BlameTargetArgs>,
    #[arg(
        value_name = "TARGET",
        required = true,
        help = "File path, Git commit ID, or positive PR number/canonical PR URL"
    )]
    pub(crate) target: Option<String>,
    #[arg(
        long = "type",
        value_enum,
        value_name = "TYPE",
        requires = "target",
        help = "Interpret TARGET as file, commit, or pr; overrides auto-detection"
    )]
    pub(crate) target_type: Option<BlameTargetType>,
    #[arg(
        long,
        value_name = "START[:END]",
        value_parser = parse_line_range,
        requires = "target",
        help = "Positive 1-based committed line or inclusive line range; file targets only"
    )]
    pub(crate) lines: Option<LineRange>,
    #[arg(
        long,
        value_name = "REPOSITORY",
        requires = "target",
        help = "Optional logical repository identity, such as forge:github.com/ctxrs/ctx; required with a PR number and never a checkout path"
    )]
    pub(crate) repository: Option<String>,
    #[arg(
        long,
        default_value_t = DEFAULT_BLAME_LIMIT,
        value_parser = parse_blame_limit,
        requires = "target",
        help = "Maximum complete matches to return, from 1 to 100"
    )]
    pub(crate) limit: u32,
    #[arg(
        long,
        requires = "target",
        help = "Opaque continuation cursor from a previous blame page"
    )]
    pub(crate) cursor: Option<String>,
    #[arg(
        long,
        value_enum,
        default_value_t = JsonOutputFormat::Text,
        requires = "target"
    )]
    pub(crate) format: JsonOutputFormat,
    #[arg(
        long,
        requires = "target",
        help = "Include exact cited local-history evidence in human output for this invocation"
    )]
    pub(crate) evidence_preview: bool,
}

impl BlameArgs {
    pub(crate) const fn json_output(&self) -> bool {
        match &self.explicit_target {
            Some(BlameTargetArgs::File(args)) => args.format.is_json(),
            Some(BlameTargetArgs::Commit(args)) => args.format.is_json(),
            Some(BlameTargetArgs::PullRequest(args)) => args.format.is_json(),
            None => self.format.is_json(),
        }
    }

    fn into_query(self) -> Result<(BlameTarget, u32, Option<String>, bool, bool)> {
        if let Some(target) = self.explicit_target {
            return validated_query(explicit_query(target));
        }
        let target = self
            .target
            .ok_or_else(|| anyhow!("invalid_request: a blame target is required"))?;
        let target_type = self
            .target_type
            .or_else(|| classify_target(&target))
            .ok_or_else(|| {
                anyhow!(
                    "invalid_request: blame target type is ambiguous; use --type file, --type commit, or --type pr"
                )
            })?;
        if self.lines.is_some() && target_type != BlameTargetType::File {
            return Err(anyhow!(
                "invalid_request: --lines is only valid for file blame; use --type file if the target is a path"
            ));
        }
        let target = match target_type {
            BlameTargetType::File => BlameTarget::File {
                path: target,
                repository: self.repository,
                lines: self.lines,
            },
            BlameTargetType::Commit => BlameTarget::Commit {
                oid: target,
                repository: self.repository,
            },
            BlameTargetType::Pr => BlameTarget::PullRequest {
                selector: target,
                repository: self.repository,
            },
        };
        validated_query((
            target,
            self.limit,
            self.cursor,
            self.format.is_json(),
            self.evidence_preview,
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum BlameTargetType {
    File,
    Commit,
    Pr,
}

#[derive(Debug, Subcommand)]
pub(crate) enum BlameTargetArgs {
    #[command(about = "Show cited provenance for committed file lines")]
    File(FileBlameArgs),
    #[command(about = "Show directly cited provenance for a commit")]
    Commit(CommitBlameArgs),
    #[command(
        name = "pr",
        about = "Show cited activity and code provenance for a pull request"
    )]
    PullRequest(PullRequestBlameArgs),
}

#[derive(Debug, Args)]
pub(crate) struct FileBlameArgs {
    #[arg(value_name = "PATH", help = "Repository-relative committed file path")]
    pub(crate) path: String,
    #[arg(
        long,
        value_name = "START[:END]",
        value_parser = parse_line_range,
        help = "Positive 1-based committed line or inclusive line range"
    )]
    pub(crate) lines: Option<LineRange>,
    #[arg(
        long,
        value_name = "REPOSITORY",
        help = "Optional logical repository identity, such as forge:github.com/ctxrs/ctx; never a checkout path"
    )]
    pub(crate) repository: Option<String>,
    #[arg(
        long,
        default_value_t = DEFAULT_BLAME_LIMIT,
        value_parser = parse_blame_limit,
        help = "Maximum complete matches to return, from 1 to 100"
    )]
    pub(crate) limit: u32,
    #[arg(long, help = "Opaque continuation cursor from a previous blame page")]
    pub(crate) cursor: Option<String>,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub(crate) format: JsonOutputFormat,
    #[arg(
        long,
        help = "Include exact cited local-history evidence in human output for this invocation"
    )]
    pub(crate) evidence_preview: bool,
}

#[derive(Debug, Args)]
pub(crate) struct CommitBlameArgs {
    #[arg(
        value_name = "SHA",
        help = "Full or unambiguous abbreviated Git commit ID"
    )]
    pub(crate) oid: String,
    #[arg(
        long,
        value_name = "REPOSITORY",
        help = "Optional logical repository identity, such as forge:github.com/ctxrs/ctx; never a checkout path"
    )]
    pub(crate) repository: Option<String>,
    #[arg(
        long,
        default_value_t = DEFAULT_BLAME_LIMIT,
        value_parser = parse_blame_limit,
        help = "Maximum complete matches to return, from 1 to 100"
    )]
    pub(crate) limit: u32,
    #[arg(long, help = "Opaque continuation cursor from a previous blame page")]
    pub(crate) cursor: Option<String>,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub(crate) format: JsonOutputFormat,
    #[arg(
        long,
        help = "Include exact cited local-history evidence in human output for this invocation"
    )]
    pub(crate) evidence_preview: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PullRequestBlameArgs {
    #[arg(
        value_name = "NUMBER_OR_URL",
        help = "Positive PR number or canonical GitHub, GitLab, or Codeberg PR/MR URL"
    )]
    pub(crate) selector: String,
    #[arg(
        long,
        value_name = "REPOSITORY",
        help = "Logical repository identity, such as forge:github.com/ctxrs/ctx; required with a PR number and optional with a canonical URL"
    )]
    pub(crate) repository: Option<String>,
    #[arg(
        long,
        default_value_t = DEFAULT_BLAME_LIMIT,
        value_parser = parse_blame_limit,
        help = "Maximum complete matches to return, from 1 to 100"
    )]
    pub(crate) limit: u32,
    #[arg(long, help = "Opaque continuation cursor from a previous blame page")]
    pub(crate) cursor: Option<String>,
    #[arg(long, value_enum, default_value_t = JsonOutputFormat::Text)]
    pub(crate) format: JsonOutputFormat,
    #[arg(
        long,
        help = "Include exact cited local-history evidence in human output for this invocation"
    )]
    pub(crate) evidence_preview: bool,
}

fn explicit_query(target: BlameTargetArgs) -> (BlameTarget, u32, Option<String>, bool, bool) {
    match target {
        BlameTargetArgs::File(args) => (
            BlameTarget::File {
                path: args.path,
                repository: args.repository,
                lines: args.lines,
            },
            args.limit,
            args.cursor,
            args.format.is_json(),
            args.evidence_preview,
        ),
        BlameTargetArgs::Commit(args) => (
            BlameTarget::Commit {
                oid: args.oid,
                repository: args.repository,
            },
            args.limit,
            args.cursor,
            args.format.is_json(),
            args.evidence_preview,
        ),
        BlameTargetArgs::PullRequest(args) => (
            BlameTarget::PullRequest {
                selector: args.selector,
                repository: args.repository,
            },
            args.limit,
            args.cursor,
            args.format.is_json(),
            args.evidence_preview,
        ),
    }
}

fn validated_query(
    query: (BlameTarget, u32, Option<String>, bool, bool),
) -> Result<(BlameTarget, u32, Option<String>, bool, bool)> {
    if query.3 && query.4 {
        return Err(anyhow!(
            "invalid_request: --evidence-preview is only available for human output; remove it or use --format text"
        ));
    }
    Ok(query)
}

fn classify_target(target: &str) -> Option<BlameTargetType> {
    let pr_candidate = BlameTarget::PullRequest {
        selector: target.to_owned(),
        repository: Some("auto-detection".to_owned()),
    };
    if pr_candidate.validate().is_ok() {
        return Some(BlameTargetType::Pr);
    }
    if (4..=64).contains(&target.len()) && target.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Some(BlameTargetType::Commit);
    }
    if looks_like_file_path(target) {
        return Some(BlameTargetType::File);
    }
    None
}

fn looks_like_file_path(target: &str) -> bool {
    if target.contains("://") {
        return false;
    }
    if target.contains(['/', '\\']) {
        return true;
    }
    let Some((stem, extension)) = target.rsplit_once('.') else {
        return false;
    };
    (!stem.is_empty() || target.starts_with('.')) && !extension.is_empty()
}

pub(crate) fn run(
    args: BlameArgs,
    data_root: PathBuf,
    local_usage: &mut crate::local_usage::CliUsage,
    ui: &mut crate::ui::Ui,
) -> Result<()> {
    run_with(
        args,
        data_root,
        local_usage,
        ui,
        crate::pro::blame,
        evidence_hydration::hydrate_evidence_previews,
    )
}

fn run_with(
    args: BlameArgs,
    data_root: PathBuf,
    local_usage: &mut crate::local_usage::CliUsage,
    ui: &mut crate::ui::Ui,
    blame: impl FnOnce(
        &std::path::Path,
        BlameTarget,
        u32,
        Option<String>,
    ) -> Result<ctx_pro_host_protocol::BlameResult>,
    hydrate: impl FnOnce(
        &std::path::Path,
        &ctx_pro_host_protocol::BlameResult,
    ) -> crate::pro::evidence_preview::EvidencePreviewModel,
) -> Result<()> {
    let (target, limit, cursor, json, evidence_preview) = args.into_query()?;
    target
        .validate()
        .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
    local_usage.bind_blame_target(&target);
    let interactive_human = !json && io::stdout().is_terminal() && io::stderr().is_terminal();

    let started = Instant::now();
    let target_kind = ProBlameTargetV1::from_protocol(&target);
    let mut telemetry = ProBlameTelemetryV1::new(Some(target_kind), ProSurfaceV1::Cli);
    let result = (|| {
        let result = present_blame_result(blame(&data_root, target, limit, cursor), json, ui)?;
        telemetry.complete(result.matches.len(), result.next.is_some());
        let previews = evidence_preview.then(|| hydrate(&data_root, &result));
        if let Some(previews) = previews.as_ref() {
            emit_blame_result(&result, json, local_usage, ui, |result, json, ui| {
                print_blame_result_with_evidence_preview(result, json, previews, ui)
            })?;
        } else {
            emit_blame_result(&result, json, local_usage, ui, print_blame_result)?;
        }
        let eligible = referral_cta_eligible(&result, json, interactive_human);
        crate::pro::show_cta_once(&data_root, eligible, ui);
        Ok(())
    })();
    finish_blame_telemetry(&data_root, &mut telemetry, started, result)
}

fn present_blame_result<T>(result: Result<T>, json: bool, ui: &mut crate::ui::Ui) -> Result<T> {
    crate::pro::human_blame_result(result, !json, ui)
}

fn referral_cta_eligible(
    result: &ctx_pro_host_protocol::BlameResult,
    json: bool,
    interactive: bool,
) -> bool {
    interactive && !json && !result.matches.is_empty()
}

fn emit_blame_result(
    result: &ctx_pro_host_protocol::BlameResult,
    json: bool,
    local_usage: &mut crate::local_usage::CliUsage,
    ui: &mut crate::ui::Ui,
    emit: impl FnOnce(&ctx_pro_host_protocol::BlameResult, bool, &mut crate::ui::Ui) -> Result<usize>,
) -> Result<()> {
    let measured_output_bytes = emit(result, json, ui)?;
    local_usage.set_blame_result(result);
    local_usage.set_measured_output_bytes(measured_output_bytes);
    Ok(())
}

#[cfg(test)]
fn blame_json_output_bytes(result: &ctx_pro_host_protocol::BlameResult) -> Result<usize> {
    Ok(serde_json::to_vec_pretty(result)?.len().saturating_add(1))
}

fn finish_blame_telemetry(
    data_root: &std::path::Path,
    telemetry: &mut ProBlameTelemetryV1,
    started: Instant,
    result: Result<()>,
) -> Result<()> {
    if let Err(error) = &result {
        if telemetry.result_count.is_some() {
            telemetry.failure = Some(ProFailureBucketV1::Output);
        } else {
            telemetry.fail(crate::pro::stable_error_code(error));
        }
    }
    send_pro_operation(
        data_root,
        ProHostOperationV1::Blame(*telemetry),
        if result.is_ok() {
            Outcome::Success
        } else {
            Outcome::Failure
        },
        started.elapsed(),
    );
    result
}

fn parse_line_range(value: &str) -> std::result::Result<LineRange, String> {
    let mut parts = value.split(':');
    let start = parse_positive_line(parts.next().unwrap_or_default())?;
    let end = match parts.next() {
        Some(value) => parse_positive_line(value)?,
        None => start,
    };
    if parts.next().is_some() || end < start {
        return Err("line range must be START or START:END with END >= START".to_owned());
    }
    Ok(LineRange { start, end })
}

fn parse_positive_line(value: &str) -> std::result::Result<u32, String> {
    let line = value
        .parse::<u32>()
        .map_err(|error| format!("invalid line number: {error}"))?;
    if line == 0 {
        return Err("line number must be positive".to_owned());
    }
    Ok(line)
}

fn parse_blame_limit(value: &str) -> std::result::Result<u32, String> {
    let limit = value
        .parse::<u32>()
        .map_err(|error| format!("invalid blame limit: {error}"))?;
    if !(1..=MAX_BLAME_RESULTS).contains(&limit) {
        return Err(format!(
            "blame limit must be between 1 and {MAX_BLAME_RESULTS}"
        ));
    }
    Ok(limit)
}

#[cfg(test)]
mod tests {
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
            crate::Cli::try_parse_from(["ctx", "blame", "0123456789abcdef", "--type", "file"])
                .unwrap();
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
        assert!(
            blame_json_output_bytes(&result).unwrap() > serde_json::to_vec(&result).unwrap().len()
        );

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
}
