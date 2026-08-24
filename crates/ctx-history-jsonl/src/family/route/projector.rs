use ctx_history_capture_runtime::SourceBackedRecordRejectionDrafts;
use ctx_history_core::{CoreRecord, TypedKey};

use super::super::{
    JsonlFamilyRuntime, JsonlReader, JsonlRecordRef, JsonlResult, JsonlRuntimeError,
};
use super::JsonlFamilyWorkerContext;

pub trait JsonlFamilyProjector: Send {
    type Runtime: JsonlFamilyRuntime;

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
