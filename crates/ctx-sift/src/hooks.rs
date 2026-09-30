// Adapted from Sift, MIT; source revision and license are in this crate’s NOTICE.
//! Completion-only host adapters. These never execute or rewrite a tool call.
//! CLI settings exclusions match exact host tool labels (Bash/PowerShell for
//! Claude, bash/powershell for Copilot), not programs inside shell expressions.
//! Usage events count measured text fields, not tool calls. Fields below 256
//! bytes and unsupported results are omitted, not estimated. Originals are
//! stored only with the state's explicit keep_originals opt-in.

use crate::rewrite::{self, Shell};
use crate::state::{self, Settings};
use crate::{
    observation::*,
    observe::{self, Observed},
};
use anyhow::Result;
use serde::de::{self, MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::value::RawValue;
use sift::Compactor;
use std::collections::HashSet;
use std::ffi::OsString;
use std::fmt;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Instant;

const MAX_INPUT: usize = 16 * 1024 * 1024;
const MAX_TEXT: usize = 8 * 1024 * 1024;

/// The existing completion-hook POSIX subset; unknown syntax stays lossless.
pub(crate) fn literal_argv(command: &str) -> Option<Vec<OsString>> {
    if command.contains(['\\', '\r', '\0']) {
        return None;
    }
    rewrite::lex(command, Shell::Posix)?
        .into_iter()
        .map(|token| match (token.word, token.operator) {
            (Some(word), None) => Some(OsString::from(word)),
            _ => None,
        })
        .collect()
}

// Only literal simple PowerShell invocations establish wrapper ownership.
// Escapes/expansions and compound commands remain on the existing path.
fn powershell_argv(command: &str) -> Option<Vec<OsString>> {
    if command.contains(['`', '\r', '\0']) {
        return None;
    }
    let command = command.trim_start();
    let command = command.strip_prefix("& ").unwrap_or(command);
    rewrite::lex(command, Shell::PowerShell)?
        .into_iter()
        .map(|token| match (token.word, token.operator) {
            (Some(word), None) => Some(OsString::from(word)),
            _ => None,
        })
        .collect()
}

/// Match wrapper raw flags, never a child's --raw or a history/graph command.
pub(crate) fn explicit_raw(argv: &[OsString]) -> bool {
    wrapper_mode(argv) == Some(true)
}

/// A known output wrapper already owns lossless/raw selection and accounting.
/// None means this is not a literal, complete wrapper invocation.
fn wrapper_mode(argv: &[OsString]) -> Option<bool> {
    let argv = if argv.first().is_some_and(|word| word == "command") {
        &argv[1..]
    } else {
        argv
    };
    let program = argv.first()?.to_str()?;
    let current_executable = std::env::current_exe().ok();
    let is_current = current_executable.as_deref() == Some(Path::new(program));
    let program = program.rsplit(['/', '\\']).next().unwrap_or(program);
    let (command, ctx_sift) = match program {
        "sift" | "sift.exe" => (&argv[1..], false),
        "ctx" | "ctx.exe" => ctx_sift_command(&argv[1..])?,
        _ if is_current => ctx_sift_command(&argv[1..])?,
        _ => return None,
    };
    let first = command.first()?.to_str()?;
    if first == "--" {
        return (ctx_sift && command.len() > 1).then_some(false);
    }
    let (mut raw, mut remaining) = match first {
        "run" | "proxy" => (first == "proxy", &command[1..]),
        "--raw" | "--capture" if ctx_sift => (false, command),
        "hook" | "filter" | "read" | "json" | "summary" | "err" | "test" | "gain" | "config"
        | "semantic" | "discover" | "ccusage" | "rewrite" | "compact" | "restore" | "recall"
        | "--help" | "-h" | "--version" => return None,
        flag if flag.starts_with('-') => return None,
        _ if ctx_sift => return Some(false),
        _ => return None,
    };
    while remaining
        .first()
        .is_some_and(|word| word == "--raw" || word == "--capture")
    {
        raw |= remaining[0] == "--raw";
        remaining = &remaining[1..];
    }
    if remaining
        .first()
        .is_some_and(|word| word == "--help" || word == "-h")
    {
        return None;
    }
    if remaining.first().is_some_and(|word| word == "--") {
        remaining = &remaining[1..];
    } else if remaining
        .first()
        .is_some_and(|word| word.as_encoded_bytes().starts_with(b"-"))
    {
        return None;
    }
    (!remaining.is_empty()).then_some(raw)
}

/// Consume only known root options, stopping before any output/child arguments.
fn ctx_sift_command(mut args: &[OsString]) -> Option<(&[OsString], bool)> {
    loop {
        let word = args.first()?.to_str()?;
        let consumed = match word {
            "run" => return Some((args, false)),
            "sift" => return Some((&args[1..], true)),
            "output" => return Some((&args[1..], false)),
            "--quiet" => 1,
            "--color" if matches!(args.get(1)?.to_str()?, "auto" | "always" | "never") => 2,
            "--color=auto" | "--color=always" | "--color=never" => 1,
            "--data-root" => {
                let value = args.get(1)?;
                if value.is_empty() || value.as_encoded_bytes().starts_with(b"-") {
                    return None;
                }
                2
            }
            _ if word.starts_with("--data-root=") && word.len() > "--data-root=".len() => 1,
            _ => return None,
        };
        args = &args[consumed..];
    }
}

// Keep values opaque: Value's arbitrary_precision number marker is also a
// legal user object key. Re-encoding through Value can change such objects.
pub(crate) struct Object(Vec<(String, Box<RawValue>)>);

impl<'de> Deserialize<'de> for Object {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor;
        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = Object;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an object with unique keys")
            }

            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Object, M::Error> {
                let mut fields = Vec::new();
                let mut keys = HashSet::new();
                while let Some((key, value)) = map.next_entry::<String, Box<RawValue>>()? {
                    if !keys.insert(key.clone()) {
                        return Err(de::Error::custom("duplicate hook field"));
                    }
                    fields.push((key, value));
                }
                Ok(Object(fields))
            }
        }
        deserializer.deserialize_map(ObjectVisitor)
    }
}

impl Serialize for Object {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in &self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

impl Object {
    pub(crate) fn get(&self, key: &str) -> Option<&RawValue> {
        self.0
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| &**value)
    }

    pub(crate) fn string(&self, key: &str) -> Option<String> {
        serde_json::from_str(self.get(key)?.get()).ok()
    }

    pub(crate) fn object(&self, key: &str) -> Option<Object> {
        serde_json::from_str(self.get(key)?.get()).ok()
    }

    pub(crate) fn set_text(&mut self, key: &str, text: &str) -> Result<()> {
        if let Some((_, value)) = self.0.iter_mut().find(|(name, _)| name == key) {
            *value = RawValue::from_string(serde_json::to_string(text)?)?;
        }
        Ok(())
    }
}

/// Return a changed host envelope only when a complete selected text wins.
/// Unknown, malformed, oversized, or unprocessable input is a no-op, including
/// tokenizer initialization failure. Literal Claude Bash argv can select the
/// existing command presentation; no shell expression is evaluated or rewritten.
// The CLI uses the observer-enabled path; retain this pure API for callers/tests.
#[allow(dead_code)]
pub fn transform(host: &str, input: &str) -> Result<Option<String>> {
    Ok(transform_inner(
        host,
        input,
        &[],
        &mut Observed::new(),
        &mut |_, _, _, _, _, _, _| {},
    )
    .unwrap_or(None))
}

fn transform_inner(
    host: &str,
    input: &str,
    exclusions: &[String],
    observed: &mut Observed,
    measured: &mut impl FnMut(&str, &str, &str, &str, usize, usize, u64),
) -> Result<Option<String>> {
    // Codex's current post hook omits native status/metadata and replacement
    // discards that context. Even text resembling legacy framing can be raw
    // command output. No payload-content heuristic can safely identify it.
    if input.len() > MAX_INPUT {
        observed.skip(SkipReason::EnvelopeLimit);
        return Ok(None);
    }
    if !matches!(host, "claude" | "copilot" | "hermes") {
        observed.skip(SkipReason::UnsupportedHost);
        return Ok(None);
    }
    observed.phase = Phase::Protocol;
    let root: Object = serde_json::from_str(input.trim_start_matches('\u{feff}'))?;
    let tool = root.string(if host == "copilot" {
        "toolName"
    } else {
        "tool_name"
    });
    let Some(tool) = tool else {
        observed.skip(SkipReason::UnsupportedTool);
        return Ok(None);
    };
    if exclusions.contains(&tool) {
        observed.skip(SkipReason::Excluded);
        return Ok(None);
    }
    let (mut response, fields) = match host {
        "claude" => {
            if root.string("hook_event_name").as_deref() != Some("PostToolUse") {
                observed.skip(SkipReason::UnsupportedEvent);
                return Ok(None);
            }
            if !matches!(tool.as_str(), "Bash" | "PowerShell") {
                observed.skip(SkipReason::UnsupportedTool);
                return Ok(None);
            }
            let Some(response) = root.object("tool_response") else {
                observed.skip(SkipReason::UnsupportedMetadata);
                return Ok(None);
            };
            for flag in ["isImage", "interrupted"] {
                if let Some(value) = response.get(flag)
                    && serde_json::from_str::<bool>(value.get()).ok() != Some(false)
                {
                    observed.skip(SkipReason::UnsupportedMetadata);
                    return Ok(None);
                }
            }
            (response, vec!["stdout", "stderr"])
        }
        "copilot" => {
            // Native CLI postToolUse input has no event marker. Its registration
            // must be post-only. Reject conflicting markers when supplied.
            for field in ["hookEventName", "hook_event_name", "event"] {
                if root.get(field).is_some() && root.string(field).as_deref() != Some("postToolUse")
                {
                    observed.skip(SkipReason::UnsupportedEvent);
                    return Ok(None);
                }
            }
            if !matches!(
                root.string("toolName").as_deref(),
                Some("bash" | "powershell")
            ) {
                observed.skip(SkipReason::UnsupportedTool);
                return Ok(None);
            }
            let Some(response) = root.object("toolResult") else {
                observed.skip(SkipReason::UnsupportedMetadata);
                return Ok(None);
            };
            if !matches!(
                response.string("resultType").as_deref(),
                Some("success" | "failure")
            ) {
                observed.skip(SkipReason::UnsupportedMetadata);
                return Ok(None);
            }
            (response, vec!["textResultForLlm"])
        }
        "hermes" => {
            if root.string("hook_event_name").as_deref() != Some("TransformToolResult") {
                observed.skip(SkipReason::UnsupportedEvent);
                return Ok(None);
            }
            if tool != "terminal" {
                observed.skip(SkipReason::UnsupportedTool);
                return Ok(None);
            }
            let Some(response) = root.object("tool_response") else {
                observed.skip(SkipReason::UnsupportedMetadata);
                return Ok(None);
            };
            (response, vec!["output"])
        }
        _ => return Ok(None),
    };

    let mut texts = Vec::new();
    let mut total = 0usize;
    for field in fields {
        if response.get(field).is_none() {
            continue;
        }
        let Some(text) = response.string(field) else {
            observed.skip(SkipReason::UnsupportedMetadata);
            return Ok(None);
        };
        total += text.len();
        texts.push((field, text));
    }
    // Bound the combined selected text, not each stream separately. Tiny
    // results do not justify tokenizer startup; leaving them raw is safe.
    if !(256..=MAX_TEXT).contains(&total) {
        observed.skip(if total < 256 {
            SkipReason::Small
        } else {
            SkipReason::CaptureLimit
        });
        return Ok(None);
    }
    // Establish literal argv once for wrapper handling. Semantic presentation
    // remains POSIX-only; unknown terminal backends stay opaque.
    let argv = if matches!(
        (host, tool.as_str()),
        ("claude", "Bash" | "PowerShell") | ("copilot", "bash" | "powershell")
    ) || (host == "hermes"
        && cfg!(unix)
        && std::env::var("TERMINAL_ENV").as_deref() == Ok("local"))
    {
        let arguments = if host == "copilot" {
            // Native Copilot toolArgs is a JSON object encoded as a string.
            root.string("toolArgs")
                .and_then(|json| serde_json::from_str::<Object>(&json).ok())
        } else {
            root.object("tool_input")
        };
        arguments
            .and_then(|input| input.string("command"))
            .and_then(|command| {
                if matches!(tool.as_str(), "PowerShell" | "powershell") {
                    powershell_argv(&command)
                } else {
                    literal_argv(&command)
                }
            })
    } else {
        None
    };
    if argv.as_deref().and_then(wrapper_mode).is_some() {
        observed.skip(SkipReason::AlreadyWrapped);
        // A manual wrapper owns this output: never compact it twice, undo raw
        // selection, or account for the same output a second time.
        return Ok(None);
    }
    // Claude's success-only PostToolUse normally omits exitCode. An explicit
    // status must agree with success; unsupported metadata stays lossless.
    let semantic_argv = argv.as_deref().filter(|_| {
        host == "claude"
            && tool == "Bash"
            && ["interrupted", "isImage"].iter().all(|flag| {
                response.get(flag).is_some_and(|value| {
                    serde_json::from_str::<bool>(value.get()).ok() == Some(false)
                })
            })
            && response
                .get("exitCode")
                .is_none_or(|value| serde_json::from_str::<i32>(value.get()).ok() == Some(0))
    });
    observed.phase = Phase::Codec;
    let compactor = Compactor::new()?;
    observed.phase = Phase::Render;
    let transformation = Instant::now();
    let mut changed = false;
    for (field, text) in texts {
        let index = usize::from(field == "stderr");
        observed.facts.streams[index] = Some(StreamFacts {
            input_bytes: Some(text.len() as u64),
            emitted_bytes: Some(text.len() as u64),
            input_complete: true,
            missing: (!text.is_empty()).then_some(Missingness::Small),
            skip: Some(SkipReason::Small),
            tokens: text.is_empty().then_some(TokenCounts {
                input: 0,
                emitted: 0,
            }),
            ..Default::default()
        });
        if text.len() < 256 {
            continue;
        }
        let start = Instant::now();
        let original = compactor.compact(&text);
        let mut presentation = if original.encoding == sift::Encoding::Raw {
            Presentation::Raw
        } else {
            Presentation::Codec
        };
        let mut encoding = original.encoding;
        let mut emitted = original.text;
        let mut output_tokens = original.output_tokens;
        if field == "stdout"
            && let Some(argv) = semantic_argv
        {
            // Claude can trim the last LF from complete native output. Supply
            // it only to the existing parser, then undo it on the proposal.
            let terminated = (!text.ends_with('\n')).then(|| format!("{text}\n"));
            if let Some(proposal) =
                crate::command_view::candidate(argv, terminated.as_deref().unwrap_or(&text), false)
            {
                let proposal = if terminated.is_some() {
                    proposal.strip_suffix('\n').unwrap_or(&proposal)
                } else {
                    &proposal
                };
                let selected = compactor.compact(proposal);
                if selected.output_tokens < output_tokens {
                    presentation = Presentation::CommandView;
                    encoding = selected.encoding;
                    emitted = selected.text;
                    output_tokens = selected.output_tokens;
                }
            }
        }
        let duration_ms = start.elapsed().as_millis().min(u64::MAX as u128) as u64;
        if output_tokens < original.input_tokens {
            response.set_text(field, &emitted)?;
            changed = true;
        }
        let selected = output_tokens < original.input_tokens;
        observed.facts.streams[index] = Some(StreamFacts {
            input_bytes: Some(text.len() as u64),
            emitted_bytes: Some(if selected { emitted.len() } else { text.len() } as u64),
            input_complete: true,
            tokens: Some(TokenCounts {
                input: original.input_tokens as u64,
                emitted: if selected {
                    output_tokens
                } else {
                    original.input_tokens
                } as u64,
            }),
            missing: None,
            presentation: if selected {
                presentation
            } else {
                Presentation::Raw
            },
            encoding: Some(observe::encoding(if selected {
                encoding
            } else {
                sift::Encoding::Raw
            })),
            skip: (!selected).then_some(SkipReason::NotSmaller),
            ..Default::default()
        });
        measured(
            &tool,
            field,
            &text,
            &emitted,
            original.input_tokens,
            output_tokens,
            duration_ms,
        );
    }
    observed.facts.transform_duration = Some(transformation.elapsed());
    if !changed {
        observed.skip(SkipReason::NotSmaller);
        return Ok(None);
    }

    // RawValue serialization preserves every untouched field's representation.
    let response = serde_json::to_string(&response)?;
    let envelope = match host {
        "claude" => format!(
            "{{\"hookSpecificOutput\":{{\"hookEventName\":\"PostToolUse\",\"updatedToolOutput\":{response}}}}}"
        ),
        "copilot" => format!("{{\"modifiedResult\":{response}}}"),
        // Hermes' final-result hook returns a JSON result *string*. Keep opaque
        // metadata lexemes intact across the Python adapter's JSON decoding.
        "hermes" => format!("{{\"result\":{}}}", serde_json::to_string(&response)?),
        _ => unreachable!(),
    };
    Ok(Some(envelope))
}

/// Read at most 16 MiB + one sentinel byte, then write one JSON line. Hook
/// failures must not obstruct the host's original result or fail its tool call.
#[cfg(test)]
#[allow(dead_code)]
pub fn run(host: &str) -> Result<()> {
    run_observed(host, &mut Observed::new())
}

pub(crate) fn run_observed(host: &str, observed: &mut Observed) -> Result<()> {
    observed.facts.entry = Entry::CompletionHook;
    observed.facts.host = Some(observe::host(host));
    observed.phase = Phase::Input;
    let mut bytes = Vec::new();
    let mut records = Vec::new();
    let mut recording = None;
    let result = std::io::stdin()
        .lock()
        .take((MAX_INPUT + 1) as u64)
        .read_to_end(&mut bytes);
    let output = if result.is_ok() && bytes.len() <= MAX_INPUT {
        observed.phase = Phase::Settings;
        let settings = match Settings::load() {
            Ok(settings) if settings.enabled => Some(settings),
            Ok(_) => {
                observed.skip(SkipReason::Disabled);
                None
            }
            Err(error) => {
                observed.fail(&error);
                observed.skip(SkipReason::SettingsUnavailable);
                observed.facts.outcome = Outcome::FailOpen;
                None
            }
        };
        settings.and_then(|settings| {
            if !settings.record_usage {
                observed.facts.local_record = Some(LocalRecordOutcome::Disabled);
            }
            let input = match std::str::from_utf8(&bytes) {
                Ok(input) => input,
                Err(_) => {
                    observed.skip(SkipReason::MalformedInput);
                    observed.facts.outcome = Outcome::FailOpen;
                    return None;
                }
            };
            let root: Object = match serde_json::from_str(input.trim_start_matches('\u{feff}')) {
                Ok(root) => root,
                Err(_) => {
                    observed.skip(SkipReason::MalformedInput);
                    observed.facts.outcome = Outcome::FailOpen;
                    return None;
                }
            };
            // Host cwd is authoritative when supplied. Invalid or unavailable
            // directories remain unscoped rather than naming the hook launcher.
            let project = if root.get("cwd").is_some() {
                root.string("cwd").and_then(|cwd| {
                    let path = Path::new(&cwd);
                    path.is_absolute()
                        .then(|| state::project_at(path).ok())
                        .flatten()
                })
            } else {
                std::env::current_dir()
                    .ok()
                    .and_then(|p| state::project_at(&p).ok())
            };
            let transformed = transform_inner(
                host,
                input,
                &settings.exclude_commands,
                observed,
                &mut |tool, field, text, emitted, input_tokens, output_tokens, duration_ms| {
                    if !settings.record_usage {
                        return;
                    }
                    let event = state::Event {
                        unix_millis: state::unix_millis(),
                        command: format!("{tool}.{field}"),
                        input_tokens: Some(input_tokens as u64),
                        output_tokens: Some(output_tokens as u64),
                        input_bytes: text.len() as u64,
                        output_bytes: emitted.len() as u64,
                        duration_ms,
                        exit_code: None,
                        source: Some(format!("hook-{host}")),
                        original_id: None,
                    };
                    // Each record is one text field, including stderr fields. Its
                    // category identifies it; recall stores its original as stdout.
                    let original = settings.keep_originals.then(|| text.as_bytes().to_vec());
                    records.push((event, original));
                },
            );
            recording = Some((settings, project));
            match transformed {
                Ok(output) => output,
                Err(error) => {
                    observed.fail(&error);
                    observed.facts.outcome = Outcome::FailOpen;
                    observed.facts.skip = Some(if observed.phase == Phase::Codec {
                        SkipReason::TokenizerUnavailable
                    } else {
                        SkipReason::MalformedInput
                    });
                    observed.facts.streams = [None, None];
                    records.clear();
                    None
                }
            }
        })
    } else {
        if let Err(error) = result {
            observed.fail(&error.into());
            observed.facts.outcome = Outcome::FailOpen;
        } else {
            observed.skip(SkipReason::EnvelopeLimit);
        }
        None
    };
    // A closed stdout is also harmless: there is no replacement to deliver.
    let mut stdout = std::io::stdout().lock();
    observed.phase = Phase::Output;
    let start = Instant::now();
    let written =
        writeln!(stdout, "{}", output.as_deref().unwrap_or("{}")).and_then(|()| stdout.flush());
    observed.facts.output_duration = Some(start.elapsed());
    observe::envelope_delivery(observed, &written, output.is_some());
    if written.is_ok() {
        for (event, original) in records {
            // Optional storage must not alter successful output delivery.
            if let Some((settings, project)) = &recording {
                let result = state::state_dir().and_then(|dir| {
                    state::record_project_observed_at(
                        &dir,
                        settings,
                        event,
                        original.as_deref().map(|text| (text, &b""[..])),
                        project.as_deref(),
                    )
                });
                let result = result.unwrap_or(LocalRecordOutcome::Unavailable);
                // A later successful field must not hide an earlier lost record.
                observed.facts.local_record = Some(
                    observed
                        .facts
                        .local_record
                        .map_or(result, |prior| prior.max(result)),
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod observation_tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn selected_text_counts_follow_replacement_and_failed_envelope_flush() {
        let original = "PRIVATE_CANARY worker completed checkpoint\n".repeat(100);
        let input = json!({"hook_event_name":"PostToolUse","tool_name":"Bash",
            "tool_input":{"command":"printf data"},
            "tool_response":{"stdout":original,"stderr":"warn", "interrupted":false,"isImage":false}}).to_string();
        let mut observed = Observed::new();
        let output = transform_inner(
            "claude",
            &input,
            &[],
            &mut observed,
            &mut |_, _, _, _, _, _, _| {},
        )
        .unwrap()
        .unwrap();
        let envelope: Value = serde_json::from_str(&output).unwrap();
        let selected = envelope["hookSpecificOutput"]["updatedToolOutput"]["stdout"]
            .as_str()
            .unwrap();
        observe::envelope_delivery(&mut observed, &Ok(()), true);
        let stream = observed.facts.streams[0].unwrap();
        let tokenizer = tiktoken_rs::o200k_base().unwrap();
        assert_eq!(
            stream.tokens.unwrap(),
            TokenCounts {
                input: tokenizer.encode_ordinary(&original).len() as u64,
                emitted: tokenizer.encode_ordinary(selected).len() as u64,
            }
        );
        assert_eq!(stream.emitted_bytes, Some(selected.len() as u64));
        assert!(stream.output_complete);
        let stderr = observed.facts.streams[1].unwrap();
        assert_eq!(stderr.emitted_bytes, Some(4));
        assert_eq!(stderr.tokens, None);
        assert_eq!(stderr.missing, Some(Missingness::Small));
        assert!(!format!("{:?}", observed.facts).contains("PRIVATE_CANARY"));
        observe::envelope_delivery(
            &mut observed,
            &Err(std::io::ErrorKind::BrokenPipe.into()),
            true,
        );
        assert_eq!(observed.facts.delivery, Delivery::Failed);
        for stream in observed.facts.streams.iter().flatten() {
            assert!(!stream.output_complete);
            assert_eq!(stream.emitted_bytes, None);
            assert_eq!(stream.tokens, None);
        }
    }

    #[test]
    fn unsupported_and_already_wrapped_hooks_do_not_recount() {
        let input = json!({"hook_event_name":"PostToolUse","tool_name":"Bash",
            "tool_input":{"command":"ctx sift run --raw -- git status"},
            "tool_response":{"stdout":"row\n".repeat(100)}})
        .to_string();
        for (host, exclusions, reason) in [
            ("claude", vec![], SkipReason::AlreadyWrapped),
            ("claude", vec!["Bash".to_owned()], SkipReason::Excluded),
            ("unknown", vec![], SkipReason::UnsupportedHost),
        ] {
            let mut observed = Observed::new();
            assert!(
                transform_inner(
                    host,
                    &input,
                    &exclusions,
                    &mut observed,
                    &mut |_, _, _, _, _, _, _| panic!("no local counts for a skipped wrapper")
                )
                .unwrap()
                .is_none()
            );
            assert_eq!(observed.facts.skip, Some(reason));
            assert_eq!(observed.facts.streams, [None, None]);
        }
    }
}
