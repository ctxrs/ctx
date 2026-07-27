use super::*;

pub(super) fn reject(
    failures: &mut Vec<ProviderImportFailure>,
    count: &mut u64,
    line: usize,
    error: String,
) -> Result<()> {
    *count = count.checked_add(1).ok_or(CaptureError::SystemInvariant(
        "Codex prompt-history rejection count overflowed",
    ))?;
    if failures.len() < crate::summaries::MAX_RETAINED_PROVIDER_FAILURES {
        failures.push(ProviderImportFailure { line, error });
    }
    Ok(())
}

pub(super) fn next_ordinal(current: u64) -> Result<u64> {
    current.checked_add(1).ok_or(CaptureError::SystemInvariant(
        "Codex prompt-history ordinal overflowed",
    ))
}

pub(super) fn generation_id(generation: u64, revision: &str, missing: bool) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx/codex-prompt-history/generation/v1\0");
    digest.update(generation.to_be_bytes());
    digest.update(revision.as_bytes());
    digest.update([u8::from(missing)]);
    format!("codex-prompt-history-generation-v1:{:x}", digest.finalize())
}

pub(super) fn publication_id(
    cursor: &PromptHistoryCursor,
    transition: &NativePathCursorTransition,
    phase: &str,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx/codex-prompt-history/publication/v1\0");
    digest.update(cursor.route_identity.as_bytes());
    digest.update(cursor.generation_id.as_bytes());
    digest.update(phase.as_bytes());
    if let Some(expected) = transition.expected_cursor() {
        digest.update(expected.as_bytes());
    }
    digest.update(transition.next().cursor.as_bytes());
    format!("codex-prompt-history-nativepath-v1:{:x}", digest.finalize())
}

pub(super) fn revision_string(
    hash: &[u8; 32],
    inventory_observation_token: Option<&str>,
) -> String {
    let mut revision = format!("codex-prompt-history-sha256-v1:{}", hex(hash));
    if let Some(token) = inventory_observation_token {
        let inventory_hash: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        revision.push_str(":inventory-sha256:");
        revision.push_str(&hex(&inventory_hash));
    }
    revision
}

pub(super) fn revision_bytes(revision: &str) -> Result<[u8; 32]> {
    let Some(encoded) = revision.strip_prefix("codex-prompt-history-sha256-v1:") else {
        return Err(CaptureError::InvalidPayload(
            "Codex prompt-history source revision is malformed".to_owned(),
        ));
    };
    let mut parts = encoded.split(':');
    let file_hash = parts.next().unwrap_or_default();
    match (parts.next(), parts.next(), parts.next()) {
        (None, None, None) => {}
        (Some("inventory-sha256"), Some(inventory_hash), None) => {
            decode_hex_hash(inventory_hash)?;
        }
        _ => {
            return Err(CaptureError::InvalidPayload(
                "Codex prompt-history source revision is malformed".to_owned(),
            ));
        }
    }
    decode_hex_hash(file_hash)
}

pub(super) fn decode_hex_hash(encoded: &str) -> Result<[u8; 32]> {
    if encoded.len() != 64 {
        return Err(CaptureError::InvalidPayload(
            "Codex prompt-history source revision is malformed".to_owned(),
        ));
    }
    let mut bytes = [0_u8; 32];
    for (index, pair) in encoded.as_bytes().chunks_exact(2).enumerate() {
        bytes[index] = (hex_value(pair[0])? << 4) | hex_value(pair[1])?;
    }
    Ok(bytes)
}

pub(super) fn revision_inventory_authority(revision: &str) -> Option<&str> {
    revision
        .split_once(":inventory-sha256:")
        .map(|(_, authority)| authority)
}

pub(super) fn hex_value(value: u8) -> Result<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(CaptureError::InvalidPayload(
            "Codex prompt-history source revision is malformed".to_owned(),
        )),
    }
}

pub(super) fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

pub(super) fn ensure_active_journal(store: &Store) -> Result<()> {
    if store.native_cold_load_active() {
        return Ok(());
    }
    match store.projection_journal_snapshot(None) {
        Ok(_) => Ok(()),
        Err(StoreError::ProjectionJournalInactive) => {
            store.activate_projection_journal(ctx_pro_host_protocol::PROTOCOL_FINGERPRINT)?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}
