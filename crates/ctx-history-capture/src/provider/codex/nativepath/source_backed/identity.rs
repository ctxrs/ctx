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

pub(super) fn codex_core_record(
    source: &SourceKey,
    session_id: StableEntityId,
    owner: &CodexSessionRow,
    row: CodexSourceBackedRowV0,
    attributor: &mut crate::repository_attribution::RepositoryAttributor,
) -> CodexSourceBackedResultV0<CoreRecord> {
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
        occurred_at,
        event_type,
        role,
        session_cwd,
        lexical_body,
        touched_paths,
        repository_tools,
        repository_result,
        repository_files,
    } = row;
    let event_id = codex_event_identity(source, native_session_id, raw_ordinal)?;
    if lexical_body.is_empty() {
        return Err(CodexSourceBackedErrorV0::MissingLexicalBody);
    }
    let mut native_tool_activities = repository_tools
        .iter()
        .map(|evidence| evidence.structured_content.clone())
        .collect::<Vec<_>>();
    let mut provider_native_repository_aliases = Vec::new();
    let mut outcome_observations = Vec::new();
    let mut outcome_abstentions = Vec::new();
    let mut outcome_operation_repository_path = None;
    let mut outcome_output_repository_path = None;
    let result_declared_workdir = repository_result
        .as_ref()
        .and_then(|evidence| evidence.declared_workdir.clone());
    let result_command = repository_result
        .as_ref()
        .map(|evidence| evidence.command.clone());
    if let Some(evidence) = repository_result {
        native_tool_activities.push(evidence.structured_content.clone());
        provider_native_repository_aliases = evidence.provider_native_repository_aliases;
        outcome_operation_repository_path = evidence.outcome_operation_repository_path;
        outcome_output_repository_path = evidence.outcome_output_repository_path;
        outcome_observations = evidence.outcomes;
        outcome_abstentions = evidence.abstentions;
    }
    let mut annotation = attributor.attribute(crate::repository_attribution::AttributionInput {
        activity_at_unix_ms: Some(occurred_at.timestamp_millis()),
        // Codex result records contribute a credential-free forge identity
        // only when an exact structured PR result carries one.
        provider_native_repository_aliases,
        session_cwd: session_cwd.clone(),
        declared_tool_workdir: result_declared_workdir,
        command: result_command,
        structured_content: None,
        file_observations: repository_files,
        vcs_observations: Vec::new(),
        outcome_operation_repository_path,
        outcome_output_repository_path,
        outcome_observations,
        outcome_abstentions,
    });
    for evidence in repository_tools {
        let activity = attributor.attribute(crate::repository_attribution::AttributionInput {
            activity_at_unix_ms: Some(occurred_at.timestamp_millis()),
            session_cwd: session_cwd.clone(),
            declared_tool_workdir: evidence.declared_workdir,
            command: evidence.command,
            structured_content: None,
            file_observations: evidence.file_observations,
            ..crate::repository_attribution::AttributionInput::default()
        });
        merge_repository_annotation(&mut annotation, activity);
    }
    if !native_tool_activities.is_empty() {
        annotation.structured_content = Some(serde_json::json!({
            "provider_native_tool_activities": native_tool_activities,
        }));
    }
    annotation.metadata.insert(
        "codex_session".to_owned(),
        serde_json::json!({
            "started_at_unix_ms": owner.started_at.timestamp_millis(),
            "originator": bounded_core_metadata(owner.originator.as_deref()),
            "cli_version": bounded_core_metadata(owner.cli_version.as_deref()),
            "source_kind": bounded_core_metadata(owner.source_kind.as_deref()),
            "external_agent_id": bounded_core_metadata(owner.external_agent_id.as_deref()),
            "role_hint": bounded_core_metadata(owner.role_hint.as_deref()),
            "model_provider": bounded_core_metadata(owner.model_provider.as_deref()),
            "git": owner.git.as_ref().map(|git| serde_json::json!({
                "commit_hash": bounded_core_metadata(git.commit_hash.as_deref()),
                "branch": bounded_core_metadata(git.branch.as_deref()),
                "repository_url": bounded_core_metadata(git.repository_url.as_deref()),
            })),
        }),
    );
    if !touched_paths.is_empty() {
        annotation.metadata.insert(
            "codex_native_activity".to_owned(),
            serde_json::json!({ "touched_paths": touched_paths }),
        );
    }

    let agent_type = if is_primary { "primary" } else { "subagent" };
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        root_session_id,
        source.clone(),
        raw_ordinal,
        event_type.as_str(),
        agent_type,
        is_primary,
        CODEX_PARSER_REVISION,
        lexical_body,
    )?;
    record.parent_session_id = parent_session_id;
    record.provider_session_id = Some(native_session_id.to_owned());
    record.native_event_id = Some(TypedKey::U64(raw_ordinal));
    record.occurred_at_unix_ms = Some(occurred_at.timestamp_millis());
    record.role = role.map(|role| role.as_str().to_owned());
    record.workspace.clone_from(&session_cwd);
    record.cwd = session_cwd;
    record.branch = owner
        .git
        .as_ref()
        .and_then(|git| bounded_core_metadata(git.branch.as_deref()));
    record.content.structured_content = annotation.structured_content;
    record.metadata = annotation.metadata;
    record.repository_candidate_evidence = annotation.repository_candidate_evidence;
    record.repository_bindings = annotation.repository_bindings;
    record.repository_abstentions = annotation.repository_abstentions;
    record.repository_file_observations = annotation.repository_file_observations;
    record.repository_vcs_observations = annotation.repository_vcs_observations;
    record.validate_contract()?;
    Ok(record)
}

fn merge_repository_annotation(
    target: &mut CoreRecordAnnotation,
    mut additional: CoreRecordAnnotation,
) {
    let target_evidence = &mut target.repository_candidate_evidence;
    let additional_evidence = additional.repository_candidate_evidence;
    target_evidence.session_cwd = target_evidence
        .session_cwd
        .take()
        .or(additional_evidence.session_cwd);
    for (target_value, additional_value) in [
        (
            &mut target_evidence.declared_tool_workdir,
            additional_evidence.declared_tool_workdir,
        ),
        (
            &mut target_evidence.derived_effective_cwd,
            additional_evidence.derived_effective_cwd,
        ),
        (
            &mut target_evidence.command_specific_repository_path,
            additional_evidence.command_specific_repository_path,
        ),
        (
            &mut target_evidence.outcome_operation_repository_path,
            additional_evidence.outcome_operation_repository_path,
        ),
        (
            &mut target_evidence.outcome_output_repository_path,
            additional_evidence.outcome_output_repository_path,
        ),
    ] {
        if additional_value.is_some() {
            *target_value = additional_value;
        }
    }
    for mut binding in additional.repository_bindings.drain(..) {
        if let Some(existing) = target
            .repository_bindings
            .iter_mut()
            .find(|existing| existing.binding_id == binding.binding_id)
        {
            for alias in binding.aliases.drain(..) {
                if !existing.aliases.contains(&alias) {
                    existing.aliases.push(alias);
                }
            }
            for evidence in binding.evidence.drain(..) {
                if !existing.evidence.contains(&evidence) {
                    existing.evidence.push(evidence);
                }
            }
            if existing.local_root_authorization.is_none() {
                existing.local_root_authorization = binding.local_root_authorization;
            }
        } else {
            target.repository_bindings.push(binding);
        }
    }
    for abstention in additional.repository_abstentions {
        if !target.repository_abstentions.contains(&abstention) {
            target.repository_abstentions.push(abstention);
        }
    }
    for observation in additional.repository_file_observations {
        if !target.repository_file_observations.contains(&observation) {
            target.repository_file_observations.push(observation);
        }
    }
    for observation in additional.repository_vcs_observations {
        if !target.repository_vcs_observations.contains(&observation) {
            target.repository_vcs_observations.push(observation);
        }
    }
}

fn bounded_core_metadata(value: Option<&str>) -> Option<String> {
    value
        .filter(|value| !value.is_empty() && value.len() <= 64 * 1024)
        .map(str::to_owned)
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
            // native session owner from which stable identities can be
            // derived. Certify the physical scan and its rejection counts,
            // but publish no Core records and no append frontier. A
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
