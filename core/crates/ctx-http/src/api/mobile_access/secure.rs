use super::*;

pub(in crate::api) async fn handle_mobile_secure(
    State(state): State<MobileStoreHandle>,
    State(proxy): State<MobileSecureProxyHandle>,
    body: Bytes,
) -> Result<Json<SecureEnvelope>, (StatusCode, Json<ApiErrorResp>)> {
    let req: MobileSecureEnvelope = parse_json_body(body)?;
    let verified = state
        .open_mobile_secure_request_for_route(MobileSecureEnvelopeForRoute {
            device_id: req.device_id,
            seq: req.seq,
            nonce: req.nonce,
            ciphertext: req.ciphertext,
        })
        .await
        .map_err(mobile_access_api_error)?;

    let response_payload = proxy
        .proxy_mobile_secure_request_for_route(
            verified.mobile_auth,
            verified.payload,
            env!("CARGO_PKG_VERSION"),
        )
        .await
        .map_err(mobile_access_api_error)?;

    let envelope = state
        .encrypt_mobile_secure_response_for_route(verified.response_encryption, response_payload)
        .await
        .map_err(mobile_access_api_error)?;

    Ok(Json(SecureEnvelope {
        device_id: envelope.device_id,
        seq: envelope.seq,
        nonce: envelope.nonce,
        ciphertext: envelope.ciphertext,
    }))
}
