use super::*;

pub(super) fn durable_build_rearmed_automatic_retry_routes(
    job: &Value,
) -> Result<BTreeSet<SourceRouteIdentity>> {
    let mut jobs = vec![job];
    if let Some(successors) = job.get("queued_successors") {
        jobs.extend(
            successors
                .as_array()
                .ok_or_else(|| anyhow!("durable source refresh successors must be an array"))?,
        );
    }
    let mut rearmed = BTreeSet::new();
    for job in jobs {
        rearmed.extend(
            recover_automatic_retry_checkpoints(job)?
                .into_iter()
                .filter(|(_, checkpoint)| checkpoint.build_version != SOURCE_REFRESH_BUILD_VERSION)
                .map(|(route, _)| route),
        );
    }
    Ok(rearmed)
}

pub(in crate::engine) fn recover_automatic_retry_checkpoints(
    job: &Value,
) -> Result<BTreeMap<SourceRouteIdentity, SourceBackedAutomaticRetryCheckpoint>> {
    let Some(value) = job.get("automatic_retry") else {
        return Ok(BTreeMap::new());
    };
    let fields = value
        .as_object()
        .ok_or_else(|| anyhow!("durable source refresh automatic retry state is not an object"))?;
    let aggregate_state = required_outcome_text(fields, "state")?;
    if !matches!(aggregate_state, "confirming" | "paused" | "mixed") {
        bail!("durable source refresh automatic retry state is invalid");
    }
    let reason = required_outcome_text(fields, "reason")?;
    if !matches!(
        reason,
        "internal_failure_confirmation" | "repeated_internal_failure"
    ) {
        bail!("durable source refresh automatic retry reason is invalid");
    }
    if fields.get("confirmation_limit").and_then(Value::as_u64)
        != Some(u64::from(SOURCE_REFRESH_AUTOMATIC_RETRY_CONFIRMATION_LIMIT))
    {
        bail!("durable source refresh automatic retry confirmation limit is invalid");
    }
    let resume_on = fields
        .get("resume_on")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            anyhow!("durable source refresh automatic retry resume policy is invalid")
        })?;
    let resume_on = resume_on.iter().map(Value::as_str).collect::<Vec<_>>();
    if resume_on
        != [
            Some("source_change"),
            Some("ctx_upgrade"),
            Some("manual_import"),
        ]
    {
        bail!("durable source refresh automatic retry resume policy is invalid");
    }
    let routes = fields
        .get("routes")
        .and_then(Value::as_object)
        .filter(|routes| !routes.is_empty() && routes.len() <= SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT)
        .ok_or_else(|| anyhow!("durable source refresh automatic retry routes are invalid"))?;
    let mut recovered = BTreeMap::new();
    for (route, value) in routes {
        let route = SourceRouteIdentity::from_sha256(route.clone())
            .map_err(|_| anyhow!("durable source refresh automatic retry route is invalid"))?;
        let checkpoint = value.as_object().ok_or_else(|| {
            anyhow!("durable source refresh automatic retry checkpoint is invalid")
        })?;
        let (state, matching_failures) = match required_outcome_text(checkpoint, "state")? {
            "confirming" => (SourceBackedAutomaticRetryState::Confirming, 1),
            "paused" => (
                SourceBackedAutomaticRetryState::Paused,
                SOURCE_REFRESH_AUTOMATIC_RETRY_CONFIRMATION_LIMIT,
            ),
            _ => bail!("durable source refresh automatic retry checkpoint state is invalid"),
        };
        if checkpoint.get("matching_failures").and_then(Value::as_u64)
            != Some(u64::from(matching_failures))
        {
            bail!("durable source refresh automatic retry failure count is invalid");
        }
        let source_observation = required_outcome_text(checkpoint, "source_observation")?;
        let failure_fingerprint = required_outcome_text(checkpoint, "failure_fingerprint")?;
        let build_version = required_outcome_text(checkpoint, "build_version")?;
        if !is_sha256_identity(source_observation)
            || !is_sha256_identity(failure_fingerprint)
            || build_version.len() > 128
        {
            bail!("durable source refresh automatic retry checkpoint identity is invalid");
        }
        recovered.insert(
            route,
            SourceBackedAutomaticRetryCheckpoint {
                state,
                matching_failures,
                source_observation: source_observation.to_owned(),
                failure_fingerprint: failure_fingerprint.to_owned(),
                build_version: build_version.to_owned(),
            },
        );
    }
    let confirming = recovered
        .values()
        .any(|checkpoint| checkpoint.state == SourceBackedAutomaticRetryState::Confirming);
    let paused = recovered
        .values()
        .any(|checkpoint| checkpoint.state == SourceBackedAutomaticRetryState::Paused);
    let expected_state = match (confirming, paused) {
        (true, true) => "mixed",
        (true, false) => "confirming",
        (false, true) => "paused",
        (false, false) => unreachable!("nonempty durable automatic retry routes"),
    };
    let expected_reason = if paused {
        "repeated_internal_failure"
    } else {
        "internal_failure_confirmation"
    };
    if aggregate_state != expected_state || reason != expected_reason {
        bail!("durable source refresh automatic retry aggregate is inconsistent");
    }
    Ok(recovered)
}
