use super::*;

impl ClineNativeReader {
    pub(super) fn make_array_page(
        &self,
        source: ClineFileSourceIdentity,
        item: Option<ParsedItem>,
        session: Option<ClineSessionRow>,
    ) -> Result<ClineCertifiedPage, ClineComponentFailure> {
        let mut events = Vec::new();
        let mut rejections = Vec::new();
        let mut core_bytes = estimated_source_bytes(&source);
        let source_record = item.as_ref().and_then(|item| item.source_record);
        if let Some(item) = item {
            core_bytes = core_bytes.saturating_add(item.core_bytes);
            events = item.rows;
            if let Some(rejection) = item.rejection {
                core_bytes = core_bytes.saturating_add(estimated_rejection_bytes(&rejection));
                rejections.push(rejection);
            }
        }
        if source_record.is_some() {
            core_bytes = core_bytes.saturating_add(1 + 8 + 8 + 8 + 32);
        }
        if let Some(session) = session.as_ref() {
            core_bytes = core_bytes.saturating_add(estimated_session_bytes(session));
        }
        let event_units = events.iter().fold(0_usize, |units, event| {
            units
                .saturating_add(1)
                .saturating_add(event.file_touches.len())
                .saturating_add(usize::from(event.kind == ClineEventKind::CommandOutput))
        });
        let core_units = CLINE_NATIVE_FIXED_PAGE_UNITS
            .saturating_add(event_units)
            .saturating_add(
                usize::from(session.is_some()).saturating_mul(CLINE_NATIVE_SESSION_PAGE_UNITS),
            );
        let logical_units = core_units;
        if !owned_page_bounds_are_valid(core_bytes, logical_units) {
            return Err(ClineComponentFailure {
                component: source.component,
                path: source.canonical_path.clone(),
                kind: ClineComponentFailureKind::AuthorityBound,
                message: "Cline certified page exceeds the 64-unit/4 MiB Core/8 MiB total contract"
                    .into(),
                retryable: false,
            });
        }
        Ok(ClineCertifiedPage {
            source,
            core: ClineCorePayload {
                session,
                events: events.into_boxed_slice(),
                rejections: rejections.into_boxed_slice(),
            },
            source_record,
        })
    }

    pub(super) fn build_metadata_page(
        &self,
        checkpoint: ClineMetadataCheckpoint,
    ) -> Result<ClineCertifiedPage, ClineNativePathError> {
        let source = file_source(
            checkpoint.observation.component,
            &checkpoint.observation.path,
        );
        let core_bytes = estimated_source_bytes(&source)
            .saturating_add(estimated_session_bytes(&checkpoint.session));
        let core_units =
            CLINE_NATIVE_FIXED_PAGE_UNITS.saturating_add(CLINE_NATIVE_SESSION_PAGE_UNITS);
        if !owned_page_bounds_are_valid(core_bytes, core_units) {
            return Err(ClineNativePathError::Invariant {
                message: "Cline metadata page exceeded the 4 MiB Core/8 MiB total page bounds"
                    .to_owned(),
            });
        }
        Ok(ClineCertifiedPage {
            source,
            core: ClineCorePayload {
                session: Some(checkpoint.session.clone()),
                events: Box::new([]),
                rejections: Box::new([]),
            },
            source_record: None,
        })
    }

    pub(super) fn build_deleted_array_page(
        &self,
        observation: &ClineComponentObservation,
    ) -> Result<ClineCertifiedPage, ClineNativePathError> {
        let source = file_source(observation.component, &observation.path);
        let core_bytes = estimated_source_bytes(&source);
        if !owned_page_bounds_are_valid(core_bytes, CLINE_NATIVE_FIXED_PAGE_UNITS) {
            return Err(ClineNativePathError::Invariant {
                message: "Cline deletion page exceeded the 4 MiB Core/8 MiB total page bounds"
                    .to_owned(),
            });
        }
        Ok(ClineCertifiedPage {
            source,
            core: ClineCorePayload {
                session: None,
                events: Box::new([]),
                rejections: Box::new([]),
            },
            source_record: None,
        })
    }
}
