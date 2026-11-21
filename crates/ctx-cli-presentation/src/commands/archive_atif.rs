use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Args;
use ctx_history_cli::{export_archive_atif, ArchiveAtifRequest};

use crate::{parse_provider_arg, ProviderArg};

/// Export selected sessions as ATIF v1.8 JSON trajectory files.
#[derive(Debug, Args)]
pub struct ArchiveAtifArgs {
    /// Output directory where `.atif.json` trajectory files will be written.
    #[arg(long)]
    pub out: PathBuf,
    /// Export this ctx session id or unambiguous id prefix (repeat to select more).
    #[arg(long)]
    pub session: Vec<String>,
    /// Filter exported sessions by provider (repeat to include more).
    #[arg(long, value_parser = parse_provider_arg, hide_possible_values = true)]
    pub provider: Vec<ProviderArg>,
    /// Substring of stored workspace/CWD/project metadata.
    #[arg(long)]
    pub workspace: Option<String>,
    /// Inclusive RFC3339 timestamp; requires --until.
    #[arg(long)]
    pub since: Option<String>,
    /// Exclusive RFC3339 timestamp; requires --since.
    #[arg(long)]
    pub until: Option<String>,
    /// Do not embed child/subagent sessions in `subagent_trajectories`.
    #[arg(long)]
    pub no_subagents: bool,
}

pub fn run(args: &ArchiveAtifArgs, root: &Path) -> Result<(serde_json::Value, String)> {
    let request = ArchiveAtifRequest {
        out_dir: args.out.clone(),
        sessions: args.session.clone(),
        providers: args
            .provider
            .iter()
            .map(|provider| provider.capture_provider())
            .collect(),
        workspace: args.workspace.clone(),
        since: args.since.clone(),
        until: args.until.clone(),
        include_subagents: !args.no_subagents,
    };
    let receipt = export_archive_atif(&request, root)?;
    let text = if receipt.sessions_exported == 0 {
        format!(
            "No matching sessions; wrote 0 ATIF v1.8 trajectory files to {}",
            receipt.output_dir.display()
        )
    } else if receipt.subagents_embedded > 0 {
        format!(
            "Exported {} ATIF v1.8 trajectory file(s) ({} step(s), {} embedded subagent(s)) to {}",
            receipt.sessions_exported,
            receipt.steps_exported,
            receipt.subagents_embedded,
            receipt.output_dir.display()
        )
    } else {
        format!(
            "Exported {} ATIF v1.8 trajectory file(s) ({} step(s)) to {}",
            receipt.sessions_exported,
            receipt.steps_exported,
            receipt.output_dir.display()
        )
    };
    Ok((receipt.to_json(), text))
}
