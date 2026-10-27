//! Native ASR export is independent of the searchable Core store.
use anyhow::Result;
use clap::ValueEnum;
use ctx_cli_presentation::commands::asr_export::AsrProvider;
pub(crate) use ctx_cli_presentation::commands::asr_export::ExportArgs;
use ctx_history_asr::{Input, Options, Provider};
use ctx_history_core::CaptureProvider;

pub(crate) fn run(args: ExportArgs) -> i32 {
    match execute(args) {
        Ok(()) => 0,
        Err(error) => {
            crate::output::write_stderr_line(format_args!("ASR export failed: {error:#}"));
            1
        }
    }
}
fn execute(args: ExportArgs) -> Result<()> {
    let ExportArgs::Asr(args) = args;
    let mut inputs = Vec::new();
    for selected in AsrProvider::value_variants()
        .iter()
        .copied()
        .filter(|p| args.provider.is_none_or(|selected| selected == *p))
    {
        let (provider, capture) = match selected {
            AsrProvider::Claude => (Provider::Claude, CaptureProvider::Claude),
            AsrProvider::Codex => (Provider::Codex, CaptureProvider::Codex),
            AsrProvider::Firebender => (Provider::Firebender, CaptureProvider::Firebender),
            AsrProvider::Forgecode => (Provider::ForgeCode, CaptureProvider::ForgeCode),
            AsrProvider::Deepagents => (Provider::DeepAgents, CaptureProvider::DeepAgents),
            AsrProvider::Devin => (Provider::Devin, CaptureProvider::Devin),
        };
        if let Some(path) = &args.path {
            inputs.push(Input {
                provider,
                path: path.clone(),
            });
            continue;
        }
        let home = crate::identity::home_dir();
        let report =
            ctx_history_cli::discovered_sources_for_provider_report(home.as_deref(), capture);
        for source in report.sources {
            if source.source_format.contains("sqlite")
                || (source.source_format.contains("jsonl")
                    && !source.source_format.contains("history_jsonl"))
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
    let result = ctx_history_asr::export(&options)?;
    crate::output::write_stdout_line(format_args!("{}", serde_json::to_string(&result)?));
    Ok(())
}
