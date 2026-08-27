use super::*;

impl<E: JsonlFamilyError> Clone for JsonlFamilyLeaf<E> {
    fn clone(&self) -> Self {
        Self {
            source: self.source.clone(),
            source_path: self.source_path.clone(),
            authority_path: self.authority_path.clone(),
            authority: Arc::clone(&self.authority),
            observation: self.observation.clone(),
            logical_eof: self.logical_eof,
            binding: self.binding.clone(),
            terminal_dependencies: self.terminal_dependencies.clone(),
            identity_probe: self.identity_probe.clone(),
            identity_probe_rejected_records: self.identity_probe_rejected_records,
            whole_record: self.whole_record,
            freeze_observation_at_scan: self.freeze_observation_at_scan,
        }
    }
}

impl<E: JsonlFamilyError> JsonlFamilyLeaf<E> {
    /// Binds admission to a descriptor already retained by an optimized
    /// adapter. The adapter may keep the same descriptor for its scan, avoiding
    /// a pathname reopen between shared leaf admission and provider parsing.
    pub fn bind_opened(
        source: SourceKey,
        source_path: PathBuf,
        authority: Arc<ProviderSourceRoot<E>>,
        authority_path: PathBuf,
        binding: TypedKey,
        opened: &OpenedProviderSourceFile<E>,
    ) -> JsonlResult<Self, E> {
        let observation = observe_opened_file(&source_path, opened)?;
        Ok(Self::bind_observed(
            source,
            source_path,
            authority,
            authority_path,
            binding,
            observation,
        ))
    }

    pub fn bind_observed(
        source: SourceKey,
        source_path: PathBuf,
        authority: Arc<ProviderSourceRoot<E>>,
        authority_path: PathBuf,
        binding: TypedKey,
        observation: JsonlFileObservation,
    ) -> Self {
        Self {
            source,
            source_path,
            authority_path,
            authority,
            observation,
            logical_eof: None,
            binding,
            terminal_dependencies: JsonlFamilyLeafTerminalDependencies::default(),
            identity_probe: None,
            identity_probe_rejected_records: 0,
            whole_record: false,
            freeze_observation_at_scan: false,
        }
    }

    pub fn bind_frozen_observed(
        source: SourceKey,
        source_path: PathBuf,
        authority: Arc<ProviderSourceRoot<E>>,
        authority_path: PathBuf,
        binding: TypedKey,
        observation: JsonlFileObservation,
    ) -> Self {
        Self {
            source,
            source_path,
            authority_path,
            authority,
            observation,
            logical_eof: None,
            binding,
            terminal_dependencies: JsonlFamilyLeafTerminalDependencies::default(),
            identity_probe: None,
            identity_probe_rejected_records: 0,
            whole_record: false,
            freeze_observation_at_scan: true,
        }
    }

    pub fn observe(
        source: SourceKey,
        source_path: PathBuf,
        authority: Arc<ProviderSourceRoot<E>>,
        authority_path: PathBuf,
        binding: TypedKey,
    ) -> JsonlResult<Self, E> {
        Self::observe_with_framing(
            source,
            source_path,
            authority,
            authority_path,
            binding,
            false,
        )
    }

    pub fn observe_whole_record(
        source: SourceKey,
        source_path: PathBuf,
        authority: Arc<ProviderSourceRoot<E>>,
        authority_path: PathBuf,
        binding: TypedKey,
    ) -> JsonlResult<Self, E> {
        Self::observe_with_framing(
            source,
            source_path,
            authority,
            authority_path,
            binding,
            true,
        )
    }

    pub fn observe_after_identity_probe(
        source: SourceKey,
        source_path: PathBuf,
        authority: Arc<ProviderSourceRoot<E>>,
        authority_path: PathBuf,
        binding: TypedKey,
        mut identity_probe: JsonlProbe,
        identity_probe_rejected_records: u64,
    ) -> JsonlResult<Self, E> {
        let opened = authority.open_file(&authority_path)?;
        let observation = observe_opened_file(&source_path, &opened)?;
        if observation != identity_probe.observation {
            revalidate_frozen_prefix(
                &source_path,
                &opened,
                &identity_probe.observation,
                identity_probe.complete_prefix_end,
                super::super::prefix_digest(&identity_probe.prefix_hasher),
            )?;
            identity_probe.observation = observation.clone();
        }
        drop(opened);
        Ok(Self {
            source,
            source_path,
            authority_path,
            authority,
            observation,
            logical_eof: None,
            binding,
            terminal_dependencies: JsonlFamilyLeafTerminalDependencies::default(),
            identity_probe: Some(identity_probe),
            identity_probe_rejected_records,
            whole_record: false,
            freeze_observation_at_scan: false,
        })
    }

    fn observe_with_framing(
        source: SourceKey,
        source_path: PathBuf,
        authority: Arc<ProviderSourceRoot<E>>,
        authority_path: PathBuf,
        binding: TypedKey,
        whole_record: bool,
    ) -> JsonlResult<Self, E> {
        let opened = authority.open_file(&authority_path)?;
        let observation = observe_opened_file(&source_path, &opened)?;
        drop(opened);
        Ok(Self {
            source,
            source_path,
            authority_path,
            authority,
            observation,
            logical_eof: None,
            binding,
            terminal_dependencies: JsonlFamilyLeafTerminalDependencies::default(),
            identity_probe: None,
            identity_probe_rejected_records: 0,
            whole_record,
            freeze_observation_at_scan: false,
        })
    }

    pub fn source(&self) -> &SourceKey {
        &self.source
    }

    pub fn source_path(&self) -> &Path {
        &self.source_path
    }

    pub fn authority_path(&self) -> &Path {
        &self.authority_path
    }

    pub fn authority(&self) -> &Arc<ProviderSourceRoot<E>> {
        &self.authority
    }

    pub fn observation(&self) -> &JsonlFileObservation {
        &self.observation
    }

    /// Sets a provider-authoritative committed boundary while retaining the
    /// complete physical observation for terminal same-object validation.
    pub fn with_logical_eof(mut self, logical_eof: u64) -> JsonlResult<Self, E> {
        if self.whole_record || logical_eof > self.observation.length() {
            return Err(E::invalid_payload(
                "JSONL logical EOF is outside its retained physical observation".to_owned(),
            ));
        }
        if self
            .identity_probe
            .as_ref()
            .is_some_and(|probe| probe.complete_prefix_end > logical_eof)
        {
            return Err(E::invalid_payload(
                "JSONL logical EOF precedes its retained identity probe".to_owned(),
            ));
        }
        self.logical_eof = Some(logical_eof);
        Ok(self)
    }

    pub fn logical_eof(&self) -> Option<u64> {
        self.logical_eof
    }

    /// Adds a control file that was parsed through `opened` and must remain
    /// exactly unchanged until this leaf's terminal publication callback.
    pub fn with_exact_present_dependency(
        mut self,
        authority_path: PathBuf,
        opened: &OpenedProviderSourceFile<E>,
    ) -> JsonlResult<Self, E> {
        self.validate_terminal_dependency_path(&authority_path)?;
        self.terminal_dependencies.ensure_additional_capacity()?;
        let source_path = self.authority.named_path().join(&authority_path);
        let routed = self.authority.open_file(&authority_path)?;
        let observation = observe_opened_file(&source_path, opened)?;
        if opened.ordinary_file_token() != routed.ordinary_file_token()
            || observation != observe_opened_file(&source_path, &routed)?
        {
            return Err(E::invalid_payload(
                "JSONL exact dependency handle does not match its retained authority path"
                    .to_owned(),
            ));
        }
        let opened_length = usize::try_from(observation.length()).map_err(|_| {
            E::invalid_payload("JSONL exact dependency length exceeds usize".to_owned())
        })?;
        let aggregate_bytes = self
            .terminal_dependencies
            .present_content_bytes()?
            .checked_add(opened_length)
            .ok_or_else(|| {
                E::invalid_payload("JSONL exact dependency byte count overflowed".to_owned())
            })?;
        if aggregate_bytes > JSONL_FAMILY_MAX_LEAF_TERMINAL_PRESENT_BYTES {
            return Err(E::invalid_payload(format!(
                "JSONL leaf exact dependencies exceed the {JSONL_FAMILY_MAX_LEAF_TERMINAL_PRESENT_BYTES} aggregate byte limit"
            )));
        }
        let content = opened.read_exact_range(
            0,
            opened_length,
            JSONL_FAMILY_MAX_LEAF_TERMINAL_PRESENT_BYTES,
        )?;
        let content_length = u64::try_from(content.len()).map_err(|_| {
            E::invalid_payload("JSONL exact dependency length exceeds u64".to_owned())
        })?;
        if content_length != observation.length() {
            return Err(E::source_changed());
        }
        routed.revalidate_leaf()?;
        self.terminal_dependencies
            .present
            .push(JsonlFamilyExactPresentDependency {
                source_path,
                authority_path,
                authority: Arc::clone(&self.authority),
                observation,
                content_length,
                content_sha256: Sha256::digest(content).into(),
            });
        Ok(self)
    }

    /// Adds a path that is absent under this leaf's retained authority and
    /// must remain absent through this leaf's terminal publication callback.
    pub fn with_exact_absent_dependency(mut self, authority_path: PathBuf) -> JsonlResult<Self, E> {
        self.validate_terminal_dependency_path(&authority_path)?;
        self.terminal_dependencies.ensure_additional_capacity()?;
        let dependency = JsonlFamilyExactAbsentDependency {
            source_path: self.authority.named_path().join(&authority_path),
            authority_path,
            authority: Arc::clone(&self.authority),
        };
        if !dependency.remains_absent()? {
            return Err(E::invalid_payload(format!(
                "JSONL absent dependency {} is present",
                dependency.source_path.display()
            )));
        }
        self.terminal_dependencies.absent.push(dependency);
        Ok(self)
    }

    fn validate_terminal_dependency_path(&self, authority_path: &Path) -> JsonlResult<(), E> {
        if authority_path.as_os_str().is_empty()
            || !authority_path
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
            || authority_path == self.authority_path
            || self.terminal_dependencies.contains_path(authority_path)
        {
            return Err(E::invalid_payload(
                "JSONL leaf terminal dependency path is invalid or duplicated".to_owned(),
            ));
        }
        Ok(())
    }

    pub(super) fn admitted_length(&self) -> u64 {
        self.logical_eof.unwrap_or(self.observation.length())
    }

    pub(super) fn exact_scan_bytes(&self) -> Option<u64> {
        self.freeze_observation_at_scan
            .then_some(self.admitted_length())
    }

    pub(super) fn exact_scan_remaining(&self, emitted_bytes: u64) -> Option<u64> {
        self.exact_scan_bytes()?.checked_sub(emitted_bytes)
    }

    pub(super) fn frozen_scan_observation(&self) -> Option<&JsonlFileObservation> {
        self.freeze_observation_at_scan.then_some(&self.observation)
    }

    pub(super) fn estimated_scan_bytes(&self) -> u64 {
        self.admitted_length()
    }

    pub fn binding(&self) -> &TypedKey {
        &self.binding
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn open_verified(&self) -> JsonlResult<Arc<OpenedProviderSourceFile<E>>, E> {
        let opened = self.authority.open_file(&self.authority_path)?;
        if observe_opened_file(&self.source_path, &opened)? != self.observation {
            return Err(E::source_changed());
        }
        Ok(Arc::new(opened))
    }

    pub(super) fn open_for_scan(&self) -> JsonlResult<(Self, Arc<OpenedProviderSourceFile<E>>), E> {
        let opened = self.authority.open_file(&self.authority_path)?;
        let current = observe_opened_file(&self.source_path, &opened)?;
        if self
            .logical_eof
            .is_some_and(|logical_eof| logical_eof > current.length())
        {
            return Err(E::source_changed());
        }
        if current == self.observation {
            return Ok((self.clone(), Arc::new(opened)));
        }
        if self.observation.differs_only_by_change_identity(&current) {
            let mut leaf = self.clone();
            leaf.observation = current;
            return Ok((leaf, Arc::new(opened)));
        }
        if self.whole_record
            || current.length() <= self.observation.length()
            || !self.observation.admits_frozen_prefix_in(&current)
        {
            return Err(E::source_changed());
        }
        if self.freeze_observation_at_scan {
            return Ok((self.clone(), Arc::new(opened)));
        }
        let mut leaf = self.clone();
        leaf.observation = current.clone();
        if let Some(probe) = leaf.identity_probe.as_mut() {
            revalidate_frozen_prefix(
                &leaf.source_path,
                &opened,
                &probe.observation,
                probe.complete_prefix_end,
                super::super::prefix_digest(&probe.prefix_hasher),
            )?;
            probe.observation = current;
        }
        Ok((leaf, Arc::new(opened)))
    }
}
