use ctx_history_core::{CoreRecord, SourceKey};
use ctx_history_provider_runtime::{
    CaptureError, JsonlFamilyProjector, JsonlRecordRef, ProviderJsonlRuntime,
    ProviderJsonlWorkerContext, ProviderRuntimeBinding,
};

use crate::{
    project_canonical_state, replay_legacy_snapshot, LegacyDefaults, ProjectionBinding,
    ReplayLimits,
};

use super::fx_error;

pub(super) struct FxLegacyProjector<B: ProviderRuntimeBinding> {
    source: SourceKey,
    defaults: LegacyDefaults,
    projected: bool,
    binding: std::marker::PhantomData<fn() -> B>,
}

impl<B: ProviderRuntimeBinding> FxLegacyProjector<B> {
    pub(super) fn new(source: SourceKey, defaults: LegacyDefaults) -> Self {
        Self {
            source,
            defaults,
            projected: false,
            binding: std::marker::PhantomData,
        }
    }
}

impl<B: ProviderRuntimeBinding> JsonlFamilyProjector for FxLegacyProjector<B> {
    type Runtime = ProviderJsonlRuntime<B>;

    fn project(
        &mut self,
        record: JsonlRecordRef<'_>,
        _worker: &mut ProviderJsonlWorkerContext<B>,
        emit: &mut dyn FnMut(CoreRecord) -> Result<(), CaptureError>,
    ) -> Result<(), CaptureError> {
        if self.projected {
            return Err(CaptureError::InvalidPayload(
                "fx legacy snapshot contains more than one whole record".to_owned(),
            ));
        }
        if record.oversized() {
            return Err(CaptureError::InvalidPayload(
                "fx legacy snapshot exceeds the whole-record limit".to_owned(),
            ));
        }
        let replay =
            replay_legacy_snapshot(record.bytes(), &self.defaults, ReplayLimits::default())
                .map_err(fx_error)?;
        for projected in project_canonical_state(
            ProjectionBinding {
                source: &self.source,
                native_session_id: &replay.state.id,
            },
            &replay.state,
        )
        .map_err(fx_error)?
        {
            emit(projected)?;
        }
        self.projected = true;
        Ok(())
    }

    fn finish(&mut self) -> Result<(), CaptureError> {
        if self.projected {
            Ok(())
        } else {
            Err(CaptureError::InvalidPayload(
                "fx legacy snapshot is empty".to_owned(),
            ))
        }
    }
}
