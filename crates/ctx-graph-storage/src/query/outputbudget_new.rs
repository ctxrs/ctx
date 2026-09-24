use super::*;

impl OutputBudget {
    pub(super) fn new(graph: &GraphResult, options: &SearchOptions) -> Result<Self> {
        let budget = Self {
            bytes: serde_json::to_vec(graph)?.len() + 1,
            limit: options.token_budget.map(|n| n * 4),
        };
        ensure!(
            budget.limit.is_none_or(|limit| budget.bytes <= limit),
            "token budget cannot hold the complete result; increase the budget"
        );
        Ok(budget)
    }
    pub(super) fn cost(value: &impl serde::Serialize) -> Result<usize> {
        Ok(serde_json::to_vec(value)?.len() + 1)
    }
    pub(super) fn take(&mut self, bytes: usize, output: &mut SearchResult) -> bool {
        if self
            .limit
            .is_some_and(|limit| bytes > limit.saturating_sub(self.bytes))
        {
            truncate(output, "token_budget");
            false
        } else {
            self.bytes += bytes;
            true
        }
    }
}
