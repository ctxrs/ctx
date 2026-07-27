use std::{
    fs::{self, File, Metadata},
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(test)]
use std::cell::Cell;

use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;
use serde_json::Value;

use crate::captured_batch::jsonl::{
    initial_jsonl_position, jsonl_position_offset, verify_jsonl_append_boundary, JsonlBatchError,
    JsonlBatchProducer,
};
use crate::captured_batch::{ProviderRecordKind, SourceObservation};
use crate::common::io::ensure_regular_provider_transcript_file;
use crate::provider::importer::{
    captured_batch_cursor_stream, drain_captured_batches, provider_path_identity,
    provider_source_cursor_stream_for_path, CapturedBatchCursorMode, CapturedSourceAdmission,
    CertifiedProviderCursor,
};
use crate::{
    CaptureError, NormalizedProviderImportOptions, ProviderAdapterContext, ProviderImportSummary,
    Result, MAX_PROVIDER_JSONL_LINE_BYTES,
};

use super::dialect::{
    native_jsonl_record_kind, native_jsonl_record_starts_session,
    validate_direct_native_jsonl_provider,
};
use super::normalization::native_jsonl_normalized_header_metadata;
use super::projector::{
    native_jsonl_batch_error, native_jsonl_header_anchor_digest, NativeJsonlCapturedBatchProjector,
    NativeJsonlParserCheckpoint,
};

pub(super) const NATIVE_JSONL_CAPTURE_REVISION: u32 = 4;
pub(super) const NATIVE_JSONL_POLICY_REVISION: u32 = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
struct NativeJsonlFrozenFileMetadata {
    length: u64,
    modified: SystemTime,
    readonly: bool,
    device: Option<u64>,
    inode: Option<u64>,
}

impl NativeJsonlFrozenFileMetadata {
    fn read(path: &Path) -> Result<Self> {
        ensure_regular_provider_transcript_file(path)?;
        Self::from_metadata(&fs::symlink_metadata(path)?)
    }

    fn from_metadata(metadata: &Metadata) -> Result<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        #[cfg(unix)]
        let (device, inode) = (Some(metadata.dev()), Some(metadata.ino()));
        #[cfg(not(unix))]
        let (device, inode) = (None, None);

        Ok(Self {
            length: metadata.len(),
            modified: metadata.modified()?,
            readonly: metadata.permissions().readonly(),
            device,
            inode,
        })
    }

    fn source_revision(&self) -> String {
        let (side, seconds, nanos) = match self.modified.duration_since(UNIX_EPOCH) {
            Ok(duration) => ('+', duration.as_secs(), duration.subsec_nanos()),
            Err(error) => {
                let duration = error.duration();
                ('-', duration.as_secs(), duration.subsec_nanos())
            }
        };
        format!(
            "native-jsonl-metadata-v1:length={};modified={side}{seconds}.{nanos:09};readonly={};device={};inode={}",
            self.length,
            self.readonly,
            self.device
                .map_or_else(|| "none".to_owned(), |value| value.to_string()),
            self.inode
                .map_or_else(|| "none".to_owned(), |value| value.to_string()),
        )
    }

    fn revalidate(&self, path: &Path) -> Result<bool> {
        match Self::read(path) {
            Ok(current) => Ok(current == *self),
            Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(false)
            }
            Err(CaptureError::InvalidProviderTranscriptPath { .. }) => Ok(false),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
std::thread_local! {
    static NATIVE_JSONL_SOURCE_FILE_OPEN_COUNT: Cell<Option<usize>> = const { Cell::new(None) };
}

fn open_native_jsonl_source_file(path: &Path) -> Result<File> {
    #[cfg(test)]
    NATIVE_JSONL_SOURCE_FILE_OPEN_COUNT.with(|count| {
        if let Some(current) = count.get() {
            count.set(Some(current.saturating_add(1)));
        }
    });
    Ok(File::open(path)?)
}

#[cfg(test)]
pub(super) fn count_native_jsonl_source_file_opens<T>(operation: impl FnOnce() -> T) -> (T, usize) {
    NATIVE_JSONL_SOURCE_FILE_OPEN_COUNT.with(|count| {
        assert_eq!(count.replace(Some(0)), None);
    });
    let output = operation();
    let opens = NATIVE_JSONL_SOURCE_FILE_OPEN_COUNT.with(|count| count.replace(None).unwrap());
    (output, opens)
}
enum NativeJsonlResumeHeaderMetadata {
    Ready(Option<Value>),
    AnchorMismatch,
}

fn native_jsonl_resume_header_metadata(
    path: &Path,
    provider: CaptureProvider,
    cursor: &CertifiedProviderCursor,
    frozen: &NativeJsonlFrozenFileMetadata,
) -> Result<NativeJsonlResumeHeaderMetadata> {
    let checkpoint: NativeJsonlParserCheckpoint = cursor.parser_checkpoint().deserialize()?;
    let Some(session) = checkpoint.session else {
        return Ok(NativeJsonlResumeHeaderMetadata::Ready(None));
    };
    let frontier_offset =
        jsonl_position_offset(cursor.native_position()).map_err(native_jsonl_batch_error)?;
    let anchor = session.header_anchor;
    if anchor.ordinal >= checkpoint.next_ordinal
        || anchor.start >= anchor.end
        || anchor.end > frontier_offset
    {
        return Err(CaptureError::InvalidPayload(
            "native JSONL header anchor exceeds its certified cursor".to_owned(),
        ));
    }
    let length = usize::try_from(anchor.end - anchor.start).map_err(|_| {
        CaptureError::InvalidPayload(
            "native JSONL header anchor length exceeds platform limits".to_owned(),
        )
    })?;
    if length > MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(2) {
        return Err(CaptureError::InvalidPayload(
            "native JSONL header anchor exceeds the provider record limit".to_owned(),
        ));
    }
    if !frozen.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let mut file = open_native_jsonl_source_file(path)?;
    if NativeJsonlFrozenFileMetadata::from_metadata(&file.metadata()?)? != *frozen {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    file.seek(SeekFrom::Start(anchor.start))?;
    let mut bytes = vec![0_u8; length];
    file.read_exact(&mut bytes)?;
    if !frozen.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    if native_jsonl_header_anchor_digest(&bytes) != anchor.payload_sha256 {
        return Ok(NativeJsonlResumeHeaderMetadata::AnchorMismatch);
    }
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return Ok(NativeJsonlResumeHeaderMetadata::AnchorMismatch);
    };
    if !native_jsonl_record_starts_session(provider, &value) {
        return Ok(NativeJsonlResumeHeaderMetadata::AnchorMismatch);
    }
    Ok(NativeJsonlResumeHeaderMetadata::Ready(Some(
        native_jsonl_normalized_header_metadata(&value),
    )))
}

pub(super) fn import_native_jsonl_file_batched(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    provider: CaptureProvider,
    source_format: &str,
    import_options: NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    validate_direct_native_jsonl_provider(provider)?;
    let mut projection_context = context.clone();
    projection_context.source_path = Some(path.to_path_buf());
    let frozen = NativeJsonlFrozenFileMetadata::read(path)?;
    let canonical_path = fs::canonicalize(path)?;
    let cursor_source_path =
        provider_path_identity(context.source_path.as_deref().unwrap_or(path))?;
    let canonical_path_identity = provider_path_identity(&canonical_path)?;
    let source = SourceObservation::new(
        provider,
        source_format,
        format!("native-jsonl-file:{canonical_path_identity}"),
        frozen.source_revision(),
        provider_source_cursor_stream_for_path(provider, source_format, &cursor_source_path),
        NATIVE_JSONL_CAPTURE_REVISION,
        NATIVE_JSONL_POLICY_REVISION,
        import_options.inventory_observation_token.as_deref(),
    )
    .map_err(native_jsonl_captured_batch_error)?;
    let source_item = canonical_path_identity.into_bytes();
    let record_kind = ProviderRecordKind::new(native_jsonl_record_kind(provider, source_format))
        .map_err(native_jsonl_captured_batch_error)?;
    let stream = captured_batch_cursor_stream(&source);
    let expected_store_cursor = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let had_expected_store_cursor = expected_store_cursor.is_some();
    let initial_position = initial_jsonl_position().map_err(native_jsonl_batch_error)?;
    let mut cursor_mode = CapturedBatchCursorMode::Resume;
    let mut start_offset = 0_u64;
    let mut start_ordinal = 0_u64;
    let mut resumed_projector = None;

    if let Some(stored_cursor) = expected_store_cursor.as_ref() {
        match CertifiedProviderCursor::decode_if_certified(&stored_cursor.cursor)? {
            Some(certified)
                if certified.parser_revision() == source.capture_revision()
                    && certified.policy_revision() == source.policy_revision() =>
            {
                let source_revision_changed =
                    certified.source_revision() != source.source_revision();
                let can_resume = if !source_revision_changed {
                    true
                } else {
                    let file = open_native_jsonl_source_file(path)?;
                    if NativeJsonlFrozenFileMetadata::from_metadata(&file.metadata()?)? != frozen {
                        return Err(CaptureError::SourceChangedDuringCapture);
                    }
                    let mut reader = BufReader::new(file);
                    match verify_jsonl_append_boundary(
                        &mut reader,
                        certified.native_position(),
                        &source,
                        frozen.length,
                    ) {
                        Ok(verified_append) => {
                            cursor_mode = CapturedBatchCursorMode::ResumeAppend(verified_append);
                            true
                        }
                        Err(JsonlBatchError::Io(error)) => return Err(CaptureError::Io(error)),
                        Err(_) => false,
                    }
                };
                if can_resume {
                    match native_jsonl_resume_header_metadata(path, provider, &certified, &frozen)?
                    {
                        NativeJsonlResumeHeaderMetadata::Ready(normalized_header_metadata) => {
                            start_offset = jsonl_position_offset(certified.native_position())
                                .map_err(native_jsonl_batch_error)?;
                            let projector = NativeJsonlCapturedBatchProjector::resume(
                                provider,
                                source_format,
                                path,
                                projection_context.clone(),
                                &certified,
                                normalized_header_metadata,
                            )?;
                            start_ordinal = projector.next_ordinal;
                            resumed_projector = Some(projector);
                        }
                        NativeJsonlResumeHeaderMetadata::AnchorMismatch
                            if source_revision_changed =>
                        {
                            cursor_mode = CapturedBatchCursorMode::ResetChangedSource;
                        }
                        NativeJsonlResumeHeaderMetadata::AnchorMismatch => {
                            return Err(CaptureError::SourceChangedDuringCapture);
                        }
                    }
                } else {
                    cursor_mode = CapturedBatchCursorMode::ResetChangedSource;
                }
            }
            Some(_) => cursor_mode = CapturedBatchCursorMode::ResetChangedSource,
            None => cursor_mode = CapturedBatchCursorMode::ReplaceLegacyCursor,
        }
    }

    let mut projector = if let Some(projector) = resumed_projector {
        projector
    } else {
        NativeJsonlCapturedBatchProjector::fresh(
            provider,
            source_format,
            path,
            projection_context.clone(),
        )
    };
    if !frozen.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }

    let file = open_native_jsonl_source_file(path)?;
    if NativeJsonlFrozenFileMetadata::from_metadata(&file.metadata()?)? != frozen {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let mut producer = JsonlBatchProducer::new(
        BufReader::new(file),
        source.clone(),
        source_item,
        record_kind,
        frozen.length,
        start_offset,
        start_ordinal,
        false,
    )
    .map_err(native_jsonl_batch_error)?;
    let admission =
        CapturedSourceAdmission::conversation_for_context(&source, &projection_context)?;
    let mut imported_any = false;
    let summary = drain_captured_batches(
        store,
        &admission,
        import_options,
        &context.machine_id,
        context.imported_at,
        expected_store_cursor,
        &initial_position,
        cursor_mode,
        &stream,
        &mut projector,
        || {
            let batch = producer.next_batch().map_err(native_jsonl_batch_error)?;
            imported_any |= batch.is_some();
            Ok(batch)
        },
        || frozen.revalidate(path),
    )?;
    if !imported_any && had_expected_store_cursor {
        projector.replay_summary()
    } else {
        Ok(summary)
    }
}

fn native_jsonl_captured_batch_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}
