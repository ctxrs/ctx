use super::*;

impl PythonInventory {
    pub(super) fn validate_source(&self, path: &str, hash: &str) -> Result<()> {
        ensure!(
            self.source_hashes
                .get(path)
                .is_none_or(|expected| expected == hash),
            "Python source changed during scan; retry indexing"
        );
        Ok(())
    }

    pub(super) fn context_token(&self, path: &str) -> &str {
        self.context_tokens
            .get(path)
            .map_or(PYTHON_TERMINAL_CONTEXT, String::as_str)
    }
}
