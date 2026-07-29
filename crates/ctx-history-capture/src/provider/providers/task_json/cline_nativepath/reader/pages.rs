use super::*;

impl ClineNativeReader {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn make_array_page(
        &self,
        source: ClineFileSourceIdentity,
        authority_observation: ClineComponentObservation,
        revision: ClineCertifiedRevision,
        expected: ClinePageFrontier,
        next: ClinePageFrontier,
        transition: ClineComponentTransition,
        item: Option<ParsedItem>,
        terminal_checkpoint: Option<ClineArrayCheckpoint>,
        terminal: bool,
        evidence: Option<ClineTerminalEvidence>,
        session: Option<ClineSessionRow>,
    ) -> Result<ClineCertifiedPage, ClineComponentFailure> {
        let mut events = Vec::new();
        let mut rejections = Vec::new();
        let mut core_bytes =
            estimated_page_envelope_bytes(&source, &revision, &expected, &next, evidence.as_ref());
        let mut fingerprint_items = Vec::new();
        let source_record = item.as_ref().and_then(|item| item.source_record);
        if let Some(item) = item {
            core_bytes = core_bytes.saturating_add(item.core_bytes);
            fingerprint_items.push(item.checkpoint);
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
        if let Some(checkpoint) = terminal_checkpoint.as_ref() {
            core_bytes = core_bytes.saturating_add(checkpoint.estimated_bytes());
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
        let fingerprint = core_payload_fingerprint(
            source.component,
            transition,
            session.as_ref(),
            &fingerprint_items,
            &rejections,
            terminal,
        );
        let identity = page_identity(&source, &revision, &expected, &next, terminal, &fingerprint);
        Ok(ClineCertifiedPage {
            identity,
            source,
            authority_observation,
            source_revision: revision,
            expected_frontier: expected,
            next_safe_frontier: next,
            terminal,
            #[cfg(test)]
            terminal_evidence: evidence,
            accounting: ClinePageAccounting {
                core_units,
                logical_units,
                conservative_core_bytes: core_bytes,
                conservative_serialized_bytes: core_bytes,
            },
            core: ClineCorePayload {
                #[cfg(test)]
                transition,
                session,
                events: events.into_boxed_slice(),
                rejections: rejections.into_boxed_slice(),
                terminal_metadata_checkpoint: None,
            },
            source_record,
        })
    }

    pub(super) fn build_metadata_page(
        &self,
        checkpoint: ClineMetadataCheckpoint,
        prior: Option<&ClineMetadataCheckpoint>,
        transition: ClineComponentTransition,
    ) -> Result<ClineCertifiedPage, ClineNativePathError> {
        let source = file_source(
            self.dialect,
            &checkpoint.session,
            checkpoint.observation.component,
            &checkpoint.observation.path,
            0,
        );
        let revision_hash = checkpoint
            .content_sha256
            .unwrap_or_else(|| missing_revision(checkpoint.observation.component));
        let revision = certified_revision(&checkpoint.observation, revision_hash);
        let expected = prior.map_or_else(
            || ClinePageFrontier::zero_component(checkpoint.observation.component),
            metadata_frontier,
        );
        let next = metadata_frontier(&checkpoint);
        let evidence = if checkpoint.content_sha256.is_some() {
            ClineTerminalEvidence::CompleteMetadata {
                content_sha256: checkpoint.content_sha256,
            }
        } else {
            ClineTerminalEvidence::Deleted
        };
        let fingerprint = core_payload_fingerprint(
            checkpoint.observation.component,
            transition,
            Some(&checkpoint.session),
            &[],
            &[],
            true,
        );
        let identity = page_identity(&source, &revision, &expected, &next, true, &fingerprint);
        let core_bytes =
            estimated_page_envelope_bytes(&source, &revision, &expected, &next, Some(&evidence))
                .saturating_add(estimated_metadata_checkpoint_bytes(&checkpoint))
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
            identity,
            source,
            authority_observation: checkpoint.observation.clone(),
            source_revision: revision,
            expected_frontier: expected,
            next_safe_frontier: next,
            terminal: true,
            #[cfg(test)]
            terminal_evidence: Some(evidence),
            accounting: ClinePageAccounting {
                core_units,
                logical_units: core_units,
                conservative_core_bytes: core_bytes,
                conservative_serialized_bytes: core_bytes,
            },
            core: ClineCorePayload {
                #[cfg(test)]
                transition,
                session: Some(checkpoint.session.clone()),
                events: Box::new([]),
                rejections: Box::new([]),
                terminal_metadata_checkpoint: Some(Box::new(checkpoint)),
            },
            source_record: None,
        })
    }

    pub(super) fn build_deleted_array_page(
        &self,
        metadata: &ClineMetadataCheckpoint,
        component: ClineEventComponent,
        observation: &ClineComponentObservation,
        prior: Option<&ClineArrayCheckpoint>,
    ) -> Result<ClineCertifiedPage, ClineNativePathError> {
        let source = file_source(
            self.dialect,
            &metadata.session,
            observation.component,
            &observation.path,
            0,
        );
        let revision = certified_revision(observation, missing_revision(observation.component));
        let expected = prior.map_or_else(
            || ClinePageFrontier::zero(component),
            |prior| prior.final_frontier.clone(),
        );
        let next = ClinePageFrontier::zero(component);
        let transition = ClineComponentTransition::MissingPhysical;
        let fingerprint =
            core_payload_fingerprint(observation.component, transition, None, &[], &[], true);
        let evidence = ClineTerminalEvidence::Deleted;
        let core_bytes =
            estimated_page_envelope_bytes(&source, &revision, &expected, &next, Some(&evidence));
        if !owned_page_bounds_are_valid(core_bytes, CLINE_NATIVE_FIXED_PAGE_UNITS) {
            return Err(ClineNativePathError::Invariant {
                message: "Cline deletion page exceeded the 4 MiB Core/8 MiB total page bounds"
                    .to_owned(),
            });
        }
        Ok(ClineCertifiedPage {
            identity: page_identity(&source, &revision, &expected, &next, true, &fingerprint),
            source,
            authority_observation: observation.clone(),
            source_revision: revision,
            expected_frontier: expected,
            next_safe_frontier: next,
            terminal: true,
            #[cfg(test)]
            terminal_evidence: Some(evidence),
            accounting: ClinePageAccounting {
                core_units: CLINE_NATIVE_FIXED_PAGE_UNITS,
                logical_units: CLINE_NATIVE_FIXED_PAGE_UNITS,
                conservative_core_bytes: core_bytes,
                conservative_serialized_bytes: core_bytes,
            },
            core: ClineCorePayload {
                #[cfg(test)]
                transition,
                session: None,
                events: Box::new([]),
                rejections: Box::new([]),
                terminal_metadata_checkpoint: None,
            },
            source_record: None,
        })
    }

    pub(super) fn run_before_exposure(&mut self, path: &Path, component: ClineComponent) {
        #[cfg(test)]
        if let Some(hook) = self.before_exposure.as_mut() {
            hook(path, component);
        }
        #[cfg(not(test))]
        let _ = (path, component);
    }
}
