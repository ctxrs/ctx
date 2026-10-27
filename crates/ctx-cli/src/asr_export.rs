//! Native ASR export is independent of the searchable Core store.
use anyhow::Result;
use clap::{Args, Subcommand, ValueEnum};
use ctx_history_asr::{Input, Options, Provider};
use std::path::PathBuf;

#[derive(Debug, Subcommand)]
pub(crate) enum ExportArgs {
    /// Convert native sessions to an Agent Session Record 0.1 archive bundle.
    Asr(AsrArgs),
}
#[derive(Debug, Clone, Copy, ValueEnum)]
enum AsrProvider {
    #[value(alias = "claude-code")]
    Claude,
    Codex,
    Opencode,
    Kilo,
    Mimocode,
    #[value(alias = "kiro-cli")]
    Kiro,
}
#[derive(Debug, Args)]
pub(crate) struct AsrArgs {
    /// Exact native session ID (a Claude subagent uses its agentId).
    #[arg(long, required_unless_present = "all", conflicts_with = "all")]
    session: Option<String>,
    /// Export every session from the selected supported converters.
    #[arg(long)]
    all: bool,
    #[arg(long, value_enum)]
    provider: Option<AsrProvider>,
    /// Read this native transcript or directory instead of automatic discovery.
    #[arg(long, requires = "provider")]
    path: Option<PathBuf>,
    /// Destination archive directory, including bundle.json and session streams.
    #[arg(long)]
    out: PathBuf,
    /// Maximum bytes in a native line, SQLite row, or assembled content field.
    #[arg(long,default_value_t=16*1024*1024)]
    max_record_bytes: usize,
    /// Stable Kiro installation/profile identity (required for legacy directory keys).
    #[arg(long)]
    namespace: Option<String>,
}
impl ExportArgs {
    pub(crate) fn run(self) -> i32 {
        match self.execute() {
            Ok(()) => 0,
            Err(error) => {
                crate::output::write_stderr_line(format_args!("ASR export failed: {error:#}"));
                1
            }
        }
    }
    fn execute(self) -> Result<()> {
        let Self::Asr(args) = self;
        let providers = match args.provider {
            Some(AsrProvider::Claude) => vec![Provider::Claude],
            Some(AsrProvider::Codex) => vec![Provider::Codex],
            Some(AsrProvider::Opencode) => vec![Provider::OpenCode],
            Some(AsrProvider::Kilo) => vec![Provider::Kilo],
            Some(AsrProvider::Mimocode) => vec![Provider::MiMoCode],
            Some(AsrProvider::Kiro) => vec![Provider::Kiro],
            None => vec![
                Provider::Claude,
                Provider::Codex,
                Provider::OpenCode,
                Provider::Kilo,
                Provider::MiMoCode,
                Provider::Kiro,
            ],
        };
        let mut inputs = Vec::new();
        for provider in providers {
            if let Some(path) = &args.path {
                inputs.push(Input {
                    provider,
                    path: path.clone(),
                });
                continue;
            }
            let capture = match provider {
                Provider::Claude => ctx_history_core::CaptureProvider::Claude,
                Provider::Codex => ctx_history_core::CaptureProvider::Codex,
                Provider::OpenCode => ctx_history_core::CaptureProvider::OpenCode,
                Provider::Kilo => ctx_history_core::CaptureProvider::Kilo,
                Provider::MiMoCode => ctx_history_core::CaptureProvider::MiMoCode,
                Provider::Kiro => ctx_history_core::CaptureProvider::KiroCli,
            };
            let home = crate::identity::home_dir();
            let report =
                ctx_history_cli::discovered_sources_for_provider_report(home.as_deref(), capture);
            for source in report.sources {
                if (source.source_format.contains("jsonl")
                    || (provider.is_sqlite() && source.source_format.contains("sqlite")))
                    && !source.source_format.contains("history_jsonl")
                {
                    inputs.push(Input {
                        provider,
                        path: source.path,
                    });
                }
            }
        }
        let mut options = Options::new(inputs, args.out);
        options.session = args.session;
        options.max_record_bytes = args.max_record_bytes;
        options.namespace = args.namespace;
        let result = ctx_history_asr::export(&options)?;
        crate::output::write_stdout_line(format_args!("{}", serde_json::to_string(&result)?));
        Ok(())
    }
}
