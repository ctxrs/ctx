use super::*;

pub(crate) fn blame(
    data_root: &Path,
    target: BlameTarget,
    limit: u32,
    cursor: Option<String>,
) -> Result<BlameResult> {
    let first = blame_once(data_root, target.clone(), limit, cursor.clone());
    let should_catch_up = first.as_ref().is_err_and(|error| {
        matches!(
            stable_error_code(error),
            Some("not_materialized" | "needs_rebuild" | "partial" | "needs_resume")
        )
    });
    if !should_catch_up {
        return first;
    }
    let mut materialization = ProMaterializationTelemetryV1::started();
    materialize(data_root, &mut materialization)?;
    blame_once(data_root, target, limit, cursor)
}

pub(super) fn blame_once(
    data_root: &Path,
    target: BlameTarget,
    limit: u32,
    cursor: Option<String>,
) -> Result<BlameResult> {
    let capabilities = required_blame_capabilities(&target);
    let mut client = ProClient::connect(data_root, &capabilities)?;
    let status = helper_status(&mut client)?;
    let request = support::current_blame_request(target, limit, cursor, &status)?;
    request
        .validate()
        .map_err(|error| anyhow!("invalid_request: {}", error.message))?;
    let request_context = request.clone();
    match client.exchange(HostMessage::Blame(request), BLAME_TIMEOUT)? {
        HelperMessage::Blame(result) => {
            validate_blame_response(&request_context, &result)?;
            Ok(result)
        }
        HelperMessage::Error(error) => Err(protocol_error(error)),
        _ => bail!("invalid_response: helper returned a non-blame response"),
    }
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
