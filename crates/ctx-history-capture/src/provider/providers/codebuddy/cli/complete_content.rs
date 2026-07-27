use std::{fs, path::Path};

use ctx_history_core::{CaptureProvider, ProviderCaptureEnvelope};
use serde_json::Value;

use crate::captured_batch::{CapturedRecord, SourceObservation};
use crate::complete_content::jsonl::{
    attach_exact_jsonl_complete_content_locator, ExactJsonlSourceBinding,
};
use crate::provider::importer::provider_path_identity;
use crate::{Result, CODEBUDDY_SOURCE_FORMAT};

use super::super::source::CodeBuddyFrozenFile;
use super::super::CODEBUDDY_CLI_POLICY_REVISION;
use super::codebuddy_cli_message_text;

#[derive(Clone)]
pub(super) struct CodeBuddyCliCompleteContentBinding {
    exact: ExactJsonlSourceBinding,
}

impl CodeBuddyCliCompleteContentBinding {
    pub(super) fn for_source(source: &SourceObservation, path_identity: &str) -> Self {
        Self {
            exact: ExactJsonlSourceBinding::new(source.source_revision(), path_identity),
        }
    }

    pub(super) fn attach(
        &self,
        capture: &mut ProviderCaptureEnvelope,
        raw_value: &Value,
        record: &CapturedRecord,
        physical_line: usize,
    ) -> Result<()> {
        let Some(event) = capture.event.as_mut() else {
            return Ok(());
        };
        attach_exact_jsonl_complete_content_locator(
            event,
            CaptureProvider::CodeBuddy,
            CODEBUDDY_SOURCE_FORMAT,
            raw_value,
            record,
            physical_line,
            &self.exact,
        )
    }
}

pub(crate) fn codebuddy_cli_complete_content_record(
    value: &Value,
    physical_line: usize,
) -> Option<(String, String)> {
    let text = codebuddy_cli_message_text(value);
    if value.get("type").and_then(Value::as_str) != Some("message") || text.trim().is_empty() {
        return None;
    }
    let native_record_id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("line-{physical_line}"));
    Some((text, native_record_id))
}

pub(crate) fn codebuddy_cli_complete_content_source(path: &Path) -> Result<(String, String)> {
    let frozen = CodeBuddyFrozenFile::read(path)?;
    let canonical_path = fs::canonicalize(path)?;
    let path_identity = provider_path_identity(&canonical_path)?;
    Ok((
        frozen.source_revision_with_policy("cli-jsonl", CODEBUDDY_CLI_POLICY_REVISION),
        path_identity,
    ))
}
