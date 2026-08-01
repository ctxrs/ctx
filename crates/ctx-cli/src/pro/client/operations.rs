use super::*;

pub(crate) fn blame(
    data_root: &Path,
    target: BlameTarget,
    limit: u32,
    cursor: Option<String>,
) -> Result<BlameResult> {
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
) -> Result<BlameResult> {
    let expected_active_generation = prepare_blame_freshness(data_root, policy)?;
    let result = blame_once(
        data_root,
        target,
        limit,
        cursor,
        expected_active_generation.as_deref(),
    )?;
    if let Some(expected_generation) = expected_active_generation {
        let active_generation = crate::semantic::pin_active_verified_generation(data_root)?
            .generation_id()
            .to_owned();
        ensure_active_core_generation_is_unchanged(&expected_generation, &active_generation)?;
    }
    Ok(result)
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

pub(in crate::pro) fn delete_graph_key(
    data_root: &Path,
    namespace: CredentialVaultNamespace,
    installation_key_thumbprint: &str,
) -> Result<()> {
    if decode_base64url(installation_key_thumbprint)
        .as_deref()
        .map(<[u8]>::len)
        != Some(32)
    {
        bail!("invalid_request: installation key thumbprint is invalid");
    }
    let required = BTreeSet::from([Capability::GraphKeyDeletion]);
    let mut client = ProClient::connect(data_root, &required)?;
    delete_graph_key_with_client(&mut client, installation_key_thumbprint, |challenge| {
        StoredAuthorizationProvider::load_for_graph_key_deletion(
            data_root,
            namespace,
            installation_key_thumbprint,
        )?
        .authorization_for_challenge(challenge)
    })
}

pub(super) fn delete_graph_key_with_client(
    client: &mut ProClient,
    installation_key_thumbprint: &str,
    authorize: impl FnOnce(
        &[u8; GRAPH_KEY_DELETION_CHALLENGE_BYTES],
    ) -> Result<ctx_pro_host_protocol::AuthorizationRequest>,
) -> Result<()> {
    let prepared = prepare_graph_key_deletion(client, installation_key_thumbprint)?;
    if !prepared.key_present {
        return Ok(());
    }
    let challenge = graph_key_deletion_challenge(&prepared)?;
    let authorization = authorize(&challenge)?;
    match client.exchange(
        HostMessage::ConfirmGraphKeyDeletion(ConfirmGraphKeyDeletionRequest { authorization }),
        HANDSHAKE_TIMEOUT,
    )? {
        HelperMessage::GraphKeyDeleted(_) => {}
        HelperMessage::Error(error) => return Err(protocol_error(error)),
        _ => bail!("invalid_response: helper returned a non-deletion response"),
    }
    let verified = prepare_graph_key_deletion(client, installation_key_thumbprint)?;
    if verified.key_present {
        bail!("key_store_unavailable: graph-key deletion could not be verified");
    }
    Ok(())
}

pub(super) fn prepare_graph_key_deletion(
    client: &mut ProClient,
    installation_key_thumbprint: &str,
) -> Result<GraphKeyDeletionPrepared> {
    let prepared = match client.exchange(
        HostMessage::PrepareGraphKeyDeletion(PrepareGraphKeyDeletionRequest {
            installation_key_thumbprint: installation_key_thumbprint.to_owned(),
        }),
        HANDSHAKE_TIMEOUT,
    )? {
        HelperMessage::GraphKeyDeletionPrepared(prepared) => prepared,
        HelperMessage::Error(error) => return Err(protocol_error(error)),
        _ => bail!("invalid_response: helper returned a non-deletion-preparation response"),
    };
    let _ = graph_key_deletion_challenge(&prepared)?;
    Ok(prepared)
}

pub(super) fn graph_key_deletion_challenge(
    prepared: &GraphKeyDeletionPrepared,
) -> Result<[u8; GRAPH_KEY_DELETION_CHALLENGE_BYTES]> {
    decode_base64url(&prepared.challenge_base64url)
        .and_then(|challenge| challenge.try_into().ok())
        .ok_or_else(|| anyhow!("invalid_response: helper returned an invalid deletion challenge"))
}
