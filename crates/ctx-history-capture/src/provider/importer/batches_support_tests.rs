use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    fs::{self, OpenOptions},
    io::{Cursor, Seek, SeekFrom, Write},
    num::NonZeroUsize,
    path::Path,
    time::{Duration, SystemTime},
};

use crate::test_support_paths::tempdir;
use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    EventType, Fidelity, ProviderCaptureEnvelope, SessionStatus, SyncCursor, SyncMetadata,
};
use ctx_history_store::{ProviderSourceLocatorObservation, Store, StoreError};
use rusqlite::Connection;
use serde_json::json;

use crate::captured_batch::jsonl::{
    initial_jsonl_position, verify_jsonl_append_boundary, JsonlBatchProducer,
};
use crate::captured_batch::{
    CapturedBatch, CapturedBatchBuilder, CapturedBatchDropObserver, CapturedRecord, NativeLocator,
    NativePosition, ProviderRecordKind, SourceObservation, CAPTURE_BATCH_MAX_BATCHES_PER_GROUP,
};
use crate::provider::file_touches::provider_file_touches_from_event;
use crate::provider::importer::{
    import_provider_capture_line, provider_import_edge_uuid, provider_scoped_source_uuid,
    provider_source_identity, BoundedParserCheckpoint, CertifiedProviderCursor,
    ProviderImportCaches,
};
use crate::provider::providers::pi::{pi_session_capture, pi_session_header};
use crate::{
    CaptureError, NormalizedProviderImportOptions, ProviderAdapterContext,
    ProviderFileTouchedEnvelope, ProviderNormalizationResult, Result,
};

use super::super::cursors::{captured_batch_cursor_stream, certified_provider_sync_cursor};
use super::contracts::MAX_PROVIDER_RECORD_REJECTION_REASON_BYTES;
use super::*;

const TEST_MACHINE_ID: &str = "captured-batch-test-machine";

fn observed_at() -> DateTime<Utc> {
    DateTime::<Utc>::UNIX_EPOCH
}

fn initial_test_cursor(
    source: &SourceObservation,
    position: &NativePosition,
) -> Result<CertifiedProviderCursor> {
    CertifiedProviderCursor::new(
        source.source_revision(),
        source.capture_revision(),
        source.policy_revision(),
        position.clone(),
        BoundedParserCheckpoint::from_serializable(&())?,
    )
}

struct RejectingProjector {
    seen_ordinals: Vec<u64>,
    cursor_position: NativePosition,
}

struct CaptureProjector {
    capture: ProviderCaptureEnvelope,
    cursor_position: NativePosition,
}

struct BatchEndRejectingProjector {
    seen_ordinals: Vec<u64>,
}

struct MultiCaptureProjector {
    capture: ProviderCaptureEnvelope,
    cursor_position: NativePosition,
}

struct StreamingMultiEventProjector {
    captures: Vec<ProviderCaptureEnvelope>,
}

struct QueuedCaptureProjector {
    captures: VecDeque<ProviderCaptureEnvelope>,
}

struct ExistingSessionEventProjector {
    projections: VecDeque<(bool, ProviderCaptureEnvelope)>,
}

struct RetainingProjector {
    advance_at_ordinal: u64,
    seen_ordinals: Vec<u64>,
    capture: Option<ProviderCaptureEnvelope>,
    reject_records: bool,
}

struct ExplicitTouchProjector {
    capture: ProviderCaptureEnvelope,
    touches: Vec<(usize, ProviderFileTouchedEnvelope)>,
}

struct FinalMetadataProjector {
    record_capture: ProviderCaptureEnvelope,
    final_capture: ProviderCaptureEnvelope,
    cursor_position: NativePosition,
    final_hook_called: bool,
    final_line_number: Option<usize>,
}

impl CapturedBatchProjector for CaptureProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        output.emit_normalization(ProviderNormalizationResult {
            captures: vec![(
                usize::try_from(record.ordinal()).expect("fixture ordinal") + 1,
                self.capture.clone(),
            )],
            ..ProviderNormalizationResult::default()
        })
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        initial_test_cursor(source, position)
    }

    fn finish_cursor(&self, batch: &CapturedBatch) -> Result<CapturedBatchCursorFinish> {
        Ok(CapturedBatchCursorFinish::Advance(
            CertifiedProviderCursor::new(
                batch.source().source_revision(),
                batch.source().capture_revision(),
                batch.source().policy_revision(),
                self.cursor_position.clone(),
                BoundedParserCheckpoint::from_serializable(&())?,
            )?,
        ))
    }
}

impl CapturedBatchProjector for RejectingProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        self.seen_ordinals.push(record.ordinal());
        output.reject_record(
            usize::try_from(record.ordinal()).expect("fixture ordinal") + 1,
            "deterministic fixture rejection".to_owned(),
        );
        Ok(())
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        initial_test_cursor(source, position)
    }

    fn finish_cursor(&self, batch: &CapturedBatch) -> Result<CapturedBatchCursorFinish> {
        Ok(CapturedBatchCursorFinish::Advance(
            CertifiedProviderCursor::new(
                batch.source().source_revision(),
                batch.source().capture_revision(),
                batch.source().policy_revision(),
                self.cursor_position.clone(),
                BoundedParserCheckpoint::from_serializable(&())?,
            )?,
        ))
    }
}

impl CapturedBatchProjector for BatchEndRejectingProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        self.seen_ordinals.push(record.ordinal());
        output.reject_record(
            usize::try_from(record.ordinal()).expect("fixture ordinal") + 1,
            "deterministic multi-batch rejection".to_owned(),
        );
        Ok(())
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        initial_test_cursor(source, position)
    }

    fn finish_cursor(&self, batch: &CapturedBatch) -> Result<CapturedBatchCursorFinish> {
        Ok(CapturedBatchCursorFinish::Advance(
            CertifiedProviderCursor::new(
                batch.source().source_revision(),
                batch.source().capture_revision(),
                batch.source().policy_revision(),
                batch.range_end().clone(),
                BoundedParserCheckpoint::from_serializable(&())?,
            )?,
        ))
    }
}

impl CapturedBatchProjector for MultiCaptureProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        let line = usize::try_from(record.ordinal()).expect("fixture ordinal") + 1;
        output.emit_normalization(ProviderNormalizationResult {
            captures: vec![(line, self.capture.clone()), (line, self.capture.clone())],
            ..ProviderNormalizationResult::default()
        })
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        initial_test_cursor(source, position)
    }

    fn finish_cursor(&self, batch: &CapturedBatch) -> Result<CapturedBatchCursorFinish> {
        Ok(CapturedBatchCursorFinish::Advance(
            CertifiedProviderCursor::new(
                batch.source().source_revision(),
                batch.source().capture_revision(),
                batch.source().policy_revision(),
                self.cursor_position.clone(),
                BoundedParserCheckpoint::from_serializable(&())?,
            )?,
        ))
    }
}

impl CapturedBatchProjector for StreamingMultiEventProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        let line = usize::try_from(record.ordinal()).expect("fixture ordinal") + 1;
        for capture in std::mem::take(&mut self.captures) {
            output.emit_normalization(ProviderNormalizationResult {
                captures: vec![(line, capture)],
                ..ProviderNormalizationResult::default()
            })?;
        }
        Ok(())
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        initial_test_cursor(source, position)
    }

    fn finish_cursor(&self, batch: &CapturedBatch) -> Result<CapturedBatchCursorFinish> {
        Ok(CapturedBatchCursorFinish::Advance(
            CertifiedProviderCursor::new(
                batch.source().source_revision(),
                batch.source().capture_revision(),
                batch.source().policy_revision(),
                batch.range_end().clone(),
                BoundedParserCheckpoint::from_serializable(&())?,
            )?,
        ))
    }
}

impl CapturedBatchProjector for QueuedCaptureProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        let capture = self.captures.pop_front().ok_or_else(|| {
            ProviderProjectionFatal::system_invariant("fixture capture queue was exhausted")
        })?;
        output.emit_normalization(ProviderNormalizationResult {
            captures: vec![(
                usize::try_from(record.ordinal()).expect("fixture ordinal") + 1,
                capture,
            )],
            ..ProviderNormalizationResult::default()
        })
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        initial_test_cursor(source, position)
    }

    fn finish_cursor(&self, batch: &CapturedBatch) -> Result<CapturedBatchCursorFinish> {
        Ok(CapturedBatchCursorFinish::Advance(
            CertifiedProviderCursor::new(
                batch.source().source_revision(),
                batch.source().capture_revision(),
                batch.source().policy_revision(),
                batch.range_end().clone(),
                BoundedParserCheckpoint::from_serializable(&())?,
            )?,
        ))
    }
}

impl CapturedBatchProjector for ExistingSessionEventProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        let (existing_session, capture) = self.projections.pop_front().ok_or_else(|| {
            ProviderProjectionFatal::system_invariant(
                "existing-session fixture projection queue was exhausted",
            )
        })?;
        let line_number = usize::try_from(record.ordinal()).expect("fixture ordinal") + 1;
        if existing_session {
            output.emit_existing_session_event(line_number, capture)?;
            Ok(())
        } else {
            output.emit_normalization(ProviderNormalizationResult {
                captures: vec![(line_number, capture)],
                ..ProviderNormalizationResult::default()
            })
        }
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        initial_test_cursor(source, position)
    }

    fn finish_cursor(&self, batch: &CapturedBatch) -> Result<CapturedBatchCursorFinish> {
        Ok(CapturedBatchCursorFinish::Advance(
            CertifiedProviderCursor::new(
                batch.source().source_revision(),
                batch.source().capture_revision(),
                batch.source().policy_revision(),
                batch.range_end().clone(),
                BoundedParserCheckpoint::from_serializable(&())?,
            )?,
        ))
    }
}

impl CapturedBatchProjector for RetainingProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        self.seen_ordinals.push(record.ordinal());
        if self.reject_records {
            output.reject_record(
                usize::try_from(record.ordinal()).expect("fixture ordinal") + 1,
                "deterministic retained-frontier rejection".to_owned(),
            );
        }
        if let Some(capture) = self.capture.clone() {
            output.emit_normalization(ProviderNormalizationResult {
                captures: vec![(
                    usize::try_from(record.ordinal()).expect("fixture ordinal") + 1,
                    capture,
                )],
                ..ProviderNormalizationResult::default()
            })?;
        }
        Ok(())
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        initial_test_cursor(source, position)
    }

    fn finish_cursor(&self, batch: &CapturedBatch) -> Result<CapturedBatchCursorFinish> {
        let final_ordinal = batch.records().last().map(CapturedRecord::ordinal).ok_or(
            CaptureError::SystemInvariant("retaining fixture received an empty batch"),
        )?;
        if final_ordinal < self.advance_at_ordinal {
            return Ok(CapturedBatchCursorFinish::RetainPrior);
        }
        Ok(CapturedBatchCursorFinish::Advance(
            CertifiedProviderCursor::new(
                batch.source().source_revision(),
                batch.source().capture_revision(),
                batch.source().policy_revision(),
                batch.range_end().clone(),
                BoundedParserCheckpoint::from_serializable(&())?,
            )?,
        ))
    }
}

impl CapturedBatchProjector for ExplicitTouchProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        output.use_explicit_file_touches();
        let line = usize::try_from(record.ordinal()).expect("fixture ordinal") + 1;
        output.emit_normalization(ProviderNormalizationResult {
            captures: vec![(line, self.capture.clone())],
            ..ProviderNormalizationResult::default()
        })?;
        for touch in self.touches.clone() {
            output.emit_normalization(ProviderNormalizationResult {
                files_touched: vec![touch],
                ..ProviderNormalizationResult::default()
            })?;
        }
        Ok(())
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        initial_test_cursor(source, position)
    }

    fn finish_cursor(&self, batch: &CapturedBatch) -> Result<CapturedBatchCursorFinish> {
        Ok(CapturedBatchCursorFinish::Advance(
            CertifiedProviderCursor::new(
                batch.source().source_revision(),
                batch.source().capture_revision(),
                batch.source().policy_revision(),
                batch.range_end().clone(),
                BoundedParserCheckpoint::from_serializable(&())?,
            )?,
        ))
    }
}

impl CapturedBatchProjector for FinalMetadataProjector {
    fn project_record(
        &mut self,
        record: &CapturedRecord,
        output: &mut dyn ProviderProjectionOutput,
    ) -> ProviderProjectionResult<()> {
        output.emit_normalization(ProviderNormalizationResult {
            captures: vec![(
                usize::try_from(record.ordinal()).expect("fixture ordinal") + 1,
                self.record_capture.clone(),
            )],
            ..ProviderNormalizationResult::default()
        })
    }

    fn initial_cursor_candidate(
        &self,
        source: &SourceObservation,
        position: &NativePosition,
    ) -> Result<CertifiedProviderCursor> {
        initial_test_cursor(source, position)
    }

    fn final_metadata_capture(
        &mut self,
        batch: &CapturedBatch,
    ) -> ProviderProjectionResult<Option<(usize, ProviderCaptureEnvelope)>> {
        self.final_hook_called = true;
        let line_number = match self.final_line_number {
            Some(line_number) => line_number,
            None => batch
                .records()
                .last()
                .and_then(|record| usize::try_from(record.ordinal()).ok())
                .and_then(|ordinal| ordinal.checked_add(1))
                .ok_or_else(|| {
                    ProviderProjectionFatal::system_invariant(
                        "final metadata fixture received an empty batch",
                    )
                })?,
        };
        Ok(Some((line_number, self.final_capture.clone())))
    }

    fn finish_cursor(&self, batch: &CapturedBatch) -> Result<CapturedBatchCursorFinish> {
        if !self.final_hook_called {
            return Err(CaptureError::SystemInvariant(
                "final metadata hook did not run before cursor finish",
            ));
        }
        Ok(CapturedBatchCursorFinish::Advance(
            CertifiedProviderCursor::new(
                batch.source().source_revision(),
                batch.source().capture_revision(),
                batch.source().policy_revision(),
                self.cursor_position.clone(),
                BoundedParserCheckpoint::from_serializable(&())?,
            )?,
        ))
    }
}

fn test_position(value: u64) -> NativePosition {
    NativePosition::new("fixture-ordinal", value.to_be_bytes().to_vec())
        .expect("valid test position")
}

#[cfg(any(unix, target_os = "windows"))]
fn inventory_observed_test_source(
    path: &Path,
) -> (SourceObservation, CapturedSourceAdmission, SystemTime) {
    fs::write(path, b"inventory-version-a\n").expect("write inventory source");
    let modified = fs::metadata(path)
        .expect("inventory source metadata")
        .modified()
        .expect("inventory source mtime");
    let observation = crate::observe_ordinary_file(path).expect("observe inventory source");
    let token = observation.token_hex();
    let source = SourceObservation::new(
        CaptureProvider::Pi,
        "pi_session_jsonl",
        "fixture://captured-batch/inventory-observation",
        "inventory-observation-test-revision",
        "provider:pi:pi-jsonl-v1:source:inventory-observation",
        1,
        1,
        Some(&token),
    )
    .expect("inventory-observed source");
    let admission = CapturedSourceAdmission::conversation_for_context(
        &source,
        &ProviderAdapterContext {
            machine_id: TEST_MACHINE_ID.to_owned(),
            source_path: Some(path.to_path_buf()),
            source_root: None,
            imported_at: observed_at(),
        },
    )
    .expect("current inventory observation is admitted");
    (source, admission, modified)
}

#[cfg(any(unix, target_os = "windows"))]
fn rewrite_inventory_source_with_restored_mtime(path: &Path, modified: SystemTime) {
    // Keep creation and rewrite out of the same filesystem change-time tick;
    // mtime is restored below, while ctime/ChangeTime remains the signal.
    std::thread::sleep(Duration::from_millis(2));
    let mut file = OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open inventory source for rewrite");
    file.seek(SeekFrom::Start(0))
        .expect("seek inventory source");
    file.write_all(b"inventory-version-b\n")
        .expect("rewrite inventory source");
    file.set_times(std::fs::FileTimes::new().set_modified(modified))
        .expect("restore inventory source mtime");
    file.sync_all().expect("sync inventory source rewrite");
}

fn test_batch_from_observed_source(source: SourceObservation) -> CapturedBatch {
    let mut builder = CapturedBatchBuilder::new(source, test_position(0));
    builder
        .push(
            CapturedRecord::content(
                0,
                NativeLocator::new("fixture-record", 0_u64.to_be_bytes().to_vec())
                    .expect("valid locator"),
                ProviderRecordKind::new("fixture").expect("valid record kind"),
                b"record-0".to_vec(),
            )
            .expect("valid record"),
        )
        .expect("record fits batch");
    builder
        .finish(test_position(1))
        .expect("valid observed batch")
        .into_source_exhausted()
}

#[cfg(any(unix, target_os = "windows"))]
#[test]
fn unchanged_inventory_observation_reaches_the_initial_cursor_boundary() {
    let temp = tempdir().expect("tempdir");
    let source_path = temp.path().join("source.jsonl");
    let (source, admission, _) = inventory_observed_test_source(&source_path);
    let mut store = Store::open(temp.path().join("work.sqlite")).expect("open store");
    let next_batch_called = Cell::new(false);
    let mut projector = RejectingProjector {
        seen_ordinals: Vec::new(),
        cursor_position: test_position(0),
    };

    let outcome = import_captured_batches(
        &mut store,
        &admission,
        NormalizedProviderImportOptions::default(),
        TEST_MACHINE_ID,
        observed_at(),
        None,
        &test_position(0),
        CapturedBatchCursorMode::Resume,
        NonZeroUsize::new(CAPTURE_BATCH_MAX_BATCHES_PER_GROUP).expect("group limit"),
        &mut projector,
        || {
            next_batch_called.set(true);
            Ok(None)
        },
        || Ok(true),
    )
    .expect("unchanged observation imports");

    assert!(next_batch_called.get());
    assert!(outcome.source_exhausted);
    assert_eq!(outcome.batches_imported, 0);
    assert!(store
        .get_sync_cursor(None, TEST_MACHINE_ID, source.cursor_stream())
        .expect("read cursor")
        .is_some());
}

#[cfg(any(unix, target_os = "windows"))]
#[test]
fn stale_inventory_observation_fails_before_the_first_batch_fetch() {
    let temp = tempdir().expect("tempdir");
    let source_path = temp.path().join("source.jsonl");
    let (source, admission, modified) = inventory_observed_test_source(&source_path);
    rewrite_inventory_source_with_restored_mtime(&source_path, modified);
    let mut store = Store::open(temp.path().join("work.sqlite")).expect("open store");
    let next_batch_called = Cell::new(false);
    let mut projector = RejectingProjector {
        seen_ordinals: Vec::new(),
        cursor_position: test_position(0),
    };

    let error = import_captured_batches(
        &mut store,
        &admission,
        NormalizedProviderImportOptions::default(),
        TEST_MACHINE_ID,
        observed_at(),
        None,
        &test_position(0),
        CapturedBatchCursorMode::Resume,
        NonZeroUsize::new(CAPTURE_BATCH_MAX_BATCHES_PER_GROUP).expect("group limit"),
        &mut projector,
        || {
            next_batch_called.set(true);
            Ok(None)
        },
        || Ok(true),
    )
    .expect_err("stale inventory observation must fail closed");

    assert!(matches!(error, CaptureError::SourceChangedDuringCapture));
    assert!(!next_batch_called.get());
    assert!(store
        .get_sync_cursor(None, TEST_MACHINE_ID, source.cursor_stream())
        .expect("read cursor")
        .is_none());
}

#[cfg(any(unix, target_os = "windows"))]
#[test]
fn inventory_change_after_fetch_fails_before_cursor_cas() {
    let temp = tempdir().expect("tempdir");
    let source_path = temp.path().join("source.jsonl");
    let (source, admission, modified) = inventory_observed_test_source(&source_path);
    let stream = source.cursor_stream().to_owned();
    let mut pending_batch = Some(test_batch_from_observed_source(source));
    let mut store = Store::open(temp.path().join("work.sqlite")).expect("open store");
    let mut projector = RejectingProjector {
        seen_ordinals: Vec::new(),
        cursor_position: test_position(1),
    };
    let fetched = Cell::new(false);

    let error = import_captured_batches(
        &mut store,
        &admission,
        NormalizedProviderImportOptions::default(),
        TEST_MACHINE_ID,
        observed_at(),
        None,
        &test_position(0),
        CapturedBatchCursorMode::Resume,
        NonZeroUsize::new(CAPTURE_BATCH_MAX_BATCHES_PER_GROUP).expect("group limit"),
        &mut projector,
        || {
            let batch = pending_batch.take();
            if batch.is_some() {
                fetched.set(true);
                rewrite_inventory_source_with_restored_mtime(&source_path, modified);
            }
            Ok(batch)
        },
        || Ok(true),
    )
    .expect_err("post-fetch inventory change must fail before cursor commit");

    assert!(matches!(error, CaptureError::SourceChangedDuringCapture));
    assert!(fetched.get());
    assert!(store
        .get_sync_cursor(None, TEST_MACHINE_ID, &stream)
        .expect("read cursor")
        .is_none());
}

fn test_batch(source_revision: &str, range_before: u64, ordinals: &[u64]) -> CapturedBatch {
    test_batch_for_source(
        CaptureProvider::Pi,
        "pi_session_jsonl",
        "provider:pi:pi-jsonl-v1:source:test",
        source_revision,
        range_before,
        ordinals,
    )
}

fn test_batch_for_source(
    provider: CaptureProvider,
    source_format: &str,
    cursor_stream: &str,
    source_revision: &str,
    range_before: u64,
    ordinals: &[u64],
) -> CapturedBatch {
    let source = SourceObservation::new(
        provider,
        source_format,
        "fixture://captured-batch/projector",
        source_revision,
        cursor_stream,
        1,
        1,
        None,
    )
    .expect("valid source observation");
    let mut builder = CapturedBatchBuilder::new(source, test_position(range_before));
    for ordinal in ordinals {
        builder
            .push(
                CapturedRecord::content(
                    *ordinal,
                    NativeLocator::new("fixture-record", ordinal.to_be_bytes().to_vec())
                        .expect("valid locator"),
                    ProviderRecordKind::new("fixture").expect("valid record kind"),
                    format!("record-{ordinal}").into_bytes(),
                )
                .expect("valid record"),
            )
            .expect("record fits batch");
    }
    let range_end = range_before + u64::try_from(ordinals.len()).expect("fixture length");
    builder
        .finish(test_position(range_end))
        .expect("valid batch")
}

fn cursor_for_batch(batch: &CapturedBatch, position: NativePosition) -> SyncCursor {
    let cursor = CertifiedProviderCursor::new(
        batch.source().source_revision(),
        batch.source().capture_revision(),
        batch.source().policy_revision(),
        position,
        BoundedParserCheckpoint::from_serializable(&()).expect("valid fixture checkpoint"),
    )
    .expect("valid cursor");
    certified_provider_sync_cursor(
        batch.source().provider(),
        TEST_MACHINE_ID,
        captured_batch_cursor_stream(batch.source()),
        &cursor,
        observed_at(),
    )
    .expect("valid sync cursor")
}

fn projected_pi_capture() -> ProviderCaptureEnvelope {
    let header = pi_session_header(json!({
        "type": "session",
        "id": "captured-batch-projection",
        "timestamp": "2026-07-17T12:00:00Z",
        "cwd": "/workspace"
    }))
    .expect("valid Pi header");
    pi_session_capture(
        &header,
        Some(&json!({
            "type": "message",
            "id": "captured-batch-message",
            "timestamp": "2026-07-17T12:00:01Z",
            "message": {"role": "user", "content": [{"type": "text", "text": "hello"}]}
        })),
        1,
        &ProviderAdapterContext {
            machine_id: TEST_MACHINE_ID.to_owned(),
            source_path: Some("/tmp/captured-batch.jsonl".into()),
            source_root: None,
            imported_at: observed_at(),
        },
    )
    .expect("valid Pi capture")
}

fn projected_warp_capture() -> ProviderCaptureEnvelope {
    let mut capture = projected_pi_capture();
    capture.provider = CaptureProvider::Warp;
    capture.source.source_format = crate::WARP_SQLITE_SOURCE_FORMAT.to_owned();
    capture
}

#[test]
fn deterministic_rejection_requires_a_bounded_nonempty_reason() {
    let reason = bounded_provider_rejection_reason(String::new());
    assert!(!reason.is_empty());
    let reason = bounded_provider_rejection_reason(
        "x".repeat(MAX_PROVIDER_RECORD_REJECTION_REASON_BYTES + 1),
    );
    assert_eq!(reason.len(), MAX_PROVIDER_RECORD_REJECTION_REASON_BYTES);
}

#[test]
fn final_metadata_refresh_precedes_cursor_finish_and_does_not_change_summary() {
    let temp = tempdir().expect("tempdir");
    let mut store = Store::open(temp.path().join("work.sqlite")).expect("open store");
    let batch = test_batch("source-revision-final-metadata", 0, &[0]);
    let record_capture = projected_pi_capture();
    let mut final_capture = record_capture.clone();
    final_capture.event = None;
    final_capture.session.metadata = json!({ "batch_final": "last" });
    let mut projector = FinalMetadataProjector {
        record_capture,
        final_capture,
        cursor_position: batch.range_end().clone(),
        final_hook_called: false,
        final_line_number: None,
    };

    let outcome = import_captured_batch(
        &mut store,
        &CapturedSourceAdmission::conversation_without_cross_record_relationships(batch.source()),
        &batch,
        NormalizedProviderImportOptions::default(),
        TEST_MACHINE_ID,
        observed_at(),
        None,
        &test_position(0),
        CapturedBatchCursorMode::Resume,
        &mut projector,
        || Ok(true),
    )
    .expect("import final metadata refresh");

    assert!(projector.final_hook_called);
    assert_eq!(outcome.summary.imported_sessions, 1);
    assert_eq!(outcome.summary.imported_events, 1);
    assert_eq!(outcome.summary.skipped_sessions, 0);
    let session = store
        .session_by_external_session(CaptureProvider::Pi, "captured-batch-projection")
        .expect("read final session")
        .expect("persisted final session");
    assert_eq!(session.sync.metadata["metadata"]["batch_final"], "last");
}

#[test]
fn final_metadata_refresh_rejects_eventful_envelopes_and_rolls_back() {
    let temp = tempdir().expect("tempdir");
    let mut store = Store::open(temp.path().join("work.sqlite")).expect("open store");
    let batch = test_batch("source-revision-eventful-final-metadata", 0, &[0]);
    let capture = projected_pi_capture();
    let mut projector = FinalMetadataProjector {
        record_capture: capture.clone(),
        final_capture: capture,
        cursor_position: batch.range_end().clone(),
        final_hook_called: false,
        final_line_number: None,
    };

    let error = import_captured_batch(
        &mut store,
        &CapturedSourceAdmission::conversation_without_cross_record_relationships(batch.source()),
        &batch,
        NormalizedProviderImportOptions::default(),
        TEST_MACHINE_ID,
        observed_at(),
        None,
        &test_position(0),
        CapturedBatchCursorMode::Resume,
        &mut projector,
        || Ok(true),
    )
    .expect_err("eventful final metadata must fail closed");

    assert!(matches!(error, CaptureError::SystemInvariant(_)));
    assert!(projector.final_hook_called);
    assert!(store.list_sessions().expect("list sessions").is_empty());
    assert!(store
        .get_sync_cursor(None, TEST_MACHINE_ID, batch.source().cursor_stream())
        .expect("read cursor")
        .is_none());
}

#[test]
fn final_metadata_refresh_rejects_lines_outside_the_batch() {
    let temp = tempdir().expect("tempdir");
    let mut store = Store::open(temp.path().join("work.sqlite")).expect("open store");
    let batch = test_batch("source-revision-invalid-final-line", 0, &[0]);
    let record_capture = projected_pi_capture();
    let mut final_capture = record_capture.clone();
    final_capture.event = None;
    let mut projector = FinalMetadataProjector {
        record_capture,
        final_capture,
        cursor_position: batch.range_end().clone(),
        final_hook_called: false,
        final_line_number: Some(99),
    };

    let error = import_captured_batch(
        &mut store,
        &CapturedSourceAdmission::conversation_without_cross_record_relationships(batch.source()),
        &batch,
        NormalizedProviderImportOptions::default(),
        TEST_MACHINE_ID,
        observed_at(),
        None,
        &test_position(0),
        CapturedBatchCursorMode::Resume,
        &mut projector,
        || Ok(true),
    )
    .expect_err("out-of-batch final metadata line must fail closed");

    assert!(matches!(error, CaptureError::SystemInvariant(_)));
    assert!(projector.final_hook_called);
    assert!(store.list_sessions().expect("list sessions").is_empty());
}

#[test]
fn transaction_rotation_defers_wal_checkpoint_past_pinned_readers() {
    let temp = tempdir().expect("tempdir");
    let store_path = temp.path().join("work.sqlite");
    let store = Store::open(&store_path).expect("open store");
    let reader = Connection::open(&store_path).expect("open reader");
    reader.execute_batch("BEGIN").expect("begin read");
    let _: i64 = reader
        .query_row("select count(*) from events", [], |row| row.get(0))
        .expect("pin read snapshot");

    let batch = test_batch("source-revision-rotation", 0, &[0]);
    let mut cursor = cursor_for_batch(&batch, test_position(0));
    let mut transaction =
        ProviderImportTransaction::begin_projection(&store).expect("begin provider transaction");
    for index in 0..=IMPORT_TRANSACTION_BATCH_UNITS {
        transaction
            .prepare_unit(&store, 1)
            .expect("prepare bounded Store unit");
        cursor.cursor = format!("rotation-{index}");
        store.upsert_sync_cursor(&cursor).expect("write cursor");
        transaction
            .record_unit(&store, 1)
            .expect("rotation does not checkpoint against pinned reader");
    }
    transaction.commit(&store).expect("commit final slice");
    assert_eq!(transaction.committed_transactions(), 2);

    reader.execute_batch("ROLLBACK").expect("release reader");
    store
        .checkpoint_wal_truncate_required()
        .expect("group owner can checkpoint after the reader releases");
}

#[test]
fn exactly_64_events_commit_once_and_65_commit_twice() {
    for (event_count, expected_commits) in [
        (IMPORT_TRANSACTION_BATCH_UNITS, 1),
        (IMPORT_TRANSACTION_BATCH_UNITS + 1, 2),
    ] {
        let temp = tempdir().expect("tempdir");
        let mut store = Store::open(temp.path().join("work.sqlite")).expect("open event Store");
        let batch = test_batch(
            &format!("source-revision-{event_count}-event-rotation"),
            0,
            &[0],
        );
        let template = projected_pi_capture();
        let captures = (0..event_count)
            .map(|index| {
                let mut capture = template.clone();
                let event = capture.event.as_mut().expect("fixture event");
                let provider_index = u64::try_from(index + 1).expect("provider index");
                let identity = format!("transaction-event-{provider_index}");
                event.provider_event_index = provider_index;
                event.provider_event_hash = Some(identity.clone());
                event.idempotency_key = Some(identity.clone());
                event.metadata["entry_id"] = json!(identity);
                event.metadata["provider_event_identity_index"] = json!(provider_index);
                event.metadata["legacy_provider_event_index"] = json!(provider_index);
                event.payload["text"] = json!(format!("event {provider_index}"));
                capture
            })
            .collect::<Vec<_>>();
        let mut projector = StreamingMultiEventProjector { captures };

        reset_provider_transaction_commits();
        let outcome = import_captured_batch(
            &mut store,
            &CapturedSourceAdmission::conversation_without_cross_record_relationships(
                batch.source(),
            ),
            &batch,
            NormalizedProviderImportOptions::default(),
            TEST_MACHINE_ID,
            observed_at(),
            None,
            &test_position(0),
            CapturedBatchCursorMode::Resume,
            &mut projector,
            || Ok(true),
        )
        .expect("import bounded event batch");

        assert_eq!(outcome.summary.imported_events, event_count);
        assert_eq!(provider_transaction_commits(), expected_commits);
    }
}

#[test]
fn transaction_rotation_is_lazy_at_the_exact_byte_boundary() {
    let temp = tempdir().expect("tempdir");
    let store = Store::open(temp.path().join("work.sqlite")).expect("open store");
    let batch = test_batch("source-revision-lazy-rotation", 0, &[0]);
    let mut cursor = cursor_for_batch(&batch, test_position(0));

    let mut exact_bytes =
        ProviderImportTransaction::begin_projection(&store).expect("begin exact-byte import");
    exact_bytes
        .prepare_unit(&store, IMPORT_TRANSACTION_BATCH_BYTES)
        .expect("prepare exact byte limit");
    cursor.cursor = "exact-byte-limit".to_owned();
    store
        .upsert_sync_cursor(&cursor)
        .expect("write exact-byte unit");
    exact_bytes
        .record_unit(&store, IMPORT_TRANSACTION_BATCH_BYTES)
        .expect("record exact byte limit");
    assert_eq!(exact_bytes.committed_transactions(), 0);
    exact_bytes.commit(&store).expect("commit exact byte limit");
    assert_eq!(exact_bytes.committed_transactions(), 1);

    let mut over_bytes =
        ProviderImportTransaction::begin_projection(&store).expect("begin byte-overflow import");
    over_bytes
        .prepare_unit(&store, IMPORT_TRANSACTION_BATCH_BYTES)
        .expect("prepare first byte slice");
    cursor.cursor = "byte-slice-one".to_owned();
    store
        .upsert_sync_cursor(&cursor)
        .expect("write first byte slice");
    over_bytes
        .record_unit(&store, IMPORT_TRANSACTION_BATCH_BYTES)
        .expect("record first byte slice");
    over_bytes
        .prepare_unit(&store, 1)
        .expect("rotate before overflowing byte limit");
    assert_eq!(over_bytes.committed_transactions(), 1);
    cursor.cursor = "byte-slice-two".to_owned();
    store
        .upsert_sync_cursor(&cursor)
        .expect("write second byte slice");
    over_bytes
        .record_unit(&store, 1)
        .expect("record second byte slice");
    over_bytes.commit(&store).expect("commit second byte slice");
    assert_eq!(over_bytes.committed_transactions(), 2);
}

#[test]
fn deterministic_rejections_walk_every_record_and_publish_only_the_batch_end() {
    let temp = tempdir().expect("tempdir");
    let mut store = Store::open(temp.path().join("work.sqlite")).expect("open store");
    let batch = test_batch("source-revision-1", 0, &[0, 1, 2]);
    let mut projector = RejectingProjector {
        seen_ordinals: Vec::new(),
        cursor_position: batch.range_end().clone(),
    };

    let outcome = import_captured_batch(
        &mut store,
        &CapturedSourceAdmission::conversation_without_cross_record_relationships(batch.source()),
        &batch,
        NormalizedProviderImportOptions::default(),
        TEST_MACHINE_ID,
        observed_at(),
        None,
        &test_position(0),
        CapturedBatchCursorMode::Resume,
        &mut projector,
        || Ok(true),
    )
    .expect("import deterministic rejections");

    assert_eq!(projector.seen_ordinals, vec![0, 1, 2]);
    assert_eq!(outcome.summary.failed, 3);
    assert_eq!(outcome.summary.failures.len(), 3);
    let stored = store
        .get_sync_cursor(
            None,
            TEST_MACHINE_ID,
            &captured_batch_cursor_stream(batch.source()),
        )
        .expect("read cursor")
        .expect("published cursor");
    let certified = CertifiedProviderCursor::decode(&stored.cursor).expect("decode cursor");
    assert_eq!(certified.native_position(), batch.range_end());
    assert_eq!(certified.rejected_records(), 3);

    let replay = import_captured_batches(
        &mut store,
        &CapturedSourceAdmission::conversation_without_cross_record_relationships(batch.source()),
        NormalizedProviderImportOptions::default(),
        TEST_MACHINE_ID,
        observed_at(),
        Some(&stored),
        &test_position(0),
        CapturedBatchCursorMode::Resume,
        NonZeroUsize::new(1).unwrap(),
        &mut projector,
        || Ok(None),
        || Ok(true),
    )
    .expect("certified no-op replay");
    assert_eq!(replay.batches_imported, 0);
    assert_eq!(replay.summary.failed, 3);
    assert!(replay.summary.failures.is_empty());
}

#[test]
fn source_scoped_import_publishes_only_the_last_of_multiple_batch_cursors() {
    let temp = tempdir().expect("tempdir");
    let mut store = Store::open(temp.path().join("work.sqlite")).expect("open store");
    let first = test_batch("source-revision-1", 0, &[0]);
    let final_position = test_position(2);
    let mut batches = VecDeque::from([
        first,
        test_batch("source-revision-1", 1, &[1]).into_source_exhausted(),
    ]);
    let admission = CapturedSourceAdmission::conversation_without_cross_record_relationships(
        batches.front().expect("first batch").source(),
    );
    let mut projector = BatchEndRejectingProjector {
        seen_ordinals: Vec::new(),
    };
    let revalidations = Cell::new(0_u32);

    let outcome = import_captured_batches(
        &mut store,
        &admission,
        NormalizedProviderImportOptions::default(),
        TEST_MACHINE_ID,
        observed_at(),
        None,
        &test_position(0),
        CapturedBatchCursorMode::Resume,
        NonZeroUsize::new(4).expect("nonzero group limit"),
        &mut projector,
        || Ok(batches.pop_front()),
        || {
            revalidations.set(revalidations.get().saturating_add(1));
            Ok(true)
        },
    )
    .expect("import source-scoped batches");

    assert_eq!(outcome.batches_imported, 2);
    assert!(outcome.source_exhausted);
    assert_eq!(outcome.summary.failed, 2);
    assert_eq!(projector.seen_ordinals, vec![0, 1]);
    assert_eq!(revalidations.get(), 1);
    let stored = store
        .get_sync_cursor(None, TEST_MACHINE_ID, "provider:pi:pi-jsonl-v1:source:test")
        .expect("read cursor")
        .expect("final cursor");
    let certified = CertifiedProviderCursor::decode(&stored.cursor).expect("decode cursor");
    assert_eq!(certified.native_position(), &final_position);
}

#[test]
fn exhausted_short_group_retains_final_batch_through_cursor_commit_without_refetch() {
    let temp = tempdir().expect("tempdir");
    let mut store = Store::open(temp.path().join("work.sqlite")).expect("open store");
    let observer = CapturedBatchDropObserver::new();
    let final_batch = test_batch("source-revision-short-eof", 1, &[1])
        .into_source_exhausted()
        .with_drop_observer(observer.clone());
    let source = final_batch.source().clone();
    let admission =
        CapturedSourceAdmission::conversation_without_cross_record_relationships(&source);
    let mut batches = VecDeque::from([
        test_batch("source-revision-short-eof", 0, &[0]),
        final_batch,
    ]);
    let requests = Cell::new(0_usize);
    let mut projector = BatchEndRejectingProjector {
        seen_ordinals: Vec::new(),
    };

    let outcome = import_captured_batches(
        &mut store,
        &admission,
        NormalizedProviderImportOptions::default(),
        TEST_MACHINE_ID,
        observed_at(),
        None,
        &test_position(0),
        CapturedBatchCursorMode::Resume,
        NonZeroUsize::new(CAPTURE_BATCH_MAX_BATCHES_PER_GROUP).expect("group limit"),
        &mut projector,
        || {
            requests.set(requests.get().saturating_add(1));
            Ok(batches.pop_front())
        },
        || {
            assert_eq!(
                observer.observed_drops(),
                0,
                "the exhausted final batch must outlive bulk-search completion and revalidation"
            );
            Ok(true)
        },
    )
    .expect("import tagged short exhausted group");

    assert!(outcome.source_exhausted);
    assert!(outcome.cursor_safe);
    assert_eq!(outcome.batches_imported, 2);
    assert_eq!(requests.get(), 2, "EOF must not require a third batch poll");
    assert_eq!(observer.observed_drops(), 1);
    assert!(store
        .get_sync_cursor(None, TEST_MACHINE_ID, source.cursor_stream())
        .expect("read cursor")
        .is_some());
}

#[test]
fn exhausted_short_group_releases_final_batch_after_retryable_source_failure() {
    let temp = tempdir().expect("tempdir");
    let mut store = Store::open(temp.path().join("work.sqlite")).expect("open store");
    let observer = CapturedBatchDropObserver::new();
    let final_batch = test_batch("source-revision-short-source-change", 0, &[0])
        .into_source_exhausted()
        .with_drop_observer(observer.clone());
    let source = final_batch.source().clone();
    let admission =
        CapturedSourceAdmission::conversation_without_cross_record_relationships(&source);
    let mut batch = Some(final_batch);
    let mut projector = BatchEndRejectingProjector {
        seen_ordinals: Vec::new(),
    };

    let error = import_captured_batches(
        &mut store,
        &admission,
        NormalizedProviderImportOptions::default(),
        TEST_MACHINE_ID,
        observed_at(),
        None,
        &test_position(0),
        CapturedBatchCursorMode::Resume,
        NonZeroUsize::new(CAPTURE_BATCH_MAX_BATCHES_PER_GROUP).expect("group limit"),
        &mut projector,
        || Ok(batch.take()),
        || {
            assert_eq!(observer.observed_drops(), 0);
            Ok(false)
        },
    )
    .expect_err("source change remains retryable after group maintenance");

    assert!(matches!(error, CaptureError::SourceChangedDuringCapture));
    assert_eq!(observer.observed_drops(), 1);
    assert!(store
        .get_sync_cursor(None, TEST_MACHINE_ID, source.cursor_stream())
        .expect("read cursor")
        .is_none());
}
