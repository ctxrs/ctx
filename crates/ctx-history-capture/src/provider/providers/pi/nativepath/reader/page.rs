use super::*;

impl PiNativeScanner {
    pub(super) fn core_units_pending(
        &self,
        units: Vec<PiNativeCoreUnit>,
        checkpoint: PiNativeCheckpoint,
    ) -> Result<PendingRecord, PiNativePathError> {
        let encoded_bytes = core_units_encoded_bytes(&units)?;
        Ok(PendingRecord {
            core_units: units,
            core_encoded_bytes: encoded_bytes,
            output: None,
            output_estimated_bytes: 0,
            checkpoint,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn bound_core_units(
        &self,
        units: &mut Vec<PiNativeCoreUnit>,
        ordinal: u64,
        line_number: u64,
        byte_start: u64,
        byte_end_exclusive: u64,
        checkpoint: PiNativeCheckpoint,
    ) -> Result<PendingRecord, PiNativePathError> {
        let encoded_bytes = self.bound_core_units_encoded(
            units,
            ordinal,
            line_number,
            byte_start,
            byte_end_exclusive,
        )?;
        Ok(PendingRecord {
            core_units: std::mem::take(units),
            core_encoded_bytes: encoded_bytes,
            output: None,
            output_estimated_bytes: 0,
            checkpoint,
        })
    }

    pub(super) fn bound_core_units_encoded(
        &self,
        units: &mut Vec<PiNativeCoreUnit>,
        ordinal: u64,
        line_number: u64,
        byte_start: u64,
        byte_end_exclusive: u64,
    ) -> Result<usize, PiNativePathError> {
        let encoded_bytes = core_units_encoded_bytes(units)?;
        if units.len() <= PI_NATIVE_PAGE_MAX_UNITS
            && PI_NATIVE_PAGE_ENCODING_RESERVE.saturating_add(encoded_bytes)
                <= PI_NATIVE_PAGE_MAX_BYTES
        {
            return Ok(encoded_bytes);
        }
        let kind = if units.len() > PI_NATIVE_PAGE_MAX_UNITS {
            PiNativeRejectionKind::TooManyCoreUnits
        } else {
            PiNativeRejectionKind::OversizedCoreUnit
        };
        *units = vec![PiNativeCoreUnit::Rejection(PiNativeRejection::new(
            kind,
            ordinal,
            line_number,
            byte_start,
            byte_end_exclusive,
            "Pi normalized record exceeds the bounded NativePath Core page",
        ))];
        Ok(core_units_encoded_bytes(units)?)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn rejection_pending(
        &self,
        kind: PiNativeRejectionKind,
        ordinal: u64,
        line_number: u64,
        byte_start: u64,
        byte_end_exclusive: u64,
        diagnostic: impl AsRef<str>,
        checkpoint: PiNativeCheckpoint,
    ) -> Result<PendingRecord, PiNativePathError> {
        let units = self.core_is_active().then(|| {
            PiNativeCoreUnit::Rejection(PiNativeRejection::new(
                kind,
                ordinal,
                line_number,
                byte_start,
                byte_end_exclusive,
                diagnostic,
            ))
        });
        self.core_units_pending(units.into_iter().collect(), checkpoint)
    }

    pub(super) fn oversized_pending(
        &self,
        ordinal: u64,
        byte_start: u64,
        byte_end_exclusive: u64,
        checkpoint: PiNativeCheckpoint,
    ) -> Result<PendingRecord, PiNativePathError> {
        self.rejection_pending(
            PiNativeRejectionKind::OversizedRecord,
            ordinal,
            ordinal.saturating_add(1),
            byte_start,
            byte_end_exclusive,
            format!("Pi JSONL record exceeds the {MAX_PROVIDER_JSONL_LINE_BYTES} byte limit"),
            checkpoint,
        )
    }

    pub(super) fn pending_requires_flush(
        &mut self,
        pending: &PendingRecord,
    ) -> Result<bool, PiNativePathError> {
        let core_needs_flush = self.core.as_ref().is_some_and(|lane| {
            lane.active
                && !lane.builder.is_empty()
                && !lane
                    .builder
                    .can_push(&pending.core_units, pending.core_encoded_bytes)
        });
        let output_needs_flush = self.output.as_ref().is_some_and(|lane| {
            lane.active
                && !lane.observations.is_empty()
                && pending.output.is_some()
                && (lane.observations.len() == PI_NATIVE_PAGE_MAX_UNITS
                    || PI_OUTPUT_PAGE_ENCODING_RESERVE
                        .saturating_add(lane.estimated_bytes)
                        .saturating_add(pending.output_estimated_bytes)
                        > PI_NATIVE_PAGE_MAX_BYTES)
        });
        if core_needs_flush {
            self.finish_core_page(false)?;
        }
        if output_needs_flush {
            self.finish_output_page(false)?;
        }
        Ok(core_needs_flush || output_needs_flush)
    }

    pub(super) fn commit_pending(
        &mut self,
        pending: PendingRecord,
    ) -> Result<(), PiNativePathError> {
        if let Some(lane) = self.core.as_mut().filter(|lane| lane.active) {
            if !lane
                .builder
                .can_push(&pending.core_units, pending.core_encoded_bytes)
            {
                return Err(PiNativePathError::Page(
                    "single Pi Core record exceeds the page bound".to_owned(),
                ));
            }
            lane.builder
                .push(pending.core_units, pending.core_encoded_bytes);
            lane.current = pending.checkpoint.clone();
        }
        if let Some(lane) = self.output.as_mut().filter(|lane| lane.active) {
            if let Some(output) = pending.output {
                let next = PI_OUTPUT_PAGE_ENCODING_RESERVE
                    .saturating_add(lane.estimated_bytes)
                    .saturating_add(pending.output_estimated_bytes);
                if lane.observations.len() == PI_NATIVE_PAGE_MAX_UNITS
                    || next > PI_NATIVE_PAGE_MAX_BYTES
                {
                    return Err(PiNativePathError::Page(
                        "single Pi output record exceeds the page bound".to_owned(),
                    ));
                }
                lane.estimated_bytes = lane
                    .estimated_bytes
                    .saturating_add(pending.output_estimated_bytes);
                lane.observations.push(output);
            }
            lane.current = pending.checkpoint;
        }
        Ok(())
    }

    pub(super) fn flush_full_lanes(&mut self) -> Result<(), PiNativePathError> {
        let core_full = self
            .core
            .as_ref()
            .is_some_and(|lane| lane.builder.units.len() == PI_NATIVE_PAGE_MAX_UNITS);
        let output_full = self
            .output
            .as_ref()
            .is_some_and(|lane| lane.observations.len() == PI_NATIVE_PAGE_MAX_UNITS);
        if core_full {
            self.finish_core_page(false)?;
            return Ok(());
        }
        if output_full {
            self.finish_output_page(false)?;
        }
        Ok(())
    }

    pub(super) fn queue_terminal_pages(&mut self) -> Result<(), PiNativePathError> {
        let terminal = self.complete;
        if let Some(lane) = self.core.as_mut().filter(|lane| lane.active) {
            lane.current.terminal = terminal;
        }
        if let Some(lane) = self.output.as_mut().filter(|lane| lane.active) {
            lane.current.terminal = terminal;
        }
        let core_changed = self
            .core
            .as_ref()
            .is_some_and(|lane| lane.active && lane.published != lane.current);
        let output_changed = self
            .output
            .as_ref()
            .is_some_and(|lane| lane.active && lane.published != lane.current);
        if core_changed {
            self.finish_core_page(terminal)?;
            return Ok(());
        }
        if output_changed {
            self.finish_output_page(terminal)?;
        }
        Ok(())
    }

    pub(super) fn finish_core_page(&mut self, terminal: bool) -> Result<(), PiNativePathError> {
        if self.ready_core.is_some() {
            return Err(PiNativePathError::Page(
                "Pi scanner retained more than one ready Core page".to_owned(),
            ));
        }
        let lane = self
            .core
            .as_mut()
            .ok_or_else(|| PiNativePathError::Page("Pi Core lane is not enabled".to_owned()))?;
        let expected = lane.published.safe_frontier().map_err(page_error)?;
        let mut next_checkpoint = lane.current.clone();
        next_checkpoint.terminal = terminal;
        let next = next_checkpoint.safe_frontier().map_err(page_error)?;
        let core = lane.builder.take();
        let accounting = NativePageAccounting {
            logical_units: core.units.len(),
            conservative_serialized_bytes: PI_NATIVE_PAGE_ENCODING_RESERVE
                .saturating_add(core.encoded_bytes)
                .saturating_add(expected.bytes.len())
                .saturating_add(next.bytes.len()),
        };
        let page = NativeIngestionPage::new(expected, next, terminal, accounting, core)
            .map_err(page_error)?;
        self.stats.core_pages = self.stats.core_pages.saturating_add(1);
        self.stats.peak_core_page_units = self
            .stats
            .peak_core_page_units
            .max(page.accounting.logical_units);
        self.stats.peak_core_page_bytes = self
            .stats
            .peak_core_page_bytes
            .max(page.accounting.conservative_serialized_bytes);
        lane.current = next_checkpoint.clone();
        lane.published = next_checkpoint;
        self.ready_core = Some(page);
        self.observe_ready_bytes();
        Ok(())
    }

    pub(super) fn finish_output_page(&mut self, terminal: bool) -> Result<(), PiNativePathError> {
        if self.ready_output.is_some() {
            return Err(PiNativePathError::Page(
                "Pi scanner retained more than one ready output page".to_owned(),
            ));
        }
        let lane = self
            .output
            .as_mut()
            .ok_or_else(|| PiNativePathError::Page("Pi output lane is not enabled".to_owned()))?;
        let expected = lane.published.safe_frontier().map_err(page_error)?;
        let mut next_checkpoint = lane.current.clone();
        next_checkpoint.terminal = terminal;
        let next = next_checkpoint.safe_frontier().map_err(page_error)?;
        let observations = std::mem::take(&mut lane.observations);
        let observation_count = observations.len();
        let output_bytes = std::mem::take(&mut lane.estimated_bytes);
        let output = NativeProOutputPage {
            inventory_generation: self.inventory_generation,
            source: self.output_source_identity.clone(),
            source_epoch: self.output_source_epoch,
            observed_revision: self.source_revision.clone(),
            parser_revision: format!(
                "pi-nativepath:{PI_NATIVEPATH_PARSER_REVISION}:{PI_NATIVEPATH_POLICY_REVISION}"
            ),
            materializer_revision: self.output_materializer_revision.clone(),
            disposition: lane.disposition,
            expected_prior_source_epoch: lane.expected_prior_source_epoch,
            expected_prior_frontier: lane.expected_prior_frontier.clone(),
            observations,
        };
        let accounting = NativePageAccounting {
            logical_units: observation_count,
            conservative_serialized_bytes: PI_OUTPUT_PAGE_ENCODING_RESERVE
                .saturating_add(output_bytes)
                .saturating_add(expected.bytes.len())
                .saturating_add(next.bytes.len()),
        };
        let page = NativeProReplayPage::new_with_source_identity(
            self.native_source_identity.clone(),
            expected,
            next,
            terminal,
            accounting,
            output,
        )
        .map_err(page_error)?;
        self.stats.output_pages = self.stats.output_pages.saturating_add(1);
        self.stats.peak_output_page_units = self
            .stats
            .peak_output_page_units
            .max(page.accounting.logical_units);
        self.stats.peak_output_page_bytes = self
            .stats
            .peak_output_page_bytes
            .max(page.accounting.conservative_serialized_bytes);
        lane.current = next_checkpoint.clone();
        lane.published = next_checkpoint.clone();
        lane.disposition = ProOutputSourceDisposition::AppendOrResume;
        lane.expected_prior_source_epoch = Some(self.output_source_epoch);
        lane.expected_prior_frontier = Some(next_checkpoint.safe_frontier().map_err(page_error)?);
        self.ready_output = Some(page);
        self.observe_ready_bytes();
        Ok(())
    }

    pub(super) fn fence_before_exposure(&mut self) -> Result<(), PiNativePathError> {
        #[cfg(test)]
        if let Some(mut hook) = self.before_exposure.take() {
            hook();
        }
        self.source.fence(self.reader.get_ref())?;
        self.stats.source_fences = self.stats.source_fences.saturating_add(1);
        Ok(())
    }

    pub(super) fn observe_ready_bytes(&mut self) {
        let bytes = self
            .ready_core
            .as_ref()
            .map_or(0, |page| page.accounting.conservative_serialized_bytes)
            .saturating_add(
                self.ready_output
                    .as_ref()
                    .map_or(0, |page| page.accounting.conservative_serialized_bytes),
            );
        self.stats.peak_ready_page_bytes = self.stats.peak_ready_page_bytes.max(bytes);
    }

    pub(super) fn core_is_active(&self) -> bool {
        self.core.as_ref().is_some_and(|lane| lane.active)
    }
}
