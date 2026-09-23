// Adapted from Sift, MIT; source revision and license are in this crate’s NOTICE.
use super::*;
mod commands;
use commands::{basename, command_label, command_words, shell_words};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::path::{Path, PathBuf};

const MAX_FILES: usize = 128;
const MAX_ENTRIES: usize = 4096;
const MAX_DEPTH: usize = 8;
const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;
const MAX_SCAN_BYTES: usize = 64 * 1024 * 1024;
const MAX_RECORDS: usize = 20_000;
const MAX_OUTPUT_BYTES: usize = 256 * 1024;
const MAX_MEASURE_BYTES: usize = 4 * 1024 * 1024;

pub fn run(args: &[OsString]) -> Result<()> {
    if !args.iter().take_while(|a| *a != "--").any(|a| {
        matches!(
            a.to_str(),
            Some("--history" | "--since" | "--project" | "--suggest")
        )
    }) {
        return replay(args);
    }
    let mut history = None;
    let mut since = None;
    let mut project = None;
    let mut json = false;
    let mut suggest = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--history") => {
                ensure!(history.is_none(), "--history can be specified only once");
                history = Some(PathBuf::from(
                    args.next().context("--history needs a path")?,
                ));
            }
            Some("--since") => {
                let value = args
                    .next()
                    .and_then(|a| a.to_str())
                    .context("--since needs YYYY-MM-DD")?;
                ensure!(
                    valid_date(value),
                    "--since needs a valid YYYY-MM-DD UTC date"
                );
                since = Some(value.to_owned());
            }
            Some("--project") => {
                project = Some(PathBuf::from(
                    args.next().context("--project needs a path")?,
                ));
            }
            Some("--json") => json = true,
            Some("--suggest") => suggest = true,
            Some("--help" | "-h") => return replay(&["--help".into()]),
            _ => bail!("history discovery: unknown option; use --help"),
        }
    }
    let path =
        history.context("--since, --project and --suggest require explicit --history PATH")?;
    let report = inspect(&path, since.as_deref(), project.as_deref(), suggest)?;
    let mut out = io::stdout().lock();
    if json {
        // Keep JSON usable directly in terminals, including filenames containing
        // C1 controls or bidi formatting. Decoding still recovers the exact path.
        let json = serde_json::to_string_pretty(&report)?;
        let mut escaped = String::with_capacity(json.len());
        for ch in json.chars() {
            if ch.is_ascii() {
                escaped.push(ch);
            } else {
                use std::fmt::Write;
                for unit in ch.encode_utf16(&mut [0; 2]) {
                    write!(escaped, "\\u{unit:04x}")?;
                }
            }
        }
        writeln!(out, "{escaped}")?;
    } else {
        writeln!(
            out,
            "Potential compaction of captured output (ordinary o200k_base tokens; not billing or actual usage):"
        )?;
        for row in &report.rows {
            write!(
                out,
                "{} {} {}: {}",
                row.id,
                location_label(&report, &row.source_id, &row.location),
                row.command,
                row.classification
            )?;
            if let Some(saved) = row.potential_saved_tokens {
                write!(out, "; {saved} potentially fewer tokens in captured output")?;
            } else {
                write!(out, "; tokens unmeasured")?;
            }
            writeln!(out)?;
        }
        for suggestion in &report.suggestions {
            writeln!(
                out,
                "Observed correction: {} ({} pairs in {} sources; review only)",
                suggestion.pattern, suggestion.occurrences, suggestion.source_count
            )?;
            for pair in &suggestion.evidence {
                writeln!(
                    out,
                    "  {} -> {}",
                    location_label(&report, &pair.source_id, &pair.failed),
                    location_label(&report, &pair.source_id, &pair.corrected)
                )?;
            }
        }
        writeln!(
            out,
            "{} files; {} records; {} missed opportunities; {} recognized Sift/ctx output calls; {} malformed records; {} skipped files; {} excluded rows; scan limited: {}",
            report.files_scanned,
            report.records_scanned,
            report.missed_opportunities,
            report.sift_calls,
            report.malformed_records,
            report.skipped_files,
            report.excluded_rows,
            report.scan_limited
        )?;
    }
    Ok(())
}

#[derive(Default, Serialize)]
struct HistoryReport {
    schema_version: u8,
    measurement: &'static str,
    files_scanned: usize,
    bytes_scanned: usize,
    records_scanned: usize,
    malformed_records: usize,
    skipped_files: usize,
    excluded_rows: usize,
    scan_limited: bool,
    sift_calls: usize,
    missed_opportunities: usize,
    potential_saved_tokens: usize,
    limits: BTreeMap<&'static str, usize>,
    sources: BTreeMap<String, HistorySource>,
    rows: Vec<HistoryRow>,
    suggestions: Vec<Suggestion>,
}

#[derive(Serialize)]
struct HistorySource {
    // Relative to the selected directory, or the selected file's parent.
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    path_bytes: Option<Vec<u8>>,
}

#[derive(Clone, Default, Serialize)]
struct Location {
    // Physical record start line (1-based), including blank and malformed lines.
    line: usize,
    // Claude message.content index (0-based); Codex has one payload per record.
    block_index: Option<usize>,
}

fn location_label(report: &HistoryReport, source: &str, location: &Location) -> String {
    let source = &report.sources[source];
    let path = if let Some(bytes) = &source.path_bytes {
        format!("b\"{}\"", bytes.escape_ascii())
    } else {
        format!("{:?}", source.path)
    };
    let mut label = format!("{path}:{}", location.line);
    if let Some(block) = location.block_index {
        label.push_str(&format!("[block {block}]"));
    }
    label
}

#[derive(Serialize)]
struct HistoryRow {
    id: String,
    source_id: String,
    location: Location,
    provider: &'static str,
    command: String,
    classification: &'static str,
    output_status: &'static str,
    captured_bytes: Option<usize>,
    input_tokens: Option<usize>,
    output_tokens: Option<usize>,
    potential_saved_tokens: Option<usize>,
}

#[derive(Serialize)]
struct Suggestion {
    pattern: String,
    occurrences: usize,
    source_count: usize,
    source_ids: Vec<String>,
    evidence: Vec<CorrectionEvidence>,
}

#[derive(Serialize)]
struct CorrectionEvidence {
    source_id: String,
    failed: Location,
    corrected: Location,
}

#[derive(Default)]
struct Call {
    location: Location,
    provider: &'static str,
    command: Vec<String>,
    project: Option<String>,
    timestamp: Option<String>,
    output: Option<String>,
    success: Option<bool>,
    has_result: bool,
    started: usize,
    completed: Option<usize>,
}

fn valid_date(date: &str) -> bool {
    let b = date.as_bytes();
    if b.len() != 10
        || b[4] != b'-'
        || b[7] != b'-'
        || !b
            .iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
    {
        return false;
    }
    let year: u32 = date[..4].parse().unwrap();
    let month: u32 = date[5..7].parse().unwrap();
    let day: u32 = date[8..].parse().unwrap();
    let days = match month {
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => {
            29
        }
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => return false,
    };
    year != 0 && day > 0 && day <= days
}

fn selected(call: &Call, since: Option<&str>, project: Option<&Path>) -> bool {
    if let Some(project) = project
        && !call
            .project
            .as_deref()
            .is_some_and(|p| Path::new(p) == project)
    {
        return false;
    }
    if let Some(since) = since {
        // UTC provider timestamps only; unknown dates cannot satisfy a date filter.
        let Some(stamp) = call.timestamp.as_deref() else {
            return false;
        };
        let Some(date) = stamp.get(..10) else {
            return false;
        };
        if !valid_date(date) || !(stamp.ends_with('Z') || stamp.ends_with("+00:00")) || date < since
        {
            return false;
        }
    }
    true
}

fn collect_files(
    path: &Path,
    depth: usize,
    entries: &mut usize,
    files: &mut Vec<PathBuf>,
    report: &mut HistoryReport,
) -> Result<()> {
    if *entries >= MAX_ENTRIES || files.len() >= MAX_FILES || depth > MAX_DEPTH {
        report.scan_limited = true;
        return Ok(());
    }
    *entries += 1;
    let meta = fs::symlink_metadata(path).context("cannot inspect selected history entry")?;
    if meta.is_file() {
        files.push(path.to_owned());
    } else if meta.is_dir() {
        // Bound enumeration as well as file reads, including directories with no useful files.
        let mut children = Vec::new();
        for child in fs::read_dir(path).context("cannot enumerate selected history directory")? {
            if children.len() + *entries >= MAX_ENTRIES {
                report.scan_limited = true;
                break;
            }
            children.push(
                child
                    .context("cannot inspect history directory entry")?
                    .path(),
            );
        }
        children.sort();
        for child in children {
            if *entries >= MAX_ENTRIES || files.len() >= MAX_FILES {
                report.scan_limited = true;
                break;
            }
            collect_files(&child, depth + 1, entries, files, report)?;
        }
    } else {
        report.skipped_files += 1; // No symlinks, sockets, FIFOs or devices.
    }
    Ok(())
}

fn inspect(
    path: &Path,
    since: Option<&str>,
    project: Option<&Path>,
    suggest: bool,
) -> Result<HistoryReport> {
    let mut report = HistoryReport {
        schema_version: 1,
        measurement: "potential_captured_output_o200k_base",
        limits: BTreeMap::from([
            ("files", MAX_FILES),
            ("entries", MAX_ENTRIES),
            ("depth", MAX_DEPTH),
            ("file_bytes", MAX_FILE_BYTES),
            ("scan_bytes", MAX_SCAN_BYTES),
            ("records", MAX_RECORDS),
            ("output_bytes", MAX_OUTPUT_BYTES),
            ("measurement_bytes", MAX_MEASURE_BYTES),
        ]),
        ..Default::default()
    };
    let mut files = Vec::new();
    collect_files(path, 0, &mut 0, &mut files, &mut report)?;
    let source_root = if fs::symlink_metadata(path)?.is_dir() {
        path
    } else {
        path.parent().unwrap_or_else(|| Path::new(""))
    };
    let mut compactor = None;
    let mut measured_bytes = 0;
    let mut patterns: BTreeMap<String, Vec<CorrectionEvidence>> = BTreeMap::new();
    for (index, file) in files.iter().enumerate() {
        if report.bytes_scanned >= MAX_SCAN_BYTES || report.records_scanned >= MAX_RECORDS {
            report.scan_limited = true;
            break;
        }
        let source = format!("source-{}", index + 1);
        let mut bytes = Vec::new();
        let limit = MAX_FILE_BYTES.min(MAX_SCAN_BYTES - report.bytes_scanned);
        // Recheck the entry before opening. The input tree should be a stable saved copy.
        let meta = fs::symlink_metadata(file).context("cannot inspect history file")?;
        if !meta.is_file() || meta.len() > limit as u64 {
            report.skipped_files += 1;
            report.scan_limited |= meta.is_file();
            continue;
        }
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let handle: File = options.open(file).context("cannot open history file")?;
        if !handle.metadata()?.is_file() {
            report.skipped_files += 1;
            continue;
        }
        handle
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .context("cannot read history file")?;
        report.bytes_scanned += bytes.len();
        report.files_scanned += 1;
        if bytes.len() > limit {
            report.scan_limited = true;
            report.skipped_files += 1;
            continue;
        }
        let relative = file
            .strip_prefix(source_root)
            .context("history source outside selected root")?;
        report.sources.insert(
            source.clone(),
            HistorySource {
                path: relative.to_string_lossy().into_owned(),
                path_bytes: relative
                    .to_str()
                    .is_none()
                    .then(|| relative.as_os_str().as_encoded_bytes().to_vec()),
            },
        );
        let calls = parse_file(
            &bytes,
            file.extension().is_some_and(|e| e == "jsonl"),
            &mut report,
        );
        let mut previous: Option<&Call> = None;
        for (n, call) in calls.iter().enumerate() {
            if !selected(call, since, project) {
                report.excluded_rows += 1;
                previous = None;
                continue;
            }
            if suggest
                && let Some(before) = previous
                && let Some(pattern) = correction(before, call)
            {
                patterns
                    .entry(pattern)
                    .or_default()
                    .push(CorrectionEvidence {
                        source_id: source.clone(),
                        failed: before.location.clone(),
                        corrected: call.location.clone(),
                    });
            }
            previous = Some(call);
            let (label, sift) = command_label(&call.command);
            let mut row = HistoryRow {
                id: format!("{source}:{}", n + 1),
                source_id: source.clone(),
                location: call.location.clone(),
                provider: call.provider,
                command: label,
                classification: if sift { "sift_usage" } else { "unmeasured" },
                output_status: if call.has_result {
                    "unsupported"
                } else {
                    "missing"
                },
                captured_bytes: call.output.as_ref().map(String::len),
                input_tokens: None,
                output_tokens: None,
                potential_saved_tokens: None,
            };
            if sift {
                report.sift_calls += 1;
            }
            if let Some(text) = &call.output {
                row.output_status = "captured";
                if sift {
                    // Observed wrapper invocation is not proof of original bytes or savings.
                } else if text.len() > MAX_OUTPUT_BYTES
                    || measured_bytes + text.len() > MAX_MEASURE_BYTES
                {
                    row.output_status = "measurement_limit";
                } else {
                    if compactor.is_none() {
                        compactor = Some(Compactor::new()?);
                    }
                    let result = compactor.as_ref().unwrap().compact(text);
                    measured_bytes += text.len();
                    let saved = result.input_tokens - result.output_tokens;
                    row.input_tokens = Some(result.input_tokens);
                    row.output_tokens = Some(result.output_tokens);
                    row.potential_saved_tokens = Some(saved);
                    row.classification = if saved > 0 {
                        "missed_opportunity"
                    } else {
                        "no_savings"
                    };
                    report.missed_opportunities += usize::from(saved > 0);
                    report.potential_saved_tokens += saved;
                }
            }
            report.rows.push(row);
        }
    }
    report.suggestions = patterns
        .into_iter()
        .map(|(pattern, evidence)| {
            let sources: BTreeSet<_> = evidence.iter().map(|pair| pair.source_id.clone()).collect();
            Suggestion {
                pattern,
                occurrences: evidence.len(),
                source_count: sources.len(),
                source_ids: sources.into_iter().collect(),
                evidence,
            }
        })
        .collect();
    Ok(report)
}

// Public wire schemas: anthropics/anthropic-sdk-python tool_use/tool_result types;
// openai/codex codex-rs/protocol/src/models.rs response items. Parse only command
// tools and their canonical results, never assistant/user prose or mirrored events.
fn parse_file(bytes: &[u8], jsonl: bool, report: &mut HistoryReport) -> Vec<Call> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return vec![Call {
            provider: "saved_output",
            location: Location {
                line: 1,
                block_index: None,
            },
            has_result: true,
            ..Default::default()
        }];
    };
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let whole = serde_json::from_str::<Value>(text).ok();
    let is_history = jsonl
        || whole.as_ref().is_some_and(history_record)
        || text
            .lines()
            .take(16)
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .any(|v| history_record(&v));
    if !is_history {
        return vec![Call {
            provider: "saved_output",
            location: Location {
                line: 1,
                block_index: None,
            },
            output: Some(text.to_owned()),
            has_result: true,
            ..Default::default()
        }];
    }
    let mut calls: Vec<Call> = Vec::new();
    let mut ids: BTreeMap<String, usize> = BTreeMap::new();
    let mut results: BTreeMap<String, (Option<String>, Option<bool>, usize)> = BTreeMap::new();
    let mut project = None;
    let mut sequence = 0;
    let mut process = |v: Value, line: usize| {
        sequence += 1;
        if v["type"] == "session_meta" || v["type"] == "turn_context" {
            project = v["payload"]["cwd"].as_str().map(str::to_owned);
            return;
        }
        let timestamp = v["timestamp"].as_str().map(str::to_owned);
        let cwd = v["cwd"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| project.clone());
        if matches!(v["type"].as_str(), Some("assistant" | "user")) {
            if let Some(blocks) = v["message"]["content"].as_array() {
                for (block_index, block) in blocks.iter().enumerate() {
                    sequence += 1;
                    if block["type"] == "tool_use" && block["name"] == "Bash" {
                        if let (Some(id), Some(command)) =
                            (block["id"].as_str(), block["input"]["command"].as_str())
                        {
                            add_call(
                                &mut calls,
                                &mut ids,
                                format!("claude:{id}"),
                                Call {
                                    provider: "claude",
                                    location: Location {
                                        line,
                                        block_index: Some(block_index),
                                    },
                                    started: sequence,
                                    command: shell_words(command),
                                    project: cwd.clone(),
                                    timestamp: timestamp.clone(),
                                    ..Default::default()
                                },
                            );
                        }
                    } else if block["type"] == "tool_result"
                        && let Some(id) = block["tool_use_id"].as_str()
                    {
                        results.entry(format!("claude:{id}")).or_insert_with(|| {
                            (
                                output_text(&block["content"]),
                                // is_error defaults false, but a malformed flag is unknown.
                                match block.get("is_error") {
                                    None => Some(true),
                                    Some(value) => value.as_bool().map(|error| !error),
                                },
                                sequence,
                            )
                        });
                    }
                }
            }
        } else if v["type"] == "response_item" {
            let p = &v["payload"];
            if p["type"] == "function_call" {
                if !matches!(
                    p["name"].as_str(),
                    Some("exec_command" | "shell" | "shell_command")
                ) {
                    return;
                }
                let Some(id) = p["call_id"].as_str() else {
                    return;
                };
                let Some(args) = p["arguments"]
                    .as_str()
                    .and_then(|s| serde_json::from_str::<Value>(s).ok())
                else {
                    return;
                };
                let command = args
                    .get("cmd")
                    .or_else(|| args.get("command"))
                    .map(command_words)
                    .unwrap_or_default();
                add_call(
                    &mut calls,
                    &mut ids,
                    format!("codex:{id}"),
                    Call {
                        provider: "codex",
                        location: Location {
                            line,
                            block_index: None,
                        },
                        started: sequence,
                        command,
                        project: args["workdir"]
                            .as_str()
                            .or_else(|| args["cwd"].as_str())
                            .map(str::to_owned)
                            .or(cwd),
                        timestamp,
                        ..Default::default()
                    },
                );
            } else if p["type"] == "local_shell_call" {
                if p["action"]["type"] != "exec" {
                    return;
                }
                if let Some(id) = p["call_id"].as_str().or_else(|| p["id"].as_str()) {
                    add_call(
                        &mut calls,
                        &mut ids,
                        format!("codex:{id}"),
                        Call {
                            provider: "codex",
                            location: Location {
                                line,
                                block_index: None,
                            },
                            started: sequence,
                            command: command_words(&p["action"]["command"]),
                            project: p["action"]["working_directory"]
                                .as_str()
                                .map(str::to_owned)
                                .or(cwd),
                            timestamp,
                            ..Default::default()
                        },
                    );
                }
            } else if p["type"] == "function_call_output"
                && let Some(id) = p["call_id"].as_str()
            {
                results.entry(format!("codex:{id}")).or_insert_with(|| {
                    let (output, success) = codex_output(&p["output"]);
                    (output, success, sequence)
                });
            }
        }
    };
    if let Some(v) = whole {
        report.records_scanned += 1;
        let leading = &text[..text.len() - text.trim_start().len()];
        process(v, leading.bytes().filter(|b| *b == b'\n').count() + 1);
    } else {
        for (index, line) in text
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty())
        {
            if report.records_scanned >= MAX_RECORDS {
                report.scan_limited = true;
                break;
            }
            report.records_scanned += 1;
            match serde_json::from_str(line) {
                Ok(v) => process(v, index + 1),
                Err(_) => report.malformed_records += 1,
            }
        }
    }
    for (id, index) in ids {
        if let Some((output, success, completed)) = results.remove(&id) {
            calls[index].completed = Some(completed);
            calls[index].output = output;
            calls[index].success = success;
            calls[index].has_result = true;
        }
    }
    calls
}

fn history_record(v: &Value) -> bool {
    matches!(
        v["type"].as_str(),
        Some(
            "session_meta" | "turn_context" | "response_item" | "event_msg" | "assistant" | "user"
        )
    )
}

fn add_call(calls: &mut Vec<Call>, ids: &mut BTreeMap<String, usize>, id: String, call: Call) {
    if let std::collections::btree_map::Entry::Vacant(entry) = ids.entry(id) {
        entry.insert(calls.len());
        calls.push(call);
    }
}

fn output_text(value: &Value) -> Option<String> {
    if let Some(s) = value.as_str() {
        return Some(s.to_owned());
    }
    // Mixed image/text output is not silently treated as complete text output.
    let blocks = value.as_array()?;
    let parts: Option<Vec<&str>> = blocks
        .iter()
        .map(|v| {
            if matches!(
                v["type"].as_str(),
                Some("text" | "input_text" | "output_text")
            ) {
                v["text"].as_str()
            } else {
                None
            }
        })
        .collect();
    parts.map(|parts| parts.join("\n"))
}

fn codex_output(value: &Value) -> (Option<String>, Option<bool>) {
    let Some(text) = output_text(value) else {
        return (None, None);
    };
    // The shell tool serializes command output with explicit exit metadata.
    if let Ok(v) = serde_json::from_str::<Value>(&text)
        && let (Some(output), Some(code)) =
            (v["output"].as_str(), v["metadata"]["exit_code"].as_i64())
    {
        return (Some(output.to_owned()), Some(code == 0));
    }
    // Unified exec's human-readable envelope: inspect only the header, never
    // search arbitrary command output for strings that resemble exit metadata.
    if (text.starts_with("Chunk ID:") || text.starts_with("Wall time:"))
        && let Some((header, output)) = text
            .split_once("\nFinal output:\n")
            .or_else(|| text.split_once("\nOutput:\n"))
    {
        let success = header
            .lines()
            .find_map(|line| {
                line.strip_prefix("Process exited with code ")
                    .and_then(|n| n.parse::<i32>().ok())
            })
            .map(|n| n == 0);
        return (Some(output.to_owned()), success);
    }
    (Some(text), None)
}

fn correction(before: &Call, after: &Call) -> Option<String> {
    if !before.completed.is_some_and(|n| n < after.started)
        || before.success != Some(false)
        || after.success != Some(true)
        || before.project != after.project
        || before.provider != after.provider
        || before.command.len() < 2
        || after.command.len() < 2
        || before.command[0] != after.command[0]
        || before.command == after.command
    {
        return None;
    }
    // Only report known typo corrections; arbitrary argument edits may be a
    // different task, and reflecting them would disclose private command data.
    let program = basename(&after.command[0]);
    let old = before.command[1].as_str();
    let new = after.command[1].as_str();
    let known = matches!(
        (program, old, new),
        ("git", "statsu" | "stauts", "status")
            | ("git", "chekout" | "checkot", "checkout")
            | ("cargo", "biuld" | "buidl", "build")
            | ("cargo", "tset" | "tets", "test")
            | ("npm" | "pnpm" | "yarn", "isntall" | "instal", "install")
    );
    if known && before.command[2..] == after.command[2..] {
        return Some(format!("{program} {old} -> {program} {new}"));
    }
    // Common CLI option spelling correction, with identical other arguments.
    if before.command.len() == after.command.len() {
        let changes: Vec<_> = before
            .command
            .iter()
            .zip(&after.command)
            .filter(|(a, b)| a != b)
            .collect();
        if changes.len() == 1 {
            let (a, b) = changes[0];
            if matches!(
                (a.as_str(), b.as_str()),
                ("--quite", "--quiet") | ("--verbsoe", "--verbose") | ("--hlep", "--help")
            ) {
                let (label, sift) = command_label(&after.command);
                if !sift && label != "other" {
                    return Some(format!("{label}: {a} -> {b}"));
                }
            }
        }
    }
    None
}
