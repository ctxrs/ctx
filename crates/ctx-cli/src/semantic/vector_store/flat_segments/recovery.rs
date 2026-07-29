use super::*;

impl FlatSegmentStore {
    pub(in crate::semantic) fn recover(&mut self) -> FlatResult<FlatRecoveryReport> {
        self.require_writable()?;
        let report = self.recover_internal()?;
        self.recovery = report.clone();
        Ok(report)
    }

    pub(in crate::semantic) fn compact(&self) -> FlatResult<FlatPublishOutcome> {
        self.require_writable()?;
        let _guard = self.lock_exclusive()?;
        let Some(current) = self.load_current_locked()? else {
            return Ok(FlatPublishOutcome {
                published: false,
                generation: 0,
                generation_hash: None,
                replaced_events: 0,
                deleted_events: 0,
            });
        };
        if current.envelope.manifest.segments.len() == 1
            && current.envelope.manifest.segments[0].kind == SegmentKind::Base
        {
            return Ok(noop_outcome(Some(&current)));
        }

        let pinned = self.load_pinned(&current)?;
        let generation = next_generation(Some(&current))?;
        let staged = write_compacted_segment(&self.root, &self.contract, generation, &pinned)?;
        let replaced_events = pinned.active_events().len();
        sync_directory(&segments_directory(&self.root))?;
        validate_staged_segment(&self.root, &self.contract, &staged.descriptor)?;

        let mut manifest = Manifest::new(self.contract.clone());
        manifest.generation = generation;
        manifest.created_unix_millis = unix_millis();
        manifest.segments.push(staged.descriptor);
        let selected = publish_manifest(&self.root, manifest)?;
        self.remember_validated(&selected)?;
        drop(pinned);
        self.clear_pinned()?;
        let _ = cleanup_obsolete_locked(&self.root, &selected);
        Ok(FlatPublishOutcome {
            published: true,
            generation,
            generation_hash: Some(selected.generation_hash),
            replaced_events,
            deleted_events: 0,
        })
    }

    pub(super) fn recover_internal(&self) -> FlatResult<FlatRecoveryReport> {
        let _guard = self.lock_exclusive()?;
        let mut report = remove_temporary_files(&self.root)?;
        let selected = select_manifest_any(&self.root)?;
        if let Some(selected) = selected {
            if selected.envelope.manifest.model == self.contract {
                let _ = self.load_pinned(&selected)?;
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
                self.remember_validated(&reset)?;
                self.clear_pinned()?;
                report.model_contract_reset = true;
                merge_recovery_reports(&mut report, cleanup_obsolete_locked(&self.root, &reset)?);
            }
        } else {
            merge_recovery_reports(&mut report, cleanup_without_manifest(&self.root)?);
            self.clear_validated()?;
            self.clear_pinned()?;
        }
        Ok(report)
    }
}

pub(super) fn write_compacted_segment(
    root: &Path,
    contract: &FlatModelContract,
    generation: u64,
    pinned: &PinnedFlatGeneration,
) -> FlatResult<StagedSegment> {
    let vector_count = u64::try_from(pinned.stats().active_chunks)
        .map_err(|_| FlatStoreError::Corrupt("active vector count is too large".to_owned()))?;
    let mutation_count = u64::try_from(pinned.active_events().len())
        .map_err(|_| FlatStoreError::Corrupt("active event count is too large".to_owned()))?;
    let directory = segments_directory(root);
    let stride = usize_from_u32(vector_stride(contract.dimensions)?, "vector stride")?;
    let mut vectors = StagedArtifactWriter::new(&directory, generation, ArtifactRole::Vectors)?;
    let mut metadata = StagedArtifactWriter::new(&directory, generation, ArtifactRole::Metadata)?;
    let mut scratch = vec![0_u8; stride];
    for segment in pinned.scan_segments() {
        for chunk in segment.chunks() {
            encode_vector(chunk.vector, &mut scratch)?;
            vectors.write_payload(&scratch)?;
            metadata.write_payload(&encode_metadata_record(FlatChunkMetadata {
                event_id: chunk.event_id,
                seq: chunk.seq,
                source_text_hash: chunk.source_text_hash,
                chunk_index: chunk.chunk_index,
                start_char: chunk.start_char,
                end_char: chunk.end_char,
            }))?;
        }
    }
    let mut mutations = StagedArtifactWriter::new(&directory, generation, ArtifactRole::Mutations)?;
    for event in pinned.active_events() {
        mutations.write_payload(&encode_mutation_record(EventMutation {
            event_id: event.event_id,
            kind: MutationKind::Replace,
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
            vectors,
            metadata,
            mutations,
        },
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
            vectors,
            metadata,
            mutations,
        },
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
    let active_segments = selected
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
        .collect::<HashSet<_>>();

    let manifest_directory = manifests_directory(root);
    for entry in fs::read_dir(&manifest_directory).map_err(|source| {
        io_error(
            "read flat manifest cleanup directory",
            &manifest_directory,
            source,
        )
    })? {
        let entry = entry.map_err(|source| {
            io_error(
                "read flat manifest cleanup entry",
                &manifest_directory,
                source,
            )
        })?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !name.starts_with(MANIFEST_PREFIX) || entry.path() == selected.path {
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
