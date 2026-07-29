use serde::Serialize;

pub(crate) const ESTIMATE_MODEL: EstimateModel = EstimateModel {
    version: 1,
    approximate_bytes_per_token: 4,
    avoided_search_token_multiplier: 49,
    result_bearing_search_seconds: 60,
    discovered_record_open_seconds: 15,
    produced_blame_seconds: 300,
    possible_blame_seconds: 120,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct EstimateModel {
    pub(crate) version: u32,
    pub(crate) approximate_bytes_per_token: u64,
    pub(crate) avoided_search_token_multiplier: u64,
    pub(crate) result_bearing_search_seconds: u64,
    pub(crate) discovered_record_open_seconds: u64,
    pub(crate) produced_blame_seconds: u64,
    pub(crate) possible_blame_seconds: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct EstimateFacts {
    pub(crate) result_bearing_searches: u64,
    pub(crate) semantic_context_eligible_samples: u64,
    pub(crate) semantic_context_bytes: u64,
    pub(crate) semantic_context_byte_samples: u64,
    pub(crate) semantic_search_result_bytes: u64,
    pub(crate) semantic_search_result_byte_samples: u64,
    pub(crate) discovered_record_opens: u64,
    pub(crate) produced_blame_requests: u64,
    pub(crate) possible_blame_requests: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub(crate) struct UsageEstimates {
    pub(crate) model: EstimateModel,
    pub(crate) approximate_context_tokens: CoveredTokenEstimate,
    pub(crate) approximate_avoided_context_tokens: CoveredTokenEstimate,
    pub(crate) estimated_time_saved_seconds: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EstimateCoverage {
    #[default]
    Complete,
    Partial,
    UnavailableLegacy,
}

impl EstimateCoverage {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::UnavailableLegacy => "unavailable_legacy",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub(crate) struct CoveredTokenEstimate {
    pub(crate) approximate_tokens: Option<u64>,
    pub(crate) coverage: EstimateCoverage,
    pub(crate) measured_samples: u64,
    pub(crate) eligible_samples: u64,
}

impl Default for EstimateModel {
    fn default() -> Self {
        ESTIMATE_MODEL
    }
}

pub(crate) fn estimate_usage(
    facts: EstimateFacts,
) -> Result<UsageEstimates, super::store::UsageStoreError> {
    let approximate_context_tokens = covered_token_estimate(
        facts.semantic_context_bytes,
        facts.semantic_context_byte_samples,
        facts.semantic_context_eligible_samples,
        1,
    )?;
    let approximate_avoided_context_tokens = covered_token_estimate(
        facts.semantic_search_result_bytes,
        facts.semantic_search_result_byte_samples,
        facts.result_bearing_searches,
        ESTIMATE_MODEL.avoided_search_token_multiplier,
    )?;
    let search_seconds = facts
        .result_bearing_searches
        .checked_mul(ESTIMATE_MODEL.result_bearing_search_seconds)
        .ok_or(super::store::UsageStoreError::Integrity)?;
    let open_seconds = facts
        .discovered_record_opens
        .checked_mul(ESTIMATE_MODEL.discovered_record_open_seconds)
        .ok_or(super::store::UsageStoreError::Integrity)?;
    let produced_seconds = facts
        .produced_blame_requests
        .checked_mul(ESTIMATE_MODEL.produced_blame_seconds)
        .ok_or(super::store::UsageStoreError::Integrity)?;
    let possible_seconds = facts
        .possible_blame_requests
        .checked_mul(ESTIMATE_MODEL.possible_blame_seconds)
        .ok_or(super::store::UsageStoreError::Integrity)?;
    let estimated_time_saved_seconds = [
        search_seconds,
        open_seconds,
        produced_seconds,
        possible_seconds,
    ]
    .into_iter()
    .try_fold(0_u64, |total, value| {
        total
            .checked_add(value)
            .ok_or(super::store::UsageStoreError::Integrity)
    })?;
    Ok(UsageEstimates {
        model: ESTIMATE_MODEL,
        approximate_context_tokens,
        approximate_avoided_context_tokens,
        estimated_time_saved_seconds,
    })
}

fn covered_token_estimate(
    bytes: u64,
    measured_samples: u64,
    eligible_samples: u64,
    multiplier: u64,
) -> Result<CoveredTokenEstimate, super::store::UsageStoreError> {
    let coverage = if measured_samples == eligible_samples {
        EstimateCoverage::Complete
    } else if measured_samples == 0 {
        EstimateCoverage::UnavailableLegacy
    } else {
        EstimateCoverage::Partial
    };
    let approximate_tokens = if coverage == EstimateCoverage::UnavailableLegacy {
        None
    } else {
        Some(
            divide_rounding_up(bytes, ESTIMATE_MODEL.approximate_bytes_per_token)?
                .checked_mul(multiplier)
                .ok_or(super::store::UsageStoreError::Integrity)?,
        )
    };
    Ok(CoveredTokenEstimate {
        approximate_tokens,
        coverage,
        measured_samples,
        eligible_samples,
    })
}

fn divide_rounding_up(value: u64, divisor: u64) -> Result<u64, super::store::UsageStoreError> {
    if divisor == 0 {
        return Err(super::store::UsageStoreError::Integrity);
    }
    (value / divisor)
        .checked_add(u64::from(value % divisor != 0))
        .ok_or(super::store::UsageStoreError::Integrity)
}
