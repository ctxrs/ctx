//! Native ASR export is independent of the searchable Core store.
use anyhow::Result;
use clap::{Args, Subcommand, ValueEnum};
use ctx_history_asr::{Input, Options, Provider};
use ctx_history_core::CaptureProvider as Capture;
use std::path::PathBuf;

#[derive(Debug, Subcommand)]
pub(crate) enum ExportArgs {
    /// Convert native sessions to an Agent Session Record 0.1 archive bundle.
    Asr(AsrArgs),
}
#[derive(Debug, Clone, Copy, PartialEq, ValueEnum)]
enum AsrAgent {
    #[value(alias = "claude-code")]
    Claude,
    Codex,
    #[value(alias = "gemini-cli")]
    Gemini,
    Antigravity,
    QwenCode,
    Pi,
    GrokBuild,
    #[value(name = "deepseek-harness")]
    DeepSeekHarness,
    #[value(name = "copilot", alias = "copilot-cli")]
    Copilot,
    #[value(alias = "factory-droid")]
    Droid,
    #[value(alias = "kimi-code")]
    Kimi,
    Qoder,
    Junie,
    MistralVibe,
    Tabnine,
    Mux,
    Cursor,
    Opencode,
    Kilo,
    Mimocode,
    #[value(alias = "kiro-cli")]
    Kiro,
    Crush,
    Goose,
    Lingma,
    Warp,
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
    provider: Option<AsrAgent>,
    /// Read this native transcript or directory instead of automatic discovery.
    #[arg(long, requires = "provider")]
    path: Option<PathBuf>,
    /// Stable Goose installation/profile identifier, reused across exports.
    #[arg(long)]
    namespace: Option<String>,
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
        let mut inputs = Vec::new();
        for choice in AsrAgent::value_variants() {
            if args.provider.is_some_and(|p| p != *choice) {
                continue;
            }
            let (provider, capture) = match choice {
                AsrAgent::Claude => (Provider::Claude, Capture::Claude),
                AsrAgent::Codex => (Provider::Codex, Capture::Codex),
                AsrAgent::Gemini => (Provider::Gemini, Capture::Gemini),
                AsrAgent::Antigravity => (Provider::Antigravity, Capture::Antigravity),
                AsrAgent::QwenCode => (Provider::QwenCode, Capture::QwenCode),
                AsrAgent::Pi => (Provider::Pi, Capture::Pi),
                AsrAgent::GrokBuild => (Provider::GrokBuild, Capture::GrokBuild),
                AsrAgent::DeepSeekHarness => (Provider::DeepSeekHarness, Capture::DeepSeekHarness),
                AsrAgent::Copilot => (Provider::Copilot, Capture::CopilotCli),
                AsrAgent::Droid => (Provider::Droid, Capture::FactoryAiDroid),
                AsrAgent::Kimi => (Provider::Kimi, Capture::KimiCodeCli),
                AsrAgent::Qoder => (Provider::Qoder, Capture::Qoder),
                AsrAgent::Junie => (Provider::Junie, Capture::Junie),
                AsrAgent::MistralVibe => (Provider::MistralVibe, Capture::MistralVibe),
                AsrAgent::Tabnine => (Provider::Tabnine, Capture::Tabnine),
                AsrAgent::Mux => (Provider::Mux, Capture::Mux),
                AsrAgent::Cursor => (Provider::Cursor, Capture::Cursor),
                AsrAgent::OpenCode => (Provider::OpenCode, Capture::OpenCode),
                AsrAgent::Kilo => (Provider::Kilo, Capture::Kilo),
                AsrAgent::MiMoCode => (Provider::MiMoCode, Capture::MiMoCode),
                AsrAgent::Kiro => (Provider::Kiro, Capture::Kiro),
                AsrAgent::Crush => (Provider::Crush, Capture::Crush),
                AsrAgent::Goose => (Provider::Goose, Capture::Goose),
                AsrAgent::Lingma => (Provider::Lingma, Capture::Lingma),
                AsrAgent::Warp => (Provider::Warp, Capture::Warp),
            };
            let paths = args.path.clone().map(|p| vec![p]).unwrap_or_else(|| {
                ctx_history_cli::discovered_sources_for_provider_report(
                    crate::identity::home_dir().as_deref(),
                    capture,
                )
                .sources
                .into_iter()
                .filter(|s| {
                    !s.source_format.contains("history_jsonl")
                })
                .map(|s| s.path)
                .collect()
            });
            inputs.extend(paths.into_iter().map(|path| Input { provider, path }));
        }
        let mut options = Options::new(inputs, args.out);
        options.session = args.session;
        options.namespace = args.namespace;
        options.max_record_bytes = args.max_record_bytes;
        options.namespace = args.namespace;
        let result = ctx_history_asr::export(&options)?;
        crate::output::write_stdout_line(format_args!("{}", serde_json::to_string(&result)?));
        Ok(())
    }
}
