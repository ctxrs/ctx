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
    let result = blame_once(
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
    let served_generation = match &result.snapshot {
        QuerySnapshotExpectation::Core { receipt } => &receipt.core_generation_id,
    };
    let freshness = if served_generation == &active_before && served_generation == &active_after {
        BlameResultFreshness::Current
    } else {
        BlameResultFreshness::StaleCommitted
    };
    if freshness == BlameResultFreshness::StaleCommitted
        && (result.outcome.attribution == ctx_pro_host_protocol::BlameAttribution::None
            || result.outcome.coverage.none > 0)
    {
        bail!(
            "stale_source: the committed Pro generation cannot prove an absent producer while Core is newer"
        );
    }
    Ok(HostedBlameResult { result, freshness })
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

pub(super) fn blame_once(
    data_root: &Path,
    target: BlameTarget,
    limit: u32,
    cursor: Option<String>,
    expected_active_core_generation_id: Option<&str>,
) -> Result<BlameResult> {
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
            validate_blame_response(&request_context, &result)?;
            result
        }
        HelperMessage::Error(error) => return Err(protocol_error(error)),
        _ => bail!("invalid_response: helper returned a non-blame response"),
    };
    let status_after = helper_status(&mut client)?;
    ensure_committed_pro_receipt_is_unchanged(&expected_receipt, &status_after)?;
    Ok(result)
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
