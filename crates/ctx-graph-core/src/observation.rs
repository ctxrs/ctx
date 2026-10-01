//! Typed projections, with no history, telemetry configuration or delivery dependency.
pub use ctx_graph_types::observation::*;

pub fn search(facts: &mut GraphObservation, result: &crate::query::SearchResult) {
    facts.graph(&result.graph);
    facts.seeds = Some(result.seeds.len() as u64);
    facts.estimated_json_tokens = Some(result.estimated_tokens as u64);
    let mut bounds = GraphBounds::default();
    for reason in &result.truncation_reasons {
        match reason.as_str() {
            "seed_limit" => bounds.seed = true,
            "node_limit" => bounds.node = true,
            "work_limit" => bounds.work = true,
            "depth_limit" => bounds.depth = true,
            "unresolved_limit" => bounds.unresolved = true,
            "token_budget" => bounds.token = true,
            _ => bounds.other = true,
        }
    }
    facts.bounds = Some(bounds);
}

pub fn path(facts: &mut GraphObservation, result: &crate::query::PathSearchResult) {
    search(facts, &result.result);
    facts.path = Some(if result.found {
        GraphPathDisposition::Found
    } else if result.result.graph.truncated {
        GraphPathDisposition::Incomplete
    } else {
        GraphPathDisposition::NotFoundWithinScope
    });
}

pub fn analysis(facts: &mut GraphObservation, report: &crate::analysis::AnalysisReport) {
    facts.nodes = Some(report.nodes.len() as u64);
    facts.communities = Some(report.communities.len() as u64);
    facts.pagerank_converged = Some(report.pagerank_converged);
    facts.community_convergence = Some(if !report.community_convergence_known {
        GraphConvergence::Unknown
    } else if report.community_converged {
        GraphConvergence::Converged
    } else {
        GraphConvergence::NotConverged
    });
    facts.community_passes = Some(report.community_passes as u64);
    facts.unsatisfied_constraints = Some(report.unsatisfied_community_constraints.len() as u64);
}

/// The report's algorithm field is human prose; use the actual typed options.
pub fn analysis_options(facts: &mut GraphObservation, options: &crate::analysis::AnalysisOptions) {
    facts.algorithm = Some(match options.community_algorithm {
        crate::analysis::CommunityAlgorithm::Leiden => GraphAlgorithm::Leiden,
        crate::analysis::CommunityAlgorithm::Louvain => GraphAlgorithm::Louvain,
    });
}

pub fn failure_kind(error: &anyhow::Error) -> GraphFailureKind {
    use GraphFailureKind as K;
    if let Some(error) = error.downcast_ref::<crate::query::QueryFailure>() {
        return match error.kind {
            crate::query::QueryFailureKind::InvalidInput => K::InvalidInput,
            crate::query::QueryFailureKind::EndpointNotFound => K::EndpointNotFound,
            crate::query::QueryFailureKind::EndpointAmbiguous => K::EndpointAmbiguous,
            crate::query::QueryFailureKind::WorkLimit => K::WorkLimit,
        };
    }
    if error.is::<crate::ingest::InputRejected>() {
        return K::InputRejected;
    }
    if error.is::<crate::store::StaleStore>() {
        return K::ConcurrentChange;
    }
    for cause in error.chain() {
        if let Some(error) = cause.downcast_ref::<serde_json::Error>() {
            return match error.io_error_kind() {
                Some(std::io::ErrorKind::BrokenPipe) => K::BrokenPipe,
                Some(std::io::ErrorKind::PermissionDenied) => K::Permission,
                Some(_) => K::Io,
                None => K::Serialize,
            };
        }
        if let Some(error) = cause.downcast_ref::<std::io::Error>() {
            return match error.kind() {
                std::io::ErrorKind::NotFound => K::NotFound,
                std::io::ErrorKind::PermissionDenied => K::Permission,
                std::io::ErrorKind::BrokenPipe => K::BrokenPipe,
                _ => K::Io,
            };
        }
        if let Some(rusqlite::Error::SqliteFailure(code, _)) =
            cause.downcast_ref::<rusqlite::Error>()
        {
            return match code.code {
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked => {
                    K::StoreBusy
                }
                rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase => {
                    K::InvalidStore
                }
                rusqlite::ErrorCode::CannotOpen => K::StoreOpen,
                rusqlite::ErrorCode::OperationInterrupted => K::WorkLimit,
                _ => K::Unknown,
            };
        }
    }

    K::Unknown
}

pub fn failed(facts: &mut GraphObservation, error: &anyhow::Error) {
    if facts.failure.is_none() {
        facts.fail(failure_kind(error));
    }
    if facts.semantic.receipts.is_none()
        && facts.semantic.reserved_generations.is_none()
        && let Some(usage) = error.downcast_ref::<crate::index::FailedSemanticUsage>()
    {
        facts.semantic.record(
            usage.semantic_usage,
            (!usage.usage_unavailable).then_some(usage.provider_usage.as_slice()),
        );
        facts.semantic.usage_unavailable = usage.usage_unavailable;
    }
}

#[cfg(test)]
pub(crate) mod tests;
