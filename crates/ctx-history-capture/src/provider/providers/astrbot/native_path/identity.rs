use super::*;

pub(super) fn page_publication_id(
    authority: &SourceAuthority,
    page: &AstrBotPage,
    cursor: &AstrBotStoreCursor,
) -> Result<String> {
    let mut hash = Sha256::new();
    hash.update(b"ctx-astrbot-nativepath-page-v1\0");
    hash_field(&mut hash, authority.locator_identity.as_bytes());
    hash_field(&mut hash, authority.source_revision.as_bytes());
    hash_field(&mut hash, cursor.encode()?.as_bytes());
    hash_field(
        &mut hash,
        &serde_json::to_vec(&page.expected_frontier).map_err(CaptureError::from)?,
    );
    for unit in &page.units {
        hash_field(&mut hash, unit.session.provider_session_id.as_bytes());
        if let Some(event) = &unit.event {
            hash.update(event.provider_event_index.to_le_bytes());
            hash_field(
                &mut hash,
                crate::compute_payload_hash(&event.payload)?.as_bytes(),
            );
        }
    }
    for rejection in &page.rejections {
        hash_field(&mut hash, rejection.detail.as_bytes());
    }
    Ok(format!("{PUBLICATION_PREFIX}{}", hex(&hash.finalize())))
}

pub(super) fn retirement_publication_id(
    retirement: &ProviderSourceRouteRetirement,
    next_cursor: &str,
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"ctx-astrbot-nativepath-retirement-v1\0");
    hash_field(&mut hash, retirement.locator_identity.as_bytes());
    hash_field(&mut hash, retirement.expected_source_revision.as_bytes());
    hash_field(&mut hash, next_cursor.as_bytes());
    format!("{RETIREMENT_PREFIX}{}", hex(&hash.finalize()))
}

pub(super) fn source_incarnation(revision: &str) -> String {
    let database = revision
        .split("database=")
        .nth(1)
        .and_then(|value| value.split(";wal=").next())
        .unwrap_or(revision);
    let device = database
        .split("device=")
        .nth(1)
        .and_then(|value| value.split(';').next())
        .unwrap_or("none");
    let inode = database
        .split("inode=")
        .nth(1)
        .and_then(|value| value.split(';').next())
        .unwrap_or("none");
    format!("device={device};inode={inode}")
}

pub(super) fn encode_output_frontier(frontier: &AstrBotFrontier) -> Result<OutputNativeCursor> {
    Ok(OutputNativeCursor {
        version: OUTPUT_CURSOR_VERSION,
        payload: serde_json::to_vec(frontier)?,
    })
}

pub(super) fn decode_output_frontier(cursor: &OutputNativeCursor) -> Result<AstrBotFrontier> {
    if cursor.version != OUTPUT_CURSOR_VERSION {
        return Err(CaptureError::InvalidPayload(
            "AstrBot output cursor version is unsupported".to_owned(),
        ));
    }
    let frontier: AstrBotFrontier = serde_json::from_slice(&cursor.payload)?;
    if frontier.version != FRONTIER_VERSION {
        return Err(CaptureError::InvalidPayload(
            "AstrBot output frontier version is unsupported".to_owned(),
        ));
    }
    Ok(frontier)
}

pub(super) fn serialized_hash(value_domain: &[u8], value: &impl Serialize) -> Result<[u8; 32]> {
    let encoded = serde_json::to_vec(value)?;
    let mut hash = Sha256::new();
    hash.update(value_domain);
    hash_field(&mut hash, &encoded);
    Ok(hash.finalize().into())
}

pub(super) fn candidate_hash(domain: &[u8], candidate: RowCandidate) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(domain);
    hash.update(candidate.physical_rowid.to_le_bytes());
    hash.update(candidate.retained_bytes.to_le_bytes());
    hash.update(candidate.legacy_order.logical_id.to_le_bytes());
    hash.update(candidate.legacy_order.timestamp.to_le_bytes());
    hash.finalize().into()
}

pub(super) fn chain_hash(prior: [u8; 32], row: [u8; 32]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"ctx-astrbot-prefix-chain-v1\0");
    hash.update(prior);
    hash.update(row);
    hash.finalize().into()
}

pub(super) fn hash_field(hash: &mut Sha256, value: &[u8]) {
    hash.update((value.len() as u64).to_le_bytes());
    hash.update(value);
}

pub(super) fn hex(bytes: &[u8]) -> String {
    let mut value = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(value, "{byte:02x}");
    }
    value
}

pub(super) fn timestamp(value: Option<i64>, fallback: DateTime<Utc>) -> DateTime<Utc> {
    provider_timestamp_millis(value, fallback)
}

pub(super) fn capped_optional(value: Option<&str>) -> Option<String> {
    value.map(|value| value.chars().take(PROVIDER_MAX_TEXT_CHARS).collect())
}

pub(super) fn estimated_unit_bytes(unit: &CoreUnit) -> usize {
    serde_json::to_vec(&unit.session.metadata)
        .map(|bytes| bytes.len())
        .unwrap_or(PAGE_MAX_CORE_BYTES)
        .saturating_add(
            unit.event
                .as_ref()
                .and_then(|event| serde_json::to_vec(&event.payload).ok())
                .map(|bytes| bytes.len())
                .unwrap_or_default(),
        )
        .saturating_add(
            unit.event
                .as_ref()
                .and_then(|event| serde_json::to_vec(&event.metadata).ok())
                .map(|bytes| bytes.len())
                .unwrap_or_default(),
        )
        .saturating_add(2048)
}

pub(super) fn ordinal_line(ordinal: u64) -> usize {
    usize::try_from(ordinal)
        .unwrap_or(usize::MAX)
        .saturating_add(1)
}
