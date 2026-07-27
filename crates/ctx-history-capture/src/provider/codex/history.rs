use std::path::Path;

use ctx_history_store::Store;

use crate::{CodexHistoryImportOptions, ProviderImportSummary, Result};

pub fn import_codex_history_jsonl(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: CodexHistoryImportOptions,
) -> Result<ProviderImportSummary> {
    super::nativepath::import_codex_native_prompt_history(path.as_ref(), store, options)
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
