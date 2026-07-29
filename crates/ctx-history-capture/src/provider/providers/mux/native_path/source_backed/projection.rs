use super::resolver::encode_mux_coordinate;
use super::*;

pub(super) fn classify_scan(
    base: Option<&CertifiedSource>,
    opening: &MuxObservedSource,
) -> MuxSourceBackedResult<MuxScanPlan> {
    let Some(base) = base else {
        return Ok(MuxScanPlan::Cold);
    };
    let checkpoint = decode_checkpoint(base)?;
    if base.parser_revision() != MUX_PARSER_REVISION {
        return Ok(MuxScanPlan::Replacement {
            reason: MuxReplacementReason::ParserRevisionChanged,
            checkpoint,
        });
    }
    if checkpoint.observation.metadata_revision != opening.wire.metadata_revision {
        return Ok(MuxScanPlan::Replacement {
            reason: MuxReplacementReason::MetadataChanged,
            checkpoint,
        });
    }
    if checkpoint.observation.partial != opening.wire.partial {
        let reason = if checkpoint.observation.partial.is_some() == opening.wire.partial.is_some() {
            MuxReplacementReason::PartialSnapshotChanged
        } else {
            MuxReplacementReason::SourceSetChanged
        };
        return Ok(MuxScanPlan::Replacement { reason, checkpoint });
    }
    let (Some(prior_chat), Some(current_chat), Some(chat_file)) = (
        checkpoint.chat.as_ref(),
        opening.wire.chat.as_ref(),
        opening.chat_file.as_ref(),
    ) else {
        return Ok(MuxScanPlan::Replacement {
            reason: MuxReplacementReason::SourceSetChanged,
            checkpoint,
        });
    };
    if prior_chat.observation.content_identity != current_chat.content_identity {
        return Ok(MuxScanPlan::Replacement {
            reason: MuxReplacementReason::ChatReplaced,
            checkpoint,
        });
    }
    if current_chat.length < prior_chat.observation.length
        || current_chat.length < prior_chat.frontier.next_offset
    {
        return Ok(MuxScanPlan::Replacement {
            reason: MuxReplacementReason::ChatTruncated,
            checkpoint,
        });
    }
    if current_chat.length <= prior_chat.observation.length {
        return Ok(MuxScanPlan::Replacement {
            reason: MuxReplacementReason::ChatPrefixChanged,
            checkpoint,
        });
    }
    if !prefix_matches_checkpoint(chat_file, &prior_chat.frontier)? {
        return Ok(MuxScanPlan::Replacement {
            reason: MuxReplacementReason::ChatPrefixChanged,
            checkpoint,
        });
    }
    Ok(MuxScanPlan::Append { checkpoint })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn scan_leaf(
    candidate: &MuxSourceBackedCandidate,
    session: &mut MuxBoundedSessionMetadata,
    path: &Path,
    file: &OpenedProviderSourceFile,
    kind: MuxStreamKind,
    observation: &MuxFileObservation,
    initial_frontier: MuxFrontier,
    source_revision_digest: [u8; 32],
    emitted_documents: &mut u64,
    emitted_unaddressable: &mut u64,
    emit: &mut impl FnMut(MuxSourceBackedPage) -> MuxSourceBackedResult<()>,
) -> MuxSourceBackedResult<MuxLeafScan> {
    let plan = source_plan(path, kind, observation.clone());
    let (mut reader, mut hasher) = open_reader_at_frontier(file, &initial_frontier)?;
    let mut frontier = initial_frontier.clone();
    let mut retained_records = 0_u64;
    let mut indexed_documents = 0_u64;
    let mut unaddressable_records = 0_u64;
    let mut rejected_records = 0_u64;
    let mut first_failure = None;

    loop {
        let page = read_core_page(
            &mut reader,
            &mut hasher,
            session,
            &plan,
            frontier.clone(),
            rejected_records,
            first_failure.clone(),
        )?
        .ok_or(CaptureError::SystemInvariant(
            "Mux source-backed parser omitted a terminal page",
        ))?;
        if session.provider_session_id != candidate.metadata.provider_session_id {
            return Err(MuxSourceBackedError::OwnerChanged);
        }
        rejected_records = page.rejected_records;
        first_failure = page.first_failure.clone();
        let terminal = page.terminal;
        let deferred_incomplete = page.deferred_incomplete;
        frontier = page.next.clone();
        retained_records = checked_add(
            retained_records,
            u64::try_from(page.rows.len()).map_err(|_| MuxSourceBackedError::CountOverflow)?,
        )?;
        let projected = project_page(candidate, file, kind, page.rows, source_revision_digest)?;
        let page_documents = u64::try_from(projected.records.len())
            .map_err(|_| MuxSourceBackedError::CountOverflow)?;
        let page_unaddressable = u64::try_from(projected.unaddressable.len())
            .map_err(|_| MuxSourceBackedError::CountOverflow)?;
        indexed_documents = checked_add(indexed_documents, page_documents)?;
        unaddressable_records = checked_add(unaddressable_records, page_unaddressable)?;
        *emitted_documents = checked_add(*emitted_documents, page_documents)?;
        *emitted_unaddressable = checked_add(*emitted_unaddressable, page_unaddressable)?;
        if !projected.records.is_empty() || !projected.unaddressable.is_empty() {
            emit(projected)?;
        }
        if terminal || deferred_incomplete {
            break;
        }
    }
    let complete_records = frontier
        .next_ordinal
        .checked_sub(initial_frontier.next_ordinal)
        .ok_or(MuxSourceBackedError::InvalidCheckpoint)?;
    let content = MuxComponentDigest {
        bytes: frontier.next_offset,
        digest: hasher.finalize().into(),
    };
    Ok(MuxLeafScan {
        checkpoint: MuxLeafCheckpoint {
            observation: leaf_observation(observation, kind),
            frontier,
            content,
        },
        complete_records,
        retained_records,
        indexed_documents,
        unaddressable_records,
    })
}

fn source_plan(path: &Path, kind: MuxStreamKind, observation: MuxFileObservation) -> MuxSourcePlan {
    MuxSourcePlan {
        path: path.to_path_buf(),
        kind,
        observation,
        generation: 0,
    }
}

fn project_page(
    candidate: &MuxSourceBackedCandidate,
    file: &OpenedProviderSourceFile,
    stream_kind: MuxStreamKind,
    rows: Vec<MuxPreparedRow>,
    source_revision_digest: [u8; 32],
) -> MuxSourceBackedResult<MuxSourceBackedPage> {
    let mut records = Vec::with_capacity(rows.len());
    let mut unaddressable = Vec::new();
    for row in rows {
        let native_item_key = NativeItemKey::native_id(
            MUX_NATIVE_ITEM_NAMESPACE,
            TypedKey::utf8(&row.native_record_id)?,
        )?;
        let event_id = derive_event_id(EventIdentityInput {
            source: &candidate.source_key,
            session_id: candidate.session_id,
            logical_item_kind: MUX_LOGICAL_EVENT_KIND,
            native_item_key: &native_item_key,
            subrecord_selector: None,
        })?;
        if let Some(reason) = row.unaddressable_output {
            let bounded_projection = row
                .event
                .as_ref()
                .map(|event| bounded_projection(candidate, event, &row, None));
            unaddressable.push(MuxUnaddressableRecord {
                event_id,
                stream_kind,
                source_record_ordinal: row.source_record_ordinal,
                native_record_id: row.native_record_id,
                reason: match reason {
                    MuxUnaddressableOutput::Redacted => MuxUnaddressableReason::RedactedOutput,
                    MuxUnaddressableOutput::Missing => MuxUnaddressableReason::MissingOutput,
                },
                bounded_projection,
            });
            continue;
        }
        let Some(event) = row.event.as_ref() else {
            continue;
        };
        let exact_body =
            exact_mux_lexical_body(file, stream_kind, &row, event.provider_event_index)?;
        let projection = bounded_projection(candidate, event, &row, Some(exact_body));
        let locator = SourceRecordLocator::new(
            candidate.source_key.clone(),
            NativeRecordCoordinate::ProviderNative {
                namespace: MUX_PROVIDER_NATIVE_LOCATOR_NAMESPACE.to_owned(),
                coordinate: encode_mux_coordinate(
                    stream_kind,
                    row.source_locator.value(),
                    row.source_record_ordinal,
                    projection.event_sequence,
                    &row.native_record_id,
                )?,
            },
            if stream_kind.is_partial() {
                LocatorRevisionPolicy::ExactSourceRevision
            } else {
                LocatorRevisionPolicy::StableRecordEvidence
            },
            Some(source_revision_digest),
            decode_record_digest(&row.source_record_digest)?,
        )?;
        let document = LexicalDocument {
            event_id,
            session_id: candidate.session_id,
            parent_session_id: candidate.parent_session_id,
            root_session_id: candidate.root_session_id,
            source: candidate.source_key.clone(),
            locator,
            provider_session_id: Some(candidate.metadata.provider_session_id.clone()),
            branch: None,
            source_path: mux_stream_path(candidate, stream_kind)
                .map(|path| path.display().to_string()),
            agent_type: if candidate.parent_session_id.is_some() {
                "subagent".to_owned()
            } else {
                "primary".to_owned()
            },
            is_primary: candidate.parent_session_id.is_none(),
            event_sequence: projection.event_sequence,
            occurred_at_unix_ms: projection.occurred_at_unix_ms,
            event_type: projection.event_type,
            role: projection.role,
            body: projection.body,
            workspace: None,
            cwd: projection.cwd,
            touched_files: projection.touched_files,
        };
        records.push(MuxSourceBackedRecord {
            document,
            stream_kind,
            source_record_ordinal: row.source_record_ordinal,
            native_record_id: row.native_record_id,
            message_content_ref: row.message_content_ref,
        });
    }
    Ok(MuxSourceBackedPage {
        source: candidate.source_key.clone(),
        session_id: candidate.session_id,
        stream_kind,
        records,
        unaddressable,
    })
}

pub(super) fn bounded_projection(
    candidate: &MuxSourceBackedCandidate,
    event: &super::super::MuxCoreEvent,
    row: &MuxPreparedRow,
    exact_body: Option<String>,
) -> MuxBoundedProjection {
    let text = event
        .payload
        .get("text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| event.event_type.as_str());
    let touched_files = row
        .file_touches
        .iter()
        .map(|touch| touch.path.clone())
        .collect();
    MuxBoundedProjection {
        provider_session_id: candidate.metadata.provider_session_id.clone(),
        event_sequence: event.provider_event_index,
        occurred_at_unix_ms: Some(event.occurred_at.timestamp_millis()),
        event_type: event.event_type.as_str().to_owned(),
        role: event.role.map(|role| role.as_str().to_owned()),
        body: exact_body.unwrap_or_else(|| text.to_owned()),
        cwd: candidate.metadata.cwd.clone(),
        touched_files,
    }
}

pub(super) fn exact_mux_lexical_body(
    file: &OpenedProviderSourceFile,
    stream_kind: MuxStreamKind,
    row: &MuxPreparedRow,
    event_sequence: u64,
) -> MuxSourceBackedResult<String> {
    if row.source_locator.kind() != MUX_LOCATOR_KIND {
        return Err(MuxSourceBackedError::InvalidLocator);
    }
    let (_, byte_start, byte_end_exclusive) = decode_mux_legacy_range(row.source_locator.value())
        .ok_or(MuxSourceBackedError::InvalidLocator)?;
    let coordinate = MuxLogicalRecordCoordinate {
        stream_kind,
        byte_start,
        byte_end_exclusive,
        source_record_ordinal: row.source_record_ordinal,
        event_sequence,
        native_record_id: row.native_record_id.clone(),
    };
    let payload = read_mux_payload(file, &coordinate).map_err(scan_projection_failure)?;
    if Sha256::digest(&payload).as_slice() != decode_record_digest(&row.source_record_digest)? {
        return Err(MuxSourceBackedError::ExactLexicalProjection(
            "native source record digest changed during scan".to_owned(),
        ));
    }
    let value = serde_json::from_slice::<serde_json::Value>(&payload)?;
    mux_exact_logical_content(&value).map_err(scan_projection_failure)
}

fn scan_projection_failure(failure: HydrationFailure) -> MuxSourceBackedError {
    MuxSourceBackedError::ExactLexicalProjection(format!("{:?}: {}", failure.kind, failure.detail))
}

pub(super) fn scan_counts(
    base: Option<&CertifiedSource>,
    plan: &MuxScanPlan,
    chat: Option<&MuxLeafScan>,
    partial: Option<&MuxLeafScan>,
    metadata: Option<&MuxComponentDigest>,
) -> MuxSourceBackedResult<ScannedSourceCounts> {
    let delta_complete = sum_leaf(chat, partial, |scan| scan.complete_records)?;
    let delta_retained = sum_leaf(chat, partial, |scan| scan.retained_records)?;
    let delta_indexed = sum_leaf(chat, partial, |scan| scan.indexed_documents)?;
    let _delta_unaddressable = sum_leaf(chat, partial, |scan| scan.unaddressable_records)?;
    let (complete_records, retained_records, indexed_documents) = match plan {
        MuxScanPlan::Append { .. } => {
            let base = base
                .ok_or(MuxSourceBackedError::InvalidCheckpoint)?
                .counts();
            (
                checked_add(base.complete_records, delta_complete)?,
                checked_add(base.retained_records, delta_retained)?,
                checked_add(base.indexed_documents, delta_indexed)?,
            )
        }
        MuxScanPlan::Cold | MuxScanPlan::Replacement { .. } => {
            (delta_complete, delta_retained, delta_indexed)
        }
    };
    let rejected_records = complete_records
        .checked_sub(retained_records)
        .ok_or(MuxSourceBackedError::InvalidCheckpoint)?;
    let certified_bytes = checked_add(
        sum_leaf(chat, partial, |scan| scan.checkpoint.content.bytes)?,
        metadata.map_or(0, |metadata| metadata.bytes),
    )?;
    Ok(ScannedSourceCounts {
        complete_records,
        retained_records,
        rejected_records,
        ignored_records: 0,
        indexed_documents,
        certified_bytes,
    })
}

fn sum_leaf(
    chat: Option<&MuxLeafScan>,
    partial: Option<&MuxLeafScan>,
    value: impl Fn(&MuxLeafScan) -> u64,
) -> MuxSourceBackedResult<u64> {
    checked_add(
        chat.map(&value).unwrap_or(0),
        partial.map(value).unwrap_or(0),
    )
}
