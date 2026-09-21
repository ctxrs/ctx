//! Retained snapshot policy: stale positives may be useful; negatives cannot
//! establish absence in a newer Core generation.
use ctx_attribution_model::*;

pub(super) fn complete(
    result: Result<BlameResult, BlameDiagnostic>,
    served_generation: &str,
    active_before: Option<&str>,
    active_after: Option<&str>,
) -> Result<HostedBlameResult, BlameDiagnostic> {
    let current =
        active_before == Some(served_generation) && active_after == Some(served_generation);
    match result {
        Ok(result) => {
            let freshness = classify(&result.outcome, current)?;
            Ok(HostedBlameResult { result, freshness })
        }
        Err(error) if !current && is_generation_bound_negative(&error) => Err(stale_negative()),
        Err(error) => Err(error),
    }
}

fn classify(
    outcome: &BlameOutcome,
    current: bool,
) -> Result<BlameResultFreshness, BlameDiagnostic> {
    if current {
        return Ok(BlameResultFreshness::Current);
    }
    if outcome.attribution == BlameAttribution::None || outcome.coverage.none > 0 {
        return Err(stale_negative());
    }
    Ok(BlameResultFreshness::StaleCommitted)
}

fn stale_negative() -> BlameDiagnostic {
    let mut diagnostic = crate::diagnostic::projection_diagnostic(CoreProjectionCurrentness::Stale)
        .expect("stale projection has a diagnostic");
    diagnostic.freshness = Some(BlameDiagnosticFreshness {
        state: BlameFreshnessState::StaleCommitted,
    });
    diagnostic
}

fn is_generation_bound_negative(error: &BlameDiagnostic) -> bool {
    use BlameDiagnosticReason::*;
    matches!(
        error.reason,
        TargetNotIndexed
            | RepositorySelectorNotIndexed
            | RepositoryNotBound
            | RepositoryAmbiguous
            | TargetOrRepositoryAmbiguous
            | TargetAmbiguous
            | CommitRewriteAmbiguous
            | OperationNotCovered
            | FileBlameNotCovered
            | CommitBlameNotCovered
            | PullRequestBlameNotCovered
    )
}

#[cfg(test)]
mod tests;
