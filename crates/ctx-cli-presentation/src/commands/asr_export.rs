//! Public argument schema for native Agent Session Record export.
use clap::{Args, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Debug, Subcommand)]
pub enum ExportArgs {
    /// Convert native sessions to an Agent Session Record 0.1 archive bundle.
    Asr(AsrArgs),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AsrProvider {
    #[value(alias = "claude-code")]
    Claude,
    Codex,
    Pi,
    GrokBuild,
    #[value(name = "deepseek-harness", alias = "deep-seek-harness")]
    DeepSeekHarness,
    #[value(alias = "gemini-cli")]
    Gemini,
    Antigravity,
    QwenCode,
    #[value(alias = "copilot-cli")]
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
    #[value(name = "opencode")]
    OpenCode,
    Kilo,
    #[value(name = "mimocode")]
    MiMoCode,
    #[value(alias = "kiro-cli")]
    Kiro,
    Crush,
    Goose,
    Lingma,
    Warp,
    Hermes,
    #[value(name = "astrbot")]
    AstrBot,
    Shelley,
    Zed,
    Firebender,
    #[value(name = "forgecode")]
    ForgeCode,
    #[value(name = "deepagents", alias = "deep-agents")]
    DeepAgents,
    Devin,
    #[value(name = "openclaw")]
    OpenClaw,
    #[value(name = "nanoclaw")]
    NanoClaw,
    #[value(name = "codebuddy")]
    CodeBuddy,
    Continue,
    #[value(name = "openhands")]
    OpenHands,
    Auggie,
    #[value(name = "rovodev")]
    RovoDev,
    Cline,
    #[value(alias = "roo-code")]
    Roo,
    Fx,
}
#[derive(Debug, Args)]
pub struct AsrArgs {
    /// Exact native session ID (a Claude subagent uses its agentId).
    #[arg(long, required_unless_present = "all", conflicts_with = "all")]
    pub session: Option<String>,
    /// Export every session from the selected supported converters.
    #[arg(long)]
    pub all: bool,
    #[arg(long, value_enum)]
    pub provider: Option<AsrProvider>,
    /// Read this native transcript or directory instead of automatic discovery.
    #[arg(long, requires = "provider")]
    pub path: Option<PathBuf>,
    /// Stable installation/profile identity for locally scoped native stores.
    #[arg(long)]
    pub namespace: Option<String>,
    /// Destination archive directory, including bundle.json and session streams.
    #[arg(long)]
    pub out: PathBuf,
    /// Maximum bytes in a native line or assembled content field.
    #[arg(long,default_value_t=16*1024*1024)]
    pub max_record_bytes: usize,
}

impl ExportArgs {
    pub fn execute(
        self,
        home: Option<&std::path::Path>,
    ) -> anyhow::Result<ctx_history_asr::Summary> {
        use ctx_history_asr::{Input, Options, Provider};
        use ctx_history_core::CaptureProvider;
        let ExportArgs::Asr(args) = self;
        let mut inputs = Vec::new();
        for selected in AsrProvider::value_variants()
            .iter()
            .copied()
            .filter(|p| args.provider.is_none_or(|selected| selected == *p))
        {
            let (provider, capture) = match selected {
                AsrProvider::Claude => (Provider::Claude, CaptureProvider::Claude),
                AsrProvider::Codex => (Provider::Codex, CaptureProvider::Codex),
                AsrProvider::Pi => (Provider::Pi, CaptureProvider::Pi),
                AsrProvider::GrokBuild => (Provider::GrokBuild, CaptureProvider::GrokBuild),
                AsrProvider::DeepSeekHarness => {
                    (Provider::DeepSeekHarness, CaptureProvider::DeepSeekHarness)
                }
                AsrProvider::Gemini => (Provider::Gemini, CaptureProvider::Gemini),
                AsrProvider::Antigravity => (Provider::Antigravity, CaptureProvider::Antigravity),
                AsrProvider::QwenCode => (Provider::QwenCode, CaptureProvider::QwenCode),
                AsrProvider::Copilot => (Provider::Copilot, CaptureProvider::CopilotCli),
                AsrProvider::Droid => (Provider::Droid, CaptureProvider::FactoryAiDroid),
                AsrProvider::Kimi => (Provider::Kimi, CaptureProvider::KimiCodeCli),
                AsrProvider::Qoder => (Provider::Qoder, CaptureProvider::Qoder),
                AsrProvider::Junie => (Provider::Junie, CaptureProvider::Junie),
                AsrProvider::MistralVibe => (Provider::MistralVibe, CaptureProvider::MistralVibe),
                AsrProvider::Tabnine => (Provider::Tabnine, CaptureProvider::Tabnine),
                AsrProvider::Mux => (Provider::Mux, CaptureProvider::Mux),
                AsrProvider::Cursor => (Provider::Cursor, CaptureProvider::Cursor),
                AsrProvider::OpenCode => (Provider::OpenCode, CaptureProvider::OpenCode),
                AsrProvider::Kilo => (Provider::Kilo, CaptureProvider::Kilo),
                AsrProvider::MiMoCode => (Provider::MiMoCode, CaptureProvider::MiMoCode),
                AsrProvider::Kiro => (Provider::Kiro, CaptureProvider::KiroCli),
                AsrProvider::Crush => (Provider::Crush, CaptureProvider::Crush),
                AsrProvider::Goose => (Provider::Goose, CaptureProvider::Goose),
                AsrProvider::Lingma => (Provider::Lingma, CaptureProvider::Lingma),
                AsrProvider::Warp => (Provider::Warp, CaptureProvider::Warp),
                AsrProvider::Hermes => (Provider::Hermes, CaptureProvider::Hermes),
                AsrProvider::AstrBot => (Provider::AstrBot, CaptureProvider::AstrBot),
                AsrProvider::Shelley => (Provider::Shelley, CaptureProvider::Shelley),
                AsrProvider::Zed => (Provider::Zed, CaptureProvider::Zed),
                AsrProvider::Firebender => (Provider::Firebender, CaptureProvider::Firebender),
                AsrProvider::ForgeCode => (Provider::ForgeCode, CaptureProvider::ForgeCode),
                AsrProvider::DeepAgents => (Provider::DeepAgents, CaptureProvider::DeepAgents),
                AsrProvider::Devin => (Provider::Devin, CaptureProvider::Devin),
                AsrProvider::OpenClaw => (Provider::OpenClaw, CaptureProvider::OpenClaw),
                AsrProvider::NanoClaw => (Provider::NanoClaw, CaptureProvider::NanoClaw),
                AsrProvider::CodeBuddy => (Provider::CodeBuddy, CaptureProvider::CodeBuddy),
                AsrProvider::Continue => (Provider::Continue, CaptureProvider::Continue),
                AsrProvider::OpenHands => (Provider::OpenHands, CaptureProvider::OpenHands),
                AsrProvider::Auggie => (Provider::Auggie, CaptureProvider::Auggie),
                AsrProvider::RovoDev => (Provider::RovoDev, CaptureProvider::RovoDev),
                AsrProvider::Cline => (Provider::Cline, CaptureProvider::Cline),
                AsrProvider::Roo => (Provider::Roo, CaptureProvider::RooCode),
                AsrProvider::Fx => (Provider::Fx, CaptureProvider::Fx),
            };
            if let Some(path) = &args.path {
                inputs.push(Input {
                    provider,
                    path: path.clone(),
                });
                continue;
            }
            let report = ctx_history_cli::discovered_sources_for_provider_report(home, capture);
            for source in report.sources {
                if !source.source_format.contains("history_jsonl") {
                    inputs.push(Input {
                        provider,
                        path: source.path,
                    });
                }
            }
        }
        let mut options = Options::new(inputs, args.out);
        options.session = args.session;
        options.namespace = args.namespace;
        options.max_record_bytes = args.max_record_bytes;
        Ok(ctx_history_asr::export(&options)?)
    }
}
