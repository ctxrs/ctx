// Adapted from Sift, MIT; source revision and license are in this crate’s NOTICE.
//! Replay saved output or inspect explicitly selected history. Never execute its contents.
use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use sift::Compactor;
use std::ffi::OsString;
use std::io::{self, Read, Write};

#[derive(Serialize)]
struct Opportunity {
    file: String,
    bytes: usize,
    input_tokens: Option<usize>,
    output_tokens: Option<usize>,
    saved_tokens: Option<usize>,
    encoding: Option<sift::Encoding>,
}

fn replay(args: &[OsString]) -> Result<()> {
    let mut files = Vec::new();
    let mut json = false;
    let mut positional = false;
    for arg in args {
        if !positional && arg == "--" {
            positional = true;
        } else if !positional && arg == "--json" {
            json = true;
        } else if !positional && (arg == "--help" || arg == "-h") {
            writeln!(
                io::stdout().lock(),
                "Usage: ctx output discover [--json] [--] [FILE ...]\nReplay saved output files (stdin when omitted). Never executes their contents.\nReports potential ordinary o200k_base savings, not actual agent usage.\nHistory: ctx output discover --history PATH [--json] [--since YYYY-MM-DD] [--project PATH] [--suggest]\nOnly the selected file/directory is inspected; bounded, read-only, no symlink traversal.\nHistory reports include relative file/record locators, but omit arguments and transcripts. --suggest only reports observed patterns."
            )?;
            return Ok(());
        } else if !positional && arg.to_string_lossy().starts_with('-') && arg != "-" {
            bail!("discover: unknown option; use --help");
        } else {
            files.push(arg.clone());
        }
    }
    if files.is_empty() {
        files.push("-".into());
    }
    ensure!(
        files.iter().filter(|name| *name == "-").count() <= 1,
        "stdin can be read only once"
    );
    let mut compactor = None;
    let mut rows = Vec::new();
    for file in files {
        let bytes = if file == "-" {
            let mut bytes = Vec::new();
            io::stdin().read_to_end(&mut bytes)?;
            bytes
        } else {
            std::fs::read(&file)
                .with_context(|| format!("cannot read {}", file.to_string_lossy()))?
        };
        let result = if let Ok(text) = std::str::from_utf8(&bytes) {
            if compactor.is_none() {
                compactor = Some(Compactor::new()?);
            }
            Some(compactor.as_ref().unwrap().compact(text))
        } else {
            None
        };
        rows.push(Opportunity {
            file: file.to_string_lossy().into_owned(),
            bytes: bytes.len(),
            input_tokens: result.as_ref().map(|r| r.input_tokens),
            output_tokens: result.as_ref().map(|r| r.output_tokens),
            saved_tokens: result.as_ref().map(|r| r.input_tokens - r.output_tokens),
            encoding: result.map(|r| r.encoding),
        });
    }
    let mut output = io::stdout().lock();
    if json {
        serde_json::to_writer_pretty(&mut output, &rows)?;
        writeln!(output)?;
    } else {
        writeln!(
            output,
            "Potential savings from saved output (ordinary o200k_base tokens):"
        )?;
        for row in rows {
            if let (Some(input), Some(result), Some(saved)) =
                (row.input_tokens, row.output_tokens, row.saved_tokens)
            {
                writeln!(output, "{}: {input} -> {result}; {saved} fewer", row.file)?;
            } else {
                writeln!(
                    output,
                    "{}: non-UTF-8; unchanged, tokens unmeasured",
                    row.file
                )?;
            }
        }
    }
    Ok(())
}

mod history;

pub(crate) fn run(args: &[OsString]) -> Result<()> {
    history::run(args)
}
