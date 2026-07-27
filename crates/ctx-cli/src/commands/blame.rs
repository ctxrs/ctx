use std::{path::PathBuf, time::Instant};

use anyhow::{anyhow, Result};
use clap::{Args, Subcommand};
use ctx_pro_host_protocol::{BlameTarget, LineRange, MAX_BLAME_RESULTS};

use crate::{
    analytics::{
        send_pro_operation, Outcome, ProBlameTargetV1, ProBlameTelemetryV1, ProFailureBucketV1,
        ProHostOperationV1, ProSurfaceV1,
    },
    pro::{print_blame_result, DEFAULT_BLAME_LIMIT},
};

#[derive(Debug, Args)]
pub(crate) struct BlameArgs {
    #[command(subcommand)]
    pub(crate) target: BlameTargetArgs,
}

impl BlameArgs {
    pub(crate) const fn json_output(&self) -> bool {
        match &self.target {
            BlameTargetArgs::File(args) => args.json,
            BlameTargetArgs::Commit(args) => args.json,
            BlameTargetArgs::PullRequest(args) => args.json,
        }
    }
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
    #[arg(long, help = "Print the typed BlameResult as JSON")]
    pub(crate) json: bool,
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
    #[arg(long, help = "Print the typed BlameResult as JSON")]
    pub(crate) json: bool,
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
    #[arg(long, help = "Print the typed BlameResult as JSON")]
    pub(crate) json: bool,
}

pub(crate) fn run(
    args: BlameArgs,
    data_root: PathBuf,
    local_usage: &mut crate::local_usage::CliUsage,
) -> Result<()> {
    let (target, limit, cursor, json) = match args.target {
        BlameTargetArgs::File(args) => (
            BlameTarget::File {
                path: args.path,
                repository: args.repository,
                lines: args.lines,
            },
            args.limit,
            args.cursor,
            args.json,
        ),
        BlameTargetArgs::Commit(args) => (
            BlameTarget::Commit {
                oid: args.oid,
                repository: args.repository,
            },
            args.limit,
            args.cursor,
            args.json,
        ),
        BlameTargetArgs::PullRequest(args) => (
            BlameTarget::PullRequest {
                selector: args.selector,
                repository: args.repository,
            },
            args.limit,
            args.cursor,
            args.json,
        ),
    };
    target
        .validate()
        .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
    local_usage.bind_blame_target(&target);

    let started = Instant::now();
    let target_kind = ProBlameTargetV1::from_protocol(&target);
    let mut telemetry = ProBlameTelemetryV1::new(Some(target_kind), ProSurfaceV1::Cli);
    let result = (|| {
        let result = crate::pro::blame(&data_root, target, limit, cursor)
            .map_err(crate::pro::actionable_error)?;
        telemetry.complete(result.matches.len(), result.next.is_some());
        emit_blame_result(&result, json, local_usage, print_blame_result)
    })();
    finish_blame_telemetry(&data_root, &mut telemetry, started, result)
}

fn emit_blame_result(
    result: &ctx_pro_host_protocol::BlameResult,
    json: bool,
    local_usage: &mut crate::local_usage::CliUsage,
    emit: impl FnOnce(&ctx_pro_host_protocol::BlameResult, bool) -> Result<()>,
) -> Result<()> {
    emit(result, json)?;
    local_usage.set_blame_result(result);
    Ok(())
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
    use clap::Parser as _;
    use ctx_pro_host_protocol::{
        BlameResult, CommitBlameMatch, CommitFactType, CommitPredicate, FactConfidence, FactState,
        ResolvedBlameTarget, ResourceKind, ResourceRef,
    };

    use super::*;

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

        let error = emit_blame_result(&result, true, &mut usage, |_, _| {
            Err(anyhow!("simulated output failure"))
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "simulated output failure");

        let completed = usage.completed(false, std::time::Duration::ZERO).unwrap();
        assert_eq!(
            completed.result_metadata_for_test(),
            (crate::local_usage::ValueClass::NotApplicable, 0, 0)
        );
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

        let error = run(args, PathBuf::from("/unused"), &mut usage).unwrap_err();
        assert!(error.to_string().contains("invalid_request"));
        let completed = usage.completed(false, std::time::Duration::ZERO).unwrap();
        assert_eq!(
            completed.target_type_for_test(),
            crate::local_usage::TargetType::NotApplicable
        );
    }
}
