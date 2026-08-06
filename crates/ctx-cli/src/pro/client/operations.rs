use super::*;

pub(crate) fn blame(
    data_root: &Path,
    target: BlameTarget,
    limit: u32,
    cursor: Option<String>,
) -> Result<HostedBlameResult> {
    blame_with_policy(
        data_root,
        target,
        limit,
        cursor,
        DEFAULT_BLAME_FRESHNESS_POLICY,
    )
}

pub(super) fn blame_with_policy(
    data_root: &Path,
    target: BlameTarget,
    limit: u32,
    cursor: Option<String>,
    policy: BlameFreshnessPolicy,
) -> Result<HostedBlameResult> {
    let expected_active_generation = prepare_blame_freshness(data_root, policy)?;
    let active_before = expected_active_generation.clone().map_or_else(
        || {
            crate::semantic::pin_active_verified_generation(data_root)
                .map(|pin| pin.generation_id().to_owned())
        },
        Ok,
    )?;
    let attempt = blame_once(
        data_root,
        target,
        limit,
        cursor,
        expected_active_generation.as_deref(),
    )?;
    let active_after = crate::semantic::pin_active_verified_generation(data_root)?
        .generation_id()
        .to_owned();
    if let Some(expected_generation) = expected_active_generation {
        ensure_active_core_generation_is_unchanged(&expected_generation, &active_after)?;
    }
    let current =
        attempt.served_generation == active_before && attempt.served_generation == active_after;
    match attempt.result {
        Ok(result) => {
            let freshness = classify_blame_freshness(&result, &active_before, &active_after)?;
            Ok(HostedBlameResult { result, freshness })
        }
        Err(error) if !current && is_generation_bound_negative(&error) => {
            Err(stale_negative_diagnostic())
        }
        Err(error) => Err(error),
    }
}

pub(super) fn classify_blame_freshness(
    result: &BlameResult,
    active_before: &str,
    active_after: &str,
) -> Result<BlameResultFreshness> {
    let served_generation = match &result.snapshot {
        QuerySnapshotExpectation::Core { receipt } => &receipt.core_generation_id,
    };
    classify_blame_snapshot_freshness(
        served_generation,
        &result.outcome,
        active_before,
        active_after,
    )
}

pub(super) fn classify_blame_snapshot_freshness(
    served_generation: &str,
    outcome: &ctx_pro_host_protocol::BlameOutcome,
    active_before: &str,
    active_after: &str,
) -> Result<BlameResultFreshness> {
    if served_generation == active_before && served_generation == active_after {
        return Ok(BlameResultFreshness::Current);
    }
    if outcome.attribution == ctx_pro_host_protocol::BlameAttribution::None
        || outcome.coverage.none > 0
    {
        bail!(
            "stale_source: the committed Pro generation cannot prove an absent producer while Core is newer"
        );
    }
    Ok(BlameResultFreshness::StaleCommitted)
}

pub(super) fn ensure_active_core_generation_is_unchanged(
    expected_generation: &str,
    active_generation: &str,
) -> Result<()> {
    if expected_generation != active_generation {
        bail!(
            "stale_source: Core generation advanced from {} to {} while blame was running",
            expected_generation,
            active_generation
        );
    }
    Ok(())
}

struct BlameAttempt {
    served_generation: String,
    result: Result<BlameResult>,
}

fn blame_once(
    data_root: &Path,
    target: BlameTarget,
    limit: u32,
    cursor: Option<String>,
    expected_active_core_generation_id: Option<&str>,
) -> Result<BlameAttempt> {
    let capabilities = required_blame_capabilities(&target);
    let mut client = ProClient::connect(data_root, &capabilities)?;
    let status = helper_status(&mut client)?;
    let expected_core_generation_id =
        blame_core_generation(&status, expected_active_core_generation_id)?;
    let expected_receipt = status.core_receipt.clone().ok_or_else(|| {
        anyhow!("not_materialized: Pro helper has no completed Core materialization receipt")
    })?;
    let request = support::current_blame_request(
        target,
        limit,
        cursor,
        &status,
        &expected_core_generation_id,
    )?;
    request
        .validate()
        .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
    let request_context = request.clone();
    let result = match client.exchange(HostMessage::Blame(request), BLAME_TIMEOUT)? {
        HelperMessage::Blame(result) => {
            validate_blame_response(&request_context, &result).map(|()| result)
        }
        HelperMessage::Error(error) => Err(protocol_blame_error(error, &request_context.target)),
        _ => Err(anyhow!(
            "invalid_response: helper returned a non-blame response"
        )),
    };
    let status_after = helper_status(&mut client)?;
    ensure_committed_pro_receipt_is_unchanged(&expected_receipt, &status_after)?;
    Ok(BlameAttempt {
        served_generation: expected_receipt.core_generation_id,
        result,
    })
}

pub(super) fn is_generation_bound_negative(error: &anyhow::Error) -> bool {
    use crate::pro::diagnostic::BlameDiagnosticReason;

    crate::pro::blame_diagnostic(error).is_some_and(|diagnostic| {
        matches!(
            diagnostic.reason,
            BlameDiagnosticReason::TargetNotIndexed
                | BlameDiagnosticReason::RepositorySelectorNotIndexed
                | BlameDiagnosticReason::RepositoryNotBound
                | BlameDiagnosticReason::RepositoryAmbiguous
                | BlameDiagnosticReason::TargetOrRepositoryAmbiguous
                | BlameDiagnosticReason::TargetAmbiguous
                | BlameDiagnosticReason::CommitRewriteAmbiguous
                | BlameDiagnosticReason::OperationNotCovered
                | BlameDiagnosticReason::FileBlameNotCovered
                | BlameDiagnosticReason::CommitBlameNotCovered
                | BlameDiagnosticReason::PullRequestBlameNotCovered
        )
    })
}

pub(super) fn stale_negative_diagnostic() -> anyhow::Error {
    anyhow::Error::new(
        crate::pro::diagnostic::BlameDiagnostic::for_stable_error_code("stale_source")
            .expect("stale_source is a stable Pro blame diagnostic"),
    )
}

pub(super) fn blame_core_generation(
    status: &ctx_pro_host_protocol::StatusResult,
    expected_active_core_generation_id: Option<&str>,
) -> Result<String> {
    if let Some(generation) = expected_active_core_generation_id {
        return Ok(generation.to_owned());
    }
    status
        .core_receipt
        .as_ref()
        .map(|receipt| receipt.core_generation_id.clone())
        .ok_or_else(|| {
            anyhow!("not_materialized: Pro helper has no completed Core materialization receipt")
        })
}

pub(super) fn ensure_committed_pro_receipt_is_unchanged(
    expected_receipt: &ctx_pro_host_protocol::CoreMaterializationReceipt,
    status: &ctx_pro_host_protocol::StatusResult,
) -> Result<()> {
    status
        .validate()
        .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
    let actual_receipt = status
        .core_receipt
        .as_ref()
        .ok_or_else(|| anyhow!("stale_source: Pro helper has no committed receipt after blame"))?;
    if actual_receipt != expected_receipt {
        bail!(
            "stale_source: Pro helper committed receipt changed from generation {} to {} while blame was running",
            expected_receipt.core_generation_id,
            actual_receipt.core_generation_id
        );
    }
    Ok(())
}

pub(super) fn validate_blame_response(
    request: &ctx_pro_host_protocol::BlameRequest,
    result: &BlameResult,
) -> Result<()> {
    result
        .validate_for_request(request)
        .map_err(|error| anyhow!("invalid_response: {}", error.message))
}
