use std::{fs, path::PathBuf};

use anyhow::{anyhow, Result};
use clap::ValueEnum;
use serde_json::{json, Value};

use ctx_history_core::CaptureProvider;

use crate::output::compact_json;

mod artifact;
use artifact::atomic_write_output;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum TranscriptMode {
    Full,
    Lite,
    Log,
}

impl TranscriptMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Lite => "lite",
            Self::Log => "log",
        }
    }
}

pub(crate) fn write_output(body: String, out: Option<PathBuf>) -> Result<()> {
    if let Some(out) = out {
        if let Some(parent) = out.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        atomic_write_output(&out, body.as_bytes())?;
    } else {
        print!("{body}");
        if !body.ends_with('\n') {
            println!();
        }
    }
    Ok(())
}

pub(crate) fn normalize_uuid_prefix(value: &str, kind: &str) -> Result<String> {
    let prefix = value.trim();
    if prefix.len() < 8 {
        return Err(anyhow!(
            "{kind} id prefix must be at least 8 hex characters, or pass a full ctx UUID"
        ));
    }
    if prefix.contains('-') || !prefix.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(anyhow!(
            "{kind} id must be a full ctx UUID or an unambiguous hex prefix from verbose search output"
        ));
    }
    Ok(prefix.to_ascii_lowercase())
}

pub(crate) fn provider_resume_json(
    provider: CaptureProvider,
    provider_session_id: Option<&str>,
) -> Value {
    let (command, argv) = match (provider, provider_session_id) {
        (CaptureProvider::Codex, Some(session_id)) => (
            Some(format!("codex resume {}", shell_quote_arg(session_id))),
            Some(vec![
                "codex".to_owned(),
                "resume".to_owned(),
                session_id.to_owned(),
            ]),
        ),
        _ => (None, None),
    };
    compact_json(json!({
        "available": command.is_some(),
        "command": command,
        "argv": argv,
    }))
}

pub(crate) fn shell_quote_arg(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':' | '@'))
    {
        return value.to_owned();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}
