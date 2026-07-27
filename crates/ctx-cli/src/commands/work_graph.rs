use std::{path::PathBuf, time::Instant};

use anyhow::{anyhow, Result};
use clap::Args;
use ctx_pro_host_protocol::{QueryKind, ResourceSelector, MAX_QUERY_RESULTS};

use crate::{
    analytics::{
        send_pro_operation, Outcome, ProFailureBucketV1, ProHostOperationV1, ProQueryKindV1,
        ProQuerySurfaceV1, ProQueryTelemetryV1,
    },
    pro::{print_query_result, ResourceKindArg, DEFAULT_QUERY_LIMIT},
};

#[derive(Debug, Args)]
pub(crate) struct WorkGraphArgs {
    #[arg(value_enum, help = "Resource kind to query")]
    pub(crate) kind: ResourceKindArg,
    #[arg(
        value_name = "VALUE",
        help = "Resource value, such as a SHA, path, number, URL, name, or opaque resource ID"
    )]
    pub(crate) value: String,
    #[arg(
        long,
        value_name = "REPOSITORY",
        help = "Optional logical repository identity, such as forge:github.com/ctxrs/ctx; never a checkout path"
    )]
    pub(crate) repository: Option<String>,
    #[arg(
        long,
        value_name = "LINE",
        help = "Positive 1-based source line; valid only when KIND is file"
    )]
    pub(crate) line: Option<u32>,
    #[arg(
        long,
        default_value_t = DEFAULT_QUERY_LIMIT,
        value_parser = parse_query_limit,
        help = "Maximum cited records to return, from 1 to 500"
    )]
    pub(crate) limit: u32,
    #[arg(long)]
    pub(crate) cursor: Option<String>,
    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ResourceValueArgs {
    #[arg(
        value_name = "VALUE",
        help = "Resource value, such as a SHA, path, number, URL, name, or opaque resource ID"
    )]
    pub(crate) value: String,
    #[arg(
        long,
        value_name = "REPOSITORY",
        help = "Optional logical repository identity, such as forge:github.com/ctxrs/ctx; never a checkout path"
    )]
    pub(crate) repository: Option<String>,
    #[arg(
        long,
        hide = true,
        value_name = "LINE",
        help = "Positive 1-based source line"
    )]
    pub(crate) line: Option<u32>,
    #[arg(
        long,
        default_value_t = DEFAULT_QUERY_LIMIT,
        value_parser = parse_query_limit,
        help = "Maximum cited records to return, from 1 to 500"
    )]
    pub(crate) limit: u32,
    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct FileResourceValueArgs {
    #[arg(
        value_name = "VALUE",
        help = "Resource value: a repository-relative file path or opaque file resource ID"
    )]
    pub(crate) value: String,
    #[arg(
        long,
        value_name = "REPOSITORY",
        help = "Optional logical repository identity, such as forge:github.com/ctxrs/ctx; never a checkout path"
    )]
    pub(crate) repository: Option<String>,
    #[arg(long, value_name = "LINE", help = "Positive 1-based source line")]
    pub(crate) line: Option<u32>,
    #[arg(
        long,
        default_value_t = DEFAULT_QUERY_LIMIT,
        value_parser = parse_query_limit,
        help = "Maximum cited records to return, from 1 to 500"
    )]
    pub(crate) limit: u32,
    #[arg(long)]
    pub(crate) json: bool,
}

impl From<FileResourceValueArgs> for ResourceValueArgs {
    fn from(args: FileResourceValueArgs) -> Self {
        Self {
            value: args.value,
            repository: args.repository,
            line: args.line,
            limit: args.limit,
            json: args.json,
        }
    }
}

impl ResourceValueArgs {
    pub(crate) fn into_work_graph(self, kind: ResourceKindArg) -> WorkGraphArgs {
        WorkGraphArgs {
            kind,
            value: self.value,
            repository: self.repository,
            line: self.line,
            limit: self.limit,
            cursor: None,
            json: self.json,
        }
    }
}

impl WorkGraphArgs {
    pub(crate) fn selector(&self) -> ResourceSelector {
        ResourceSelector {
            kind: self.kind.protocol(),
            value: self.value.clone(),
            repository: self.repository.clone(),
            line: self.line,
        }
    }
}

#[derive(Debug, Args)]
pub(crate) struct BlameArgs {
    #[arg(
        value_name = "FILE",
        help = "Repository-relative file path or opaque file resource ID"
    )]
    pub(crate) file: String,
    #[arg(
        long,
        value_name = "REPOSITORY",
        help = "Optional logical repository identity, such as forge:github.com/ctxrs/ctx; never a checkout path"
    )]
    pub(crate) repository: Option<String>,
    #[arg(long, value_name = "LINE", help = "Positive 1-based source line")]
    pub(crate) line: Option<u32>,
    #[arg(
        long,
        default_value_t = DEFAULT_QUERY_LIMIT,
        value_parser = parse_query_limit,
        help = "Maximum cited records to return, from 1 to 500"
    )]
    pub(crate) limit: u32,
    #[arg(long)]
    pub(crate) json: bool,
}

pub(crate) fn run(
    args: WorkGraphArgs,
    data_root: PathBuf,
    kind: QueryKind,
    payload_type: &'static str,
) -> Result<()> {
    let started = Instant::now();
    let mut telemetry =
        ProQueryTelemetryV1::new(ProQueryKindV1::from_protocol(kind), ProQuerySurfaceV1::Cli);
    let result = (|| {
        let target = args.selector();
        validate_selector(&target)?;
        let result = crate::pro::query(
            &data_root,
            kind,
            target.clone(),
            args.limit,
            args.cursor,
            &mut telemetry,
        )
        .map_err(crate::pro::actionable_error)?;
        telemetry.complete(result.records.len(), result.truncated, result.stale);
        print_query_result(payload_type, &target, &result, args.json)
    })();
    finish_query_telemetry(&data_root, &mut telemetry, started, result)
}

pub(crate) fn run_blame(args: BlameArgs, data_root: PathBuf) -> Result<()> {
    let started = Instant::now();
    let mut telemetry = ProQueryTelemetryV1::new(ProQueryKindV1::Blame, ProQuerySurfaceV1::Cli);
    let result = (|| {
        let target = ResourceSelector {
            kind: ctx_pro_host_protocol::ResourceKind::File,
            value: args.file,
            repository: args.repository,
            line: args.line,
        };
        validate_selector(&target)?;
        let result = crate::pro::query(
            &data_root,
            QueryKind::Blame,
            target.clone(),
            args.limit,
            None,
            &mut telemetry,
        )
        .map_err(crate::pro::actionable_error)?;
        telemetry.complete(result.records.len(), result.truncated, result.stale);
        print_query_result("pro_blame", &target, &result, args.json)
    })();
    finish_query_telemetry(&data_root, &mut telemetry, started, result)
}

fn finish_query_telemetry(
    data_root: &std::path::Path,
    telemetry: &mut ProQueryTelemetryV1,
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
        ProHostOperationV1::Query(*telemetry),
        if result.is_ok() {
            Outcome::Success
        } else {
            Outcome::Failure
        },
        started.elapsed(),
    );
    result
}

fn validate_selector(target: &ResourceSelector) -> Result<()> {
    target
        .validate()
        .map_err(|error| anyhow!("invalid_request: {}", error.message))
}

fn parse_query_limit(value: &str) -> std::result::Result<u32, String> {
    let limit = value
        .parse::<u32>()
        .map_err(|error| format!("invalid query limit: {error}"))?;
    if !(1..=MAX_QUERY_RESULTS).contains(&limit) {
        return Err(format!(
            "query limit must be between 1 and {MAX_QUERY_RESULTS}"
        ));
    }
    Ok(limit)
}
