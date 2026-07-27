use super::query::{hydrate_row, next_candidate, row_decode_error_is_local, CrushCandidate};
use super::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CrushOutputCursor {
    version: u32,
    source_revision: String,
    after_rowid: Option<i64>,
    next_ordinal: u64,
    terminal: bool,
}

impl CrushOutputCursor {
    fn initial(source_revision: String) -> Self {
        Self {
            version: CRUSH_NATIVE_OUTPUT_CURSOR_VERSION,
            source_revision,
            after_rowid: None,
            next_ordinal: 0,
            terminal: false,
        }
    }

    fn native_cursor(&self) -> Result<OutputNativeCursor> {
        Ok(OutputNativeCursor {
            version: CRUSH_NATIVE_OUTPUT_CURSOR_VERSION,
            payload: serde_json::to_vec(self)?,
        })
    }
}

pub(super) fn replay_crush_outputs(source: &CrushNativeSource, sink: Option<&dyn ProOutputSink>) {
    let Some(sink) = sink else {
        return;
    };
    if let Err(error) = replay_crush_outputs_inner(source, sink) {
        sink.mark_behind(ProOutputSinkError::new(
            "crush_nativepath_output_replay",
            error.to_string(),
        ));
    }
}

fn replay_crush_outputs_inner(source: &CrushNativeSource, sink: &dyn ProOutputSink) -> Result<()> {
    let output_source = OutputSourceIdentity {
        provider: CaptureProvider::Crush.as_str().to_owned(),
        namespace_id: source.source_root.clone(),
        source_id: source.locator_identity.clone(),
    };
    let progress = match sink.observe_source(&output_source) {
        Ok(progress) => progress,
        Err(error) => {
            sink.mark_behind(error);
            return Ok(());
        }
    };
    let materializer_revision = sink.materializer_revision().to_owned();
    let progress_cursor = progress
        .as_ref()
        .and_then(|progress| progress.cursor.as_ref())
        .filter(|cursor| cursor.version == CRUSH_NATIVE_OUTPUT_CURSOR_VERSION)
        .and_then(|cursor| serde_json::from_slice::<CrushOutputCursor>(&cursor.payload).ok())
        .filter(|cursor| {
            cursor.version == CRUSH_NATIVE_OUTPUT_CURSOR_VERSION
                && cursor.after_rowid.is_none_or(|rowid| rowid > 0)
        });
    let can_resume = progress.as_ref().is_some_and(|progress| {
        progress.parser_revision == CRUSH_NATIVE_OUTPUT_PARSER_REVISION
            && progress.materializer_revision == materializer_revision
            && progress.observed_revision == source.source_revision
            && progress_cursor.is_some()
    });
    if can_resume
        && progress_cursor
            .as_ref()
            .is_some_and(|cursor| cursor.terminal)
    {
        return Ok(());
    }
    let (mut cursor, source_epoch, mut expected_epoch, mut expected_cursor, mut disposition) =
        match progress {
            None => (
                CrushOutputCursor::initial(source.source_revision.clone()),
                0,
                None,
                None,
                ProOutputSourceDisposition::NewSource,
            ),
            Some(progress) if can_resume => (
                progress_cursor.ok_or(CaptureError::SystemInvariant(
                    "Crush resumable output progress lost its cursor",
                ))?,
                progress.source_epoch,
                Some(progress.source_epoch),
                progress.cursor,
                ProOutputSourceDisposition::AppendOrResume,
            ),
            Some(progress) => (
                CrushOutputCursor::initial(source.source_revision.clone()),
                progress
                    .source_epoch
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Crush output source epoch exhausted",
                    ))?,
                Some(progress.source_epoch),
                progress.cursor,
                ProOutputSourceDisposition::Rewrite,
            ),
        };

    loop {
        if !source.snapshot.revalidate(&source.canonical_path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let candidate =
            next_message_candidate(&source.connection, &source.schema, cursor.after_rowid)?;
        let mut next = cursor.clone();
        let observations = match candidate {
            Some(candidate) => {
                next.after_rowid = Some(candidate.rowid);
                let ordinal = next.next_ordinal;
                next.next_ordinal =
                    next.next_ordinal
                        .checked_add(1)
                        .ok_or(CaptureError::SystemInvariant(
                            "Crush output ordinal exhausted",
                        ))?;
                if candidate.observed_bytes > CRUSH_NATIVE_MAX_ROW_BYTES {
                    return Err(CaptureError::InvalidPayload(format!(
                        "Crush output row {} exceeds the NativePath retained-row bound",
                        candidate.rowid
                    )));
                } else {
                    let observation = match hydrate_row(
                        source,
                        CrushNativePhase::Messages,
                        candidate.rowid,
                        candidate.observed_bytes,
                    )
                    .and_then(|row| match row {
                        CrushHydratedRow::Message { row, session, .. } => {
                            output_observation(source, ordinal, row, session.as_ref())
                        }
                        _ => Ok(None),
                    }) {
                        Ok(observation) => observation,
                        Err(error) if row_decode_error_is_local(&error) => None,
                        Err(error) => return Err(error),
                    };
                    observation.into_iter().collect()
                }
            }
            None => {
                next.terminal = true;
                Vec::new()
            }
        };
        let next_native = next.native_cursor()?;
        let page = ProOutputMaterializationPage {
            inventory_generation: sink.inventory_generation(),
            source: output_source.clone(),
            source_epoch,
            observed_revision: source.source_revision.clone(),
            parser_revision: CRUSH_NATIVE_OUTPUT_PARSER_REVISION.to_owned(),
            materializer_revision: materializer_revision.clone(),
            disposition,
            expected_prior_source_epoch: expected_epoch,
            expected_prior_cursor: expected_cursor.clone(),
            next_safe_cursor: next_native.clone(),
            terminal: next.terminal,
            observations,
        };
        let result = match sink.materialize_page(page) {
            Ok(result)
                if result.source_epoch == source_epoch
                    && result.committed_cursor == next_native =>
            {
                result
            }
            Ok(_) => {
                sink.mark_behind(ProOutputSinkError::new(
                    "crush_nativepath_output_receipt_mismatch",
                    "Crush output sink acknowledged another source epoch or cursor",
                ));
                return Ok(());
            }
            Err(error) => {
                sink.mark_behind(error);
                return Ok(());
            }
        };
        expected_epoch = Some(result.source_epoch);
        expected_cursor = Some(result.committed_cursor);
        disposition = ProOutputSourceDisposition::AppendOrResume;
        cursor = next;
        if cursor.terminal {
            return Ok(());
        }
    }
}

fn next_message_candidate(
    conn: &Connection,
    schema: &CrushNativeSchema,
    after_rowid: Option<i64>,
) -> Result<Option<CrushCandidate>> {
    next_candidate(
        conn,
        schema,
        &CrushNativeFrontier {
            phase: CrushNativePhase::Messages,
            after_rowid,
            next_ordinal: 0,
        },
    )
}

fn output_observation(
    source: &CrushNativeSource,
    ordinal: u64,
    row: CrushMessageRow,
    session: Option<&CrushSessionRow>,
) -> Result<Option<ProOutputObservation>> {
    let CrushRecordProjection::Message(projected) = project_message(
        &row,
        session,
        &ProviderAdapterContext {
            machine_id: String::new(),
            source_path: Some(source.canonical_path.clone()),
            source_root: Some(PathBuf::from(&source.source_root)),
            imported_at: DateTime::<Utc>::UNIX_EPOCH,
        },
    )?
    else {
        return Ok(None);
    };
    let Some(output) = projected.output else {
        return Ok(None);
    };
    let content = crush_normalized_result_content(&projected.raw_parts)
        .unwrap_or_default()
        .into_bytes();
    if content
        .len()
        .saturating_add(CRUSH_NATIVE_OUTPUT_OVERHEAD_BYTES)
        > ctx_history_store::NATIVE_PATH_MAX_RETAINED_PAGE_BYTES
    {
        return Err(CaptureError::InvalidPayload(format!(
            "Crush output row {} exceeds the NativePath output-page bound",
            row.rowid
        )));
    }
    Ok(Some(ProOutputObservation {
        kind: output.kind,
        coordinate: OutputNativeCoordinate {
            unit_key: format!(
                "crush:{}:{}:output",
                projected.provider_session_id, projected.native_record_id
            ),
            native_sequence: ordinal,
            native_record_id: Some(projected.native_record_id.clone()),
            source_record_ordinal: Some(ordinal),
            source_record_subrecord_index: Some(0),
            byte_start: None,
            byte_end_exclusive: None,
        },
        occurred_at_unix_ms: Some(projected.occurred_at.timestamp_millis()),
        associations: OutputAssociations {
            direct_session_id: projected.provider_session_id.clone(),
            root_session_id: projected
                .parent_session_id
                .clone()
                .unwrap_or_else(|| projected.provider_session_id.clone()),
            parent_session_id: projected.parent_session_id,
            provider_session_id: Some(projected.provider_session_id),
            agent_id: None,
            repository: None,
        },
        call_id: output.call_id,
        command: output.command,
        outcome: output.outcome,
        locator: OutputSourceLocator {
            version: 1,
            kind: super::super::capture::CRUSH_LOCATOR_KIND.to_owned(),
            payload: message_locator(row.rowid)?.value().to_vec(),
        },
        content,
    }))
}
