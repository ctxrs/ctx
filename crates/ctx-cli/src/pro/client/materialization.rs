use super::*;

pub(crate) fn materialize(
    data_root: &Path,
    telemetry: &mut ProMaterializationTelemetryV1,
) -> Result<MaterializeReport> {
    let result = materialize_once(data_root, telemetry)
        .and_then(|report| super::super::pending_materialization::clear_after(data_root, report));
    if let Err(error) = &result {
        telemetry.fail(stable_error_code(error));
    }
    result
}

fn materialize_once(
    data_root: &Path,
    telemetry: &mut ProMaterializationTelemetryV1,
) -> Result<MaterializeReport> {
    let required = BTreeSet::from([Capability::Status]);
    let prior_generation = match ProClient::connect(data_root, &required) {
        Ok(mut client) => {
            telemetry.helper_connection = ProHelperConnectionOutcomeV1::Connected;
            helper_status(&mut client)?
                .source_receipt
                .map(|receipt| receipt.core_generation_id)
        }
        Err(error) => {
            telemetry.helper_connection = pro_helper_connection_outcome(stable_error_code(&error));
            return Err(error);
        }
    };

    let config = crate::config::AppConfig::load(data_root)
        .context("source_unavailable: load daemon configuration for Pro materialization")?;
    if config.daemon.mode.runs_only_source_refresh() {
        bail!("not_materialized: source-refresh-only daemon mode excludes Pro materialization");
    }
    crate::semantic::autostart_daemon_and_wait(
        data_root,
        &config,
        crate::DaemonTriggerCommandArg::Setup,
    )
    .context("source_unavailable: start daemon-owned source materialization")?;
    let refresh = crate::semantic::coordinate_source_backed_refresh(
        data_root,
        crate::semantic::SourceBackedRefreshMode::Wait,
    )
    .context("source_unavailable: publish provider sources before Pro materialization")?;
    let core_generation_id = refresh.pin.generation_id().to_owned();

    crate::semantic::wait_for_source_backed_pro_generation(data_root, &core_generation_id)?;

    let mut client = match ProClient::connect(data_root, &required) {
        Ok(client) => {
            telemetry.helper_connection = ProHelperConnectionOutcomeV1::Connected;
            client
        }
        Err(error) => {
            telemetry.helper_connection = pro_helper_connection_outcome(stable_error_code(&error));
            return Err(error);
        }
    };
    let status = helper_status(&mut client)?;
    if status.authority != ctx_pro_host_protocol::MaterializationAuthority::Source {
        bail!("not_materialized: Pro helper did not activate v0.26 source-manifest authority");
    }
    let receipt = status.source_receipt.ok_or_else(|| {
        anyhow!("not_materialized: Pro helper has no completed source-manifest receipt")
    })?;
    if receipt.core_generation_id != core_generation_id {
        bail!(
            "stale_source: Pro helper generation {} does not match Core generation {}",
            receipt.core_generation_id,
            core_generation_id
        );
    }

    let source_count = u64::try_from(receipt.progress.len()).unwrap_or(u64::MAX);
    let no_op = prior_generation.as_deref() == Some(core_generation_id.as_str());
    telemetry.mode = Some(if no_op {
        ProMaterializationModeV1::NoOp
    } else if prior_generation.is_none() {
        ProMaterializationModeV1::Full
    } else {
        ProMaterializationModeV1::Incremental
    });
    telemetry.complete(
        u64::from(!no_op),
        if no_op { 0 } else { source_count },
        if no_op { 0 } else { source_count },
        0,
        if no_op { 0 } else { source_count },
    );

    Ok(MaterializeReport {
        schema_version: 1,
        payload_type: "pro_source_materialization",
        core_generation_id,
        source_count,
        batches: u64::from(!no_op),
        observations: source_count,
        replayed_batches: 0,
    })
}

pub(super) fn required_blame_capabilities(target: &BlameTarget) -> BTreeSet<Capability> {
    let mut capabilities = BTreeSet::from([Capability::Status, Capability::Query]);
    if target.requires_git_read() {
        capabilities.insert(Capability::GitRead);
    }
    capabilities
}

pub(super) fn helper_status(client: &mut ProClient) -> Result<ctx_pro_host_protocol::StatusResult> {
    helper_status_with(&mut |message, timeout| client.exchange(message, timeout))
}

pub(super) fn helper_status_with(
    exchange: &mut impl FnMut(HostMessage, Duration) -> Result<HelperMessage>,
) -> Result<StatusResult> {
    match exchange(HostMessage::Status(StatusRequest {}), HANDSHAKE_TIMEOUT)? {
        HelperMessage::Status(status) => {
            status
                .validate()
                .map_err(|error| anyhow!("invalid_response: {}", error.message))?;
            Ok(status)
        }
        HelperMessage::Error(error) => Err(protocol_error(error)),
        _ => bail!("invalid_response: helper returned a non-status response"),
    }
}
