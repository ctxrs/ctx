use super::*;

impl SemanticBudget {
    pub fn new(max_calls: Option<usize>, max_output_tokens: Option<u64>) -> Self {
        Self {
            max_calls,
            max_output_tokens,
            usage: Mutex::new(SemanticUsage::default()),
        }
    }

    pub fn usage(&self) -> Result<SemanticUsage> {
        self.usage
            .lock()
            .map(|usage| *usage)
            .map_err(|_| anyhow::anyhow!("semantic budget lock poisoned"))
    }

    pub(super) fn reserve_calls(&self, call_count: usize, output_tokens: u32) -> Result<()> {
        let mut usage = self
            .usage
            .lock()
            .map_err(|_| anyhow::anyhow!("semantic budget lock poisoned"))?;
        let calls = usage
            .calls
            .checked_add(call_count)
            .context("semantic call counter overflow")?;
        let tokens = usage
            .reserved_output_tokens
            .checked_add(u64::from(output_tokens))
            .context("semantic output counter overflow")?;
        ensure!(
            self.max_calls.is_none_or(|limit| calls <= limit)
                && self.max_output_tokens.is_none_or(|limit| tokens <= limit),
            "corpus semantic call/token budget exhausted"
        );
        *usage = SemanticUsage {
            calls,
            reserved_output_tokens: tokens,
        };
        Ok(())
    }
}
