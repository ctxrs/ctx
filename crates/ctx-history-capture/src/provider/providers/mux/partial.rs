use std::{num::NonZeroUsize, path::PathBuf};

use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;

use crate::captured_batch::whole_json::{WholeJsonBatchProducer, WholeJsonItem};
use crate::captured_batch::{NativePosition, ProviderRecordKind, SourceObservation};
use crate::provider::importer::{
    captured_batch_cursor_stream, import_captured_batches, provider_path_identity,
    provider_source_cursor_stream_for_path, CapturedBatchCursorMode, CapturedSourceAdmission,
    CertifiedProviderCursor,
};
use crate::{
    CaptureError, NormalizedProviderImportOptions, ProviderAdapterContext, ProviderImportSummary,
    Result, MUX_SOURCE_FORMAT,
};

use super::metadata::mux_bounded_session_metadata;
use super::projector::{MuxCapturedBatchProjector, MuxCapturedStreamKind};
use super::source::{MuxFileObservation, MuxSessionSource};
use super::{
    mux_captured_batch_error, mux_file_context, mux_whole_json_error, MUX_CAPTURE_REVISION,
    MUX_PARTIAL_RECORD_KIND, MUX_POLICY_REVISION, MUX_WHOLE_JSON_POSITION_KIND,
};

pub(super) fn import_mux_partial_batched(
    session_source: MuxSessionSource,
    partial_path: PathBuf,
    store: &mut Store,
    context: &ProviderAdapterContext,
    import_options: &NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let observation =
        MuxFileObservation::read(&partial_path, session_source.metadata_path.as_deref())?;
    let path_identity = provider_path_identity(&observation.canonical_path)?;
    let source = SourceObservation::new(
        CaptureProvider::Mux,
        MUX_SOURCE_FORMAT,
        format!("mux-partial-json:{path_identity}"),
        observation.source_revision("partial-json"),
        provider_source_cursor_stream_for_path(
            CaptureProvider::Mux,
            MUX_SOURCE_FORMAT,
            &path_identity,
        ),
        MUX_CAPTURE_REVISION,
        MUX_POLICY_REVISION,
        import_options.inventory_observation_token.as_deref(),
    )
    .map_err(mux_captured_batch_error)?;
    let record_kind =
        ProviderRecordKind::new(MUX_PARTIAL_RECORD_KIND).map_err(mux_captured_batch_error)?;
    let initial_position = mux_whole_json_position(0)?;
    let stream = captured_batch_cursor_stream(&source);
    let expected_store_cursor = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let file_context = mux_file_context(context, &partial_path);
    let bounded_session = mux_bounded_session_metadata(
        &session_source,
        &observation.metadata_revision(),
        context.imported_at,
    )?;
    let mut cursor_mode = CapturedBatchCursorMode::Resume;
    let mut resumable_cursor = None;

    if let Some(stored_cursor) = expected_store_cursor.as_ref() {
        match CertifiedProviderCursor::decode_if_certified(&stored_cursor.cursor)? {
            Some(certified)
                if certified.source_revision() == source.source_revision()
                    && certified.parser_revision() == source.capture_revision()
                    && certified.policy_revision() == source.policy_revision() =>
            {
                let ordinal = mux_whole_json_position_ordinal(certified.native_position())?;
                if ordinal > 1 {
                    return Err(CaptureError::InvalidPayload(
                        "Mux partial cursor exceeds its source".to_owned(),
                    ));
                }
                let projector = MuxCapturedBatchProjector::resume(
                    file_context.clone(),
                    session_source.clone(),
                    partial_path.clone(),
                    MuxCapturedStreamKind::Partial,
                    bounded_session.clone(),
                    &certified,
                )?;
                if ordinal == 1 {
                    if !observation
                        .revalidate(&partial_path, session_source.metadata_path.as_deref())?
                    {
                        return Err(CaptureError::SourceChangedDuringCapture);
                    }
                    return projector.replay_summary();
                }
                resumable_cursor = Some(certified);
            }
            Some(_) => cursor_mode = CapturedBatchCursorMode::ResetChangedSource,
            None => cursor_mode = CapturedBatchCursorMode::ReplaceLegacyCursor,
        }
    }

    let mut projector = match resumable_cursor.as_ref() {
        Some(cursor) => MuxCapturedBatchProjector::resume(
            file_context.clone(),
            session_source.clone(),
            partial_path.clone(),
            MuxCapturedStreamKind::Partial,
            bounded_session.clone(),
            cursor,
        )?,
        None => MuxCapturedBatchProjector::fresh(
            file_context.clone(),
            session_source.clone(),
            partial_path.clone(),
            MuxCapturedStreamKind::Partial,
            bounded_session,
        ),
    };
    if !observation.revalidate(&partial_path, session_source.metadata_path.as_deref())? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let observed_size = observation.content.length;
    let source_item = path_identity.into_bytes();
    let item_path = partial_path.clone();
    let mut emitted = false;
    let mut producer = WholeJsonBatchProducer::new(source.clone(), record_kind, move || {
        if std::mem::replace(&mut emitted, true) {
            return Ok(None);
        }
        WholeJsonItem::new(0, source_item.clone(), observed_size, item_path.clone()).map(Some)
    })
    .map_err(mux_whole_json_error)?;
    let admission = CapturedSourceAdmission::file_for_context(&source, &file_context)?;
    let outcome = import_captured_batches(
        store,
        &admission,
        import_options.clone(),
        &context.machine_id,
        context.imported_at,
        expected_store_cursor.as_ref(),
        &initial_position,
        cursor_mode,
        NonZeroUsize::new(1).ok_or(CaptureError::SystemInvariant(
            "Mux whole-JSON batch limit must be nonzero",
        ))?,
        &mut projector,
        || producer.next_batch().map_err(mux_whole_json_error),
        || observation.revalidate(&partial_path, session_source.metadata_path.as_deref()),
    )?;
    if outcome.batches_imported == 0 && expected_store_cursor.is_some() {
        projector.replay_summary()
    } else {
        Ok(outcome.summary)
    }
}

fn mux_whole_json_position(ordinal: u64) -> Result<NativePosition> {
    NativePosition::new(MUX_WHOLE_JSON_POSITION_KIND, ordinal.to_be_bytes().to_vec())
        .map_err(mux_captured_batch_error)
}

fn mux_whole_json_position_ordinal(position: &NativePosition) -> Result<u64> {
    if position.kind() != MUX_WHOLE_JSON_POSITION_KIND || position.value().len() != 8 {
        return Err(CaptureError::InvalidPayload(
            "Mux cursor has an invalid whole-JSON position".to_owned(),
        ));
    }
    let bytes: [u8; 8] = position.value().try_into().map_err(|_| {
        CaptureError::InvalidPayload("Mux cursor has an invalid whole-JSON ordinal".to_owned())
    })?;
    Ok(u64::from_be_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_json_position_preserves_kind_and_big_endian_bytes() {
        let position = mux_whole_json_position(0x0102_0304_0506_0708).unwrap();

        assert_eq!(position.kind(), MUX_WHOLE_JSON_POSITION_KIND);
        assert_eq!(position.value(), &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(
            mux_whole_json_position_ordinal(&position).unwrap(),
            0x0102_0304_0506_0708
        );
    }
}
