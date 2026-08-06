use crate::{current_source_generation_policy_hash, IndexError, Result};

pub(crate) fn current_core_record_contract_fingerprint() -> String {
    ctx_history_core::core_record_contract_fingerprint()
}

/// Accepts only the Core contract emitted by this build.
pub(crate) fn validate_core_contract_fingerprint(actual: &str) -> Result<()> {
    let current = current_core_record_contract_fingerprint();
    if actual == current {
        return Ok(());
    }
    Err(IndexError::CoreRecordContractMismatch {
        expected: current,
        actual: actual.to_owned(),
    })
}

pub(crate) fn expected_source_generation_policy_hash() -> Result<String> {
    Ok(current_source_generation_policy_hash()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RETIRED_CORE_FINGERPRINT: &str =
        "7552eee7cae0695a98f202b02f52cbf5680845cb7bacea4ed754e283bc15f051";

    #[test]
    fn only_the_current_core_contract_is_accepted() {
        let current = current_core_record_contract_fingerprint();
        validate_core_contract_fingerprint(&current).unwrap();
        for retired_or_unknown in [RETIRED_CORE_FINGERPRINT, &"b".repeat(64)] {
            assert!(matches!(
                validate_core_contract_fingerprint(retired_or_unknown),
                Err(IndexError::CoreRecordContractMismatch { actual, .. })
                    if actual == retired_or_unknown
            ));
        }
        assert_eq!(
            expected_source_generation_policy_hash().unwrap(),
            current_source_generation_policy_hash().unwrap()
        );
    }
}
