use super::*;

impl CoreRefreshEngine {
    pub(super) fn observed_published_generation(&self, data_root: &Path) -> Result<Option<String>> {
        let (retained, active_previous_generation) = {
            let state = self.lock_state();
            let active_previous_generation = state
                .active_request_id
                .as_deref()
                .and_then(|request_id| find_attempt(&state, request_id))
                .filter(|attempt| attempt.state.is_active())
                .map(|attempt| attempt.previous_generation.clone());
            (
                state.current_published_generation.clone(),
                active_previous_generation,
            )
        };
        if retained.is_some() {
            return Ok(retained);
        }
        // A mutating Tantivy commit can briefly expose segment metadata before
        // its ctx payload is durable. A concurrent request must coalesce onto
        // the active attempt instead of reopening that in-flight index state.
        if let Some(previous_generation) = active_previous_generation {
            return Ok(previous_generation);
        }
        if let Some(generation_id) = retained_generation_hint(data_root)? {
            return Ok(Some(generation_id));
        }
        published_generation_id(data_root)
    }
}
