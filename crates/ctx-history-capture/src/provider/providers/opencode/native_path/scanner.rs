use super::*;

mod page;

use page::*;
pub(super) use page::{retained_event_kind, retained_file_touches, retained_searchable_text};

pub(in super::super) struct OpenCodeNativeScanner<'reader> {
    schema: &'reader OpenCodeNativeSchema,
    snapshot: &'reader OpenCodeSnapshotGeneration,
    pub(super) index: OpenCodeScanIndex,
    authority: OpenCodeNativeSourceAuthority,
    limits: OpenCodeNativePageLimits,
    profile: OpenCodeNativeProfile,
    dialect: super::super::OpenCodeSqliteDialect,
    session_keyset: SessionKeyset,
    event_keyset: EventKeyset,
    position: OpenCodeNativeScanPosition,
    pro_keyset: ProKeyset,
    pro_frontier: OpenCodeNativeProFrontier,
    pending_core_page: Option<OpenCodeNativePage>,
    core_exhausted: bool,
    semantic_hasher: Sha256,
    metrics: OpenCodeNativeMetrics,
}

impl<'reader> OpenCodeNativeScanner<'reader> {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        conn: &'reader Connection,
        schema: &'reader OpenCodeNativeSchema,
        snapshot: &'reader OpenCodeSnapshotGeneration,
        authority: OpenCodeNativeSourceAuthority,
        limits: OpenCodeNativePageLimits,
        profile: OpenCodeNativeProfile,
        dialect: super::super::OpenCodeSqliteDialect,
        prior: Option<&OpenCodeNativePersistedState>,
    ) -> Result<Self> {
        let mut semantic_hasher = Sha256::new();
        semantic_hasher.update(OPENCODE_SEMANTIC_DIGEST_DOMAIN);
        hash_str(&mut semantic_hasher, schema.family.label());
        let index = OpenCodeScanIndex::build(
            conn,
            schema,
            OPENCODE_CORE_SESSION_INDEX_PAGE_BYTES,
            profile,
            &dialect,
            prior.map(|state| &state.ordered_prefix_evidence),
        )?;
        let build_metrics = index.build_metrics();
        Ok(Self {
            schema,
            snapshot,
            index,
            authority,
            limits,
            profile,
            dialect,
            session_keyset: SessionKeyset::default(),
            event_keyset: EventKeyset::default(),
            position: OpenCodeNativeScanPosition::default(),
            pro_keyset: ProKeyset::default(),
            pro_frontier: OpenCodeNativeProFrontier::default(),
            pending_core_page: None,
            core_exhausted: false,
            semantic_hasher,
            metrics: OpenCodeNativeMetrics {
                snapshot_attempts: snapshot.attempts(),
                source_session_rows_scanned: build_metrics.source_session_rows_scanned,
                source_event_rows_scanned: build_metrics.source_event_rows_scanned,
                snapshot_session_rows_indexed: build_metrics.snapshot_session_rows_indexed,
                snapshot_event_rows_indexed: build_metrics.snapshot_event_rows_indexed,
                snapshot_ordering_passes: build_metrics.snapshot_ordering_passes,
                prefix_session_rows_read: build_metrics.prefix_session_rows_read,
                prefix_event_rows_read: build_metrics.prefix_event_rows_read,
                prefix_pro_rows_read: build_metrics.prefix_pro_rows_read,
                json_records_visited: build_metrics.json_records_visited,
                json_bytes_visited: build_metrics.json_bytes_visited,
                ..OpenCodeNativeMetrics::default()
            },
        })
    }

    pub(super) fn next_page(&mut self) -> Result<Option<OpenCodeNativePage>> {
        if self.core_exhausted {
            return Ok(None);
        }
        loop {
            match self.next_unbuffered_core_page()? {
                Some(page) if page_logical_units(&page) != 0 => {
                    if let Some(mut previous) = self.pending_core_page.replace(page) {
                        if let Some(pending) = self.pending_core_page.as_mut() {
                            pending.expected_frontier = previous.next_frontier;
                        }
                        finalize_core_page(&mut previous, false)?;
                        return Ok(Some(previous));
                    }
                }
                Some(empty) => {
                    if let Some(pending) = self.pending_core_page.as_mut() {
                        pending.next_frontier = empty.next_frontier;
                        pending.position = empty.position;
                    }
                }
                None => {
                    self.core_exhausted = true;
                    if let Some(mut final_page) = self.pending_core_page.take() {
                        final_page.next_frontier = OpenCodeNativeFrontier {
                            phase: OpenCodeNativeScanPhase::Complete,
                            scan_ordinal: self.position.native_events_seen,
                        };
                        final_page.position = self.position.clone();
                        finalize_core_page(&mut final_page, true)?;
                        return Ok(Some(final_page));
                    }
                    let mut final_page = self.empty_page();
                    final_page.expected_frontier = OpenCodeNativeFrontier {
                        phase: OpenCodeNativeScanPhase::Sessions,
                        scan_ordinal: 0,
                    };
                    final_page.next_frontier = OpenCodeNativeFrontier {
                        phase: OpenCodeNativeScanPhase::Complete,
                        scan_ordinal: self.position.native_events_seen,
                    };
                    finalize_core_page(&mut final_page, true)?;
                    return Ok(Some(final_page));
                }
            }
        }
    }

    fn next_unbuffered_core_page(&mut self) -> Result<Option<OpenCodeNativePage>> {
        loop {
            match self.position.phase {
                OpenCodeNativeScanPhase::Sessions => {
                    self.metrics.session_page_queries =
                        self.metrics.session_page_queries.saturating_add(1);
                    let scanned = fetch_session_page(
                        self.index.connection(),
                        self.session_keyset,
                        self.limits.rows,
                        self.limits
                            .retained_bytes
                            .min(OPENCODE_CORE_SESSION_INDEX_PAGE_BYTES),
                    )?;
                    self.metrics.indexed_session_rows_read = self
                        .metrics
                        .indexed_session_rows_read
                        .saturating_add(scanned.len() as u64);
                    if scanned.is_empty() {
                        self.position.phase = OpenCodeNativeScanPhase::Events;
                        continue;
                    }
                    let mut page = self.empty_page();
                    for scanned_session in scanned {
                        self.session_keyset = SessionKeyset {
                            scan_ordinal: scanned_session.scan_ordinal,
                            metadata_prefix_bytes: scanned_session.metadata_prefix_bytes,
                        };
                        let session = scanned_session.row;
                        self.position.native_sessions_seen =
                            self.position.native_sessions_seen.saturating_add(1);
                        self.metrics.native_sessions =
                            self.metrics.native_sessions.saturating_add(1);
                        self.hash_session(&session);
                        page.sessions.push(session);
                    }
                    page.position = self.position.clone();
                    page.next_frontier = frontier_for_position(&self.position);
                    return Ok(Some(page));
                }
                OpenCodeNativeScanPhase::Events => {
                    self.metrics.event_metadata_page_queries =
                        self.metrics.event_metadata_page_queries.saturating_add(1);
                    let metadata = fetch_event_metadata_page(
                        self.index.connection(),
                        self.event_keyset,
                        self.limits.rows,
                        self.limits
                            .retained_bytes
                            .min(OPENCODE_CORE_EVENT_PROJECTION_PAGE_BYTES),
                        self.schema.family,
                    )?;
                    self.metrics.indexed_event_rows_read = self
                        .metrics
                        .indexed_event_rows_read
                        .saturating_add(metadata.len() as u64);
                    if metadata.is_empty() {
                        self.position.phase = OpenCodeNativeScanPhase::Complete;
                        return Ok(None);
                    }
                    return self.project_event_page(metadata).map(Some);
                }
                OpenCodeNativeScanPhase::Complete => return Ok(None),
            }
        }
    }

    pub(super) fn next_pro_output_page(&mut self) -> Result<Option<OpenCodeNativeProOutputPage>> {
        if self.profile == OpenCodeNativeProfile::CoreOnly || self.pro_frontier.terminal {
            return Ok(None);
        }
        let metadata = fetch_pro_metadata_page(
            self.index.connection(),
            self.pro_keyset,
            self.limits.rows,
            OPENCODE_NATIVE_PAGE_MAX_BYTES,
            self.schema.family,
        )?;
        if metadata.is_empty() {
            let expected_frontier = self.pro_frontier;
            self.pro_frontier.terminal = true;
            let mut page = OpenCodeNativeProOutputPage {
                identity: OpenCodeNativeProPageIdentity([0; 32]),
                source_authority: self.authority.clone(),
                expected_frontier,
                next_frontier: self.pro_frontier,
                terminal: true,
                accounting: OpenCodeNativePageAccounting::default(),
                observations: Vec::new(),
                rejections: Vec::new(),
            };
            finalize_pro_page(&mut page)?;
            return Ok(Some(page));
        }
        let expected_frontier = self.pro_frontier;
        let mut observations = Vec::new();
        let mut rejections = Vec::new();
        for record in &metadata {
            if let Some(observation) = project_pro_output(record, self.dialect.source_format) {
                self.metrics.output_content_cells_transferred = self
                    .metrics
                    .output_content_cells_transferred
                    .saturating_add(1);
                self.metrics.output_content_bytes_transferred = self
                    .metrics
                    .output_content_bytes_transferred
                    .saturating_add(observation.content.len() as u64);
                observations.push(observation);
            } else {
                rejections.push(project_pro_rejection(record)?);
            }
        }
        let last = metadata.last().ok_or(CaptureError::SystemInvariant(
            "OpenCode nonempty Pro metadata page lost its final unit",
        ))?;
        self.pro_keyset = ProKeyset {
            pro_ordinal: last.pro_ordinal,
            output_prefix_bytes: last.output_prefix_bytes,
        };
        let terminal = !has_pro_metadata_after(self.index.connection(), last.pro_ordinal)?;
        self.pro_frontier = OpenCodeNativeProFrontier {
            source_event_ordinal: last.source_event_ordinal,
            subrecord_index: last.subrecord_index,
            terminal,
        };
        let mut page = OpenCodeNativeProOutputPage {
            identity: OpenCodeNativeProPageIdentity([0; 32]),
            source_authority: self.authority.clone(),
            expected_frontier,
            next_frontier: self.pro_frontier,
            terminal,
            accounting: OpenCodeNativePageAccounting::default(),
            observations,
            rejections,
        };
        finalize_pro_page(&mut page)?;
        Ok(Some(page))
    }

    pub(super) fn resume_pro_from(&mut self, frontier: OpenCodeNativeProFrontier) -> Result<()> {
        if self.profile != OpenCodeNativeProfile::CoreAndPro {
            return Err(CaptureError::InvalidPayload(
                "OpenCode Pro replay requires the CoreAndPro profile".to_owned(),
            ));
        }
        if self.pro_keyset != ProKeyset::default()
            || self.pro_frontier != OpenCodeNativeProFrontier::default()
        {
            return Err(CaptureError::InvalidPayload(
                "OpenCode Pro replay frontier must be installed before reading output pages"
                    .to_owned(),
            ));
        }
        self.pro_keyset = pro_keyset_for_frontier(self.index.connection(), frontier)?;
        self.pro_frontier = frontier;
        Ok(())
    }

    pub(super) fn finish_pro_replay(self) -> Result<OpenCodeNativeProReplaySummary> {
        if self.profile != OpenCodeNativeProfile::CoreAndPro || !self.pro_frontier.terminal {
            return Err(CaptureError::InvalidPayload(
                "OpenCode Pro replay must exhaust its exact output frontier before finish"
                    .to_owned(),
            ));
        }
        if !self.snapshot.revalidate_live()? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        Ok(OpenCodeNativeProReplaySummary {
            source_authority: self.authority,
            source_generation_digest: self.snapshot.observation().generation_digest(),
            capability_digest: self.schema.capability_digest.clone(),
            frontier: self.pro_frontier,
            complete: true,
        })
    }

    pub(super) fn finish(self) -> Result<OpenCodeNativeScanSummary> {
        if self.position.phase != OpenCodeNativeScanPhase::Complete {
            return Err(CaptureError::InvalidPayload(
                "OpenCode NativePath scan must be exhausted before finish".to_owned(),
            ));
        }
        if !self.snapshot.revalidate_live()? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let semantic_digest: [u8; 32] = self.semantic_hasher.finalize().into();
        Ok(OpenCodeNativeScanSummary {
            source_authority: self.authority,
            source_generation_digest: self.snapshot.observation().generation_digest(),
            physical_source_identity: self.snapshot.observation().physical_source_identity(),
            capability_digest: self.schema.capability_digest.clone(),
            semantic_digest: hex_digest(semantic_digest),
            schema_family: self.schema.family,
            identity_semantics: self.schema.family.identity_semantics(),
            ordering_semantics: self.schema.family.ordering_semantics(),
            complete: true,
            profile: self.profile,
            core_frontier: OpenCodeNativeFrontier {
                phase: OpenCodeNativeScanPhase::Complete,
                scan_ordinal: self.position.native_events_seen,
            },
            pro_frontier: self.pro_frontier,
            ordered_prefix_evidence: Box::new(self.index.ordered_prefix_evidence().clone()),
            restart_prefix_comparison: self
                .index
                .restart_prefix_comparison()
                .cloned()
                .map(Box::new),
            metrics: self.metrics,
        })
    }

    fn project_event_page(
        &mut self,
        mut metadata: Vec<RecordMetadata>,
    ) -> Result<OpenCodeNativePage> {
        let mut page = self.empty_page();
        for mut record in metadata.drain(..) {
            self.event_keyset = EventKeyset {
                scan_ordinal: record.scan_ordinal,
                retained_prefix_bytes: record.retained_prefix_bytes,
            };
            self.position.native_events_seen = self.position.native_events_seen.saturating_add(1);
            self.metrics.native_events = self.metrics.native_events.saturating_add(1);
            let projection = std::mem::replace(
                &mut record.projection,
                OpenCodeJsonProjection::Rejected(
                    OpenCodeNativeRejectionKind::RetainedParseMismatch,
                ),
            );
            match projection {
                OpenCodeJsonProjection::Retained(retained) => {
                    if retained.body.get("result_outcome").is_some() {
                        self.metrics.excluded_outputs =
                            self.metrics.excluded_outputs.saturating_add(1);
                    }
                    self.metrics.retained_content_cells_transferred = self
                        .metrics
                        .retained_content_cells_transferred
                        .saturating_add(1);
                    self.metrics.retained_content_bytes_transferred = self
                        .metrics
                        .retained_content_bytes_transferred
                        .saturating_add(record.content_bytes);
                    match normalize_retained_event(&record, retained) {
                        Ok(event) => {
                            self.hash_event(&event);
                            self.metrics.retained_events =
                                self.metrics.retained_events.saturating_add(1);
                            page.events.push(event);
                        }
                        Err(error) => {
                            let rejection = rejection(
                                &record,
                                OpenCodeNativeRejectionKind::RetainedParseMismatch,
                                error.to_string(),
                            );
                            self.hash_rejection(&rejection);
                            self.metrics.rejected_records =
                                self.metrics.rejected_records.saturating_add(1);
                            page.rejections.push(rejection);
                        }
                    }
                }
                OpenCodeJsonProjection::Output(output) => {
                    self.metrics.excluded_outputs = self.metrics.excluded_outputs.saturating_add(1);
                    if let Some(diagnostic) = output.diagnostic {
                        match normalize_retained_event(&record, diagnostic) {
                            Ok(event) => {
                                self.hash_event(&event);
                                self.metrics.retained_events =
                                    self.metrics.retained_events.saturating_add(1);
                                page.events.push(event);
                            }
                            Err(error) => {
                                let rejection = rejection(
                                    &record,
                                    OpenCodeNativeRejectionKind::RetainedParseMismatch,
                                    error.to_string(),
                                );
                                self.hash_rejection(&rejection);
                                self.metrics.rejected_records =
                                    self.metrics.rejected_records.saturating_add(1);
                                page.rejections.push(rejection);
                            }
                        }
                    }
                }
                OpenCodeJsonProjection::ExcludedOutput => {
                    self.metrics.excluded_outputs = self.metrics.excluded_outputs.saturating_add(1);
                }
                OpenCodeJsonProjection::Rejected(kind) => {
                    let rejection = rejection(
                        &record,
                        kind,
                        format!(
                            "OpenCode native record {} rejected as {}",
                            record.native_identity,
                            kind.label()
                        ),
                    );
                    self.hash_rejection(&rejection);
                    self.metrics.rejected_records = self.metrics.rejected_records.saturating_add(1);
                    page.rejections.push(rejection);
                }
                OpenCodeJsonProjection::RejectedWithReason(kind, reason) => {
                    let rejection = rejection(&record, kind, reason);
                    self.hash_rejection(&rejection);
                    self.metrics.rejected_records = self.metrics.rejected_records.saturating_add(1);
                    page.rejections.push(rejection);
                }
            }
        }
        page.position = self.position.clone();
        page.next_frontier = frontier_for_position(&self.position);
        Ok(page)
    }

    fn empty_page(&self) -> OpenCodeNativePage {
        let frontier = frontier_for_position(&self.position);
        OpenCodeNativePage {
            identity: OpenCodeNativePageIdentity([0; 32]),
            source_authority: self.authority.clone(),
            expected_frontier: frontier,
            next_frontier: frontier,
            terminal: false,
            accounting: OpenCodeNativePageAccounting::default(),
            position: self.position.clone(),
            sessions: Vec::new(),
            events: Vec::new(),
            excluded_outputs: Vec::new(),
            rejections: Vec::new(),
        }
    }

    fn hash_session(&mut self, session: &OpenCodeNativeSession) {
        self.semantic_hasher.update(b"session");
        hash_str(&mut self.semantic_hasher, &session.native_identity);
        hash_str(&mut self.semantic_hasher, &session.content_digest);
    }

    fn hash_event(&mut self, event: &OpenCodeNativeEvent) {
        self.semantic_hasher.update(b"event");
        hash_str(&mut self.semantic_hasher, &event.native_identity);
        hash_order(&mut self.semantic_hasher, &event.native_order);
        hash_str(&mut self.semantic_hasher, &event.content_digest);
    }

    fn hash_rejection(&mut self, rejection: &OpenCodeNativeRejection) {
        self.semantic_hasher.update(b"rejection");
        hash_str(&mut self.semantic_hasher, &rejection.native_identity);
        hash_str(&mut self.semantic_hasher, rejection.kind.label());
    }
}
