use ctx_history_capture_runtime::SourceBackedRecordRejectionDrafts;
use ctx_history_core::{CoreRecord, SourceKey, TypedKey};

use super::super::{
    JsonlFamilyRuntime, JsonlReader, JsonlRecordRef, JsonlResult, JsonlRuntimeError,
};
use super::JsonlFamilyWorkerContext;

/// Explicit failure scope for provider semantic preflight.
///
/// Ordinary runtime errors stay internal to the leaf scan and retain their
/// existing route-level classification. A provider may select the logical
/// source variant only after a complete pre-staging pass proves that one
/// exactly owned source is independently invalid. Record rejections are
/// handled by the projector while it keeps scanning; surfacing one here is a
/// contract violation because the shared family cannot safely resume an
/// interrupted preflight callback.
#[derive(Debug)]
pub enum JsonlFamilyProjectorPreflightError<E> {
    RecordRejection {
        detail: String,
    },
    LogicalSourceFailure {
        source: Box<SourceKey>,
        detail: String,
    },
    Internal(E),
}

impl<E> JsonlFamilyProjectorPreflightError<E> {
    pub fn record_rejection(detail: impl Into<String>) -> Self {
        Self::RecordRejection {
            detail: detail.into(),
        }
    }

    pub fn logical_source_failure(source: SourceKey, detail: impl Into<String>) -> Self {
        Self::LogicalSourceFailure {
            source: Box::new(source),
            detail: detail.into(),
        }
    }

    pub fn internal(error: E) -> Self {
        Self::Internal(error)
    }
}

impl<E> From<E> for JsonlFamilyProjectorPreflightError<E> {
    fn from(error: E) -> Self {
        Self::Internal(error)
    }
}

pub trait JsonlFamilyProjector: Send {
    type Runtime: JsonlFamilyRuntime;

    /// Classifies only provider semantic failures proven before writer
    /// staging. Existing projectors inherit the route-fatal runtime behavior;
    /// adapters that can prove one exact logical source invalid may override
    /// this method without changing every projector's ordinary error type.
    fn preflight_with_failure_scope(
        &mut self,
        reader: &mut JsonlReader<JsonlRuntimeError<Self::Runtime>>,
        certified_prefix_end: Option<u64>,
    ) -> JsonlResult<bool, JsonlFamilyProjectorPreflightError<JsonlRuntimeError<Self::Runtime>>>
    {
        self.preflight(reader, certified_prefix_end)
            .map_err(JsonlFamilyProjectorPreflightError::Internal)
    }

    fn preflight(
        &mut self,
        _reader: &mut JsonlReader<JsonlRuntimeError<Self::Runtime>>,
        _certified_prefix_end: Option<u64>,
    ) -> JsonlResult<bool, JsonlRuntimeError<Self::Runtime>> {
        Ok(false)
    }

    fn retry_replacement(&mut self) {}

    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        worker: &mut JsonlFamilyWorkerContext<Self::Runtime>,
        emit: &mut dyn FnMut(CoreRecord) -> JsonlResult<(), JsonlRuntimeError<Self::Runtime>>,
    ) -> JsonlResult<(), JsonlRuntimeError<Self::Runtime>>;

    fn finish(&mut self) -> JsonlResult<(), JsonlRuntimeError<Self::Runtime>> {
        Ok(())
    }

    fn finish_projecting(
        &mut self,
        _worker: &mut JsonlFamilyWorkerContext<Self::Runtime>,
        _emit: &mut dyn FnMut(CoreRecord) -> JsonlResult<(), JsonlRuntimeError<Self::Runtime>>,
    ) -> JsonlResult<(), JsonlRuntimeError<Self::Runtime>> {
        self.finish()
    }

    fn rejected_records(&self) -> u64 {
        0
    }

    fn take_record_rejections(&mut self) -> SourceBackedRecordRejectionDrafts {
        SourceBackedRecordRejectionDrafts::default()
    }

    /// Opaque, contract-bounded provider state to carry into the next certified
    /// suffix projection. The family persists the value without interpreting it.
    fn provider_checkpoint(
        &self,
    ) -> JsonlResult<Option<TypedKey>, JsonlRuntimeError<Self::Runtime>> {
        Ok(None)
    }
}
