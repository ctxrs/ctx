use super::*;

impl SemanticUsageRecorder {
    pub fn snapshot(&self) -> Result<Vec<ProviderUsage>> {
        self.0
            .lock()
            .map(|v| v.clone())
            .map_err(|_| anyhow::anyhow!("semantic usage lock poisoned"))
    }

    pub(super) fn record(&self, usage: ProviderUsage) -> Result<()> {
        self.0
            .lock()
            .map_err(|_| anyhow::anyhow!("semantic usage lock poisoned"))?
            .push(usage);
        Ok(())
    }
}
