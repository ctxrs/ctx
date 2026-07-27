use std::path::Path;

use ctx_history_store::Store;

use crate::{
    CaptureError, NormalizedProviderImportOptions, ProviderAdapterContext, ProviderImportFailure,
    ProviderImportSummary, Result,
};

mod cli;
mod extension;
mod normalization;
mod source;

pub(crate) use normalization::{codebuddy_decoded_message, codebuddy_message_text};

const CODEBUDDY_CAPTURE_REVISION: u32 = 2;
const CODEBUDDY_POLICY_REVISION: u32 = 4;
const CODEBUDDY_CLI_POLICY_REVISION: u32 = 5;
const CODEBUDDY_EXTENSION_RECORD_KIND: &str = "codebuddy-extension-message-json-v1";
const CODEBUDDY_CLI_RECORD_KIND: &str = "codebuddy-cli-jsonl-v1";
const CODEBUDDY_CLI_LOCATOR_KIND: &str = "jsonl-source-item-byte-range-v1";
const CODEBUDDY_CLI_TITLE_ANCHOR_HASH_DOMAIN: &[u8] = b"ctx-codebuddy-cli-title-anchor-sha256-v1\0";
const CODEBUDDY_WHOLE_JSON_POSITION_KIND: &str = "whole-json-item-v1";
const CODEBUDDY_WHOLE_JSON_LOCATOR_KIND: &str = "whole-json-source-item-v1";
const CODEBUDDY_MAX_CHECKPOINT_TEXT_BYTES: usize = 8 * 1024;
const CODEBUDDY_MAX_FAILURE_BYTES: usize = 2 * 1024;
const CODEBUDDY_MAX_CHECKPOINT_FAILURES: usize = 64;

pub(crate) fn import_codebuddy_history_batched(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    import_options: NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let mut merged = ProviderImportSummary::default();
    let mut extension_ordinal = 0_usize;
    let extension_count = extension::visit_sessions(path, &mut |session_dir| {
        extension_ordinal = extension_ordinal.saturating_add(1);
        merged.merge(extension::import_session_batched(
            session_dir,
            extension_ordinal,
            store,
            &context,
            &import_options,
        )?);
        Ok(())
    })?;

    let mut cli_ordinal = 0_usize;
    let cli_count = cli::visit_jsonl_files(path, &mut |jsonl_path| {
        cli_ordinal = cli_ordinal.saturating_add(1);
        merged.merge(cli::import_jsonl_file_batched(
            jsonl_path,
            cli_ordinal,
            store,
            &context,
            &import_options,
        )?);
        Ok(())
    })?;

    if extension_count == 0 && cli_count == 0 {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "no CodeBuddy history sessions with index.json and messages/*.json or CLI project JSONL files were found",
        });
    }
    if !merged.has_accepted_content() && merged.failed == 0 {
        merged.record_failure(ProviderImportFailure {
            line: 0,
            error: "CodeBuddy history contained no real conversation messages".to_owned(),
        });
    }
    Ok(merged)
}

pub(crate) use cli::complete_content::{
    codebuddy_cli_complete_content_record, codebuddy_cli_complete_content_source,
};
