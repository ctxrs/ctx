use std::{cell::RefCell, path::PathBuf};

use ctx_history_core::{
    CaptureProvider, ProviderCaptureEnvelope,
    PROVIDER_CAPTURE_ENVELOPE_MIN_SUPPORTED_SCHEMA_VERSION,
    PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
};

use crate::captured_batch::{CapturedBatch, CapturedDataClassification, SourceObservation};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderFileTouchedEnvelope, ProviderNormalizationResult,
    Result,
};

use super::super::commands::validate_provider_event_for_import;
use super::super::ids::provider_source_identity;

pub(crate) struct CapturedSourceAdmission {
    pub(super) source: SourceObservation,
    pub(super) scope: CapturedProjectionScope,
    inventory_observation: Option<CapturedInventoryObservation>,
    pub(super) locator_resolution:
        RefCell<Option<ctx_history_store::ProviderSourceLocatorResolution>>,
}

struct CapturedInventoryObservation {
    path: PathBuf,
    token: String,
}

#[derive(Clone)]
pub(super) struct CapturedProjectionScope {
    pub(super) provider: CaptureProvider,
    pub(super) source_format: String,
    pub(super) machine_id: String,
    pub(super) raw_source_path: Option<String>,
    pub(super) source_root: Option<String>,
    // Physical capture sources may be individual files within one conversation root.
    pub(super) stable_source_identity: Option<String>,
    // File-scoped sources keep sessions grouped by their conversation root.
    pub(super) stable_session_identity: Option<String>,
}

impl CapturedProjectionScope {
    fn new(source: &SourceObservation, context: &ProviderAdapterContext) -> Self {
        Self::with_file_scoped_identity(source, context, false)
    }

    fn for_file(source: &SourceObservation, context: &ProviderAdapterContext) -> Self {
        Self::with_file_scoped_identity(source, context, true)
    }

    fn with_file_scoped_identity(
        source: &SourceObservation,
        context: &ProviderAdapterContext,
        file_scoped: bool,
    ) -> Self {
        let raw_source_path = context
            .source_path
            .as_ref()
            .map(|path| path.display().to_string());
        let source_root = context.source_root_display();
        let stable_session_identity = if file_scoped {
            provider_source_identity(
                source.provider(),
                source.source_format(),
                source_root.as_deref(),
                raw_source_path.as_deref(),
                None,
                &serde_json::Value::Null,
            )
        } else {
            None
        };
        let identity_source_root = if file_scoped {
            None
        } else {
            source_root.as_deref()
        };
        let stable_source_identity = provider_source_identity(
            source.provider(),
            source.source_format(),
            identity_source_root,
            raw_source_path.as_deref(),
            None,
            &serde_json::Value::Null,
        );
        Self {
            provider: source.provider(),
            source_format: source.source_format().to_owned(),
            machine_id: context.machine_id.clone(),
            raw_source_path,
            source_root,
            stable_source_identity,
            stable_session_identity,
        }
    }

    pub(super) fn validate_capture(&self, capture: &ProviderCaptureEnvelope) -> Result<()> {
        if capture.provider != self.provider
            || capture.source.source_format != self.source_format
            || capture.source.machine_id != self.machine_id
            || capture.source.raw_source_path != self.raw_source_path
            || capture.source.source_root != self.source_root
        {
            return Err(CaptureError::SystemInvariant(
                "projected provider capture does not match its admitted source scope",
            ));
        }
        Ok(())
    }

    pub(super) fn validate_file_touch(&self, file: &ProviderFileTouchedEnvelope) -> Result<()> {
        if file.provider != self.provider
            || file.source_format != self.source_format
            || file.raw_source_path != self.raw_source_path
            || file.source_root != self.source_root
        {
            return Err(CaptureError::SystemInvariant(
                "projected provider file touch does not match its admitted source scope",
            ));
        }
        Ok(())
    }

    pub(super) fn stable_source_identity(&self) -> Option<String> {
        self.stable_source_identity.clone()
    }

    pub(super) fn stable_session_identity(&self) -> Option<String> {
        self.stable_session_identity.clone()
    }
}

impl CapturedSourceAdmission {
    pub(super) fn source(&self) -> &SourceObservation {
        &self.source
    }

    pub(crate) fn conversation_for_context(
        source: &SourceObservation,
        context: &ProviderAdapterContext,
    ) -> Result<Self> {
        Self::for_context(
            source,
            context,
            CapturedProjectionScope::new(source, context),
        )
    }

    pub(crate) fn file_for_context(
        source: &SourceObservation,
        context: &ProviderAdapterContext,
    ) -> Result<Self> {
        Self::for_context(
            source,
            context,
            CapturedProjectionScope::for_file(source, context),
        )
    }

    fn for_context(
        source: &SourceObservation,
        context: &ProviderAdapterContext,
        scope: CapturedProjectionScope,
    ) -> Result<Self> {
        let inventory_observation = match source.inventory_observation_token() {
            Some(token) => Some(CapturedInventoryObservation {
                path: context
                    .source_path
                    .clone()
                    .ok_or(CaptureError::SystemInvariant(
                        "inventory-observed capture admission requires a physical source path",
                    ))?,
                token: token.to_owned(),
            }),
            None => None,
        };
        let admission = Self {
            source: source.clone(),
            scope,
            inventory_observation,
            locator_resolution: RefCell::new(None),
        };
        admission.require_current_inventory_observation()?;
        Ok(admission)
    }

    #[cfg(test)]
    pub(super) fn conversation_without_cross_record_relationships(
        source: &SourceObservation,
    ) -> Self {
        Self::conversation_for_context(
            source,
            &ProviderAdapterContext {
                machine_id: "captured-batch-test-machine".to_owned(),
                source_path: Some("/tmp/captured-batch.jsonl".into()),
                source_root: None,
                imported_at: chrono::DateTime::<chrono::Utc>::UNIX_EPOCH,
            },
        )
        .expect("test admission without an inventory token is infallible")
    }

    pub(super) fn require_current_inventory_observation(&self) -> Result<()> {
        let Some(admitted) = &self.inventory_observation else {
            return Ok(());
        };
        let current = match crate::observe_ordinary_file(&admitted.path) {
            Ok(current) => current,
            Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            Err(CaptureError::InvalidProviderTranscriptPath { .. }) => {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            Err(error) => return Err(error),
        };
        if current.token_hex() != admitted.token {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        Ok(())
    }

    pub(super) fn require_revalidated_source(
        &self,
        provider_source_is_current: bool,
    ) -> Result<()> {
        if !provider_source_is_current {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        self.require_current_inventory_observation()
    }

    pub(super) fn validate_batch(&self, batch: &CapturedBatch) -> Result<()> {
        if batch.classification() != CapturedDataClassification::LocalPrivate {
            return Err(CaptureError::SystemInvariant(
                "captured source admission requires local-private data",
            ));
        }
        if self.source != *batch.source() {
            return Err(CaptureError::SystemInvariant(
                "captured source admission does not match the captured batch",
            ));
        }
        Ok(())
    }

    pub(super) fn validate_normalization(
        &self,
        normalization: &ProviderNormalizationResult,
    ) -> Result<()> {
        if normalization.summary.failed != 0 || !normalization.summary.failures.is_empty() {
            return Err(CaptureError::SystemInvariant(
                "accepted provider record returned normalization failures",
            ));
        }
        for (_, capture) in &normalization.captures {
            self.validate_capture(capture)?;
        }
        for (_, file) in &normalization.files_touched {
            self.scope.validate_file_touch(file)?;
        }
        Ok(())
    }

    pub(super) fn validate_capture(&self, capture: &ProviderCaptureEnvelope) -> Result<()> {
        self.scope.validate_capture(capture)?;
        if !(PROVIDER_CAPTURE_ENVELOPE_MIN_SUPPORTED_SCHEMA_VERSION
            ..=PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION)
            .contains(&capture.schema_version)
        {
            return Err(CaptureError::InvalidPayload(format!(
                "unsupported provider capture envelope schema version {}",
                capture.schema_version
            )));
        }
        if let Some(event) = &capture.event {
            validate_provider_event_for_import(event)?;
        }
        Ok(())
    }
}
