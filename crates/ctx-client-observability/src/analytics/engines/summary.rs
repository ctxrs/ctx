use super::wire::{count, pair};
use super::*;
use serde_json::json;

/// Exact aggregate over the same successfully presented, comparable subset.
/// Serde is exclusively for owner-private aggregate state, never the wire DTO.
/// Wire serialization must go through PublicEventV1 and the bucketed sender.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct PresentedTotals {
    pub samples: u64,
    pub input: u64,
    pub output: u64,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct SiftSummaryV1 {
    pub operation: SiftOperation,
    pub mode: SiftMode,
    pub host: SiftHost,
    pub window: Duration,
    pub observed: u64,
    pub execution_failed: u64,
    pub output_failed: u64,
    pub complete: u64,
    pub partial: u64,
    pub unmeasured: u64,
    pub bytes: Option<PresentedTotals>,
    pub tokens: Option<PresentedTotals>,
    /// Counts in NATIVE_DURATION_BUCKETS order; missing means not measured.
    pub latency: Option<[u64; 14]>,
    pub collection_limited: bool,
    pub cohort: SiftCohort,
    pub semantic: Option<SiftSemanticSummary>,
}
impl SiftSummaryV1 {
    pub fn new(operation: SiftOperation, mode: SiftMode, window: Duration) -> Self {
        Self {
            operation,
            mode,
            host: SiftHost::Unknown,
            window,
            observed: 0,
            execution_failed: 0,
            output_failed: 0,
            complete: 0,
            partial: 0,
            unmeasured: 0,
            bytes: None,
            tokens: None,
            latency: None,
            collection_limited: false,
            cohort: SiftCohort::default(),
            semantic: None,
        }
    }
    pub fn execution_outcome(&self) -> SiftOutcome {
        if self.execution_failed == 0 {
            SiftOutcome::Success
        } else if self.execution_failed == self.observed {
            SiftOutcome::Failure
        } else {
            SiftOutcome::Mixed
        }
    }
    /// Reject incoherent optional statistics locally; never invent missing values.
    pub fn into_event(self) -> Option<PublicEventV1> {
        let partition = self
            .complete
            .checked_add(self.partial)?
            .checked_add(self.unmeasured)?;
        if self.observed == 0
            || partition != self.observed
            || self.execution_failed > self.observed
            || self.output_failed > self.observed
        {
            return None;
        }
        for pair in [self.bytes, self.tokens].into_iter().flatten() {
            if pair.samples == 0 || pair.samples > self.complete {
                return None;
            }
        }
        if let Some(bins) = self.latency {
            if bins.into_iter().try_fold(0u64, u64::checked_add)? > self.observed {
                return None;
            }
        }
        if self.cohort.failure_phase.is_some() != self.cohort.failure_kind.is_some() {
            return None;
        }
        if let Some(s) = self.semantic {
            if s.observed == 0
                || s.observed > self.observed
                || s.requests_attempted > s.observed
                || s.cache_hits > s.observed
                || ![s.input_tokens, s.output_tokens]
                    .into_iter()
                    .all(|x| windows::valid_total(x, s.requests_attempted, false))
                || s.provider_latency.is_some_and(|b| {
                    windows::histogram_total(b).is_none_or(|n| n > s.requests_attempted)
                })
            {
                return None;
            }
        }
        Some(PublicEventV1::SiftSummary(self))
    }
    pub(super) fn properties(&self) -> Map<String, Value> {
        let mut p = Map::new();
        p.insert("sift_operation".into(), json!(self.operation.as_str()));
        p.insert("sift_mode".into(), json!(self.mode.as_str()));
        p.insert("sift_host".into(), json!(self.host.as_str()));
        p.insert(
            "sift_execution_outcome".into(),
            json!(self.execution_outcome().as_str()),
        );
        p.insert("collection_scope".into(), json!("best_effort_observed"));
        p.insert("collection_limited".into(), json!(self.collection_limited));
        p.insert(
            "observation_window_bucket".into(),
            json!(duration_bucket(self.window).as_str()),
        );
        for (key, value) in [
            ("observed_count_bucket", self.observed),
            ("execution_failed_count_bucket", self.execution_failed),
            ("output_failed_count_bucket", self.output_failed),
            ("complete_measurement_count_bucket", self.complete),
            ("partial_measurement_count_bucket", self.partial),
            ("unmeasured_count_bucket", self.unmeasured),
        ] {
            count(&mut p, key, Some(value));
        }
        if let Some(value) = self.bytes {
            pair(&mut p, "bytes", value);
        }
        if let Some(value) = self.tokens {
            pair(&mut p, "tokens", value);
            p.insert("sift_token_basis".into(), json!("o200k_base_v1"));
        }
        if let Some(bins) = self.latency {
            let measured = bins.iter().copied().fold(0u64, u64::saturating_add); // validated by into_event
            count(&mut p, "latency_measured_count_bucket", Some(measured));
            for (index, value) in bins.into_iter().enumerate() {
                count(
                    &mut p,
                    &format!("sift_latency_{index}_count_bucket"),
                    Some(value),
                );
            }
        }
        self.cohort.insert(&mut p);
        if let Some(s) = self.semantic {
            s.insert(&mut p);
        }
        p
    }
}
