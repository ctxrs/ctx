// Adapted from Sift, MIT; source revision and license are in this crate’s NOTICE.
//! Native pre-execution adapters. Host metadata remains opaque, and unsupported
//! commands return no replacement. Host permission settings are never modified.
use crate::hooks::Object;
use crate::rewrite::{self, Shell};
use crate::state::Settings;
use crate::{
    observation::*,
    observe::{self, Observed},
};
use anyhow::Result;
use std::io::{Read, Write};
use std::path::Path;

const MAX_INPUT: usize = 1024 * 1024;

#[allow(dead_code)] // Retained convenience entry point.
pub fn transform(
    host: &str,
    input: &str,
    executable: &Path,
    exclusions: &[String],
) -> Result<Option<String>> {
    transform_observed(host, input, executable, exclusions, &mut Observed::new())
}

pub(crate) fn transform_observed(
    host: &str,
    input: &str,
    executable: &Path,
    exclusions: &[String],
    observed: &mut Observed,
) -> Result<Option<String>> {
    // Whole-request policy rules do not necessarily survive command rewriting.
    // Enable only hosts with a qualified original-command policy path. Other
    // CLI names remain accepted as no-ops for existing/manual registrations.
    if input.len() > MAX_INPUT {
        observed.skip(SkipReason::EnvelopeLimit);
        return Ok(None);
    }
    if !matches!(host, "codex" | "vibe") {
        observed.skip(SkipReason::UnsupportedHost);
        return Ok(None);
    }
    observed.phase = Phase::Protocol;
    let root: Object = serde_json::from_str(input.trim_start_matches('\u{feff}'))?;
    let Some(tool) = root.string("tool_name") else {
        observed.skip(SkipReason::UnsupportedTool);
        return Ok(None);
    };
    let (event, accepted) = match host {
        "codex" => ("PreToolUse", matches!(tool.as_str(), "Bash" | "bash")),
        "vibe" => ("pre_tool", tool == "bash"),
        _ => return Ok(None),
    };
    if !accepted {
        observed.skip(SkipReason::UnsupportedTool);
        return Ok(None);
    }
    if exclusions.iter().any(|e| e == &tool) {
        observed.skip(SkipReason::Excluded);
        return Ok(None);
    }
    if root.get("hook_event_name").is_some()
        && root.string("hook_event_name").as_deref() != Some(event)
    {
        observed.skip(SkipReason::UnsupportedEvent);
        return Ok(None);
    }
    let Some(mut arguments) = root.object("tool_input") else {
        observed.skip(SkipReason::MalformedInput);
        return Ok(None);
    };
    let Some(command) = arguments.string("command") else {
        observed.skip(SkipReason::MalformedInput);
        return Ok(None);
    };
    let shell = match arguments
        .string("shell")
        .or_else(|| root.string("shell"))
        .as_deref()
    {
        Some("powershell" | "pwsh" | "powershell.exe" | "pwsh.exe") => Shell::PowerShell,
        Some("bash" | "sh" | "zsh" | "/bin/bash" | "/bin/sh" | "/bin/zsh") => Shell::Posix,
        Some(_) => {
            observed.skip(SkipReason::UnsupportedShell);
            return Ok(None);
        }
        None => Shell::native(),
    };
    let Some(changed) =
        rewrite::command_observed(&command, executable, shell, exclusions, observed)
    else {
        return Ok(None);
    };
    arguments.set_text("command", &changed)?;
    let args = serde_json::to_string(&arguments)?;
    // Do not pass incoming user objects through serde_json::Value: opaque
    // number lexemes and the legal reserved-number-marker key must survive.
    let response = match host {
        "codex" => format!(
            "{{\"hookSpecificOutput\":{{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"allow\",\"updatedInput\":{args}}}}}"
        ),
        "vibe" => format!("{{\"hook_specific_output\":{{\"tool_input\":{args}}}}}"),
        _ => unreachable!(),
    };
    Ok(Some(response))
}

pub fn run_observed(host: &str, observed: &mut Observed) -> Result<i32> {
    observed.facts.entry = Entry::PreHook;
    observed.facts.host = Some(observe::host(host));
    observed.facts.mode = Mode::Rewrite;
    let mut bytes = Vec::new();
    let output = (|| -> Result<Option<String>> {
        observed.phase = Phase::Input;
        std::io::stdin()
            .lock()
            .take((MAX_INPUT + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_INPUT {
            observed.skip(SkipReason::EnvelopeLimit);
            return Ok(None);
        }
        observed.phase = Phase::Settings;
        let settings = Settings::load()?;
        if !settings.enabled {
            observed.skip(SkipReason::Disabled);
            return Ok(None);
        }
        observed.phase = Phase::Protocol;
        let input = match std::str::from_utf8(&bytes) {
            Ok(input) => input,
            Err(_) => {
                observed.skip(SkipReason::MalformedInput);
                return Ok(None);
            }
        };
        transform_observed(
            host,
            input,
            &std::env::current_exe()?,
            &settings.exclude_commands,
            observed,
        )
    })();
    let output = match output {
        Ok(output) => output,
        Err(error) => {
            observed.fail(&error);
            observed.facts.outcome = Outcome::FailOpen;
            observed.facts.skip = Some(if observed.phase == Phase::Settings {
                SkipReason::SettingsUnavailable
            } else {
                SkipReason::MalformedInput
            });
            None
        }
    };
    observed.phase = Phase::Output;
    if let Some(output) = output {
        let mut stdout = std::io::stdout().lock();
        let written = writeln!(stdout, "{output}").and_then(|()| stdout.flush());
        observe::envelope_delivery(observed, &written, true);
        Ok(0)
    } else if host == "cursor" {
        // Preserve Cursor's documented non-2 no-op exit.
        observed.facts.delivery = Delivery::Unchanged;
        Ok(1)
    } else {
        let mut stdout = std::io::stdout().lock();
        let written = writeln!(stdout, "{{}}").and_then(|()| stdout.flush());
        observe::envelope_delivery(observed, &written, false);
        Ok(0)
    }
}
