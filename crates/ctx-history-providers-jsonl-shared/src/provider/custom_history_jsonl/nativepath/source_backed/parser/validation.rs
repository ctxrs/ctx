use super::*;

pub(super) fn retained_metadata_bytes(lengths: &[usize]) -> usize {
    lengths.iter().fold(
        CUSTOM_HISTORY_CATALOG_ENTRY_OVERHEAD_BYTES,
        |total, length| total.saturating_add(*length),
    )
}

const CORE_ACTIVITY_TEXT_METADATA_MAX_BYTES: usize = 64 * 1024;

pub(super) fn validate_unmerged_activity(
    activity: &CoreActivity,
    normalized_body: &str,
) -> std::result::Result<(), &'static str> {
    if activity.revision != ctx_history_core::CORE_ACTIVITY_REVISION {
        return Err("neutral activity revision is unsupported");
    }
    if activity.invocation.is_none() && activity.result.is_none() && activity.facts.is_empty() {
        return Err("neutral activity must contain an invocation, result, or literal fact");
    }
    if activity
        .provider_call_id
        .as_ref()
        .is_some_and(|call_id| call_id.validate_contract().is_err())
    {
        return Err("neutral activity call identity is invalid");
    }
    if (activity.invocation.is_some() || activity.result.is_some())
        && activity.provider_call_id.is_none()
    {
        return Err("neutral activity invocation or result requires an exact call identity");
    }
    if activity.facts.len() > MAX_PROVIDER_DECLARED_FACTS {
        return Err("neutral activity fact count exceeds the Core bound");
    }
    if activity
        .facts
        .iter()
        .any(|fact| !valid_core_text(&fact.value, MAX_CORE_CONTENT_BYTES))
    {
        return Err("neutral activity fact value exceeds the Core bound");
    }
    if let Some(invocation) = &activity.invocation {
        if !valid_optional_core_text(
            invocation.protocol.as_deref(),
            CORE_ACTIVITY_TEXT_METADATA_MAX_BYTES,
        ) || !valid_optional_core_text(
            invocation.server.as_deref(),
            CORE_ACTIVITY_TEXT_METADATA_MAX_BYTES,
        ) || !valid_core_text(&invocation.tool, CORE_ACTIVITY_TEXT_METADATA_MAX_BYTES)
            || !valid_json_capture(&invocation.arguments)
        {
            return Err("neutral activity invocation exceeds the Core contract");
        }
    }
    if let Some(result) = &activity.result {
        if !valid_optional_core_text(
            result.status.as_deref(),
            CORE_ACTIVITY_TEXT_METADATA_MAX_BYTES,
        ) || !valid_text_capture(&result.text, normalized_body)
            || !valid_json_capture(&result.structured_content)
        {
            return Err("neutral activity result exceeds the Core contract");
        }
    }
    Ok(())
}

fn valid_core_text(value: &str, maximum: usize) -> bool {
    !value.is_empty() && value.len() <= maximum
}

fn valid_optional_core_text(value: Option<&str>, maximum: usize) -> bool {
    value.is_none_or(|value| valid_core_text(value, maximum))
}

fn valid_json_capture(capture: &ctx_history_core::ActivityJsonCapture) -> bool {
    match capture {
        ctx_history_core::ActivityJsonCapture::Omitted { reason, .. } => {
            valid_core_text(reason, CORE_ACTIVITY_TEXT_METADATA_MAX_BYTES)
        }
        ctx_history_core::ActivityJsonCapture::Present { .. }
        | ctx_history_core::ActivityJsonCapture::Absent
        | ctx_history_core::ActivityJsonCapture::Unavailable => true,
    }
}

fn valid_text_capture(
    capture: &ctx_history_core::ActivityTextCapture,
    normalized_body: &str,
) -> bool {
    match capture {
        ctx_history_core::ActivityTextCapture::Present { value } => {
            valid_core_text(value, MAX_CORE_CONTENT_BYTES)
        }
        ctx_history_core::ActivityTextCapture::NormalizedBody => !normalized_body.is_empty(),
        ctx_history_core::ActivityTextCapture::Omitted { reason, .. } => {
            valid_core_text(reason, CORE_ACTIVITY_TEXT_METADATA_MAX_BYTES)
        }
        ctx_history_core::ActivityTextCapture::Absent
        | ctx_history_core::ActivityTextCapture::Unavailable => true,
    }
}

pub(super) fn ensure_retained_key_bound(
    limit: CustomHistorySourceBackedBound,
    value: Option<&str>,
) -> CustomHistorySourceBackedResult<()> {
    let Some(value) = value else {
        return Ok(());
    };
    if value.len() > CUSTOM_HISTORY_IDENTIFIER_MAX_BYTES {
        return Err(CustomHistorySourceBackedError::Bounds {
            limit,
            maximum: CUSTOM_HISTORY_IDENTIFIER_MAX_BYTES,
            observed: value.len(),
        });
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn finish_projection(
    mut catalog: ProjectionCatalog,
    event_spool: File,
    certified_prefix_bytes: u64,
    complete_records: usize,
    terminal: bool,
    content_digest: [u8; 32],
    observed_prior_prefix_digest: Option<[u8; 32]>,
    prior_prefix_bytes: Option<u64>,
) -> CustomHistorySourceBackedResult<ParsedProjection> {
    if catalog.manifest_line.is_none() {
        catalog.manifest_failure = Some((
            ProviderSourceFailureKind::InvalidSource,
            format!("missing manifest record for {CUSTOM_HISTORY_PUBLIC_SCHEMA_VERSION}"),
        ));
    }
    if let Some((kind, detail)) = catalog.manifest_failure {
        return Err(CustomHistorySourceBackedError::StructuralManifest { kind, detail });
    }
    apply_session_lineage_contract(&mut catalog);
    catalog.reference_keys.clear();
    catalog.edge_keys.clear();

    let mut file_references = BTreeMap::<CustomEventKey, Vec<ProviderDeclaredFact>>::new();
    {
        let valid_sessions =
            session_catalog(&catalog.sources, &catalog.sessions, &mut catalog.summary);
        catalog
            .sessions
            .retain(|key, _| valid_sessions.contains(key));

        let mut invalid_events = Vec::new();
        catalog.events.retain(|key, event| {
            let valid = catalog
                .sessions
                .contains_key(&(key.0.clone(), key.1.clone()));
            if !valid {
                invalid_events.push((
                    event.line_number,
                    format!(
                        "event references unknown session `{}` in source `{}`",
                        key.1, key.0
                    ),
                ));
            }
            valid
        });
        for (line_number, error) in invalid_events {
            push_provider_import_failure(&mut catalog.summary, line_number, error);
        }

        for reference in catalog.file_references.drain(..) {
            let session_key = (
                reference.source_id.clone(),
                reference.provider_session_id.clone(),
            );
            let error = if !catalog.sessions.contains_key(&session_key) {
                Some(format!(
                    "file_reference references unknown provider_session_id `{}` in source `{}`",
                    reference.provider_session_id, reference.source_id
                ))
            } else {
                let event_index = reference.event_index;
                let event_key = (
                    reference.source_id.clone(),
                    reference.provider_session_id.clone(),
                    event_index,
                );
                (!catalog.events.contains_key(&event_key)).then(|| {
                    format!("file_reference references unknown event_index `{event_index}`")
                })
            };
            if let Some(error) = error {
                push_provider_import_failure(&mut catalog.summary, reference.line_number, error);
            } else {
                file_references
                    .entry((
                        reference.source_id,
                        reference.provider_session_id,
                        reference.event_index,
                    ))
                    .or_default()
                    .push(ProviderDeclaredFact {
                        kind: ctx_history_core::LiteralFactKind::File,
                        value: reference.value,
                    });
            }
        }

        let mut invalid_activities = Vec::new();
        catalog.events.retain(|key, event| {
            let session = catalog
                .sessions
                .get(&(key.0.clone(), key.1.clone()))
                .expect("event sessions were retained above");
            let references = file_references.get(key).map(Vec::as_slice).unwrap_or(&[]);
            let error = validate_merged_activity(event, session, references).err();
            if let Some(error) = error {
                invalid_activities.push((key.clone(), event.line_number, error));
                false
            } else {
                true
            }
        });
        for (key, line_number, error) in invalid_activities {
            file_references.remove(&key);
            push_provider_import_failure(&mut catalog.summary, line_number, error.to_owned());
        }

        for edge in catalog.edges.drain(..) {
            let from_key = (
                edge.source_id.clone(),
                edge.from_provider_session_id.clone(),
            );
            let to_key = (edge.source_id.clone(), edge.to_provider_session_id.clone());
            let error = if !catalog.sessions.contains_key(&from_key) {
                Some(format!(
                    "edge references unknown from_provider_session_id `{}`",
                    edge.from_provider_session_id
                ))
            } else if !catalog.sessions.contains_key(&to_key) {
                Some(format!(
                    "edge references unknown to_provider_session_id `{}`",
                    edge.to_provider_session_id
                ))
            } else if edge.relationship.is_some() {
                catalog.sessions.get(&to_key).and_then(|child| {
                    child
                        .parent_provider_session_id
                        .as_ref()
                        .and_then(|parent| {
                            (parent != &edge.from_provider_session_id).then(|| {
                            format!(
                                "edge from_provider_session_id `{}` conflicts with session parent_provider_session_id `{parent}`",
                                edge.from_provider_session_id
                            )
                        })
                    })
                })
            } else {
                None
            };
            if let Some(error) = error {
                push_provider_import_failure(&mut catalog.summary, edge.line_number, error);
            } else if let Some(relationship) = edge.relationship {
                if let Some(child) = catalog.sessions.get_mut(&to_key) {
                    child.parent_provider_session_id = Some(edge.from_provider_session_id.clone());
                    child.session_relationship = Some(relationship);
                }
            }
        }

        apply_session_lineage_contract(&mut catalog);

        let required = catalog
            .events
            .keys()
            .map(|key| (key.0.clone(), key.1.clone()))
            .collect::<BTreeSet<_>>();
        catalog.sessions.retain(|key, _| required.contains(key));
    }
    let copied_origins = validate_copied_origins(
        catalog.lineage_contract,
        &catalog.sessions,
        &catalog.events,
        &mut catalog.summary,
    );

    let mut rejected_lines = catalog
        .summary
        .failures
        .iter()
        .filter_map(|failure| (failure.line != 0).then_some(failure.line))
        .collect::<BTreeSet<_>>();
    rejected_lines.extend(catalog.oversized_lines);
    let retained_lines = catalog
        .events
        .values()
        .map(|event| event.line_number)
        .collect::<BTreeSet<_>>();
    let complete_records = u64::try_from(complete_records)
        .map_err(|_| CustomHistorySourceBackedError::CountMismatch)?;
    let retained_records = u64::try_from(catalog.events.len())
        .map_err(|_| CustomHistorySourceBackedError::CountMismatch)?;
    let retained_records_before_prior_prefix = prior_prefix_bytes
        .map(|prior_prefix_bytes| {
            u64::try_from(
                catalog
                    .events
                    .values()
                    .filter(|event| event.line.byte_offset < prior_prefix_bytes)
                    .count(),
            )
            .map_err(|_| CustomHistorySourceBackedError::CountMismatch)
        })
        .transpose()?;
    #[cfg(test)]
    record_custom_history_work(|work| {
        work.retained_events_before_prior_prefix = retained_records_before_prior_prefix
            .and_then(|count| usize::try_from(count).ok())
            .unwrap_or(0);
    });
    let rejected_records = u64::try_from(
        rejected_lines
            .iter()
            .filter(|line| **line <= complete_records as usize && !retained_lines.contains(*line))
            .count(),
    )
    .map_err(|_| CustomHistorySourceBackedError::CountMismatch)?;
    let ignored_records = complete_records
        .checked_sub(retained_records)
        .and_then(|value| value.checked_sub(rejected_records))
        .ok_or(CustomHistorySourceBackedError::CountMismatch)?;
    let counts = ScannedSourceCounts {
        complete_records,
        retained_records,
        rejected_records,
        ignored_records,
        indexed_documents: retained_records,
        certified_bytes: certified_prefix_bytes,
    };
    Ok(ParsedProjection {
        sources: catalog.sources,
        sessions: catalog.sessions,
        events: catalog.events,
        copied_origins,
        file_references,
        event_spool,
        observed_prior_prefix_digest,
        retained_records_before_prior_prefix,
        counts,
        checkpoint: CustomHistoryCheckpoint {
            version: CUSTOM_CHECKPOINT_VERSION,
            certified_prefix_bytes,
            complete_records,
            terminal,
        },
        content_digest,
    })
}

fn validate_merged_activity(
    event: &CustomEventCatalogEntry,
    session: &CustomSessionCatalogEntry,
    file_references: &[ProviderDeclaredFact],
) -> std::result::Result<(), &'static str> {
    let fact_count = event
        .activity_fact_count
        .checked_add(usize::from(session.cwd.is_some()))
        .and_then(|count| count.checked_add(file_references.len()))
        .ok_or("merged neutral activity fact count exceeds the Core bound")?;
    if fact_count > MAX_PROVIDER_DECLARED_FACTS {
        return Err("merged neutral activity fact count exceeds the Core bound");
    }
    if session
        .cwd
        .as_deref()
        .is_some_and(|cwd| !valid_core_text(cwd, MAX_CORE_CONTENT_BYTES))
        || file_references
            .iter()
            .any(|fact| !valid_core_text(&fact.value, MAX_CORE_CONTENT_BYTES))
    {
        return Err("merged neutral activity fact value exceeds the Core bound");
    }
    Ok(())
}

fn apply_session_lineage_contract(catalog: &mut ProjectionCatalog) {
    if catalog.lineage_contract.is_none() {
        for session in catalog.sessions.values_mut() {
            session.session_relationship = None;
        }
        for event in catalog.events.values_mut() {
            event.copied_from = None;
        }
        return;
    }

    for session in catalog.sessions.values_mut() {
        let Some(kind) = session.session_relationship else {
            continue;
        };
        let valid = match kind {
            ProviderNativeSessionRelationship::Root => {
                session.parent_provider_session_id.is_none()
                    && session
                        .root_provider_session_id
                        .as_deref()
                        .is_none_or(|root| root == session.provider_session_id)
            }
            ProviderNativeSessionRelationship::Delegated
            | ProviderNativeSessionRelationship::Forked
            | ProviderNativeSessionRelationship::ResumedFrom
            | ProviderNativeSessionRelationship::WorkflowChild => session
                .parent_provider_session_id
                .as_deref()
                .is_some_and(|parent| {
                    parent != session.provider_session_id
                        && session
                            .root_provider_session_id
                            .as_deref()
                            .is_none_or(|root| root != session.provider_session_id)
                }),
        };
        if !valid {
            push_provider_import_failure(
                &mut catalog.summary,
                session.line_number,
                "session_relationship conflicts with parent_provider_session_id/root_provider_session_id"
                    .to_owned(),
            );
            session.session_relationship = None;
        }
    }
}

fn validate_copied_origins(
    lineage_contract: Option<CtxHistoryJsonlLineageContract>,
    sessions: &BTreeMap<CustomSessionKey, CustomSessionCatalogEntry>,
    events: &BTreeMap<CustomEventKey, CustomEventCatalogEntry>,
    summary: &mut ProviderImportSummary,
) -> BTreeMap<CustomEventKey, ValidatedCopiedFrom> {
    if lineage_contract.is_none() {
        return BTreeMap::new();
    }

    let mut native_events = BTreeMap::<(String, String, String), Option<(u64, usize)>>::new();
    for (key, event) in events {
        let Some(event_id) = event.event_id.as_ref() else {
            continue;
        };
        if !stable_lineage_identifier(event_id) {
            continue;
        }
        let entry = native_events
            .entry((key.0.clone(), key.1.clone(), event_id.clone()))
            .or_insert(Some((key.2, event.line_number)));
        if entry.is_some_and(|(event_index, _)| event_index != key.2) {
            *entry = None;
        }
    }

    let mut admitted = BTreeMap::new();
    for (key, event) in events {
        let Some(selector) = event.copied_from.as_ref() else {
            continue;
        };
        if !stable_lineage_identifier(&selector.ancestor_provider_session_id)
            || !stable_lineage_identifier(&selector.ancestor_event_id)
        {
            push_provider_import_failure(
                summary,
                event.line_number,
                "copied_from provider session and native event selectors must be non-empty bounded identifiers"
                    .to_owned(),
            );
            continue;
        }
        let child_session_key = (key.0.clone(), key.1.clone());
        let child_session = sessions.get(&child_session_key);
        let child_event_is_exact = event.event_id.as_ref().is_some_and(|event_id| {
            native_events
                .get(&(key.0.clone(), key.1.clone(), event_id.clone()))
                .and_then(|entry| *entry)
                .is_some_and(|(event_index, _)| event_index == key.2)
        });
        let has_typed_ancestor_claim = child_session.is_some_and(|session| {
            matches!(
                session.session_relationship,
                Some(
                    ProviderNativeSessionRelationship::Delegated
                        | ProviderNativeSessionRelationship::Forked
                        | ProviderNativeSessionRelationship::ResumedFrom
                        | ProviderNativeSessionRelationship::WorkflowChild
                )
            ) && selector.ancestor_provider_session_id != session.provider_session_id
        });
        let proof_identity_is_exact = !matches!(
            selector.proof,
            CtxHistoryJsonlCopyProofKind::NativeEventIdentity
        ) || event.event_id.as_deref()
            == Some(selector.ancestor_event_id.as_str());

        if !child_event_is_exact || !has_typed_ancestor_claim || !proof_identity_is_exact {
            push_provider_import_failure(
                summary,
                event.line_number,
                "copied_from requires a unique stable child event ID, a distinct provider-declared ancestor under a typed non-root relationship, and proof-consistent identity"
                    .to_owned(),
            );
            continue;
        }
        // The typed relationship and copied selector are child-owned proof.
        // Derive the durable unresolved IDs from them without consulting the
        // mutable target catalog; target presence is resolution state, not
        // claim validity.
        let proof = match selector.proof {
            CtxHistoryJsonlCopyProofKind::NativeEventIdentity => {
                ProviderNativeCopyProof::NativeEventIdentity
            }
            CtxHistoryJsonlCopyProofKind::NativeCopiedFromField => {
                ProviderNativeCopyProof::NativeCopiedFromField
            }
            CtxHistoryJsonlCopyProofKind::NativeCallResultIdentity => {
                ProviderNativeCopyProof::NativeCallResultIdentity
            }
        };
        admitted.insert(
            key.clone(),
            ValidatedCopiedFrom {
                ancestor_provider_session_id: selector.ancestor_provider_session_id.clone(),
                ancestor_event_id: selector.ancestor_event_id.clone(),
                proof,
            },
        );
    }
    admitted
}

fn stable_lineage_identifier(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= CUSTOM_HISTORY_IDENTIFIER_MAX_BYTES
        && !value.chars().any(char::is_control)
}
