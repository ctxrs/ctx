use super::*;

pub(super) fn codex_source_key(native_session_id: &str) -> CodexSourceBackedResultV0<SourceKey> {
    let anchor = SourceAnchor::provider_native(
        CODEX_SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(native_session_id)?,
    )?;
    Ok(SourceKey::derive(
        CaptureProvider::Codex.as_str(),
        CODEX_SESSION_SOURCE_FORMAT,
        CODEX_SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )?)
}

pub(super) fn codex_session_identity(
    source: &SourceKey,
    native_session_id: &str,
) -> CodexSourceBackedResultV0<StableEntityId> {
    let native_session_key = NativeSessionKey::native_id(
        CODEX_NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(native_session_id)?,
    )?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: CODEX_LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })?)
}

pub(super) fn codex_event_identity(
    source: &SourceKey,
    native_session_id: &str,
    raw_ordinal: u64,
) -> CodexSourceBackedResultV0<StableEntityId> {
    let session_id = codex_session_identity(source, native_session_id)?;
    let native_item_key = NativeItemKey::certified_position(
        CODEX_NATIVE_EVENT_POSITION_KIND,
        TypedKey::U64(raw_ordinal),
        PositionStability::AppendStable,
    )?;
    Ok(derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: CODEX_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?)
}

pub(super) fn codex_lexical_document(
    catalog_source: &CodexCatalogSource,
    source: &SourceKey,
    session_id: StableEntityId,
    owner: &CodexSessionRow,
    row: CodexSourceBackedRowV0,
    attributor: &mut crate::repository_attribution::RepositoryAttributor,
) -> CodexSourceBackedResultV0<CodexCoreDocument> {
    let native_session_id = owner.native_session_id.as_str();
    let parent_session_id = owner
        .parent_native_session_id
        .as_deref()
        .map(codex_session_id_for_native_id)
        .transpose()?;
    let root_session_id = owner
        .root_native_session_id
        .as_deref()
        .map(codex_session_id_for_native_id)
        .transpose()?
        .unwrap_or(session_id);
    let is_primary = parent_session_id.is_none();
    let CodexSourceBackedRowV0 {
        raw_ordinal,
        source_record: evidence,
        occurred_at,
        event_type,
        role,
        lexical_body,
        touched_paths,
        repository_tool,
        repository_files,
    } = row;
    let event_id = codex_event_identity(source, native_session_id, raw_ordinal)?;
    let locator = SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::Jsonl {
            byte_offset: evidence.byte_offset,
            byte_length: evidence.byte_length,
            physical_ordinal: raw_ordinal,
            native_session_key: Some(TypedKey::utf8(native_session_id)?),
            native_event_key: Some(TypedKey::U64(raw_ordinal)),
        },
        LocatorRevisionPolicy::StableRecordEvidence,
        None,
        evidence.record_digest,
    )?;
    if lexical_body.is_empty() {
        return Err(CodexSourceBackedErrorV0::MissingLexicalBody);
    }
    let (declared_tool_workdir, command, structured_content) =
        repository_tool.map_or((None, None, None), |evidence| {
            (
                evidence.declared_workdir,
                evidence.command,
                Some(evidence.structured_content),
            )
        });
    let annotation = attributor.attribute(crate::repository_attribution::AttributionInput {
        // Current Codex session/tool records expose cwd, workdir, command, and
        // file activity, but no structured credential-free project identity.
        provider_native_repository_aliases: Vec::new(),
        session_cwd: owner.cwd.clone(),
        declared_tool_workdir,
        command,
        structured_content,
        file_observations: repository_files,
        vcs_observations: Vec::new(),
    });
    let document = LexicalDocument {
        event_id,
        session_id,
        parent_session_id,
        root_session_id,
        source: source.clone(),
        locator,
        provider_session_id: Some(native_session_id.to_owned()),
        branch: None,
        source_path: Some(catalog_source.source_path.display().to_string()),
        agent_type: if is_primary {
            "primary".to_owned()
        } else {
            "subagent".to_owned()
        },
        is_primary,
        event_sequence: raw_ordinal,
        occurred_at_unix_ms: Some(occurred_at.timestamp_millis()),
        event_type: event_type.as_str().to_owned(),
        role: role.map(|role| role.as_str().to_owned()),
        body: lexical_body,
        workspace: owner.cwd.clone(),
        cwd: owner.cwd.clone(),
        touched_files: touched_paths,
    };
    Ok(CodexCoreDocument {
        document,
        annotation,
    })
}

fn codex_session_id_for_native_id(
    native_session_id: &str,
) -> CodexSourceBackedResultV0<StableEntityId> {
    let source = codex_source_key(native_session_id)?;
    codex_session_identity(&source, native_session_id)
}

pub(super) fn validate_owner(
    owner: &CodexSessionRow,
    native_session_id: &str,
) -> CodexSourceBackedResultV0<()> {
    if owner.native_session_id != native_session_id {
        return Err(CodexSourceBackedErrorV0::OwnerMismatch {
            expected: native_session_id.to_owned(),
            actual: owner.native_session_id.clone(),
        });
    }
    Ok(())
}

pub(super) fn decode_append_proof(
    source: &CodexCatalogSource,
    source_key: &SourceKey,
    base: &CertifiedSource,
) -> CodexSourceBackedResultV0<CodexAppendProof> {
    let frontier = base
        .frontier()
        .ok_or(CodexSourceBackedErrorV0::MissingCheckpoint)?;
    if frontier.checkpoint_kind() != CODEX_FRONTIER_KIND {
        return Err(CodexSourceBackedErrorV0::InvalidCheckpoint);
    }
    let TypedKey::Bytes(checkpoint_bytes) = frontier.checkpoint() else {
        return Err(CodexSourceBackedErrorV0::InvalidCheckpoint);
    };
    let checkpoint = CodexNativeCheckpoint::decode(checkpoint_bytes)
        .map_err(|_| CodexSourceBackedErrorV0::InvalidCheckpoint)?;
    let identity = CodexSourceIdentity::new(
        source_key.identity().to_string(),
        source.source_root.clone(),
        source.source_path.clone(),
    )?;
    Ok(CodexAppendProof::new(
        identity,
        CodexCheckpointGeneration::new(base.counts().complete_records),
        checkpoint,
    ))
}

pub(super) fn certify_scan(
    source_key: &SourceKey,
    scan: &super::CodexSourceScan,
    base: Option<&CertifiedSource>,
    staged_documents: u64,
    scan_counters: CodexScanCounters,
) -> CodexSourceBackedResultV0<CertifiedSource> {
    if scan_counters.retained_records != staged_documents {
        return Err(CodexSourceBackedErrorV0::ScanCountMismatch);
    }
    let counts = cumulative_counts(base, scan, staged_documents, scan_counters)?;
    let opening = source_observation(source_key, &scan.before_observation)?;
    let closing = source_observation(source_key, &scan.after_observation)?;
    let frontier = match scan.checkpoint() {
        Some(checkpoint) => Some(SourceFrontier::new(
            CODEX_FRONTIER_KIND,
            TypedKey::bytes(checkpoint.encode()?)?,
            scan.complete_prefix_end,
            scan.complete_prefix_sha256,
        )?),
        None if scan.owner.is_none()
            && staged_documents == 0
            && scan_counters.retained_records == 0
            && scan.disposition == CodexParseDisposition::FullGeneration =>
        {
            // A malformed or missing session_meta makes every otherwise
            // retainable row in this source ineligible: there is no exact
            // native session owner from which stable identities or locators
            // can be derived. Certify the physical scan and its rejection
            // counts, but publish no documents and no append frontier. A
            // later source change is therefore reparsed as a replacement.
            None
        }
        None => return Err(CodexSourceBackedErrorV0::MissingCheckpoint),
    };
    Ok(CertifiedSource::certify_with_frontier(
        opening,
        closing,
        CODEX_PARSER_REVISION,
        scan.complete_prefix_sha256,
        counts,
        frontier,
    )?)
}

fn cumulative_counts(
    base: Option<&CertifiedSource>,
    scan: &super::CodexSourceScan,
    staged_documents: u64,
    scan_counters: CodexScanCounters,
) -> CodexSourceBackedResultV0<ScannedSourceCounts> {
    let base_counts = base.map(CertifiedSource::counts).unwrap_or_default();
    let complete_records =
        checked_add(base_counts.complete_records, scan_counters.complete_records)?;
    let retained_records =
        checked_add(base_counts.retained_records, scan_counters.retained_records)?;
    let rejected_records = checked_add(
        base_counts.rejected_records,
        scan_counters.rejected_complete_records,
    )?;
    let indexed_documents = checked_add(base_counts.indexed_documents, staged_documents)?;
    let classified = checked_add(retained_records, rejected_records)?;
    let ignored_records = complete_records
        .checked_sub(classified)
        .ok_or(CodexSourceBackedErrorV0::ScanCountMismatch)?;
    if complete_records != scan.next_raw_ordinal || indexed_documents != retained_records {
        return Err(CodexSourceBackedErrorV0::ScanCountMismatch);
    }
    Ok(ScannedSourceCounts {
        complete_records,
        retained_records,
        rejected_records,
        ignored_records,
        indexed_documents,
        certified_bytes: scan.complete_prefix_end,
    })
}

fn checked_add(left: u64, right: u64) -> CodexSourceBackedResultV0<u64> {
    left.checked_add(right)
        .ok_or(CodexSourceBackedErrorV0::CountOverflow)
}

pub(crate) fn source_observation(
    source: &SourceKey,
    observation: &CodexFileObservation,
) -> CodexSourceBackedResultV0<SourceObservation> {
    Ok(SourceObservation::new(
        source.clone(),
        CODEX_SOURCE_REVISION_KIND,
        serde_json::to_vec(observation)?,
    )?)
}
