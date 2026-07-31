use super::*;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(super) enum BlameFreshnessPolicy {
    LatestCommitted,
    WaitForCurrent,
}

pub(super) const DEFAULT_BLAME_FRESHNESS_POLICY: BlameFreshnessPolicy =
    BlameFreshnessPolicy::LatestCommitted;

pub(super) fn prepare_blame_generation(
    data_root: &Path,
    policy: BlameFreshnessPolicy,
) -> Result<String> {
    prepare_blame_generation_with(
        policy,
        || latest_committed_blame_generation(data_root),
        || wait_for_current_blame_generation(data_root),
    )
}

pub(super) fn prepare_blame_generation_with(
    policy: BlameFreshnessPolicy,
    latest_committed: impl FnOnce() -> Result<String>,
    wait_for_current: impl FnOnce() -> Result<String>,
) -> Result<String> {
    match policy {
        BlameFreshnessPolicy::LatestCommitted => latest_committed(),
        BlameFreshnessPolicy::WaitForCurrent => wait_for_current(),
    }
}

fn latest_committed_blame_generation(data_root: &Path) -> Result<String> {
    latest_committed_blame_generation_with(
        || {
            crate::semantic::coordinate_source_backed_refresh(
                data_root,
                crate::semantic::SourceBackedRefreshMode::Background,
            )
            .map(|_| ())
        },
        || {
            Ok(crate::semantic::pin_active_verified_generation(data_root)?
                .generation_id()
                .to_owned())
        },
    )
}

pub(super) fn latest_committed_blame_generation_with(
    wake_daemon: impl FnOnce() -> Result<()>,
    pin_active_generation: impl FnOnce() -> Result<String>,
) -> Result<String> {
    // Waking the daemon is best effort. Its availability must not hide an
    // independently valid committed Core generation from the reader.
    let wake_error = wake_daemon().err();
    match pin_active_generation() {
        Ok(generation) => Ok(generation),
        Err(pin_error) => match wake_error {
            Some(wake_error) => Err(pin_error).context(format!(
                "source_unavailable: bounded daemon wake failed before reading the latest committed Core generation: {wake_error:#}"
            )),
            None => Err(pin_error),
        },
    }
}

fn wait_for_current_blame_generation(data_root: &Path) -> Result<String> {
    let mut materialization = ProMaterializationTelemetryV1::started();
    materialize(data_root, &mut materialization)?;

    let mut expected = crate::semantic::pin_active_verified_generation(data_root)?
        .generation_id()
        .to_owned();
    for _ in 0..3 {
        crate::semantic::wait_for_source_backed_pro_generation(data_root, &expected)?;
        let active = crate::semantic::pin_active_verified_generation(data_root)?
            .generation_id()
            .to_owned();
        if active == expected {
            return Ok(expected);
        }
        expected = active;
    }
    bail!("stale_source: active verified Core generation advanced repeatedly while preparing blame")
}

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
    // Status is part of the materialization authority decision, so bind it to
    // the stored installation identity exactly like the public status path.
    // A capability-only Status connection is intentionally unauthenticated
    // and helpers must reject it for an initialized graph.
    let prior_generation = match ProClient::connect_for_status(data_root, &required) {
        Ok(mut client) => {
            telemetry.helper_connection = ProHelperConnectionOutcomeV1::Connected;
            helper_status(&mut client)?
                .core_receipt
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

    let mut client = match ProClient::connect_for_status(data_root, &required) {
        Ok(client) => {
            telemetry.helper_connection = ProHelperConnectionOutcomeV1::Connected;
            client
        }
        Err(error) => {
            telemetry.helper_connection = pro_helper_connection_outcome(stable_error_code(&error));
            return Err(error);
        }
    };
    let status = helper_status_for(&mut client, Some(core_generation_id.clone()))?;
    if status.currentness != ctx_pro_host_protocol::CoreProjectionCurrentness::Current {
        bail!("not_materialized: Pro Core projection is not current");
    }
    let receipt = status.core_receipt.ok_or_else(|| {
        anyhow!("not_materialized: Pro helper has no completed Core materialization receipt")
    })?;
    if receipt.core_generation_id != core_generation_id {
        bail!(
            "stale_source: Pro helper generation {} does not match Core generation {}",
            receipt.core_generation_id,
            core_generation_id
        );
    }

    let source_count = u64::from(receipt.source_count);
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
        payload_type: "pro_core_materialization",
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
    helper_status_for(client, None)
}

fn helper_status_for(
    client: &mut ProClient,
    requested_core_generation_id: Option<String>,
) -> Result<ctx_pro_host_protocol::StatusResult> {
    helper_status_with(requested_core_generation_id, &mut |message, timeout| {
        client.exchange(message, timeout)
    })
}

pub(super) fn helper_status_with(
    requested_core_generation_id: Option<String>,
    exchange: &mut impl FnMut(HostMessage, Duration) -> Result<HelperMessage>,
) -> Result<StatusResult> {
    match exchange(
        HostMessage::Status(StatusRequest {
            requested_core_generation_id,
        }),
        HANDSHAKE_TIMEOUT,
    )? {
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
