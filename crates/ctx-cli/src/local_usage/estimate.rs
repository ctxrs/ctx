use serde::Serialize;

use super::store::UsageStoreError;

pub(crate) const ESTIMATE_MODEL_VERSION: &str = "matched_normalized_sessions_v1";
pub(crate) const COEFFICIENT_VERSION: &str = "utf8_token_equivalent_range_v1";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct EstimateFacts {
    pub(crate) complete_calls: u64,
    pub(crate) unavailable_calls: u64,
    pub(crate) delivered_context_bytes: u64,
    pub(crate) matched_normalized_session_bytes: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub(crate) struct TokenEquivalentRange {
    pub(crate) low: u64,
    pub(crate) central: u64,
    pub(crate) high: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct ApproximateContextTokens {
    pub(crate) coefficient_version: &'static str,
    pub(crate) delivered_context_bytes: u64,
    #[serde(flatten)]
    pub(crate) token_equivalents: TokenEquivalentRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct EstimatedContextReduction {
    pub(crate) estimate_model_version: &'static str,
    pub(crate) coefficient_version: &'static str,
    pub(crate) covered_calls: u64,
    pub(crate) unavailable_calls: u64,
    pub(crate) comparison_baseline_bytes: u64,
    pub(crate) observed_delivered_context_bytes: u64,
    pub(crate) estimated_avoided_context_bytes: u64,
    #[serde(flatten)]
    pub(crate) approximate_token_equivalents: TokenEquivalentRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct UsageEstimates {
    pub(crate) approximate_context_tokens: ApproximateContextTokens,
    pub(crate) estimated_context_reduction: EstimatedContextReduction,
}

pub(crate) fn estimate_usage(
    facts: EstimateFacts,
) -> Result<Option<UsageEstimates>, UsageStoreError> {
    if facts.complete_calls == 0 {
        return Ok(None);
    }
    let estimated_avoided_context_bytes = facts
        .matched_normalized_session_bytes
        .checked_sub(facts.delivered_context_bytes)
        .ok_or(UsageStoreError::Integrity)?;
    Ok(Some(UsageEstimates {
        approximate_context_tokens: ApproximateContextTokens {
            coefficient_version: COEFFICIENT_VERSION,
            delivered_context_bytes: facts.delivered_context_bytes,
            token_equivalents: token_equivalent_range(facts.delivered_context_bytes)?,
        },
        estimated_context_reduction: EstimatedContextReduction {
            estimate_model_version: ESTIMATE_MODEL_VERSION,
            coefficient_version: COEFFICIENT_VERSION,
            covered_calls: facts.complete_calls,
            unavailable_calls: facts.unavailable_calls,
            comparison_baseline_bytes: facts.matched_normalized_session_bytes,
            observed_delivered_context_bytes: facts.delivered_context_bytes,
            estimated_avoided_context_bytes,
            approximate_token_equivalents: token_equivalent_range(estimated_avoided_context_bytes)?,
        },
    }))
}

fn token_equivalent_range(bytes: u64) -> Result<TokenEquivalentRange, UsageStoreError> {
    Ok(TokenEquivalentRange {
        low: bytes / 5,
        central: bytes / 4,
        high: multiply_then_floor_divide(bytes, 2, 5)?,
    })
}

fn multiply_then_floor_divide(
    value: u64,
    multiplier: u64,
    divisor: u64,
) -> Result<u64, UsageStoreError> {
    if divisor == 0 {
        return Err(UsageStoreError::Integrity);
    }
    let whole = value
        .checked_div(divisor)
        .and_then(|value| value.checked_mul(multiplier))
        .ok_or(UsageStoreError::Integrity)?;
    let remainder = value
        .checked_rem(divisor)
        .and_then(|value| value.checked_mul(multiplier))
        .and_then(|value| value.checked_div(divisor))
        .ok_or(UsageStoreError::Integrity)?;
    whole
        .checked_add(remainder)
        .ok_or(UsageStoreError::Integrity)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coefficient_range_uses_floor_math() {
        assert_eq!(
            token_equivalent_range(19).unwrap(),
            TokenEquivalentRange {
                low: 3,
                central: 4,
                high: 7,
            }
        );
    }

    #[test]
    fn estimate_uses_only_complete_definition_two_facts() {
        let estimate = estimate_usage(EstimateFacts {
            complete_calls: 2,
            unavailable_calls: 3,
            delivered_context_bytes: 400,
            matched_normalized_session_bytes: 1_000,
        })
        .unwrap()
        .unwrap();
        assert_eq!(
            estimate.approximate_context_tokens.token_equivalents,
            TokenEquivalentRange {
                low: 80,
                central: 100,
                high: 160,
            }
        );
        assert_eq!(
            estimate
                .estimated_context_reduction
                .estimated_avoided_context_bytes,
            600
        );
        assert_eq!(
            estimate
                .estimated_context_reduction
                .approximate_token_equivalents,
            TokenEquivalentRange {
                low: 120,
                central: 150,
                high: 240,
            }
        );
    }

    #[test]
    fn estimate_is_absent_without_complete_coverage() {
        assert!(estimate_usage(EstimateFacts {
            unavailable_calls: 5,
            ..EstimateFacts::default()
        })
        .unwrap()
        .is_none());
    }
}
