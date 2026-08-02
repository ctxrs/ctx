use super::*;

impl FlatSegmentStore {
    pub(in crate::semantic) fn compact(&self) -> FlatResult<FlatPublishOutcome> {
        self.require_writable()?;
        let _transaction = self.lock_transaction()?;
        let _guard = self.lock_exclusive()?;
        let Some(current) = self.load_current_locked()? else {
            return Ok(noop_outcome(None));
        };
        if current
            .envelope
            .manifest
            .segments
            .iter()
            .all(|segment| segment.kind == SegmentKind::Base)
        {
            return Ok(noop_outcome(Some(&current)));
        }
        let (events, catalog_touches) =
            load_active_events(&self.root, &self.contract, &current.envelope.manifest, None)?;
        self.touch_metadata(catalog_touches);
        let active_chunks = events.iter().try_fold(0_u64, |total, event| {
            total
                .checked_add(u64::from(event.chunk_count))
                .ok_or_else(|| FlatStoreError::Corrupt("active chunk count overflow".to_owned()))
        })?;
        if u64::try_from(events.len()).ok() != Some(current.envelope.manifest.active_events)
            || active_chunks != current.envelope.manifest.active_chunks
        {
            return Err(FlatStoreError::Corrupt(
                "manifest counters disagree before full compaction".to_owned(),
            ));
        }
        let mut groups = BTreeMap::<String, (String, Vec<FlatActiveEvent>)>::new();
        for snapshot in &current.envelope.manifest.source_snapshots {
            let reconciliation_id = snapshot
                .receipt
                .as_ref()
                .map(|receipt| receipt.source_reconciliation_id.clone())
                .or_else(|| {
                    current
                        .envelope
                        .manifest
                        .segments
                        .iter()
                        .find(|descriptor| {
                            descriptor.generation == snapshot.generation
                                && descriptor.source_identity_digest
                                    == snapshot.source_identity_digest
                                && descriptor.kind == SegmentKind::Base
                        })
                        .map(|descriptor| descriptor.source_reconciliation_id.clone())
                })
                .ok_or_else(|| {
                    FlatStoreError::Corrupt(format!(
                        "source {} has no compaction reconciliation authority",
                        snapshot.source_identity_digest
                    ))
                })?;
            groups.insert(
                snapshot.source_identity_digest.clone(),
                (reconciliation_id, Vec::new()),
            );
        }
        for event in events.iter() {
            let (reconciliation_id, source_events) = groups
                .entry(event.source_identity_digest.clone())
                .or_insert_with(|| (event.source_reconciliation_id.clone(), Vec::new()));
            if *reconciliation_id != event.source_reconciliation_id {
                return Err(FlatStoreError::Corrupt(format!(
                    "source {} has mixed reconciliation authority",
                    event.source_identity_digest
                )));
            }
            source_events.push(event.clone());
        }
        let mut generation = current.envelope.manifest.generation;
        let mut staged = Vec::new();
        if groups.is_empty() {
            generation = generation.checked_add(1).ok_or_else(|| {
                FlatStoreError::Corrupt("manifest generation overflow".to_owned())
            })?;
            staged.push(write_empty_base_segment(
                &self.root,
                &self.contract,
                generation,
            )?);
        } else {
            for (source_identity_digest, (source_reconciliation_id, source_events)) in &groups {
                generation = generation.checked_add(1).ok_or_else(|| {
                    FlatStoreError::Corrupt("manifest generation overflow".to_owned())
                })?;
                staged.push(write_source_compacted_segment(
                    &self.root,
                    &self.contract,
                    generation,
                    &FlatSourceScope {
                        source_identity_digest: source_identity_digest.clone(),
                        source_reconciliation_id: source_reconciliation_id.clone(),
                    },
                    source_events,
                    &current.envelope.manifest,
                )?);
            }
        }
        sync_directory(&segments_directory(&self.root))?;
        for segment in &staged {
            validate_staged_segment(&self.root, &self.contract, &segment.descriptor)?;
        }
        let mut manifest = Manifest::new(self.contract.clone());
        manifest.generation = generation;
        manifest.created_unix_millis = unix_millis();
        manifest.active_events = u64::try_from(events.len())
            .map_err(|_| FlatStoreError::Corrupt("active event count is too large".to_owned()))?;
        manifest.active_chunks = active_chunks;
        manifest.source_snapshots = current.envelope.manifest.source_snapshots.clone();
        manifest.segments = staged
            .into_iter()
            .map(|segment| segment.descriptor)
            .collect();
        let snapshots = manifest
            .segments
            .iter()
            .filter(|segment| segment.source_identity_digest != UNSCOPED_SOURCE_IDENTITY)
            .map(|segment| (segment.source_identity_digest.clone(), segment.generation))
            .collect::<Vec<_>>();
        for (source, generation) in snapshots {
            set_source_snapshot(&mut manifest, &source, generation);
        }
        let selected = publish_manifest(&self.root, manifest)?;
        self.clear_pinned()?;
        self.record_compaction_work(active_chunks, events.len())?;
        let _ = cleanup_obsolete_locked(&self.root, &selected);
        Ok(FlatPublishOutcome {
            published: true,
            generation,
            generation_hash: Some(selected.generation_hash),
            replaced_events: events.len(),
            deleted_events: 0,
        })
    }

    pub(super) fn record_compaction_work(&self, vectors: u64, events: usize) -> FlatResult<()> {
        let vector_bytes = vectors
            .checked_mul(u64::from(self.contract.dimensions))
            .and_then(|value| value.checked_mul(4))
            .ok_or_else(|| {
                FlatStoreError::Corrupt("compaction vector bytes overflow".to_owned())
            })?;
        let event_records = u64::try_from(events).map_err(|_| {
            FlatStoreError::Corrupt("compaction event count is too large".to_owned())
        })?;
        self.vectors_touched.fetch_add(vectors, Ordering::Relaxed);
        self.vector_bytes_touched
            .fetch_add(vector_bytes, Ordering::Relaxed);
        self.touch_metadata(vectors.saturating_mul(2).saturating_add(event_records));
        Ok(())
    }

    pub(super) fn recover_internal(&self) -> FlatResult<FlatRecoveryReport> {
        let _transaction = self.lock_transaction()?;
        let _guard = self.lock_exclusive()?;
        let mut report = remove_temporary_files(&self.root)?;
        let selected = match select_manifest_any(&self.root) {
            Ok(selected) => selected,
            Err(FlatStoreError::LegacySchema(_)) => {
                reset_legacy_store(&self.root, &self.contract, &mut report)?;
                self.clear_pinned()?;
                report.model_contract_reset = true;
                return Ok(report);
            }
            Err(error) => return Err(error),
        };
        if let Some(selected) = selected {
            if selected.envelope.manifest.model == self.contract {
                merge_recovery_reports(
                    &mut report,
                    cleanup_obsolete_locked(&self.root, &selected)?,
                );
            } else {
                // A prior reset may have reached immutable segment rename but
                // not manifest publication. Retire only artifacts not named by
                // the still-active old manifest before retrying its generation.
                merge_recovery_reports(
                    &mut report,
                    cleanup_obsolete_locked(&self.root, &selected)?,
                );
                let generation = next_generation(Some(&selected))?;
                let staged = write_empty_base_segment(&self.root, &self.contract, generation)?;
                sync_directory(&segments_directory(&self.root))?;
                validate_staged_segment(&self.root, &self.contract, &staged.descriptor)?;
                let mut manifest = Manifest::new(self.contract.clone());
                manifest.generation = generation;
                manifest.created_unix_millis = unix_millis();
                manifest.segments.push(staged.descriptor);
                let reset = publish_manifest(&self.root, manifest)?;
                self.clear_pinned()?;
                report.model_contract_reset = true;
                merge_recovery_reports(&mut report, cleanup_obsolete_locked(&self.root, &reset)?);
            }
        } else {
            merge_recovery_reports(&mut report, cleanup_without_manifest(&self.root)?);
            self.clear_pinned()?;
        }
        Ok(report)
    }
}

fn reset_legacy_store(
    root: &Path,
    contract: &FlatModelContract,
    report: &mut FlatRecoveryReport,
) -> FlatResult<()> {
    for entry in fs::read_dir(manifests_directory(root)).map_err(|source| {
        io_error(
            "read legacy flat manifest directory",
            &manifests_directory(root),
            source,
        )
    })? {
        let entry = entry.map_err(|source| {
            io_error(
                "read legacy flat manifest entry",
                &manifests_directory(root),
                source,
            )
        })?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if parse_manifest_name(&name).is_some() {
            remove_recoverable_file(
                &entry.path(),
                &mut report.removed_obsolete_manifests,
                &mut report.retained_busy_files,
            )?;
        }
    }
    for entry in fs::read_dir(segments_directory(root)).map_err(|source| {
        io_error(
            "read legacy flat segment directory",
            &segments_directory(root),
            source,
        )
    })? {
        let entry = entry.map_err(|source| {
            io_error(
                "read legacy flat segment entry",
                &segments_directory(root),
                source,
            )
        })?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if name.starts_with(SEGMENT_PREFIX) {
            remove_recoverable_file(
                &entry.path(),
                &mut report.removed_orphan_segments,
                &mut report.retained_busy_files,
            )?;
        }
    }
    if report.retained_busy_files != 0 {
        return Err(FlatStoreError::Io {
            operation: "retire incompatible flat store",
            path: root.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "legacy flat artifacts are still busy",
            ),
        });
    }
    sync_directory(&manifests_directory(root))?;
    sync_directory(&segments_directory(root))?;
    let generation = 1;
    let staged = write_empty_base_segment(root, contract, generation)?;
    sync_directory(&segments_directory(root))?;
    validate_staged_segment(root, contract, &staged.descriptor)?;
    let mut manifest = Manifest::new(contract.clone());
    manifest.generation = generation;
    manifest.created_unix_millis = unix_millis();
    manifest.segments.push(staged.descriptor);
    let _ = publish_manifest(root, manifest)?;
    Ok(())
}

pub(super) fn write_source_compacted_segment(
    root: &Path,
    contract: &FlatModelContract,
    generation: u64,
    source: &FlatSourceScope,
    events: &[FlatActiveEvent],
    manifest: &Manifest,
) -> FlatResult<StagedSegment> {
    let vector_count = events.iter().try_fold(0_u64, |total, event| {
        total
            .checked_add(u64::from(event.chunk_count))
            .ok_or_else(|| FlatStoreError::Corrupt("active vector count overflow".to_owned()))
    })?;
    let mutation_count = u64::try_from(events.len())
        .map_err(|_| FlatStoreError::Corrupt("active event count is too large".to_owned()))?;
    let mut loaded = BTreeMap::<u64, LoadedSegment>::new();
    for event in events {
        if loaded.contains_key(&event.vector_generation) {
            continue;
        }
        let descriptor = manifest
            .segments
            .binary_search_by_key(&event.vector_generation, |segment| segment.generation)
            .ok()
            .map(|index| &manifest.segments[index])
            .ok_or_else(|| {
                FlatStoreError::Corrupt(format!(
                    "event {} references absent compacted vector generation",
                    event.event_id
                ))
            })?;
        if descriptor.source_identity_digest != source.source_identity_digest {
            return Err(FlatStoreError::Corrupt(format!(
                "event {} references a vector owned by another source",
                event.event_id
            )));
        }
        loaded.insert(
            event.vector_generation,
            load_and_validate_segment(root, contract, descriptor)?,
        );
    }
    let directory = segments_directory(root);
    let stride = usize_from_u32(vector_stride(contract.dimensions)?, "vector stride")?;
    let mut vectors = StagedArtifactWriter::new(&directory, generation, ArtifactRole::Vectors)?;
    let mut metadata = StagedArtifactWriter::new(&directory, generation, ArtifactRole::Metadata)?;
    let mut first_ordinals = HashMap::<Uuid, u64>::new();
    let mut ordinal = 0_u64;
    for event in events {
        first_ordinals.insert(event.event_id, ordinal);
        let segment = &loaded[&event.vector_generation];
        for offset in 0..u64::from(event.chunk_count) {
            let source_ordinal =
                event
                    .first_vector_ordinal
                    .checked_add(offset)
                    .ok_or_else(|| {
                        FlatStoreError::Corrupt("source vector ordinal overflow".to_owned())
                    })?;
            let source_ordinal = usize_from_u64(source_ordinal, "source vector ordinal")?;
            let vector_start = HEADER_BYTES
                .checked_add(
                    source_ordinal
                        .checked_mul(segment.stride_bytes)
                        .ok_or_else(|| {
                            FlatStoreError::Corrupt("source vector byte offset overflow".to_owned())
                        })?,
                )
                .ok_or_else(|| {
                    FlatStoreError::Corrupt("source vector byte offset overflow".to_owned())
                })?;
            let vector_end = vector_start.checked_add(stride).ok_or_else(|| {
                FlatStoreError::Corrupt("source vector byte range overflow".to_owned())
            })?;
            let vector = segment
                .vectors
                .get(vector_start..vector_end)
                .ok_or_else(|| {
                    FlatStoreError::Corrupt("source vector range exceeds its segment".to_owned())
                })?;
            let chunk = metadata_at(&segment.metadata, source_ordinal);
            if chunk.event_id != event.event_id || chunk.source_text_hash != event.source_text_hash
            {
                return Err(FlatStoreError::Corrupt(format!(
                    "event {} compacted vector metadata disagrees with authority",
                    event.event_id
                )));
            }
            vectors.write_payload(vector)?;
            metadata.write_payload(&encode_metadata_record(FlatChunkMetadata {
                event_id: event.event_id,
                seq: event.seq,
                source_text_hash: event.source_text_hash,
                chunk_index: chunk.chunk_index,
                start_char: chunk.start_char,
                end_char: chunk.end_char,
            }))?;
            ordinal = ordinal.checked_add(1).ok_or_else(|| {
                FlatStoreError::Corrupt("compacted vector ordinal overflow".to_owned())
            })?;
        }
    }
    let mut mutations = StagedArtifactWriter::new(&directory, generation, ArtifactRole::Mutations)?;
    for event in events {
        mutations.write_payload(&encode_mutation_record(EventMutation {
            event_id: event.event_id,
            kind: MutationKind::Replace,
            seq: event.seq,
            source_text_hash: event.source_text_hash,
            stable_identity_hash: event.stable_identity_hash,
            vector_generation: generation,
            first_vector_ordinal: first_ordinals[&event.event_id],
            chunk_count: event.chunk_count,
        }))?;
    }
    let vectors = vectors.finalize(vector_count, stride as u32, contract.dimensions)?;
    let metadata = metadata.finalize(vector_count, METADATA_RECORD_BYTES as u32, 0)?;
    let mutations = mutations.finalize(mutation_count, MUTATION_RECORD_BYTES as u32, 0)?;
    Ok(StagedSegment {
        descriptor: SegmentDescriptor {
            format_version: SEGMENT_FORMAT_VERSION,
            generation,
            kind: SegmentKind::Base,
            vector_count,
            mutation_count,
            source_identity_digest: source.source_identity_digest.clone(),
            source_reconciliation_id: source.source_reconciliation_id.clone(),
            vectors,
            metadata,
            mutations,
        },
        mutations: events.iter().map(event_mutation).collect(),
    })
}

pub(super) fn write_empty_base_segment(
    root: &Path,
    contract: &FlatModelContract,
    generation: u64,
) -> FlatResult<StagedSegment> {
    let directory = segments_directory(root);
    let stride = vector_stride(contract.dimensions)?;
    let vectors = StagedArtifactWriter::new(&directory, generation, ArtifactRole::Vectors)?
        .finalize(0, stride, contract.dimensions)?;
    let metadata = StagedArtifactWriter::new(&directory, generation, ArtifactRole::Metadata)?
        .finalize(0, METADATA_RECORD_BYTES as u32, 0)?;
    let mutations = StagedArtifactWriter::new(&directory, generation, ArtifactRole::Mutations)?
        .finalize(0, MUTATION_RECORD_BYTES as u32, 0)?;
    Ok(StagedSegment {
        descriptor: SegmentDescriptor {
            format_version: SEGMENT_FORMAT_VERSION,
            generation,
            kind: SegmentKind::Base,
            vector_count: 0,
            mutation_count: 0,
            source_identity_digest: UNSCOPED_SOURCE_IDENTITY.to_owned(),
            source_reconciliation_id: UNSCOPED_RECONCILIATION_ID.to_owned(),
            vectors,
            metadata,
            mutations,
        },
        mutations: Vec::new(),
    })
}

pub(super) fn remove_temporary_files(root: &Path) -> FlatResult<FlatRecoveryReport> {
    let mut report = FlatRecoveryReport::default();
    for directory in [manifests_directory(root), segments_directory(root)] {
        let entries = fs::read_dir(&directory)
            .map_err(|source| io_error("read flat recovery directory", &directory, source))?;
        for entry in entries {
            let entry =
                entry.map_err(|source| io_error("read flat recovery entry", &directory, source))?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !name.starts_with(TEMP_PREFIX) {
                continue;
            }
            remove_recoverable_file(
                &entry.path(),
                &mut report.removed_temporary_files,
                &mut report.retained_busy_files,
            )?;
        }
    }
    Ok(report)
}

pub(super) fn cleanup_obsolete_locked(
    root: &Path,
    selected: &SelectedManifest,
) -> FlatResult<FlatRecoveryReport> {
    let mut report = FlatRecoveryReport::default();
    let mut active_segments = selected
        .envelope
        .manifest
        .segments
        .iter()
        .flat_map(|segment| {
            [
                segment.vectors.file.as_str(),
                segment.metadata.file.as_str(),
                segment.mutations.file.as_str(),
            ]
        })
        .map(str::to_owned)
        .collect::<HashSet<_>>();

    let manifest_directory = manifests_directory(root);
    let entries = fs::read_dir(&manifest_directory)
        .map_err(|source| {
            io_error(
                "read flat manifest cleanup directory",
                &manifest_directory,
                source,
            )
        })?
        .map(|entry| {
            entry.map_err(|source| {
                io_error(
                    "read flat manifest cleanup entry",
                    &manifest_directory,
                    source,
                )
            })
        })
        .collect::<FlatResult<Vec<_>>>()?;
    let previous = entries
        .iter()
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            let (generation, digest) = parse_manifest_name(name)?;
            (entry.path() != selected.path && generation < selected.envelope.manifest.generation)
                .then_some((generation, digest, entry.path()))
        })
        .max_by_key(|(generation, _, _)| *generation);
    let previous_path = if let Some((generation, digest, path)) = previous {
        let envelope = read_manifest(&path)?;
        validate_manifest(&envelope, generation, &digest)?;
        active_segments.extend(envelope.manifest.segments.iter().flat_map(|segment| {
            [
                segment.vectors.file.clone(),
                segment.metadata.file.clone(),
                segment.mutations.file.clone(),
            ]
        }));
        Some(path)
    } else {
        None
    };
    for entry in entries {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !name.starts_with(MANIFEST_PREFIX)
            || entry.path() == selected.path
            || previous_path
                .as_ref()
                .is_some_and(|path| *path == entry.path())
        {
            continue;
        }
        if parse_manifest_name(&name).is_none() {
            continue;
        }
        remove_recoverable_file(
            &entry.path(),
            &mut report.removed_obsolete_manifests,
            &mut report.retained_busy_files,
        )?;
    }

    let segment_directory = segments_directory(root);
    for entry in fs::read_dir(&segment_directory).map_err(|source| {
        io_error(
            "read flat segment cleanup directory",
            &segment_directory,
            source,
        )
    })? {
        let entry = entry.map_err(|source| {
            io_error(
                "read flat segment cleanup entry",
                &segment_directory,
                source,
            )
        })?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !name.starts_with(SEGMENT_PREFIX) || active_segments.contains(name.as_str()) {
            continue;
        }
        remove_recoverable_file(
            &entry.path(),
            &mut report.removed_orphan_segments,
            &mut report.retained_busy_files,
        )?;
    }
    sync_directory(&manifest_directory)?;
    sync_directory(&segment_directory)?;
    Ok(report)
}

pub(super) fn cleanup_without_manifest(root: &Path) -> FlatResult<FlatRecoveryReport> {
    let mut report = FlatRecoveryReport::default();
    let directory = segments_directory(root);
    for entry in fs::read_dir(&directory)
        .map_err(|source| io_error("read orphan flat segment directory", &directory, source))?
    {
        let entry = entry
            .map_err(|source| io_error("read orphan flat segment entry", &directory, source))?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !name.starts_with(SEGMENT_PREFIX) {
            continue;
        }
        remove_recoverable_file(
            &entry.path(),
            &mut report.removed_orphan_segments,
            &mut report.retained_busy_files,
        )?;
    }
    sync_directory(&directory)?;
    Ok(report)
}

pub(super) fn remove_recoverable_file(
    path: &Path,
    removed: &mut usize,
    retained_busy: &mut usize,
) -> FlatResult<()> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| io_error("stat recoverable flat file", path, source))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(FlatStoreError::Corrupt(format!(
            "recoverable flat path {} is not a regular file",
            path.display()
        )));
    }
    match fs::remove_file(path) {
        Ok(()) => {
            *removed = removed.saturating_add(1);
            Ok(())
        }
        Err(source)
            if matches!(
                source.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::WouldBlock
            ) =>
        {
            *retained_busy = retained_busy.saturating_add(1);
            Ok(())
        }
        Err(source) => Err(io_error("remove recoverable flat file", path, source)),
    }
}

pub(super) fn merge_recovery_reports(target: &mut FlatRecoveryReport, other: FlatRecoveryReport) {
    target.model_contract_reset |= other.model_contract_reset;
    target.removed_temporary_files = target
        .removed_temporary_files
        .saturating_add(other.removed_temporary_files);
    target.removed_obsolete_manifests = target
        .removed_obsolete_manifests
        .saturating_add(other.removed_obsolete_manifests);
    target.removed_orphan_segments = target
        .removed_orphan_segments
        .saturating_add(other.removed_orphan_segments);
    target.retained_busy_files = target
        .retained_busy_files
        .saturating_add(other.retained_busy_files);
}
