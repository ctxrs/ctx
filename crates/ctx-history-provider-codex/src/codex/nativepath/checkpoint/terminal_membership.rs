use super::{CodexTerminalAuthorityCheckpointV0, MAX_CODEX_TERMINAL_AUTHORITIES};

impl Default for CodexTerminalAuthorityCheckpointV0 {
    fn default() -> Self {
        Self::indexed(Vec::new())
    }
}

impl CodexTerminalAuthorityCheckpointV0 {
    pub(in crate::codex::nativepath) fn may_contain_unindexed(&self, fingerprint: u64) -> bool {
        match self {
            Self::Indexed { fingerprints } => fingerprints.binary_search(&fingerprint).is_ok(),
            Self::Saturated => true,
        }
    }

    pub(in crate::codex::nativepath) fn insert_unindexed(&mut self, fingerprint: u64) {
        if let Self::Indexed { fingerprints } = self {
            if let Err(index) = fingerprints.binary_search(&fingerprint) {
                if fingerprints.len() < MAX_CODEX_TERMINAL_AUTHORITIES {
                    fingerprints.insert(index, fingerprint);
                } else {
                    *self = Self::Saturated;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unindexed_terminals_remain_bounded_and_conservative_after_restart() {
        let mut checkpoint = CodexTerminalAuthorityCheckpointV0::default();
        for fingerprint in 0..MAX_CODEX_TERMINAL_AUTHORITIES as u64 {
            checkpoint.insert_unindexed(fingerprint);
        }
        let encoded = serde_json::to_vec(&checkpoint).unwrap();
        assert!(encoded.len() < 44 * 1024);
        let mut restored: CodexTerminalAuthorityCheckpointV0 =
            serde_json::from_slice(&encoded).unwrap();
        restored.validate_wire_state().unwrap();
        assert!(restored.may_contain_unindexed(1));
        assert!(!restored.may_contain_unindexed(u64::MAX));
        restored.insert_unindexed(u64::MAX);
        assert!(restored.may_contain_unindexed(123_456));
    }
}
