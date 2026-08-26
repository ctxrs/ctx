use super::*;

impl<E: JsonlFamilyError> JsonlReader<E> {
    #[cfg(any(test, feature = "test-support"))]
    pub fn open(
        identity: JsonlSourceIdentity,
        source_file: Arc<OpenedProviderSourceFile<E>>,
        previous: Option<&JsonlCheckpoint>,
        probe: Option<JsonlProbe>,
    ) -> JsonlResult<Self, E> {
        Self::open_with_record_framing(
            identity,
            source_file,
            previous,
            probe,
            JsonlRecordFraming::ordinary(),
        )
    }

    pub fn open_with_record_framing(
        identity: JsonlSourceIdentity,
        source_file: Arc<OpenedProviderSourceFile<E>>,
        previous: Option<&JsonlCheckpoint>,
        probe: Option<JsonlProbe>,
        record_framing: JsonlRecordFraming,
    ) -> JsonlResult<Self, E> {
        Self::open_with_record_framing_and_encoding(
            identity,
            source_file,
            previous,
            probe,
            JsonlPhysicalEncoding::RawJsonl,
            record_framing,
        )
    }

    pub fn open_with_record_framing_and_encoding(
        identity: JsonlSourceIdentity,
        source_file: Arc<OpenedProviderSourceFile<E>>,
        previous: Option<&JsonlCheckpoint>,
        probe: Option<JsonlProbe>,
        physical_encoding: JsonlPhysicalEncoding,
        record_framing: JsonlRecordFraming,
    ) -> JsonlResult<Self, E> {
        Self::open_with_framing(
            identity,
            source_file,
            previous,
            probe,
            JsonlReaderFramingOptions {
                physical_encoding,
                record_framing,
                whole_record: false,
                bind_admitted_eof: false,
                deferred_append_eof_sha256: None,
                frozen_observation: None,
                direct_append: false,
                route_resources: None,
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn open_with_record_framing_and_encoding_and_resources(
        identity: JsonlSourceIdentity,
        source_file: Arc<OpenedProviderSourceFile<E>>,
        previous: Option<&JsonlCheckpoint>,
        probe: Option<JsonlProbe>,
        physical_encoding: JsonlPhysicalEncoding,
        record_framing: JsonlRecordFraming,
        route_resources: &ctx_history_capture_runtime::SourceBackedRouteResources,
    ) -> JsonlResult<Self, E> {
        Self::open_with_framing(
            identity,
            source_file,
            previous,
            probe,
            JsonlReaderFramingOptions {
                physical_encoding,
                record_framing,
                whole_record: false,
                bind_admitted_eof: false,
                deferred_append_eof_sha256: None,
                frozen_observation: None,
                direct_append: false,
                route_resources: Some(route_resources),
            },
        )
    }

    pub fn open_semantic_with_record_framing(
        identity: JsonlSourceIdentity,
        source_file: Arc<OpenedProviderSourceFile<E>>,
        previous: Option<&JsonlCheckpoint>,
        mode: JsonlSemanticPreflightMode,
        probe: Option<JsonlProbe>,
        record_framing: JsonlRecordFraming,
        frozen_observation: Option<&JsonlFileObservation>,
    ) -> JsonlResult<Self, E> {
        Self::open_semantic_with_record_framing_and_encoding(
            identity,
            source_file,
            previous,
            mode,
            probe,
            JsonlPhysicalEncoding::RawJsonl,
            record_framing,
            frozen_observation,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn open_semantic_with_record_framing_and_encoding(
        identity: JsonlSourceIdentity,
        source_file: Arc<OpenedProviderSourceFile<E>>,
        previous: Option<&JsonlCheckpoint>,
        mode: JsonlSemanticPreflightMode,
        probe: Option<JsonlProbe>,
        physical_encoding: JsonlPhysicalEncoding,
        record_framing: JsonlRecordFraming,
        frozen_observation: Option<&JsonlFileObservation>,
    ) -> JsonlResult<Self, E> {
        Self::open_semantic_with_record_framing_and_encoding_direct(
            identity,
            source_file,
            previous,
            mode,
            probe,
            physical_encoding,
            record_framing,
            frozen_observation,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn open_semantic_with_record_framing_and_encoding_direct(
        identity: JsonlSourceIdentity,
        source_file: Arc<OpenedProviderSourceFile<E>>,
        previous: Option<&JsonlCheckpoint>,
        mode: JsonlSemanticPreflightMode,
        probe: Option<JsonlProbe>,
        physical_encoding: JsonlPhysicalEncoding,
        record_framing: JsonlRecordFraming,
        frozen_observation: Option<&JsonlFileObservation>,
        direct_append: bool,
    ) -> JsonlResult<Self, E> {
        Self::open_semantic_with_record_framing_and_encoding_direct_and_resources(
            identity,
            source_file,
            previous,
            mode,
            probe,
            physical_encoding,
            record_framing,
            frozen_observation,
            direct_append,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn open_semantic_with_record_framing_and_encoding_direct_and_resources(
        identity: JsonlSourceIdentity,
        source_file: Arc<OpenedProviderSourceFile<E>>,
        previous: Option<&JsonlCheckpoint>,
        mode: JsonlSemanticPreflightMode,
        probe: Option<JsonlProbe>,
        physical_encoding: JsonlPhysicalEncoding,
        record_framing: JsonlRecordFraming,
        frozen_observation: Option<&JsonlFileObservation>,
        direct_append: bool,
        route_resources: Option<&ctx_history_capture_runtime::SourceBackedRouteResources>,
    ) -> JsonlResult<Self, E> {
        let (bind_admitted_eof, deferred_append_eof_sha256) = match mode {
            JsonlSemanticPreflightMode::AdmittedEof(previous) => (true, previous.map(Some)),
            JsonlSemanticPreflightMode::CompletePrefix => (false, Some(None)),
        };
        Self::open_with_framing(
            identity,
            source_file,
            previous,
            probe,
            JsonlReaderFramingOptions {
                physical_encoding,
                record_framing,
                whole_record: false,
                bind_admitted_eof,
                deferred_append_eof_sha256,
                frozen_observation,
                direct_append,
                route_resources,
            },
        )
    }

    pub fn open_whole_record(
        identity: JsonlSourceIdentity,
        source_file: Arc<OpenedProviderSourceFile<E>>,
        previous: Option<&JsonlCheckpoint>,
    ) -> JsonlResult<Self, E> {
        Self::open_with_framing(
            identity,
            source_file,
            previous,
            None,
            JsonlReaderFramingOptions {
                physical_encoding: JsonlPhysicalEncoding::RawJsonl,
                record_framing: JsonlRecordFraming::ordinary(),
                whole_record: true,
                bind_admitted_eof: false,
                deferred_append_eof_sha256: None,
                frozen_observation: None,
                direct_append: false,
                route_resources: None,
            },
        )
    }

    fn open_with_framing(
        identity: JsonlSourceIdentity,
        source_file: Arc<OpenedProviderSourceFile<E>>,
        previous: Option<&JsonlCheckpoint>,
        probe: Option<JsonlProbe>,
        options: JsonlReaderFramingOptions<'_>,
    ) -> JsonlResult<Self, E> {
        let JsonlReaderFramingOptions {
            physical_encoding,
            record_framing,
            whole_record,
            bind_admitted_eof,
            deferred_append_eof_sha256,
            frozen_observation,
            direct_append,
            route_resources,
        } = options;
        source_file.revalidate_same_object()?;
        let current_metadata = source_file.file().metadata()?;
        let current_observation = observe_metadata::<E>(
            identity.source_path(),
            source_file.file(),
            &current_metadata,
        )?;
        let mut file = source_file.reopen_same_object()?;
        if observe_metadata::<E>(identity.source_path(), &file, &file.metadata()?)?
            != current_observation
        {
            return Err(E::source_changed());
        }
        let observation = match frozen_observation {
            Some(frozen) if frozen.admits_frozen_prefix_in(&current_observation) => frozen.clone(),
            Some(_) => return Err(E::source_changed()),
            None => current_observation,
        };

        let mut prefix_hasher = new_prefix_hasher();
        let mut complete_prefix_end = 0_u64;
        let mut next_physical_ordinal = 0_u64;
        let mut source_change = if previous.is_some() {
            JsonlSourceChange::Replace
        } else {
            JsonlSourceChange::Cold
        };
        let mut skip_scan = false;
        let mut unchanged_checkpoint = None;
        let mut semantic_append_resume = None;
        let mut used_direct_append = false;
        let physical_suffix_resume = physical_encoding != JsonlPhysicalEncoding::StandardZstdJsonl;

        if let Some(previous) = previous.filter(|checkpoint| checkpoint.supports(&identity)) {
            let previous_observation = previous.source_observation();
            let same_file = previous_observation.same_stable_file(&observation);
            if same_file
                && previous_observation.supports_exact_revalidation()
                && previous_observation == &observation
            {
                // Exact physical equality also proves an unfinished tail is
                // unchanged. Its complete prefix remains the certified
                // frontier, so no provider projection or publication work is
                // needed until the file itself changes.
                complete_prefix_end = previous.complete_prefix_end();
                next_physical_ordinal = previous.next_physical_ordinal();
                source_change = JsonlSourceChange::Unchanged;
                skip_scan = true;
                unchanged_checkpoint = Some(previous.clone());
            } else if previous_observation.differs_only_by_change_identity(&observation) {
                if let Some(admitted_eof_sha256) = previous.admitted_eof_sha256() {
                    super::authenticate_frozen_prefix_sha256(
                        identity.source_path(),
                        source_file.as_ref(),
                        &observation,
                        observation.length(),
                        admitted_eof_sha256,
                    )?;
                } else if previous.complete_prefix_end() == previous_observation.length() {
                    super::authenticate_frozen_prefix(
                        identity.source_path(),
                        source_file.as_ref(),
                        &observation,
                        observation.length(),
                        *previous.complete_prefix_sha256(),
                    )?;
                } else {
                    return Err(E::source_changed());
                }
                complete_prefix_end = previous.complete_prefix_end();
                next_physical_ordinal = previous.next_physical_ordinal();
                source_change = JsonlSourceChange::Unchanged;
                skip_scan = true;
                unchanged_checkpoint = Some(previous.clone());
            } else if physical_suffix_resume
                && same_file
                && observation.length() >= previous.complete_prefix_end()
            {
                if direct_append
                    && previous.terminal()
                    && observation.length() > previous.source_observation().length()
                    && previous.source_observation().length() == previous.complete_prefix_end()
                    && (!bind_admitted_eof
                        || deferred_append_eof_sha256
                            .flatten()
                            .is_some_and(|expected| {
                                previous
                                    .restore_admitted_eof_hasher()
                                    .is_some_and(|hasher| hasher.digest() == expected)
                            }))
                    && previous
                        .restore_complete_prefix_hasher()
                        .is_some_and(|restored| {
                            prefix_hasher = restored;
                            true
                        })
                {
                    complete_prefix_end = previous.complete_prefix_end();
                    next_physical_ordinal = previous.next_physical_ordinal();
                    source_change = JsonlSourceChange::Append;
                    semantic_append_resume = Some(JsonlSemanticAppendResume {
                        previous: previous.clone(),
                        admitted_eof_sha256: None,
                        position: None,
                    });
                    used_direct_append = true;
                } else if direct_append {
                    // A provider requested the versioned direct contract, but
                    // its physical continuation state was missing, corrupt,
                    // or bound to another frontier. Leave this as replacement
                    // instead of silently downgrading to a different append
                    // protocol.
                } else if let Some(admitted_eof_sha256) = deferred_append_eof_sha256 {
                    source_change = JsonlSourceChange::Append;
                    semantic_append_resume = Some(JsonlSemanticAppendResume {
                        previous: previous.clone(),
                        admitted_eof_sha256,
                        position: None,
                    });
                } else {
                    let observed_prefix = hash_prefix::<E, _>(
                        identity.source_path(),
                        &mut file,
                        previous.complete_prefix_end(),
                        new_prefix_hasher(),
                    )?;
                    if prefix_digest(&observed_prefix) == *previous.complete_prefix_sha256() {
                        prefix_hasher = observed_prefix;
                        complete_prefix_end = previous.complete_prefix_end();
                        next_physical_ordinal = previous.next_physical_ordinal();
                        if previous.terminal()
                            && observation.length() == previous.complete_prefix_end()
                        {
                            source_change = JsonlSourceChange::Unchanged;
                            skip_scan = true;
                            unchanged_checkpoint = Some(previous.clone());
                        } else {
                            source_change = JsonlSourceChange::Append;
                        }
                    }
                }
            }
        }

        if matches!(
            source_change,
            JsonlSourceChange::Cold | JsonlSourceChange::Replace
        ) {
            if let Some(probe) = probe {
                if probe.observation != observation {
                    if !probe.observation.admits_frozen_prefix_in(&observation) {
                        return Err(E::source_changed());
                    }
                    revalidate_frozen_prefix(
                        identity.source_path(),
                        source_file.as_ref(),
                        &probe.observation,
                        probe.complete_prefix_end,
                        prefix_digest(&probe.prefix_hasher),
                    )?;
                }
                prefix_hasher = probe.prefix_hasher;
                complete_prefix_end = probe.complete_prefix_end;
                next_physical_ordinal = probe.next_physical_ordinal;
            }
        }
        let full_hasher = if whole_record || skip_scan {
            None
        } else if used_direct_append {
            let restored = semantic_append_resume
                .as_ref()
                .and_then(|resume| resume.previous.restore_admitted_eof_hasher());
            Some(match restored {
                Some(restored) => restored,
                None => hash_prefix::<E, _>(
                    identity.source_path(),
                    &mut file,
                    complete_prefix_end,
                    JsonlResumableSha256::new(),
                )?,
            })
        } else if semantic_append_resume
            .as_ref()
            .is_some_and(|resume| resume.admitted_eof_sha256.is_some())
        {
            Some(JsonlResumableSha256::new())
        } else {
            let restored = (source_change == JsonlSourceChange::Append)
                .then_some(previous)
                .flatten()
                .filter(|previous| previous.source_observation().length() == complete_prefix_end)
                .and_then(JsonlCheckpoint::restore_admitted_eof_hasher);
            Some(match restored {
                Some(restored) => restored,
                None => hash_prefix::<E, _>(
                    identity.source_path(),
                    &mut file,
                    complete_prefix_end,
                    JsonlResumableSha256::new(),
                )?,
            })
        };
        file.seek(SeekFrom::Start(complete_prefix_end))?;
        let (reader, physical) = if skip_scan {
            (None, None)
        } else if whole_record {
            (Some(BufReader::new(file)), None)
        } else {
            (
                None,
                Some(JsonlPhysicalStream::open_with_encoding_and_resources(
                    file,
                    observation.length(),
                    complete_prefix_end,
                    next_physical_ordinal,
                    physical_encoding,
                    record_framing,
                    match (full_hasher, semantic_append_resume.as_ref()) {
                        (Some(full), Some(resume)) if resume.admitted_eof_sha256.is_some() => {
                            JsonlPhysicalDigest::full_complete_and_bounded_prefix(
                                full,
                                prefix_hasher.clone(),
                                Sha256::new(),
                                resume.previous.source_observation().length(),
                            )
                        }
                        (Some(full), _) => {
                            JsonlPhysicalDigest::full_and_complete(full, prefix_hasher.clone())
                        }
                        (None, _) => JsonlPhysicalDigest::complete(prefix_hasher.clone()),
                    },
                    E::source_changed,
                    route_resources,
                )?),
            )
        };
        Ok(Self {
            identity,
            observation,
            source_file,
            reader,
            physical,
            prefix_hasher,
            complete_prefix_end,
            next_physical_ordinal,
            source_change,
            skip_scan,
            unchanged_checkpoint,
            finished: false,
            outcome: None,
            record_buffer: Vec::new(),
            whole_record,
            append_log: !whole_record,
            bind_admitted_eof,
            complete_prefix_ends_with_terminal_nul_padding: false,
            semantic_append_resume,
            direct_append_resume: used_direct_append,
            semantic_preflight_binding: None,
            oversized_record_policy: JsonlOversizedRecordPolicy::RejectSource,
        })
    }

    pub fn set_oversized_record_policy(&mut self, policy: JsonlOversizedRecordPolicy) {
        self.oversized_record_policy = policy;
    }

    pub fn source_change(&self) -> JsonlSourceChange {
        self.source_change
    }

    pub fn outcome(&self) -> Option<&JsonlScanOutcome> {
        self.outcome.as_ref()
    }

    pub(super) fn next_execution_record(&mut self) -> JsonlResult<Option<JsonlPhysicalRecord>, E> {
        if self.finished {
            return Ok(None);
        }
        if self.skip_scan {
            self.finish(true)?;
            return Ok(None);
        }
        if self.whole_record {
            return Err(E::system_invariant(
                "whole-record JSON input cannot use the semantic executor",
            ));
        }
        self.capture_semantic_append_position()?;
        let record = self
            .physical
            .as_mut()
            .ok_or_else(|| E::system_invariant("semantic JSONL input lost its physical stream"))?
            .next_record()?;
        match record {
            None => self.finish(true)?,
            Some(record) if !record.complete => self.finish(false)?,
            Some(record) => {
                self.complete_prefix_ends_with_terminal_nul_padding = record.terminal_nul_padding;
            }
        }
        Ok(record)
    }

    fn capture_semantic_append_position(&mut self) -> JsonlResult<(), E> {
        if let Some(resume) = self.semantic_append_resume.as_mut() {
            let physical = self.physical.as_ref().ok_or_else(|| {
                E::system_invariant("semantic JSONL append lost its physical stream")
            })?;
            let expected_end = resume.previous.complete_prefix_end();
            if resume.position.is_none()
                && physical.offset() == expected_end
                && physical.next_physical_ordinal() == resume.previous.next_physical_ordinal()
                && prefix_digest(physical.digest().complete_hasher())
                    == *resume.previous.complete_prefix_sha256()
            {
                resume.position = Some(physical.position());
            }
            if self.direct_append_resume && resume.position.is_none() {
                return Err(E::system_invariant(
                    "direct JSONL append did not begin at its certified frontier",
                ));
            }
        }
        Ok(())
    }

    pub(super) fn execution_record_bytes(
        &self,
        record: JsonlPhysicalRecord,
    ) -> JsonlResult<&[u8], E> {
        Ok(self
            .physical
            .as_ref()
            .ok_or_else(|| E::system_invariant("semantic JSONL input lost its physical stream"))?
            .record_bytes(record))
    }

    pub(super) fn execution_position(&self) -> JsonlResult<JsonlPhysicalStreamPosition, E> {
        Ok(self
            .physical
            .as_ref()
            .ok_or_else(|| E::system_invariant("semantic JSONL input lost its physical stream"))?
            .position())
    }

    pub(super) fn restore_execution_position(
        &mut self,
        position: JsonlPhysicalStreamPosition,
    ) -> JsonlResult<(), E> {
        self.physical
            .as_mut()
            .ok_or_else(|| E::system_invariant("semantic JSONL input lost its physical stream"))?
            .restore(position)
    }

    pub(super) fn execution_offset(&self) -> JsonlResult<u64, E> {
        Ok(self
            .physical
            .as_ref()
            .ok_or_else(|| E::system_invariant("semantic JSONL input lost its physical stream"))?
            .offset())
    }

    pub(super) fn execution_complete_prefix_end(&self) -> JsonlResult<u64, E> {
        Ok(self
            .physical
            .as_ref()
            .ok_or_else(|| E::system_invariant("semantic JSONL input lost its physical stream"))?
            .complete_prefix_end())
    }

    pub(super) fn execution_certified_prefix_end(&self) -> Option<u64> {
        self.semantic_append_resume
            .as_ref()
            .map(|resume| resume.previous.complete_prefix_end())
    }

    pub(super) fn execution_is_direct_append_resume(&self) -> bool {
        self.direct_append_resume
    }

    pub(super) fn release_execution_record_buffer(&mut self) -> JsonlResult<(), E> {
        self.physical
            .as_mut()
            .ok_or_else(|| E::system_invariant("semantic JSONL input lost its physical stream"))?
            .release_record_buffer();
        Ok(())
    }

    pub(super) fn admitted_eof_sha256(&self) -> JsonlResult<Option<[u8; 32]>, E> {
        if !self.bind_admitted_eof {
            return Ok(None);
        }
        let full = self
            .physical
            .as_ref()
            .ok_or_else(|| {
                E::system_invariant("admitted-EOF JSONL input lost its physical stream")
            })?
            .digest()
            .full_hasher()
            .ok_or_else(|| E::system_invariant("admitted-EOF JSONL input lost its full digest"))?;
        Ok(Some(full.digest()))
    }

    pub(super) fn complete_prefix_ends_with_terminal_nul_padding(&self) -> bool {
        self.complete_prefix_ends_with_terminal_nul_padding
    }

    pub(super) fn settle_semantic_preflight(
        &mut self,
        initial: JsonlPhysicalStreamPosition,
        resume_append: bool,
        retain_failed_preflight: bool,
    ) -> JsonlResult<bool, E> {
        let binding = self.semantic_pass_binding()?;
        let (restore, ready) = match self.semantic_append_resume.as_ref() {
            Some(resume) => {
                let prefix_matches = resume.admitted_eof_sha256.is_none_or(|expected| {
                    self.physical
                        .as_ref()
                        .and_then(|physical| physical.digest().bounded_prefix())
                        .is_some_and(|(digest, remaining)| {
                            remaining == 0
                                && <[u8; 32]>::from(digest.clone().finalize()) == expected
                        })
                });
                match (resume_append && prefix_matches, resume.position.clone()) {
                    (true, Some(position)) => (position, true),
                    _ if !retain_failed_preflight => return Ok(false),
                    _ => (initial, false),
                }
            }
            None => (initial, true),
        };
        self.semantic_preflight_binding = Some(binding);
        #[cfg(any(test, feature = "test-support"))]
        revalidation::run_after_jsonl_semantic_preflight_hook(self.identity.source_path());
        self.physical
            .as_mut()
            .ok_or_else(|| E::system_invariant("semantic JSONL input lost its physical stream"))?
            .restore(restore)?;
        self.finished = false;
        self.outcome = None;
        Ok(ready)
    }

    fn semantic_pass_binding(&self) -> JsonlResult<JsonlSemanticPreflightBinding, E> {
        let physical = self
            .physical
            .as_ref()
            .ok_or_else(|| E::system_invariant("semantic JSONL input lost its physical stream"))?;
        if !self.finished
            || self.outcome.is_none()
            || physical.offset() != self.observation.length()
        {
            return Err(E::system_invariant(
                "semantic JSONL pass was sealed before its admitted EOF",
            ));
        }
        Ok(JsonlSemanticPreflightBinding {
            physical: physical.admitted_pass_binding(),
            complete_prefix_ends_with_terminal_nul_padding: self
                .complete_prefix_ends_with_terminal_nul_padding,
        })
    }

    pub fn visit_page<V>(
        &mut self,
        visit: &mut impl FnMut(JsonlRecordRef<'_>) -> std::result::Result<(), V>,
    ) -> std::result::Result<Option<JsonlPage>, V>
    where
        V: From<E>,
    {
        if self.finished {
            return Ok(None);
        }
        if self.skip_scan {
            self.finish(true).map_err(V::from)?;
            return Ok(None);
        }
        if self.whole_record {
            return self.visit_whole_record(visit);
        }

        let mut records = 0_usize;
        let mut page_bytes = 0_usize;
        while records < PAGE_MAX_RECORDS {
            self.capture_semantic_append_position().map_err(V::from)?;
            let (position, record) = {
                let physical = self.physical.as_mut().ok_or_else(|| {
                    V::from(E::system_invariant(
                        "ordinary JSONL source lost its physical stream",
                    ))
                })?;
                let position = physical.position();
                (position, physical.next_record().map_err(V::from)?)
            };
            let Some(record) = record else {
                self.finish(true).map_err(V::from)?;
                break;
            };
            if !record.complete {
                self.finish(false).map_err(V::from)?;
                break;
            }
            self.complete_prefix_ends_with_terminal_nul_padding = record.terminal_nul_padding;
            let wire_bytes = usize::try_from(record.byte_len()).unwrap_or(usize::MAX);
            let stored_record_bytes = {
                let record_bytes = self
                    .physical
                    .as_ref()
                    .ok_or_else(|| {
                        V::from(E::system_invariant(
                            "ordinary JSONL source lost its physical stream",
                        ))
                    })?
                    .record_bytes(record);
                record_bytes
                    .strip_suffix(b"\r")
                    .unwrap_or(record_bytes)
                    .len()
            };
            let oversized = record.oversized || stored_record_bytes > MAX_PROVIDER_JSONL_LINE_BYTES;
            if oversized && self.oversized_record_policy != JsonlOversizedRecordPolicy::RejectRecord
            {
                return Err(V::from(E::invalid_payload(format!(
                    "{}:{} exceeds the {} byte JSONL record limit",
                    self.identity.source_path().display(),
                    record.physical_ordinal.saturating_add(1),
                    MAX_PROVIDER_JSONL_LINE_BYTES
                ))));
            }

            if records != 0 && page_bytes.saturating_add(wire_bytes) > PAGE_MAX_BYTES {
                self.physical
                    .as_mut()
                    .ok_or_else(|| {
                        V::from(E::system_invariant(
                            "ordinary JSONL source lost its physical stream",
                        ))
                    })?
                    .restore(position)
                    .map_err(V::from)?;
                break;
            }

            let evidence = JsonlRecordEvidence::new(
                record.physical_ordinal,
                record.byte_start,
                record.byte_end_exclusive,
                record.sha256,
            );
            let record_bytes = self
                .physical
                .as_ref()
                .ok_or_else(|| {
                    V::from(E::system_invariant(
                        "ordinary JSONL source lost its physical stream",
                    ))
                })?
                .record_bytes(record);
            let record_bytes = record_bytes.strip_suffix(b"\r").unwrap_or(record_bytes);
            visit(JsonlRecordRef::new(record_bytes, evidence, oversized))?;
            records = records.saturating_add(1);
            page_bytes = page_bytes.saturating_add(wire_bytes);
        }

        if records == 0 {
            return Ok(None);
        }
        Ok(Some(JsonlPage))
    }

    fn visit_whole_record<V>(
        &mut self,
        visit: &mut impl FnMut(JsonlRecordRef<'_>) -> std::result::Result<(), V>,
    ) -> std::result::Result<Option<JsonlPage>, V>
    where
        V: From<E>,
    {
        if self.complete_prefix_end != 0 || self.next_physical_ordinal != 0 {
            return Err(V::from(E::invalid_payload(
                "whole-record JSON source has a non-empty scan frontier".to_owned(),
            )));
        }
        if self.observation.length() == 0 {
            self.finish(true).map_err(V::from)?;
            return Ok(None);
        }
        let length = usize::try_from(self.observation.length()).map_err(|_| {
            V::from(E::invalid_payload(
                "whole-record JSON source exceeds platform limits".to_owned(),
            ))
        })?;
        if length > MAX_PROVIDER_JSONL_LINE_BYTES {
            return Err(V::from(E::invalid_payload(format!(
                "{} exceeds the {} byte whole-record JSON limit",
                self.identity.source_path().display(),
                MAX_PROVIDER_JSONL_LINE_BYTES
            ))));
        }
        self.record_buffer.resize(length, 0);
        self.reader
            .as_mut()
            .ok_or_else(|| {
                V::from(E::system_invariant(
                    "whole-record JSON source lost its reader",
                ))
            })?
            .read_exact(&mut self.record_buffer)
            .map_err(E::from)
            .map_err(V::from)?;
        self.prefix_hasher.update(&self.record_buffer);
        let evidence = JsonlRecordEvidence::new(
            0,
            0,
            self.observation.length(),
            Sha256::digest(&self.record_buffer).into(),
        );
        visit(JsonlRecordRef::new(&self.record_buffer, evidence, false))?;
        self.complete_prefix_end = self.observation.length();
        self.next_physical_ordinal = 1;
        self.finish(true).map_err(V::from)?;
        Ok(Some(JsonlPage))
    }

    fn checkpoint(&self, terminal: bool) -> JsonlCheckpoint {
        let (complete_prefix_end, complete_prefix_hasher, next_physical_ordinal) =
            match self.physical.as_ref() {
                Some(physical) => (
                    physical.complete_prefix_end(),
                    physical.digest().complete_hasher(),
                    physical.next_physical_ordinal(),
                ),
                None => (
                    self.complete_prefix_end,
                    &self.prefix_hasher,
                    self.next_physical_ordinal,
                ),
            };
        JsonlCheckpoint::new_with_prefix_state(
            self.identity.clone(),
            self.observation.clone(),
            complete_prefix_end,
            complete_prefix_hasher,
            self.physical
                .as_ref()
                .and_then(|physical| physical.digest().full_hasher()),
            next_physical_ordinal,
            terminal,
        )
    }

    fn finish(&mut self, terminal: bool) -> JsonlResult<(), E> {
        if let Some(expected) = self.semantic_preflight_binding.as_ref() {
            let physical = self.physical.as_ref().ok_or_else(|| {
                E::system_invariant("semantic JSONL input lost its physical stream")
            })?;
            if physical.terminal() != terminal {
                return Err(E::system_invariant(
                    "semantic JSONL terminal state disagreed with physical framing",
                ));
            }
            let actual = JsonlSemanticPreflightBinding {
                physical: physical.admitted_pass_binding(),
                complete_prefix_ends_with_terminal_nul_padding: self
                    .complete_prefix_ends_with_terminal_nul_padding,
            };
            if &actual != expected {
                return Err(E::source_changed());
            }
        }
        let checkpoint = self.checkpoint(terminal);
        let current = observe_metadata::<E>(
            self.identity.source_path(),
            self.source_file.file(),
            &self.source_file.file().metadata()?,
        )?;
        if current == self.observation {
            if self.append_log {
                // The retained authority may have been opened before an
                // identity probe observed a legitimate append. The scan is
                // bound to `self.observation`, so require that exact
                // observation plus same-object routing rather than the
                // authority handle's older, metadata-sensitive stamp.
                self.source_file.revalidate_same_object()?;
            } else {
                self.source_file.revalidate()?;
            }
        } else {
            if self.observation.differs_only_by_change_identity(&current) {
                if let Some(admitted_eof_sha256) = checkpoint.admitted_eof_sha256() {
                    super::authenticate_frozen_prefix_sha256(
                        self.identity.source_path(),
                        self.source_file.as_ref(),
                        &current,
                        current.length(),
                        admitted_eof_sha256,
                    )?;
                } else if checkpoint.complete_prefix_end() == self.observation.length() {
                    super::authenticate_frozen_prefix(
                        self.identity.source_path(),
                        self.source_file.as_ref(),
                        &current,
                        current.length(),
                        *checkpoint.complete_prefix_sha256(),
                    )?;
                } else {
                    return Err(E::source_changed());
                }
                self.source_file.revalidate_same_object()?;
            } else if !self.append_log {
                return Err(E::source_changed());
            } else if self.direct_append_resume {
                if !self.observation.admits_frozen_prefix_in(&current) {
                    return Err(E::source_changed());
                }
                self.source_file.revalidate_same_object()?;
            } else {
                revalidate_frozen_prefix(
                    self.identity.source_path(),
                    self.source_file.as_ref(),
                    &self.observation,
                    checkpoint.complete_prefix_end(),
                    *checkpoint.complete_prefix_sha256(),
                )?;
            }
        }
        self.outcome = Some(JsonlScanOutcome::new(
            self.unchanged_checkpoint.clone().unwrap_or(checkpoint),
        ));
        self.finished = true;
        Ok(())
    }
}
