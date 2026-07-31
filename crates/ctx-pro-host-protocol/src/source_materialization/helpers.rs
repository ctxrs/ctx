fn validate_manifest_entries(
    sources: &[CertifiedSource],
    removals: &[SourceRemoval],
) -> Result<(), ProtocolError> {
    if sources.len() > MAX_SOURCE_MANIFEST_SOURCES
        || removals.len() > MAX_SOURCE_MANIFEST_REMOVALS
        || sources.len().saturating_add(removals.len()) > MAX_SOURCE_MANIFEST_SOURCES
    {
        return Err(ProtocolError::new(
            ErrorClass::Bounds,
            "source manifest exceeds its source or removal count bound",
        ));
    }
    let mut prior_source = None;
    for source in sources {
        source
            .validate_contract()
            .map_err(|error| invalid_contract("certified source", error))?;
        let current = source_identity_digest(source);
        if prior_source.is_some_and(|prior| prior >= current) {
            return Err(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "source manifest sources must be sorted and unique by stable lineage",
            ));
        }
        prior_source = Some(current);
    }
    let retained = sources
        .iter()
        .map(source_identity_digest)
        .collect::<BTreeSet<_>>();
    let mut prior_removal = None;
    for removal in removals {
        removal.validate()?;
        let current = removal.deletion.source().identity().digest();
        if retained.contains(&current) {
            return Err(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "source manifest cannot retain and delete the same stable lineage",
            ));
        }
        if prior_removal.is_some_and(|prior| prior >= current) {
            return Err(ProtocolError::new(
                ErrorClass::InvalidRequest,
                "source manifest removals must be sorted and unique by stable lineage",
            ));
        }
        prior_removal = Some(current);
    }
    Ok(())
}

fn source_manifest_aggregate_sha256(
    header: &SourceManifestHeader,
    sources: &[CertifiedSource],
    removals: &[SourceRemoval],
) -> Result<String, ProtocolError> {
    let mut digest = Sha256::new();
    digest.update(b"ctx-pro-source-manifest-admission-v1\0");
    digest.update(header.contract_version.to_be_bytes());
    digest_field(&mut digest, header.core_generation_id.as_bytes());
    digest.update(header.generation_manifest_version.to_be_bytes());
    digest.update(header.identity_version.to_be_bytes());
    digest.update(header.lexical_schema_version.to_be_bytes());
    digest.update(header.lexical_analyzer_version.to_be_bytes());
    digest_field(&mut digest, header.policy_schema_hash.as_bytes());
    digest.update(header.source_count.to_be_bytes());
    digest.update(header.removal_count.to_be_bytes());
    digest.update(header.page_count.to_be_bytes());
    for source in sources {
        digest.update(b"s");
        digest_json(&mut digest, source)?;
    }
    for removal in removals {
        digest.update(b"r");
        digest_json(&mut digest, removal)?;
    }
    Ok(hex_digest(digest.finalize()))
}

fn source_manifest_initial_chain_sha256(header: &SourceManifestHeader) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-pro-source-manifest-chain-start-v1\0");
    digest.update(header.contract_version.to_be_bytes());
    digest_field(&mut digest, header.core_generation_id.as_bytes());
    digest.update(header.generation_manifest_version.to_be_bytes());
    digest.update(header.identity_version.to_be_bytes());
    digest.update(header.lexical_schema_version.to_be_bytes());
    digest.update(header.lexical_analyzer_version.to_be_bytes());
    digest_field(&mut digest, header.policy_schema_hash.as_bytes());
    digest.update(header.source_count.to_be_bytes());
    digest.update(header.removal_count.to_be_bytes());
    digest.update(header.page_count.to_be_bytes());
    digest_field(&mut digest, header.aggregate_sha256.as_bytes());
    hex_digest(digest.finalize())
}

fn source_manifest_page_sha256(page: &SourceManifestPage) -> Result<String, ProtocolError> {
    let mut digest = Sha256::new();
    digest.update(b"ctx-pro-source-manifest-page-v1\0");
    digest.update(page.contract_version.to_be_bytes());
    digest_field(&mut digest, page.core_generation_id.as_bytes());
    digest_field(&mut digest, page.aggregate_sha256.as_bytes());
    digest_field(&mut digest, page.previous_page_sha256.as_bytes());
    digest.update(page.page_index.to_be_bytes());
    digest.update(page.item_index.to_be_bytes());
    digest_json(&mut digest, &page.entries)?;
    Ok(hex_digest(digest.finalize()))
}

fn digest_json<T: Serialize>(digest: &mut Sha256, value: &T) -> Result<(), ProtocolError> {
    let bytes = serde_json::to_vec(value).map_err(|_| {
        ProtocolError::new(
            ErrorClass::Internal,
            "source manifest digest encoding failed",
        )
    })?;
    digest_field(digest, &bytes);
    Ok(())
}

fn digest_field(digest: &mut Sha256, bytes: &[u8]) {
    digest.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(bytes);
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn source_progress_aggregate_sha256(progress: &[SourceProgress]) -> Result<String, ProtocolError> {
    let source_count = u32::try_from(progress.len())
        .map_err(|_| ProtocolError::new(ErrorClass::Bounds, "source progress count overflowed"))?;
    let page_count = u32::try_from(progress.len().div_ceil(MAX_SOURCE_PROGRESS_PAGE_ITEMS))
        .map_err(|_| {
            ProtocolError::new(ErrorClass::Bounds, "source progress page count overflowed")
        })?;
    let mut digest = Sha256::new();
    digest.update(b"ctx-pro-source-progress-receipt-v1\0");
    digest.update(SOURCE_MATERIALIZATION_CONTRACT_VERSION.to_be_bytes());
    digest.update(source_count.to_be_bytes());
    digest.update(page_count.to_be_bytes());
    for value in progress {
        digest_json(&mut digest, value)?;
    }
    Ok(hex_digest(digest.finalize()))
}

fn source_progress_page_sha256(
    receipt: &SourceProgressReceipt,
    page: &SourceProgressPage,
) -> Result<String, ProtocolError> {
    let mut digest = Sha256::new();
    digest.update(b"ctx-pro-source-progress-page-v1\0");
    digest.update(SOURCE_MATERIALIZATION_CONTRACT_VERSION.to_be_bytes());
    digest.update(receipt.source_count.to_be_bytes());
    digest.update(receipt.page_count.to_be_bytes());
    digest_field(&mut digest, receipt.aggregate_sha256.as_bytes());
    digest.update(page.page_index.to_be_bytes());
    digest_json(&mut digest, &page.progress)?;
    Ok(hex_digest(digest.finalize()))
}

fn validate_progress_set(
    progress: &[SourceProgress],
    required_materializer_revision: Option<&str>,
    require_terminal: bool,
    name: &str,
) -> Result<(), ProtocolError> {
    if progress.len() > MAX_SOURCE_PROGRESS_SOURCES {
        return Err(ProtocolError::new(
            ErrorClass::Bounds,
            format!("{name} exceeds its source count bound"),
        ));
    }
    let mut prior = None;
    for value in progress {
        value.validate()?;
        if require_terminal && !value.terminal {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                format!("{name} contains nonterminal progress"),
            ));
        }
        if required_materializer_revision
            .is_some_and(|revision| revision != value.materializer_revision)
        {
            return Err(ProtocolError::new(
                ErrorClass::Sequence,
                format!("{name} contains a mismatched materializer revision"),
            ));
        }
        let current = value.source.identity().digest();
        if prior.is_some_and(|prior| prior >= current) {
            return Err(ProtocolError::new(
                ErrorClass::InvalidRequest,
                format!("{name} must be sorted and unique by stable source lineage"),
            ));
        }
        prior = Some(current);
    }
    Ok(())
}

fn validate_session_id(session_id: StableEntityId, name: &str) -> Result<(), ProtocolError> {
    session_id
        .validate_contract()
        .map_err(|error| invalid_contract(name, error))?;
    if session_id.entity_kind() != StableEntityKind::Session {
        return Err(ProtocolError::new(
            ErrorClass::InvalidRequest,
            format!("source record {name} is not a stable session identity"),
        ));
    }
    Ok(())
}

fn validate_session_id_for_locator(
    session_id: StableEntityId,
    locator: &SourceRecordLocator,
    name: &str,
) -> Result<(), ProtocolError> {
    validate_session_id(session_id, name)?;
    if session_id.source_digest() != locator.source().identity().digest()
        || session_id.source_descriptor_digest() != locator.source().exact_descriptor_digest()
    {
        return Err(ProtocolError::new(
            ErrorClass::InvalidRequest,
            format!("source record {name} does not belong to its locator source"),
        ));
    }
    Ok(())
}

fn validate_sha256(value: &str, name: &str) -> Result<(), ProtocolError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ProtocolError::new(
            ErrorClass::InvalidRequest,
            format!("{name} must be lowercase SHA-256"),
        ));
    }
    Ok(())
}

fn validate_identity(value: &str, name: &str) -> Result<(), ProtocolError> {
    if value.trim().is_empty()
        || value.len() > MAX_SOURCE_IDENTITY_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(ProtocolError::new(
            ErrorClass::Bounds,
            format!("{name} is empty, unsafe, or exceeds its byte bound"),
        ));
    }
    Ok(())
}

fn validate_optional_identity(value: Option<&str>, name: &str) -> Result<(), ProtocolError> {
    value.map_or(Ok(()), |value| validate_identity(value, name))
}

fn validate_path(value: &str, name: &str) -> Result<(), ProtocolError> {
    if value.trim().is_empty() || value.len() > MAX_SOURCE_PATH_BYTES || value.contains('\0') {
        return Err(ProtocolError::new(
            ErrorClass::Bounds,
            format!("{name} is empty, unsafe, or exceeds its byte bound"),
        ));
    }
    Ok(())
}

fn validate_optional_path(value: Option<&str>, name: &str) -> Result<(), ProtocolError> {
    value.map_or(Ok(()), |value| validate_path(value, name))
}

fn source_identity_digest(source: &CertifiedSource) -> [u8; 32] {
    source.observation().source().identity().digest()
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn invalid_contract(name: &str, error: impl fmt::Display) -> ProtocolError {
    ProtocolError::new(
        ErrorClass::InvalidRequest,
        format!("invalid {name}: {error}"),
    )
}

fn validate_encoded_bound<T: Serialize>(
    value: &T,
    maximum: usize,
    message: &'static str,
) -> Result<(), ProtocolError> {
    let mut counter = SerializedByteCounter { bytes: 0 };
    serde_json::to_writer(&mut counter, value)
        .map_err(|_| ProtocolError::new(ErrorClass::Internal, "encoded-size validation failed"))?;
    if counter.bytes > maximum {
        return Err(ProtocolError::new(ErrorClass::Bounds, message));
    }
    Ok(())
}

struct SerializedByteCounter {
    bytes: usize,
}

impl Write for SerializedByteCounter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes = self.bytes.saturating_add(buffer.len());
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
