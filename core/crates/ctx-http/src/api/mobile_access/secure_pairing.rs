use super::*;

pub(in crate::api) async fn pair_mobile_device(
    State(state): State<MobileStoreHandle>,
    body: Bytes,
) -> Result<Json<SecureEnvelope>, (StatusCode, Json<ApiErrorResp>)> {
    let req: PairMobileDeviceReq = parse_json_body(body)?;
    let envelope = state
        .pair_mobile_device_for_route(PairMobileDeviceRequest {
            device_id: req.device_id,
            public_key: req.public_key,
            seq: req.seq,
            nonce: req.nonce,
            ciphertext: req.ciphertext,
        })
        .await
        .map_err(mobile_access_api_error)?;

    Ok(Json(SecureEnvelope {
        device_id: envelope.device_id,
        seq: envelope.seq,
        nonce: envelope.nonce,
        ciphertext: envelope.ciphertext,
    }))
}
