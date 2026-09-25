// Adapted from Sift, MIT; source revision and license are in this crate’s NOTICE.
use crate::{
    discover, execute::execute, filter, hooks, pre_hooks, protocol::protocol, rewrite, state,
    usage, views,
};
use anyhow::{Context, Result, bail, ensure};
use sift::{Compactor, Encoding};
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};

const HELP: &str = "ctx sift — token-counted command output and recovery

Usage:
  ctx sift run [--raw|--capture] -- COMMAND [ARG...]
  ctx sift compact [FILE|-]
  ctx sift compact --protocol=json-v1|session-v1|session-v2
  ctx sift restore --encoding ENCODING [FILE|-]
  ctx sift recall --list | ID [--stderr] [--from N] [--lines N] [--grep TEXT]
  ctx sift proxy [--] COMMAND [ARG...]
  ctx sift filter [--capture]
  ctx sift read [FILE|-] [--from N] [--lines N] [--grep TEXT]
  ctx sift json [FILE|-] [--pointer POINTER] [--field KEY] [--limit N]
  ctx sift summary|err|test [OPTIONS] -- COMMAND [ARG...]
  ctx sift gain [--json|--csv] [--history] [--daily] [--graph]
  ctx sift config [show|--create]
  ctx sift semantic enable|shadow|disable|status [--project PATH]
  ctx sift discover [--json] [FILE ...]
  ctx sift discover --history PATH [--suggest] [--json]
  ctx sift ccusage --import FILE [--json|--csv]
  ctx sift hook HOST
  ctx sift rewrite [--json] [--shell posix|powershell] -- 'COMMAND'

Encodings: raw, json-v1, json-rows-v1, json-min-v1, json-columns-v1,
           text-runs-v1, text-prefixes-v1, text-refs-v1, text-lines-v1,
           text-symbols-v1

Read stdin when FILE is omitted or '-'; use '--' before a filename starting '-'.
Compact adds no newline. Restore requires an explicit encoding; raw accepts binary.
Run preserves native argv, stdin, separate streams, and numeric child exit status.
Interactive and long-running output passes through; --raw always passes through.
--capture delays progress until completion or the combined 8 MiB streaming limit.
Git/Cargo/Tape presentations can omit formatting or passing rows; use --raw for native bytes.
Recall requires keep_originals and record_usage in output config; streaming runs aren't retained.
CTX_OUTPUT_CONFIG_DIR / CTX_OUTPUT_STATE_DIR select output stores independently.
Nonempty SIFT_CONFIG_DIR / SIFT_STATE_DIR are fallback overrides; no automatic data migration.
Defaults: XDG config/state ctx/output; APPDATA/LOCALAPPDATA ctx/output on Windows.
Output state is separate from history --data-root. Install automatic hooks explicitly with ctx integrations install sift.
Pi sessions require --record-source pi with --record-tool bash (v1) or pi (v2).
Semantic selection is off by default; explicit enable/shadow uses TypeSafe and TYPESAFE_API_KEY.
Only enabled projects and eligible session-v2 selections can use the remote service.
";

fn parse_encoding(value: &str) -> Result<Encoding> {
    match value {
        "raw" => Ok(Encoding::Raw),
        "json-v1" => Ok(Encoding::JsonV1),
        "json-rows-v1" => Ok(Encoding::JsonRowsV1),
        "json-min-v1" => Ok(Encoding::JsonMinV1),
        "json-columns-v1" => Ok(Encoding::JsonColumnsV1),
        "text-runs-v1" => Ok(Encoding::TextRunsV1),
        "text-prefixes-v1" => Ok(Encoding::TextPrefixesV1),
        "text-refs-v1" => Ok(Encoding::TextRefsV1),
        "text-lines-v1" => Ok(Encoding::TextLinesV1),
        "text-symbols-v1" => Ok(Encoding::TextSymbolsV1),
        _ => bail!("unsupported encoding; use 'ctx sift --help' for supported encodings"),
    }
}

pub(crate) fn run(args: impl IntoIterator<Item = OsString>) -> Result<i32> {
    let mut args = args.into_iter().collect::<Vec<_>>().into_iter();
    let Some(command) = args.next() else {
        io::stdout().write_all(HELP.as_bytes())?;
        return Ok(0);
    };
    if command == "--help" || command == "-h" {
        io::stdout().write_all(HELP.as_bytes())?;
        return Ok(0);
    }
    if command == "--version" {
        writeln!(
            io::stdout(),
            "ctx sift {}",
            option_env!("CARGO_PKG_VERSION").unwrap_or("development")
        )?;
        return Ok(0);
    }
    if (command == "hook" || command == "rewrite" || command == "filter")
        && args.as_slice().len() == 1
        && matches!(args.as_slice()[0].to_str(), Some("--help" | "-h"))
    {
        io::stdout().write_all(HELP.as_bytes())?;
        return Ok(0);
    }
    if command == "hook" {
        let host = args
            .next()
            .and_then(|s| s.into_string().ok())
            .context("hook requires a supported agent name")?;
        ensure!(
            [
                "claude", "copilot", "hermes", "codex", "cursor", "gemini", "vscode", "droid",
                "vibe"
            ]
            .contains(&host.as_str())
                && args.next().is_none(),
            "hook requires a supported agent name"
        );
        if !matches!(host.as_str(), "claude" | "copilot" | "hermes") {
            return pre_hooks::run(&host);
        }
        hooks::run(&host)?;
        return Ok(0);
    }
    if command == "rewrite" {
        return rewrite::run(&args.collect::<Vec<_>>());
    }
    if command == "filter" {
        let remaining: Vec<_> = args.collect();
        ensure!(
            remaining.is_empty() || remaining == [OsString::from("--capture")],
            "filter accepts only --capture"
        );
        filter::run(!remaining.is_empty())?;
        return Ok(0);
    }
    if command == "gain" || command == "config" || command == "recall" || command == "semantic" {
        let remaining: Vec<_> = args.collect();
        match command.to_str().unwrap() {
            "gain" => state::gain(&remaining)?,
            "config" => state::config(&remaining)?,
            "recall" => state::recall(&remaining)?,
            _ => state::semantic(&remaining)?,
        }
        return Ok(0);
    }
    if command == "discover" {
        discover::run(&args.collect::<Vec<_>>())?;
        return Ok(0);
    }
    if command == "ccusage" {
        usage::run(&args.collect::<Vec<_>>())?;
        return Ok(0);
    }
    if matches!(
        command.to_str(),
        Some("read" | "json" | "summary" | "err" | "test")
    ) {
        match views::parse(command.to_str().unwrap(), &args.collect::<Vec<_>>())? {
            views::Action::Help => {
                io::stdout().write_all(views::HELP.as_bytes())?;
                return Ok(0);
            }
            views::Action::Input { path, view }
                if command == "read" && !view.is_read_selection() =>
            {
                args = path
                    .map(|path| vec![OsString::from("--"), path])
                    .unwrap_or_default()
                    .into_iter();
            }
            views::Action::Input { path, view } => {
                views::run_input(path.as_deref(), &view)?;
                return Ok(0);
            }
            views::Action::Command { argv, view } => {
                return execute(&argv, false, true, Some(&view));
            }
        }
    }
    let command = if command == "pipe" || command == "read" {
        OsString::from("compact")
    } else {
        command
    };
    if command != "compact" && command != "restore" {
        if command == "run" || command == "proxy" {
            let mut remaining: Vec<_> = args.collect();
            let mut raw = command == "proxy";
            let mut capture = false;
            while remaining
                .first()
                .is_some_and(|arg| arg == "--raw" || arg == "--capture")
            {
                let flag = remaining.remove(0);
                raw |= flag == "--raw";
                capture |= flag == "--capture";
            }
            if remaining
                .first()
                .is_some_and(|arg| arg == "--help" || arg == "-h")
            {
                io::stdout().write_all(HELP.as_bytes())?;
                return Ok(0);
            }
            if remaining.first().is_some_and(|arg| arg == "--") {
                remaining.remove(0);
            }
            return execute(&remaining, raw, capture, None);
        }
        bail!(
            "unknown sift command {}; use 'ctx sift --help'",
            command.to_string_lossy()
        );
    }

    let mut file: Option<OsString> = None;
    let mut encoding = None;
    let mut jsonl = false;
    let mut session = 0;
    let mut record_source = None;
    let mut record_tool = None;
    let mut positional = false;
    while let Some(arg) = args.next() {
        let value = arg.to_str();
        if !positional && value == Some("--") {
            positional = true;
        } else if !positional && matches!(value, Some("--help" | "-h")) {
            io::stdout().write_all(HELP.as_bytes())?;
            return Ok(0);
        } else if !positional
            && value.is_some_and(|v| v == "--record-source" || v.starts_with("--record-source="))
        {
            ensure!(
                command == "compact" && record_source.is_none(),
                "--record-source is allowed once for compact only"
            );
            let source = option_value(value.unwrap(), &mut args)?;
            ensure!(
                ["pi", "omp", "opencode", "kilo"].contains(&source.as_str()),
                "unsupported integration source"
            );
            record_source = Some(source);
        } else if !positional
            && value.is_some_and(|v| v == "--record-tool" || v.starts_with("--record-tool="))
        {
            ensure!(
                command == "compact" && record_tool.is_none(),
                "--record-tool is allowed once for compact only"
            );
            let tool = option_value(value.unwrap(), &mut args)?;
            ensure!(
                ["bash", "powershell", "exec", "pi"].contains(&tool.as_str()),
                "unsupported integration tool"
            );
            record_tool = Some(tool);
        } else if !positional
            && value.is_some_and(|v| v == "--protocol" || v.starts_with("--protocol="))
        {
            ensure!(
                command == "compact" && !jsonl,
                "--protocol is allowed once for compact only"
            );
            let option = option_value(value.unwrap(), &mut args)?;
            ensure!(
                option == "json-v1" || option == "session-v1" || option == "session-v2",
                "unsupported protocol; expected json-v1, session-v1, or session-v2"
            );
            session = match option.as_str() {
                "session-v1" => 1,
                "session-v2" => 2,
                _ => 0,
            };
            jsonl = true;
        } else if !positional
            && value.is_some_and(|v| v == "--encoding" || v.starts_with("--encoding="))
        {
            ensure!(
                command == "restore" && encoding.is_none(),
                "--encoding is allowed once for restore only"
            );
            encoding = Some(parse_encoding(&option_value(value.unwrap(), &mut args)?)?);
        } else {
            ensure!(
                positional || !value.is_some_and(|v| v.starts_with('-') && v != "-"),
                "unknown option; use 'ctx sift --help'"
            );
            ensure!(file.is_none(), "expected at most one input file");
            file = Some(arg);
        }
    }
    let stdout = io::stdout();
    let mut output = BufWriter::new(stdout.lock());
    ensure!(
        record_tool.is_none() || record_source.is_some(),
        "--record-tool requires --record-source"
    );
    if jsonl {
        ensure!(file.is_none(), "JSONL protocol reads stdin; omit FILE");
        ensure!(
            session == 0
                || (record_source.as_deref() == Some("pi")
                    && ((session == 1 && record_tool.as_deref() == Some("bash"))
                        || (session == 2 && record_tool.as_deref() == Some("pi")))),
            "session-v1 requires Pi Bash and session-v2 requires Pi recording context"
        );
        return protocol(
            io::stdin().lock(),
            output,
            record_source.as_deref(),
            record_tool.as_deref(),
            session,
        )
        .map(i32::from);
    }
    ensure!(
        record_source.is_none(),
        "--record-source requires --protocol=json-v1"
    );
    if command == "restore" {
        ensure!(encoding.is_some(), "restore requires --encoding");
    }
    let mut input: Box<dyn Read> = match file {
        Some(path) if path != "-" => Box::new(BufReader::new(
            File::open(path).context("cannot open input file")?,
        )),
        _ => Box::new(io::stdin().lock()),
    };
    if encoding == Some(Encoding::Raw) {
        io::copy(&mut input, &mut output)?;
    } else {
        let mut bytes = Vec::new();
        input.read_to_end(&mut bytes).context("cannot read input")?;
        match (encoding, std::str::from_utf8(&bytes)) {
            (None, Err(_)) => output.write_all(&bytes)?,
            (None, Ok(text)) => {
                output.write_all(Compactor::new()?.compact(text).text.as_bytes())?
            }
            (Some(encoding), Ok(text)) => {
                output.write_all(sift::restore(encoding, text)?.as_bytes())?
            }
            (Some(_), Err(_)) => {
                bail!("encoded input must be UTF-8; raw mode accepts arbitrary bytes")
            }
        }
    }
    output.flush()?;
    Ok(0)
}

fn option_value(arg: &str, args: &mut impl Iterator<Item = OsString>) -> Result<String> {
    if let Some((_, value)) = arg.split_once('=') {
        return Ok(value.to_owned());
    }
    args.next()
        .context("missing option value")?
        .into_string()
        .map_err(|_| anyhow::anyhow!("option value must be UTF-8"))
}
