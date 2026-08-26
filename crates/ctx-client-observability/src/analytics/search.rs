use std::time::Duration;

use serde_json::{json, Map, Value};

use super::{bytes_bucket, count_bucket, RefreshStatus, SearchBackend};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchStopReason {
    Decisive,
    Exhausted,
    CandidateCap,
    FixedPool,
}

impl SearchStopReason {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Decisive => "decisive",
            Self::Exhausted => "exhausted",
            Self::CandidateCap => "candidate_cap",
            Self::FixedPool => "fixed_pool",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchFailurePhase {
    Preparation,
    Refresh,
    GenerationOpen,
    QueryPreparation,
    SemanticRetrieval,
    IndexQueryDecode,
    ResultProjection,
    Render,
    Output,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchLiteralRootAvailability {
    Observed,
    NotObservedDense,
}

impl SearchLiteralRootAvailability {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Observed => "observed",
            Self::NotObservedDense => "not_observed_dense",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchCopyClusterAvailability {
    NotConstructedV1,
}

impl SearchCopyClusterAvailability {
    const fn as_str(self) -> &'static str {
        match self {
            Self::NotConstructedV1 => "not_constructed_v1",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchDiversificationStatus {
    Applied,
    NotApplicable,
    Indeterminate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchConcentrationFacts {
    pub candidate_sessions: u32,
    pub largest_session_candidate_count: u32,
    pub literal_roots: SearchLiteralRootFacts,
    pub provider_copy_candidate_count: u32,
    pub copy_cluster_availability: SearchCopyClusterAvailability,
    pub diversification_status: SearchDiversificationStatus,
    pub diversification_changed_final_top_n: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchLiteralRootFacts {
    Observed {
        candidate_families: u32,
        candidate_count: u32,
        largest_family_candidate_count: u32,
    },
    NotObservedDense,
}

impl SearchDiversificationStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::NotApplicable => "not_applicable",
            Self::Indeterminate => "indeterminate",
        }
    }
}

impl SearchFailurePhase {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Preparation => "preparation",
            Self::Refresh => "refresh",
            Self::GenerationOpen => "generation_open",
            Self::QueryPreparation => "query_preparation",
            Self::SemanticRetrieval => "semantic_retrieval",
            Self::IndexQueryDecode => "index_query_decode",
            Self::ResultProjection => "result_projection",
            Self::Render => "render",
            Self::Output => "output",
        }
    }
}

/// Missing and derived search-health facts serialized only on the terminal event.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SearchHealthFacts {
    pub retrieval_rounds: Option<u64>,
    pub query_executions: Option<u64>,
    pub candidate_rows: Option<u64>,
    pub records_decoded: Option<u64>,
    pub encoded_core_bytes_decoded: Option<u64>,
    pub final_candidate_pool: Option<u64>,
    pub candidate_pool_truncated: Option<bool>,
    pub concentration: Option<SearchConcentrationFacts>,
    pub stop_reason: Option<SearchStopReason>,
    pub failure_phase: Option<SearchFailurePhase>,
}

/// Exact search fields attached to an MCP terminal event before serialization.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SearchTerminalFacts {
    pub refresh_duration: Option<Duration>,
    pub refresh_status: Option<RefreshStatus>,
    pub refresh_source_count: Option<u64>,
    pub query_duration: Option<Duration>,
    pub backend_requested: Option<SearchBackend>,
    pub backend_effective: Option<SearchBackend>,
    pub health: SearchHealthFacts,
    pub output_duration: Option<Duration>,
    pub output_served: Option<bool>,
}

impl SearchHealthFacts {
    pub(crate) fn insert_properties(self, properties: &mut Map<String, Value>) {
        insert_count(
            properties,
            "search_retrieval_round_count_bucket",
            self.retrieval_rounds,
        );
        insert_count(
            properties,
            "search_query_execution_count_bucket",
            self.query_executions,
        );
        insert_count(
            properties,
            "search_candidate_rows_total_bucket",
            self.candidate_rows,
        );
        insert_count(
            properties,
            "search_candidate_records_decoded_bucket",
            self.records_decoded,
        );
        if let Some(value) = self.encoded_core_bytes_decoded {
            properties.insert(
                "search_candidate_core_bytes_decoded_bucket".to_owned(),
                json!(bytes_bucket(value).as_str()),
            );
        }
        insert_count(
            properties,
            "search_final_candidate_pool_bucket",
            self.final_candidate_pool,
        );
        if let Some(value) = self.candidate_pool_truncated {
            properties.insert("search_candidate_pool_truncated".to_owned(), json!(value));
        }
        if let Some(concentration) = self.concentration {
            insert_count(
                properties,
                "search_candidate_session_count_bucket",
                Some(u64::from(concentration.candidate_sessions)),
            );
            insert_share(
                properties,
                "search_largest_session_candidate_share_bucket",
                Some(u64::from(concentration.largest_session_candidate_count)),
                self.final_candidate_pool,
            );
            let (availability, candidate_families, candidate_count, largest_family_count) =
                match concentration.literal_roots {
                    SearchLiteralRootFacts::Observed {
                        candidate_families,
                        candidate_count,
                        largest_family_candidate_count,
                    } => (
                        SearchLiteralRootAvailability::Observed,
                        Some(u64::from(candidate_families)),
                        Some(u64::from(candidate_count)),
                        Some(u64::from(largest_family_candidate_count)),
                    ),
                    SearchLiteralRootFacts::NotObservedDense => (
                        SearchLiteralRootAvailability::NotObservedDense,
                        None,
                        None,
                        None,
                    ),
                };
            properties.insert(
                "search_literal_root_concentration_availability".to_owned(),
                json!(availability.as_str()),
            );
            insert_count(
                properties,
                "search_candidate_literal_root_family_count_bucket",
                candidate_families,
            );
            insert_share(
                properties,
                "search_literal_root_candidate_coverage_bucket",
                candidate_count,
                self.final_candidate_pool,
            );
            insert_share(
                properties,
                "search_largest_literal_root_candidate_share_bucket",
                largest_family_count,
                self.final_candidate_pool,
            );
            insert_count(
                properties,
                "search_provider_copy_candidate_count_bucket",
                Some(u64::from(concentration.provider_copy_candidate_count)),
            );
            insert_share(
                properties,
                "search_provider_copy_candidate_share_bucket",
                Some(u64::from(concentration.provider_copy_candidate_count)),
                self.final_candidate_pool,
            );
            properties.insert(
                "search_copy_cluster_availability".to_owned(),
                json!(concentration.copy_cluster_availability.as_str()),
            );
            properties.insert(
                "search_diversification_status".to_owned(),
                json!(concentration.diversification_status.as_str()),
            );
            if let Some(value) = concentration.diversification_changed_final_top_n {
                properties.insert(
                    "search_diversification_changed_final_top_n".to_owned(),
                    json!(value),
                );
            }
        }
        if let Some(value) = self.stop_reason {
            properties.insert("search_stop_reason".to_owned(), json!(value.as_str()));
        }
        if let Some(value) = self.failure_phase {
            properties.insert("search_failure_phase".to_owned(), json!(value.as_str()));
        }
    }
}

fn insert_count(properties: &mut Map<String, Value>, name: &str, value: Option<u64>) {
    if let Some(value) = value {
        properties.insert(name.to_owned(), json!(count_bucket(value).as_str()));
    }
}

fn insert_share(
    properties: &mut Map<String, Value>,
    name: &str,
    numerator: Option<u64>,
    denominator: Option<u64>,
) {
    let (Some(numerator), Some(denominator)) = (numerator, denominator) else {
        return;
    };
    let Some(bucket) = share_bucket(numerator, denominator) else {
        return;
    };
    properties.insert(name.to_owned(), json!(bucket));
}

fn share_bucket(numerator: u64, denominator: u64) -> Option<&'static str> {
    if numerator > denominator {
        return None;
    }
    if denominator == 0 {
        return (numerator == 0).then_some("not_applicable");
    }
    if numerator == 0 {
        return Some("0");
    }
    let percent = (u128::from(numerator) * 100) / u128::from(denominator);
    Some(match percent {
        0..=25 => "1-25pct",
        26..=50 => "26-50pct",
        51..=75 => "51-75pct",
        76..=99 => "76-99pct",
        _ => "100pct",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_health_facts_are_bucketed_only_at_serialization() {
        let health = SearchHealthFacts {
            candidate_rows: Some(21),
            encoded_core_bytes_decoded: Some(102_400),
            final_candidate_pool: Some(2),
            concentration: Some(SearchConcentrationFacts {
                candidate_sessions: 1,
                largest_session_candidate_count: 2,
                literal_roots: SearchLiteralRootFacts::Observed {
                    candidate_families: 1,
                    candidate_count: 1,
                    largest_family_candidate_count: 1,
                },
                provider_copy_candidate_count: 1,
                copy_cluster_availability: SearchCopyClusterAvailability::NotConstructedV1,
                diversification_status: SearchDiversificationStatus::Applied,
                diversification_changed_final_top_n: Some(true),
            }),
            ..SearchHealthFacts::default()
        };
        let mut properties = Map::new();
        health.insert_properties(&mut properties);

        assert_eq!(properties["search_candidate_rows_total_bucket"], "21-100");
        assert_eq!(
            properties["search_candidate_core_bytes_decoded_bucket"],
            "100kb-1mb"
        );
        assert_eq!(properties["search_final_candidate_pool_bucket"], "2-5");
        assert_eq!(
            properties["search_largest_session_candidate_share_bucket"],
            "100pct"
        );
        assert_eq!(
            properties["search_literal_root_candidate_coverage_bucket"],
            "26-50pct"
        );
        assert_eq!(
            properties["search_provider_copy_candidate_share_bucket"],
            "26-50pct"
        );
        assert_eq!(
            properties["search_copy_cluster_availability"],
            "not_constructed_v1"
        );
        assert_eq!(properties["search_diversification_status"], "applied");
    }

    #[test]
    fn share_buckets_distinguish_empty_zero_and_invalid_receipts() {
        assert_eq!(share_bucket(0, 0), Some("not_applicable"));
        assert_eq!(share_bucket(0, 4), Some("0"));
        assert_eq!(share_bucket(1, 4), Some("1-25pct"));
        assert_eq!(share_bucket(2, 4), Some("26-50pct"));
        assert_eq!(share_bucket(3, 4), Some("51-75pct"));
        assert_eq!(share_bucket(4, 4), Some("100pct"));
        assert_eq!(share_bucket(5, 4), None);
    }
}
