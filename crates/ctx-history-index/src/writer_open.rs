use super::*;

impl GenerationWriter {
    /// Captures an exact event-identity lookup pinned to this writer's base generation.
    pub fn base_event_identity_lookup(&self) -> BaseEventIdentityLookup {
        BaseEventIdentityLookup {
            searcher: self
                .base_publication
                .as_ref()
                .map(PinnedPublication::searcher)
                .cloned(),
            event_id_field: self.fields.event_id,
        }
    }

    pub fn open(
        root: impl AsRef<Path>,
        options: WriterOptions,
    ) -> Result<GenerationWriterOpenOutcome> {
        ctx_history_platform::raise_open_file_soft_limit();
        let indexer_threads = options.indexer_threads.clamp(1, 8);
        let minimum = INDEX_MEMORY_MIN_PER_THREAD.saturating_mul(indexer_threads);
        if options.memory_bytes < minimum {
            return Err(IndexError::IndexMemoryTooSmall {
                actual: options.memory_bytes,
                minimum,
            });
        }
        let options = WriterOptions {
            indexer_threads,
            memory_bytes: options.memory_bytes,
        };
        let changed_session_registry_memory_bytes = options.memory_bytes;
        let requested_root = root.as_ref().to_path_buf();
        ctx_history_platform::platform_security::ensure_private_directory(&requested_root)?;
        let directory =
            DurableMmapDirectory::open(&requested_root).map_err(tantivy::TantivyError::from)?;
        let root = directory.root_path().to_path_buf();
        ctx_history_platform::platform_security::ensure_private_directory(
            &root.join(MANIFEST_DIRECTORY),
        )?;
        let generation_writer_lock = Lock {
            filepath: PathBuf::from(GENERATION_WRITER_LOCK_FILE),
            is_blocking: false,
        };
        let preflight_lock =
            acquire_generation_writer_lock_with_retry(&directory, &generation_writer_lock)?;
        reclaim_abandoned_atomic_writes(&root)?;
        reclaim_abandoned_atomic_writes(&root.join(MANIFEST_DIRECTORY))?;
        ctx_history_index_generation::ensure_generation_control_files_private_with_writer_lock_held(
            &root,
        )?;
        ctx_history_index_format::clear_manifest_cache_for_root(&root)?;

        let (active_authority, mut pointer_requires_rebuild) =
            match load_active_publication_authority(&root) {
                Ok(pointer) => (pointer, false),
                Err(error) if generation_incompatibility_requires_rebuild(&error) => (None, true),
                Err(error) => return Err(error),
            };
        if !pointer_requires_rebuild {
            if let Some(authority) = active_authority.as_ref() {
                let schema_check = open_slot_index(&root, authority.pointer().active())
                    .and_then(|index| validate_schema(&index.schema()));
                if let Err(error) = schema_check {
                    if generation_incompatibility_requires_rebuild(&error) {
                        // A retired schema is disposable source projection, not
                        // clone authority. Keep its pointer untouched until a
                        // fresh current candidate is completely published.
                        pointer_requires_rebuild = true;
                    } else {
                        return Err(error);
                    }
                }
            }
        }
        let active_pointer_fence =
            ctx_history_index_generation::ActiveGenerationPointerFence::capture(
                &root,
                active_authority
                    .as_ref()
                    .map(ActivePublicationAuthority::pointer),
            )?;
        if !pointer_requires_rebuild {
            let active_pointer_ref = active_authority
                .as_ref()
                .map(ActivePublicationAuthority::pointer);
            let retention_lease = load_generation_retention_lease(&root)?;
            reclaim_inactive_generation_directories(
                &root,
                active_pointer_ref,
                retention_lease.as_ref(),
            )?;
            let mut retained_generation_ids = active_pointer_ref
                .into_iter()
                .flat_map(|pointer| std::iter::once(pointer.active()).chain(pointer.previous()))
                .map(|slot| slot.generation_id().to_owned())
                .collect::<Vec<_>>();
            retained_generation_ids.extend(
                retention_lease
                    .as_ref()
                    .map(|lease| lease.generation_id().to_owned()),
            );
            reclaim_unreferenced_manifests(&root, &retained_generation_ids)?;
            reclaim_unreferenced_certifications(
                &root,
                active_pointer_ref,
                retention_lease.as_ref(),
            )?;
        }

        let writer = (|| -> Result<Self> {
            let rebuild_marked = if pointer_requires_rebuild {
                // The unsupported pointer remains the sole durable publication
                // authority until a complete current candidate atomically replaces
                // it. Its slots and manifests are intentionally not decoded or
                // reclaimed during staging.
                true
            } else if let Some(marker) = load_active_generation_rebuild_marker(&root)? {
                if active_authority.as_ref().is_some_and(|authority| {
                    authority.pointer().active().generation_id() == marker.generation_id
                        && authority.pointer().active().directory() == marker.directory
                }) {
                    // The prior physical integrity check failed. Keep serving the
                    // old pointer until a fresh source-authoritative candidate is
                    // verified and atomically replaces it, but do not expose the
                    // corrupt generation as reusable base state.
                    true
                } else {
                    // Publication completed after the marker was written but before
                    // its cleanup. It no longer applies to the active generation.
                    clear_active_generation_rebuild_marker(&root)?;
                    false
                }
            } else {
                false
            };

            let reusable_generation = if !rebuild_marked {
                active_authority
                    .as_ref()
                    .map(|authority| open_pinned_publication(&root, authority))
                    .transpose()
                    .or_else(|error| {
                        if matches!(error, IndexError::ChecksumMismatch) {
                            Err(error)
                        } else if generation_incompatibility_requires_rebuild(&error) {
                            Ok(None)
                        } else {
                            Err(error)
                        }
                    })?
            } else {
                None
            };
            let active_pointer = active_authority.map(ActivePublicationAuthority::into_pointer);

            let (
                index,
                candidate_directory_name,
                candidate_physical_proof,
                candidate_activation_fence,
                fields,
                base_publication,
                base_opstamp,
            ) = match reusable_generation {
                Some(OpenedPinnedPublication::Published(publication)) => {
                    let (index, fields, opstamp, publication) = publication.into_writer_parts()?;
                    (index, None, None, None, fields, Some(publication), opstamp)
                }
                Some(OpenedPinnedPublication::Empty(empty)) => {
                    let (index, fields, opstamp) = empty.into_parts();
                    (index, None, None, None, fields, None, opstamp)
                }
                None => {
                    // The active slot is absent, physically rejected, or belongs to
                    // an incompatible disposable generation. Build an empty current
                    // candidate and retain only the pointer as publication authority.
                    let candidate = create_candidate_generation(&root, None, options.memory_bytes)?;
                    validate_schema(&candidate.index.schema())?;
                    let fields = fields_from_schema(&candidate.index.schema())?;
                    let metas = candidate.index.load_metas()?;
                    (
                        candidate.index,
                        Some(candidate.directory_name),
                        None,
                        Some(candidate.activation_fence),
                        fields,
                        None,
                        metas.opstamp,
                    )
                }
            };
            let core_record_preparer = CoreRecordPreparer::new(
                fields,
                active_pointer
                    .as_ref()
                    .map(|pointer| pointer.active().generation_id().to_owned()),
            );
            let mut source_identities = HashMap::new();
            if let Some(manifest) = base_publication.as_ref().map(PinnedPublication::manifest) {
                for source in &manifest.sources {
                    register_compact_identity(
                        &mut source_identities,
                        source.observation().source().identity(),
                        "source",
                        false,
                    )?;
                }
            }
            Ok(Self {
                root,
                index,
                active_pointer,
                active_pointer_fence,
                candidate_directory_name,
                candidate_physical_proof,
                candidate_activation_fence,
                preflight_lock: Some(preflight_lock),
                writer: None,
                writer_options: options,
                fields,
                base_publication,
                base_opstamp,
                core_record_preparer,
                complete_inventories: Vec::new(),
                pending: HashMap::new(),
                deletions: HashMap::new(),
                route_deletions: HashSet::new(),
                present_source_routes: None,
                applied_provider_roots: None,
                authorized_topology_route_retirements: None,
                observed_missing_routes: HashMap::new(),
                route_publication_revalidations: Vec::new(),
                partially_reconciled_routes: BTreeSet::new(),
                partial_source_route_deltas: BTreeMap::new(),
                source_identities,
                changed_sessions: HashMap::new(),
                changed_session_registry_memory_bytes,
                source_route_plan: None,
                active_source_route_stage: None,
                active_source_route_cohort_stage: None,
                reusable_base_rebuild_detail: None,
                #[cfg(test)]
                index_writer_constructions: std::sync::Arc::new(
                    std::sync::atomic::AtomicUsize::new(0),
                ),
                #[cfg(test)]
                before_writer_handoff: None,
                #[cfg(test)]
                before_candidate_commit: None,
                #[cfg(test)]
                after_candidate_commit: None,
                #[cfg(test)]
                return_commit_error_after_visibility: false,
                #[cfg(test)]
                before_pointer_switch: None,
                #[cfg(test)]
                before_pointer_publication: None,
                #[cfg(test)]
                after_pointer_switch: None,
            })
        })();
        writer.map(GenerationWriterOpenOutcome::Ready)
    }

    /// Returns the base generation captured after this writer acquired
    /// Tantivy's exclusive writer lock.
    pub fn base_manifest(&self) -> Option<&GenerationManifest> {
        self.base_publication
            .as_ref()
            .map(PinnedPublication::manifest)
    }

    /// Returns the exact persisted manifest descriptor captured with the base
    /// publication. This may differ from the digest of the materialized full
    /// manifest when the publication uses a compact delta descriptor.
    pub fn base_generation_id(&self) -> Option<&str> {
        self.base_publication
            .as_ref()
            .map(PinnedPublication::generation_id)
    }

    /// Registers one complete provider inventory captured by the current
    /// refresh. Exact no-op admission requires these inventories to cover the
    /// full retained/removal set and requires a separate terminal callback to
    /// revalidate each exact certificate.
    pub fn certify_complete_inventory(
        &mut self,
        inventory: CertifiedSourceInventory,
    ) -> Result<()> {
        if self.source_route_plan.is_some() && self.active_source_route_stage.is_none() {
            return Err(IndexError::InvalidSourceRoutePlan(
                "complete inventory certification requires an active selected route".to_owned(),
            ));
        }
        inventory.validate_contract()?;
        let observation = inventory.observation();
        if self.complete_inventories.iter().any(|existing| {
            let existing = existing.observation();
            existing.provider() == observation.provider()
                && existing.authority_namespace() == observation.authority_namespace()
                && existing.authority_key() == observation.authority_key()
        }) {
            return Err(IndexError::DuplicateCompleteInventoryAuthority {
                provider: observation.provider().to_owned(),
                authority_namespace: observation.authority_namespace().to_owned(),
            });
        }
        self.complete_inventories.push(inventory);
        Ok(())
    }

    pub(super) fn exact_replay_inventory_witness(
        &self,
    ) -> Result<Option<ExactReplayInventoryWitness<'_>>> {
        if self.writer.is_some() || !self.deletions.is_empty() {
            return Ok(None);
        }
        // Reuse would leave a migrated v8/v9 descriptor as durable authority.
        // Send this one publication through the atomic candidate path instead.
        if self
            .base_publication
            .as_ref()
            .is_some_and(PinnedPublication::requires_current_manifest_anchor)
        {
            return Ok(None);
        }
        let Some(base) = self.base_manifest() else {
            return Ok(None);
        };
        if self
            .applied_provider_roots
            .as_ref()
            .is_some_and(|(automatic, digest, roots)| {
                *automatic != base.automatic_provider_discovery()
                    || digest != base.provider_root_config_digest()
                    || roots != base.provider_roots()
            })
        {
            // Provider-source policy is part of the durable manifest. Even a
            // route-empty refresh must publish that policy transition instead
            // of reusing a byte-identical source generation.
            return Ok(None);
        }
        if !self.observed_missing_routes.is_empty() || !self.route_deletions.is_empty() {
            return Ok(None);
        }
        if self.present_source_routes.as_ref().is_some_and(|routes| {
            routes.len() != base.source_routes().len()
                || routes
                    .iter()
                    .zip(base.source_routes())
                    .any(|(present, prior)| !present.exact_snapshot_eq(prior))
        }) {
            // Missing-state reset and route membership changes are manifest
            // mutations even when every Core source is otherwise unchanged.
            return Ok(None);
        }

        // A no-work candidate is a full-inventory claim except for routes
        // explicitly authenticated as exact carry-forward from this locked
        // base. Do not silently carry any other omitted source.
        if let Some(missing) = base
            .source_routes()
            .iter()
            .filter(|route| {
                !self
                    .source_route_plan
                    .as_ref()
                    .is_some_and(|plan| plan.carried_from_base.contains(route.route_identity()))
                    && !self
                        .partially_reconciled_routes
                        .contains(route.route_identity())
            })
            .flat_map(SourceRouteSnapshot::sources)
            .find(|source| !self.pending.contains_key(&source_token(source)))
        {
            return Err(IndexError::IncompleteExactReplayCoverage {
                source_id: missing.identity().to_string(),
            });
        }
        let retained_sources_are_exact = self.pending.values().all(|pending| {
            base.sources
                .binary_search_by_key(&pending.source.identity().digest(), |base_source| {
                    base_source.observation().source().identity().digest()
                })
                .ok()
                .and_then(|index| base.sources.get(index))
                .filter(|base_source| {
                    base_source
                        .observation()
                        .source()
                        .exact_descriptor_eq(&pending.source)
                })
                .is_some_and(|base_source| {
                    matches!(
                        (&pending.mode, &pending.certificate),
                        (
                            PendingSourceMode::Append { base }
                                | PendingSourceMode::Retain { base },
                            Some(current),
                        )
                            if pending.staged_documents == 0
                                && base == base_source
                                && current == base_source
                    )
                })
        });
        if !retained_sources_are_exact {
            return Ok(None);
        }
        if self.complete_inventories.is_empty()
            && (!self.pending.is_empty() || self.source_route_plan.is_none())
        {
            // Without a certified current inventory this is not an admissible
            // exact no-op. Preserve compatibility for mutation-oriented
            // callers by taking the ordinary IndexWriter publication path.
            return Ok(None);
        }

        let mut covered_sources = HashSet::new();
        for inventory in &self.complete_inventories {
            let newly_matched = base
                .sources
                .iter()
                .filter(|source| inventory.contains(source.observation().source()))
                .filter(|source| {
                    covered_sources.insert(source.observation().source().identity().digest())
                })
                .count();
            if newly_matched != inventory.observed_sources() {
                return Err(IndexError::ExactReplayInventoryCountMismatch {
                    provider: inventory.observation().provider().to_owned(),
                    observed: inventory.observed_sources(),
                    matched: newly_matched,
                });
            }
        }
        if let Some(missing) = base
            .source_routes()
            .iter()
            .filter(|route| {
                !self
                    .source_route_plan
                    .as_ref()
                    .is_some_and(|plan| plan.carried_from_base.contains(route.route_identity()))
                    && !self
                        .partially_reconciled_routes
                        .contains(route.route_identity())
            })
            .flat_map(SourceRouteSnapshot::sources)
            .find(|source| {
                !self.pending.contains_key(&source_token(source))
                    && !covered_sources.contains(&source.identity().digest())
            })
        {
            return Err(IndexError::IncompleteExactReplayCoverage {
                source_id: missing.identity().to_string(),
            });
        }
        Ok(Some(ExactReplayInventoryWitness { base }))
    }
}

#[cfg(all(test, unix))]
mod process_resource_tests {
    use std::{env, fs::File, process::Command};

    use tempfile::tempdir;

    use super::*;

    const CHILD_ENV: &str = "CTX_TEST_GENERATION_WRITER_OPEN_FILE_LIMIT_CHILD";
    const LOW_SOFT_LIMIT: libc::rlim_t = 64;
    const EXPECTED_SOFT_LIMIT_TARGET: libc::rlim_t = 4_096;

    #[test]
    fn generation_writer_open_raises_child_open_file_limit_before_staging() {
        if env::var_os(CHILD_ENV).is_some() {
            run_open_file_limit_child();
            return;
        }

        let parent_limits_before = open_file_limits();
        assert!(
            parent_limits_before.1 > LOW_SOFT_LIMIT,
            "regression requires a hard open-file limit above {LOW_SOFT_LIMIT}"
        );
        let status = Command::new(env::current_exe().unwrap())
            .arg("--exact")
            .arg(
                "writer_open::process_resource_tests::generation_writer_open_raises_child_open_file_limit_before_staging",
            )
            .arg("--nocapture")
            .env(CHILD_ENV, "1")
            .status()
            .unwrap();
        assert!(status.success(), "open-file limit child failed: {status}");
        assert_eq!(open_file_limits(), parent_limits_before);
    }

    fn run_open_file_limit_child() {
        let root = tempdir().unwrap();
        let inherited = open_file_limits();
        assert!(inherited.1 > LOW_SOFT_LIMIT);
        let lowered = libc::rlimit {
            rlim_cur: LOW_SOFT_LIMIT,
            rlim_max: inherited.1,
        };
        assert_eq!(
            unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const lowered) },
            0,
            "failed to lower only the child open-file soft limit: {}",
            std::io::Error::last_os_error()
        );
        assert_eq!(open_file_limits(), (LOW_SOFT_LIMIT, inherited.1));

        let mut retained_files = Vec::new();
        loop {
            match File::open("/dev/null") {
                Ok(file) => retained_files.push(file),
                Err(error) => {
                    assert_eq!(error.raw_os_error(), Some(libc::EMFILE));
                    break;
                }
            }
        }

        let outcome =
            GenerationWriter::open(root.path().join("index"), WriterOptions::default()).unwrap();
        let raised = open_file_limits();
        assert_eq!(raised.1, inherited.1);
        assert_eq!(raised.0, inherited.1.min(EXPECTED_SOFT_LIMIT_TARGET));
        drop(outcome);
        drop(retained_files);
    }

    fn open_file_limits() -> (libc::rlim_t, libc::rlim_t) {
        let mut limits = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limits) },
            0,
            "failed to read the open-file limit: {}",
            std::io::Error::last_os_error()
        );
        (limits.rlim_cur, limits.rlim_max)
    }
}
