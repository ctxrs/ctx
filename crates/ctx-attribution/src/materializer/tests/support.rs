// Shared fixtures and assertions for the materializer responsibility suites.

fn protocol<T>(result: Result<T, crate::protocol::ProtocolError>) -> TestResult<T> {
    result.map_err(|error| io::Error::other(error.message).into())
}

fn stable_entity(
    source: &SourceKey,
    kind: StableEntityKind,
    byte: u8,
) -> TestResult<StableEntityId> {
    let mut uuid_bytes = [byte; 16];
    uuid_bytes[6] = 0x80 | (uuid_bytes[6] & 0x0f);
    uuid_bytes[8] = 0x80 | (uuid_bytes[8] & 0x3f);
    let identity: StableEntityId = serde_json::from_value(serde_json::json!({
        "contract_version": IDENTITY_VERSION,
        "entity_kind": kind,
        "digest": vec![byte; 32],
        "source_digest": source.identity().digest(),
        "source_descriptor_digest": source.exact_descriptor_digest(),
        "uuid": uuid::Uuid::from_bytes(uuid_bytes),
    }))?;
    identity.validate_contract()?;
    Ok(identity)
}

fn indexed_stable_entity(
    source: &SourceKey,
    kind: StableEntityKind,
    index: u32,
) -> TestResult<StableEntityId> {
    let mut digest = [0_u8; 32];
    digest[..4].copy_from_slice(&index.to_be_bytes());
    let mut uuid_bytes = [0_u8; 16];
    uuid_bytes.copy_from_slice(&digest[..16]);
    uuid_bytes[6] = 0x80 | (uuid_bytes[6] & 0x0f);
    uuid_bytes[8] = 0x80 | (uuid_bytes[8] & 0x3f);
    let identity: StableEntityId = serde_json::from_value(serde_json::json!({
        "contract_version": IDENTITY_VERSION,
        "entity_kind": kind,
        "digest": digest,
        "source_digest": source.identity().digest(),
        "source_descriptor_digest": source.exact_descriptor_digest(),
        "uuid": uuid::Uuid::from_bytes(uuid_bytes),
    }))?;
    identity.validate_contract()?;
    Ok(identity)
}

fn indexed_record(template: &CoreRecord, index: u32) -> TestResult<CoreRecord> {
    let mut record = template.clone();
    let identity_index = index
        .checked_mul(2)
        .ok_or_else(|| io::Error::other("indexed record identity overflow"))?;
    let session = indexed_stable_entity(
        &record.source,
        StableEntityKind::Session,
        identity_index
            .checked_add(1)
            .ok_or_else(|| io::Error::other("indexed session identity overflow"))?,
    )?;
    record.event_id = indexed_stable_entity(
        &record.source,
        StableEntityKind::Event,
        identity_index
            .checked_add(2)
            .ok_or_else(|| io::Error::other("indexed event identity overflow"))?,
    )?;
    record.session_id = session;
    record.event_sequence = u64::from(index) + 1;
    record.occurred_at_unix_ms = Some(1_700_000_000_000_i64 + i64::from(index));
    record.validate_contract()?;
    Ok(record)
}

pub(super) fn one_record() -> TestResult<CoreRecord> {
    let source = SourceKey::derive(
        "zed",
        "zed_threads_sqlite",
        "zed-nativepath-sqlite-v0",
        1,
        SourceAnchor::ProviderNative {
            namespace: "thread-database".to_owned(),
            key: TypedKey::Utf8("materializer-test.db".to_owned()),
        },
    )?;
    let root = stable_entity(&source, StableEntityKind::Session, 0x30)?;
    let session = stable_entity(&source, StableEntityKind::Session, 0x31)?;
    let event = stable_entity(&source, StableEntityKind::Event, 0x41)?;
    let mut record = CoreRecord::new_selected(
        event,
        session,
        source,
        1,
        "message",
        "zed-nativepath-source-backed-v3-neutral-core",
        "materializer test event".to_owned(),
    )?;
    record.parent_session_id = Some(root);
    record.root_session_id = Some(root);
    record.session_relationship = Some(ProviderNativeSessionRelationship::Delegated);
    record.event_copy = None;
    record.agent_scope = Some(AgentScope::Subagent);
    record.occurred_at_unix_ms = Some(1_700_000_000_000);
    record.validate_contract()?;
    Ok(record)
}

pub(super) fn source_state(record: &CoreRecord, accumulator: u8) -> CoreSourceState {
    CoreSourceState {
        source: record.source.clone(),
        core_record_accumulator: hex::encode([accumulator; 32]),
        event_count: 1,
    }
}

pub(super) fn head(generation: u8, sources: &[CoreSourceState]) -> TestResult<CoreGenerationHead> {
    protocol(CoreGenerationHead::new(
        hex::encode([generation; 32]),
        1,
        IDENTITY_VERSION,
        core_record_contract_fingerprint(),
        1,
        1,
        "22".repeat(32),
        sources,
    ))
}

pub(super) fn prepared_page(page: CoreEventDeltaPage) -> TestResult<PreparedCoreEventDeltaPage> {
    let units = page
        .deltas
        .iter()
        .filter_map(CoreEventDelta::record)
        .map(|record| {
            let mut stable_entities = vec![record.event_id, record.session_id];
            let facts = record
                .root_session_id
                .map_or_else(Vec::new, |root_session_id| {
                    stable_entities.push(root_session_id);
                    vec![Fact::create(
                        "test.support",
                        ResourceRef::new(ResourceKind::Session, record.session_id.to_string()),
                        "supports",
                        None,
                        record.occurred_at_unix_ms.map(|value| value.to_string()),
                        Confidence::Verified,
                        FactState::Asserted,
                        "segment_materializer.test",
                        "1",
                        record.session_id.to_string(),
                        Some(root_session_id.to_string()),
                        Vec::new(),
                        BTreeMap::new(),
                    )]
                });
            let evidence = (!facts.is_empty()).then(|| PreparedCoreEvidence {
                citation: EvidenceCitation {
                    core_generation_id: page.core_generation_id.clone(),
                    source: record.source.clone(),
                    session_id: record.session_id,
                    event_id: record.event_id,
                    event_sequence: record.event_sequence,
                    byte_range: None,
                    evidence_sha256: Some("33".repeat(32)),
                },
            });
            (
                record.event_id.to_string(),
                PreparedCoreUnit {
                    origin_event_id: record.event_id.to_string(),
                    producer_authority_disposition:
                        crate::core_materialization::producer_authority_disposition(record),
                    stable_entities,
                    facts,
                    evidence,
                    coverage: CoreProjectionCoverage::default(),
                },
            )
        })
        .collect();
    protocol(PreparedCoreEventDeltaPage::for_test(page, units))
}
