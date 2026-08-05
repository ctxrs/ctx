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

    fn into_query(self) -> Result<(BlameTarget, u32, Option<String>, bool)> {
        if let Some(target) = self.explicit_target {
            return Ok(explicit_query(target));
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
        Ok((target, self.limit, self.cursor, self.format.is_json()))
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
}

fn explicit_query(target: BlameTargetArgs) -> (BlameTarget, u32, Option<String>, bool) {
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
        ),
        BlameTargetArgs::Commit(args) => (
            BlameTarget::Commit {
                oid: args.oid,
                repository: args.repository,
            },
            args.limit,
            args.cursor,
            args.format.is_json(),
        ),
        BlameTargetArgs::PullRequest(args) => (
            BlameTarget::PullRequest {
                selector: args.selector,
                repository: args.repository,
            },
            args.limit,
            args.cursor,
            args.format.is_json(),
        ),
    }
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
        hydrate_evidence_context,
    )
}

pub(crate) fn hydrate_evidence_context(
    data_root: &std::path::Path,
    result: &ctx_pro_host_protocol::BlameResult,
) -> crate::pro::evidence_preview::EvidencePreviewModel {
    evidence_hydration::hydrate_evidence_previews(data_root, result)
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
    ) -> Result<crate::pro::HostedBlameResult>,
    hydrate: impl FnOnce(
        &std::path::Path,
        &ctx_pro_host_protocol::BlameResult,
    ) -> crate::pro::evidence_preview::EvidencePreviewModel,
) -> Result<()> {
    let (target, limit, cursor, json) = args.into_query()?;
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
        if matches!(
            &result.target,
            ctx_pro_host_protocol::ResolvedBlameTarget::File { .. }
        ) {
            let previews = hydrate(&data_root, &result.result);
            emit_blame_result(&result, json, local_usage, ui, |result, json, ui| {
                print_blame_result_with_evidence_preview(result, json, &previews, ui)
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
    result: &crate::pro::HostedBlameResult,
    json: bool,
    interactive: bool,
) -> bool {
    interactive && !json && !result.matches.is_empty()
}

fn emit_blame_result(
    result: &crate::pro::HostedBlameResult,
    json: bool,
    local_usage: &mut crate::local_usage::CliUsage,
    ui: &mut crate::ui::Ui,
    emit: impl FnOnce(&crate::pro::HostedBlameResult, bool, &mut crate::ui::Ui) -> Result<usize>,
) -> Result<()> {
    let measured_output_bytes = emit(result, json, ui)?;
    local_usage.set_blame_result(&result.result);
    local_usage.set_measured_output_bytes(measured_output_bytes);
    Ok(())
}

#[cfg(test)]
fn blame_json_output_bytes(
    result: &ctx_pro_host_protocol::BlameResult,
    previews: Option<&crate::pro::evidence_preview::EvidencePreviewModel>,
) -> Result<usize> {
    Ok(
        serde_json::to_vec_pretty(&crate::pro::blame_result_json(result, previews))?
            .len()
            .saturating_add(1),
    )
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
#[path = "blame/tests.rs"]
mod tests;
