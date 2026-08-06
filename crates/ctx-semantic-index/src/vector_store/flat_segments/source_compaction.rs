use super::*;

pub(super) fn write_final_source_catalog(
    directory: &Path,
    contract: &FlatModelContract,
    generation: u64,
    source: &FlatSourceScope,
    pages: &[SourceStagePage],
    receipt_input: Option<&FlatSourceReceiptInput>,
) -> FlatResult<(StagedSegment, Option<FlatSourceReceipt>, u64, u64)> {
    let catalog_source = if receipt_input.is_some() {
        source.clone()
    } else {
        unscoped_source()
    };
    let stride = vector_stride(contract.dimensions)?;
    let vectors = StagedArtifactWriter::new(directory, generation, ArtifactRole::Vectors)?
        .finalize(0, stride, contract.dimensions)?;
    let metadata = StagedArtifactWriter::new(directory, generation, ArtifactRole::Metadata)?
        .finalize(0, METADATA_RECORD_BYTES as u32, 0)?;
    let mut mutations = StagedArtifactWriter::new(directory, generation, ArtifactRole::Mutations)?;
    let mut digest = Sha256::new();
    digest.update(FLAT_SOURCE_RECEIPT_DOMAIN);
    let mut previous = None::<Uuid>;
    let mut active_events = 0_u64;
    let mut active_chunks = 0_u64;
    if receipt_input.is_some() {
        for page in pages {
            let Some(descriptor) = page.descriptor.as_ref() else {
                continue;
            };
            let page_mutations =
                load_catalog_mutations_in_directory(directory, contract, descriptor)?;
            for mutation in page_mutations
                .into_iter()
                .filter(|mutation| mutation.kind == MutationKind::Replace)
            {
                if previous.is_some_and(|prior| prior >= mutation.event_id) {
                    return Err(FlatStoreError::Corrupt(
                        "source staging pages are not globally identity ordered".to_owned(),
                    ));
                }
                mutations.write_payload(&encode_mutation_record(mutation))?;
                digest.update(mutation.event_id.as_bytes());
                digest.update([0]);
                digest.update(mutation.seq.to_be_bytes());
                digest.update(mutation.source_text_hash.as_bytes());
                digest.update(mutation.stable_identity_hash);
                digest.update([0]);
                active_events = active_events.checked_add(1).ok_or_else(|| {
                    FlatStoreError::Corrupt("source final event count overflow".to_owned())
                })?;
                active_chunks = active_chunks
                    .checked_add(u64::from(mutation.chunk_count))
                    .ok_or_else(|| {
                        FlatStoreError::Corrupt("source final chunk count overflow".to_owned())
                    })?;
                previous = Some(mutation.event_id);
            }
        }
    }
    let mutation_artifact = mutations.finalize(active_events, MUTATION_RECORD_BYTES as u32, 0)?;
    let descriptor = SegmentDescriptor {
        format_version: SEGMENT_FORMAT_VERSION,
        generation,
        kind: SegmentKind::Base,
        vector_count: 0,
        mutation_count: active_events,
        source_identity_digest: catalog_source.source_identity_digest.clone(),
        source_reconciliation_id: catalog_source.source_reconciliation_id.clone(),
        vectors,
        metadata,
        mutations: mutation_artifact,
    };
    validate_staged_segment_in_directory(directory, contract, &descriptor)?;
    let receipt = receipt_input
        .map(|input| build_streamed_receipt(input, source, active_events, digest))
        .transpose()?;
    Ok((
        StagedSegment {
            descriptor,
            mutations: Vec::new(),
        },
        receipt,
        active_events,
        active_chunks,
    ))
}

fn build_streamed_receipt(
    input: &FlatSourceReceiptInput,
    source: &FlatSourceScope,
    owned_event_count: u64,
    digest: Sha256,
) -> FlatResult<FlatSourceReceipt> {
    for (value, field) in [
        (&input.source_identity_digest, "source identity digest"),
        (&input.core_record_accumulator, "Core record accumulator"),
        (&input.contract_fingerprint, "source contract fingerprint"),
        (
            &input.semantic_policy_fingerprint,
            "semantic policy fingerprint",
        ),
    ] {
        if decode_sha256(value).is_none() {
            return Err(FlatStoreError::InvalidInput(format!(
                "{field} must be lowercase SHA-256"
            )));
        }
    }
    if input.source_identity_digest != source.source_identity_digest
        || input.source_reconciliation_id != source.source_reconciliation_id
        || input.source_reconciliation_id.is_empty()
        || owned_event_count > input.semantic_eligible_documents
    {
        return Err(FlatStoreError::InvalidInput(
            "source receipt does not match its staged Core aggregate".to_owned(),
        ));
    }
    Ok(FlatSourceReceipt {
        source_identity_digest: input.source_identity_digest.clone(),
        source_reconciliation_id: input.source_reconciliation_id.clone(),
        indexed_documents: input.indexed_documents,
        semantic_eligible_documents: input.semantic_eligible_documents,
        core_record_accumulator: input.core_record_accumulator.clone(),
        contract_fingerprint: input.contract_fingerprint.clone(),
        semantic_policy_fingerprint: input.semantic_policy_fingerprint.clone(),
        owned_event_count,
        owned_event_ids_hash: encode_hex(&digest.finalize()),
    })
}

pub(super) struct StagedSourceCompaction<'a> {
    pub(super) root: &'a Path,
    pub(super) staging: &'a Path,
    pub(super) contract: &'a FlatModelContract,
    pub(super) generation: u64,
    pub(super) source: &'a FlatSourceScope,
    pub(super) current: Option<&'a Manifest>,
    pub(super) pages: &'a [SourceStagePage],
    pub(super) active_chunks: u64,
}

pub(super) fn compact_staged_source_if_needed(
    input: StagedSourceCompaction<'_>,
    catalog: StagedSegment,
) -> FlatResult<(StagedSegment, bool)> {
    let StagedSourceCompaction {
        root,
        staging,
        contract,
        generation,
        source,
        current,
        pages,
        active_chunks,
    } = input;
    let mut vectors = current
        .into_iter()
        .flat_map(|manifest| &manifest.segments)
        .filter(|descriptor| {
            descriptor.source_identity_digest == source.source_identity_digest
                && descriptor.vector_count != 0
        })
        .cloned()
        .collect::<Vec<_>>();
    vectors.extend(
        pages
            .iter()
            .filter_map(|page| page.descriptor.as_ref())
            .filter(|descriptor| descriptor.vector_count != 0)
            .cloned(),
    );
    let stored_chunks = vectors.iter().try_fold(0_u64, |total, descriptor| {
        total
            .checked_add(descriptor.vector_count)
            .ok_or_else(|| FlatStoreError::Corrupt("stored source chunk overflow".to_owned()))
    })?;
    let compact = vectors.len().saturating_add(1) >= COMPACT_SEGMENT_THRESHOLD
        || (active_chunks > 0 && stored_chunks > active_chunks.saturating_mul(2));
    if !compact {
        return Ok((catalog, false));
    }

    let segment_directory = segments_directory(root);
    for page in pages {
        if let Some(descriptor) = page
            .descriptor
            .as_ref()
            .filter(|descriptor| descriptor.vector_count != 0)
        {
            link_staged_descriptor(staging, &segment_directory, descriptor)?;
        }
    }
    link_staged_descriptor(staging, &segment_directory, &catalog.descriptor)?;
    sync_directory(&segment_directory)?;

    let mutation_map = map_artifact(
        root,
        &catalog.descriptor,
        &catalog.descriptor.mutations,
        ArtifactRole::Mutations,
        contract,
    )?;
    let mutation_header = decode_header(&mutation_map)?;
    let event_count = usize_from_u64(mutation_header.record_count, "source compaction events")?;
    let descriptors = vectors
        .into_iter()
        .map(|descriptor| (descriptor.generation, descriptor))
        .collect::<HashMap<_, _>>();
    let stride = usize_from_u32(vector_stride(contract.dimensions)?, "vector stride")?;
    let mut loaded = HashMap::<u64, LoadedSegment>::new();
    let mut vector_writer = StagedArtifactWriter::new(staging, generation, ArtifactRole::Vectors)?;
    let mut metadata_writer =
        StagedArtifactWriter::new(staging, generation, ArtifactRole::Metadata)?;
    let mut mutation_writer =
        StagedArtifactWriter::new(staging, generation, ArtifactRole::Mutations)?;
    let mut output_ordinal = 0_u64;
    for ordinal in 0..event_count {
        let mutation = staged_mutation_at(&mutation_map, ordinal)?;
        let descriptor = descriptors
            .get(&mutation.vector_generation)
            .ok_or_else(|| {
                FlatStoreError::Corrupt(format!(
                    "source compaction event {} references an absent vector segment",
                    mutation.event_id
                ))
            })?;
        if let std::collections::hash_map::Entry::Vacant(entry) =
            loaded.entry(mutation.vector_generation)
        {
            entry.insert(load_and_validate_segment(root, contract, descriptor)?);
        }
        let segment = loaded.get(&mutation.vector_generation).ok_or_else(|| {
            FlatStoreError::Corrupt("source compaction lost a loaded segment".to_owned())
        })?;
        let first_ordinal = output_ordinal;
        for offset in 0..u64::from(mutation.chunk_count) {
            let source_ordinal = mutation
                .first_vector_ordinal
                .checked_add(offset)
                .ok_or_else(|| {
                    FlatStoreError::Corrupt("source vector range overflow".to_owned())
                })?;
            let source_ordinal = usize_from_u64(source_ordinal, "source vector ordinal")?;
            let start = HEADER_BYTES
                .checked_add(
                    source_ordinal
                        .checked_mul(segment.stride_bytes)
                        .ok_or_else(|| {
                            FlatStoreError::Corrupt("source vector offset overflow".to_owned())
                        })?,
                )
                .ok_or_else(|| {
                    FlatStoreError::Corrupt("source vector offset overflow".to_owned())
                })?;
            let vector = segment.vectors.get(start..start + stride).ok_or_else(|| {
                FlatStoreError::Corrupt("source vector range is truncated".to_owned())
            })?;
            let metadata = metadata_at(&segment.metadata, source_ordinal);
            if metadata.event_id != mutation.event_id
                || metadata.source_text_hash != mutation.source_text_hash
            {
                return Err(FlatStoreError::Corrupt(format!(
                    "source compaction metadata disagrees for {}",
                    mutation.event_id
                )));
            }
            vector_writer.write_payload(vector)?;
            metadata_writer.write_payload(&encode_metadata_record(FlatChunkMetadata {
                event_id: mutation.event_id,
                seq: mutation.seq,
                source_text_hash: mutation.source_text_hash,
                chunk_index: metadata.chunk_index,
                start_char: metadata.start_char,
                end_char: metadata.end_char,
            }))?;
            output_ordinal = output_ordinal.checked_add(1).ok_or_else(|| {
                FlatStoreError::Corrupt("source compacted vector count overflow".to_owned())
            })?;
        }
        mutation_writer.write_payload(&encode_mutation_record(EventMutation {
            vector_generation: generation,
            first_vector_ordinal: first_ordinal,
            ..mutation
        }))?;
    }
    if output_ordinal != active_chunks {
        return Err(FlatStoreError::Corrupt(
            "source compacted chunk count disagrees with its catalog".to_owned(),
        ));
    }
    let vectors = vector_writer.finalize(output_ordinal, stride as u32, contract.dimensions)?;
    let metadata = metadata_writer.finalize(output_ordinal, METADATA_RECORD_BYTES as u32, 0)?;
    let mutations = mutation_writer.finalize(
        u64::try_from(event_count)
            .map_err(|_| FlatStoreError::Corrupt("source event count overflow".to_owned()))?,
        MUTATION_RECORD_BYTES as u32,
        0,
    )?;
    let descriptor = SegmentDescriptor {
        format_version: SEGMENT_FORMAT_VERSION,
        generation,
        kind: SegmentKind::Base,
        vector_count: output_ordinal,
        mutation_count: u64::try_from(event_count)
            .map_err(|_| FlatStoreError::Corrupt("source event count overflow".to_owned()))?,
        source_identity_digest: source.source_identity_digest.clone(),
        source_reconciliation_id: source.source_reconciliation_id.clone(),
        vectors,
        metadata,
        mutations,
    };
    validate_staged_segment_in_directory(staging, contract, &descriptor)?;
    Ok((
        StagedSegment {
            descriptor,
            mutations: Vec::new(),
        },
        true,
    ))
}

fn staged_mutation_at(mapping: &[u8], ordinal: usize) -> FlatResult<EventMutation> {
    let start =
        HEADER_BYTES
            .checked_add(ordinal.checked_mul(MUTATION_RECORD_BYTES).ok_or_else(|| {
                FlatStoreError::Corrupt("source mutation offset overflow".to_owned())
            })?)
            .ok_or_else(|| FlatStoreError::Corrupt("source mutation offset overflow".to_owned()))?;
    let record = mapping
        .get(start..start + MUTATION_RECORD_BYTES)
        .ok_or_else(|| FlatStoreError::Corrupt("source mutation record is truncated".to_owned()))?;
    decode_mutation_record(record)
}

pub(super) fn link_staged_descriptor(
    staging: &Path,
    active: &Path,
    descriptor: &SegmentDescriptor,
) -> FlatResult<()> {
    for artifact in [
        &descriptor.vectors,
        &descriptor.metadata,
        &descriptor.mutations,
    ] {
        let source = staging.join(&artifact.file);
        let destination = active.join(&artifact.file);
        match fs::hard_link(&source, &destination) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let metadata = symlink_metadata_file(&destination)?;
                if metadata.len() != artifact.file_bytes {
                    return Err(FlatStoreError::Corrupt(format!(
                        "retained staged artifact {} has the wrong size",
                        artifact.file
                    )));
                }
            }
            Err(source) => {
                return Err(io_error("link staged Flat artifact", &destination, source));
            }
        }
    }
    Ok(())
}
