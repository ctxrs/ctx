// Adapted from Sift, MIT; source revision and license are in this crate’s NOTICE.
//! Native pre-execution adapters. Host metadata remains opaque, and unsupported
//! commands return no replacement. Host permission settings are never modified.
use crate::hooks::Object;
use crate::rewrite::{self, Shell};
use crate::state::Settings;
use anyhow::Result;
use std::io::{Read, Write};
use std::path::Path;

const MAX_INPUT: usize = 1024 * 1024;

pub fn transform(
    host: &str,
    input: &str,
    executable: &Path,
    exclusions: &[String],
) -> Result<Option<String>> {
    // Whole-request policy rules do not necessarily survive command rewriting.
    // Enable only hosts with a qualified original-command policy path. Other
    // CLI names remain accepted as no-ops for existing/manual registrations.
    if input.len() > MAX_INPUT || !matches!(host, "codex" | "vibe") {
        return Ok(None);
    }
    let root: Object = serde_json::from_str(input.trim_start_matches('\u{feff}'))?;
    let Some(tool) = root.string("tool_name") else {
        return Ok(None);
    };
    let (event, accepted) = match host {
        "codex" => ("PreToolUse", matches!(tool.as_str(), "Bash" | "bash")),
        "vibe" => ("pre_tool", tool == "bash"),
        _ => return Ok(None),
    };
    if !accepted || exclusions.iter().any(|e| e == &tool) {
        return Ok(None);
    }
    if root.get("hook_event_name").is_some()
        && root.string("hook_event_name").as_deref() != Some(event)
    {
        return Ok(None);
    }
    let Some(mut arguments) = root.object("tool_input") else {
        return Ok(None);
    };
    let Some(command) = arguments.string("command") else {
        return Ok(None);
    };
    let shell = match arguments
        .string("shell")
        .or_else(|| root.string("shell"))
        .as_deref()
    {
        Some("powershell" | "pwsh" | "powershell.exe" | "pwsh.exe") => Shell::PowerShell,
        Some("bash" | "sh" | "zsh" | "/bin/bash" | "/bin/sh" | "/bin/zsh") => Shell::Posix,
        Some(_) => return Ok(None),
        None => Shell::native(),
    };
    let Some(changed) = rewrite::command(&command, executable, shell, exclusions) else {
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

pub fn run(host: &str) -> Result<i32> {
    let mut bytes = Vec::new();
    let output = (|| -> Option<String> {
        std::io::stdin()
            .lock()
            .take((MAX_INPUT + 1) as u64)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() > MAX_INPUT {
            return None;
        }
        let settings = Settings::load().ok().filter(|s| s.enabled)?;
        transform(
            host,
            std::str::from_utf8(&bytes).ok()?,
            &std::env::current_exe().ok()?,
            &settings.exclude_commands,
        )
        .ok()
        .flatten()
    })();
    if let Some(output) = output {
        let mut stdout = std::io::stdout().lock();
        let _ = writeln!(stdout, "{output}").and_then(|()| stdout.flush());
        Ok(0)
    } else if host == "cursor" {
        // Cursor can reject invalid permission-hook output. Its documented
        // non-2 error exit leaves the original action to normal processing.
        Ok(1)
    } else {
        let _ = writeln!(std::io::stdout().lock(), "{{}}");
        Ok(0)
    }
}
