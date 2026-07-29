use super::*;

pub(super) fn validate_header(
    header: &SegmentHeader,
    segment: &SegmentDescriptor,
    artifact: &ArtifactDescriptor,
    role: ArtifactRole,
    contract: &FlatModelContract,
) -> FlatResult<()> {
    if header.magic != role.magic()
        || header.format_version != SEGMENT_FORMAT_VERSION
        || header.header_bytes != HEADER_BYTES as u32
        || header.generation != segment.generation
    {
        return Err(FlatStoreError::Corrupt(format!(
            "{} has an incompatible {} header",
            artifact.file,
            role.name()
        )));
    }
    let expected_count = match role {
        ArtifactRole::Vectors | ArtifactRole::Metadata => segment.vector_count,
        ArtifactRole::Mutations => segment.mutation_count,
    };
    if header.record_count != expected_count {
        return Err(FlatStoreError::Corrupt(format!(
            "{} record count does not match its manifest",
            artifact.file
        )));
    }
    let expected_record_bytes = match role {
        ArtifactRole::Vectors => vector_stride(contract.dimensions)?,
        ArtifactRole::Metadata => METADATA_RECORD_BYTES as u32,
        ArtifactRole::Mutations => MUTATION_RECORD_BYTES as u32,
    };
    let expected_dimensions = match role {
        ArtifactRole::Vectors => contract.dimensions,
        ArtifactRole::Metadata | ArtifactRole::Mutations => 0,
    };
    if header.record_bytes != expected_record_bytes || header.dimensions != expected_dimensions {
        return Err(FlatStoreError::Corrupt(format!(
            "{} record layout does not match the model contract",
            artifact.file
        )));
    }
    let expected_payload = header
        .record_count
        .checked_mul(u64::from(header.record_bytes))
        .ok_or_else(|| FlatStoreError::Corrupt("segment payload length overflow".to_owned()))?;
    let expected_file_bytes = HEADER_BYTES_U64
        .checked_add(expected_payload)
        .ok_or_else(|| FlatStoreError::Corrupt("segment file length overflow".to_owned()))?;
    if header.payload_bytes != expected_payload || artifact.file_bytes != expected_file_bytes {
        return Err(FlatStoreError::Corrupt(format!(
            "{} payload length does not match its header",
            artifact.file
        )));
    }
    let expected_digest = decode_sha256(&artifact.payload_sha256).ok_or_else(|| {
        FlatStoreError::Corrupt(format!(
            "{} has an invalid manifest checksum",
            artifact.file
        ))
    })?;
    if header.payload_sha256 != expected_digest {
        return Err(FlatStoreError::Corrupt(format!(
            "{} header checksum does not match its manifest",
            artifact.file
        )));
    }
    Ok(())
}

pub(super) fn validate_vector_payload(
    mapping: &Mmap,
    header: &SegmentHeader,
    contract: &FlatModelContract,
) -> FlatResult<()> {
    let count = usize_from_u64(header.record_count, "vector count")?;
    let dimensions = usize_from_u32(contract.dimensions, "dimensions")?;
    let stride = usize_from_u32(header.record_bytes, "vector stride")?;
    let vector_bytes = dimensions
        .checked_mul(4)
        .ok_or_else(|| FlatStoreError::Corrupt("vector byte length overflow".to_owned()))?;
    for ordinal in 0..count {
        let start = HEADER_BYTES
            .checked_add(ordinal.checked_mul(stride).ok_or_else(|| {
                FlatStoreError::Corrupt("vector payload offset overflow".to_owned())
            })?)
            .ok_or_else(|| FlatStoreError::Corrupt("vector payload offset overflow".to_owned()))?;
        let row = mapping.get(start..start + stride).ok_or_else(|| {
            FlatStoreError::Corrupt("vector payload is shorter than declared".to_owned())
        })?;
        let mut norm_squared = 0.0_f64;
        for value in row[..vector_bytes].chunks_exact(4) {
            let value = f32::from_le_bytes([value[0], value[1], value[2], value[3]]);
            if !value.is_finite() {
                return Err(FlatStoreError::Corrupt(format!(
                    "vector {ordinal} contains a non-finite component"
                )));
            }
            norm_squared += f64::from(value) * f64::from(value);
        }
        if (norm_squared - 1.0).abs() > NORMALIZED_NORM_SQUARED_TOLERANCE {
            return Err(FlatStoreError::Corrupt(format!(
                "vector {ordinal} is not L2-normalized (norm squared {norm_squared})"
            )));
        }
        if row[vector_bytes..].iter().any(|byte| *byte != 0) {
            return Err(FlatStoreError::Corrupt(format!(
                "vector {ordinal} has non-zero alignment padding"
            )));
        }
    }
    Ok(())
}

pub(super) fn validate_mutation_payload(
    mapping: &Mmap,
    header: &SegmentHeader,
    kind: SegmentKind,
    generation: u64,
) -> FlatResult<Vec<EventMutation>> {
    let count = usize_from_u64(header.record_count, "mutation count")?;
    let mut mutations = Vec::with_capacity(count);
    let mut previous = None::<Uuid>;
    for ordinal in 0..count {
        let start = HEADER_BYTES + ordinal * MUTATION_RECORD_BYTES;
        let record = mapping
            .get(start..start + MUTATION_RECORD_BYTES)
            .ok_or_else(|| {
                FlatStoreError::Corrupt("mutation payload is shorter than declared".to_owned())
            })?;
        let mutation = decode_mutation_record(record)?;
        if previous.is_some_and(|value| value >= mutation.event_id) {
            return Err(FlatStoreError::Corrupt(format!(
                "segment generation {generation} mutations are not uniquely sorted"
            )));
        }
        if kind == SegmentKind::Base && mutation.kind != MutationKind::Replace {
            return Err(FlatStoreError::Corrupt(format!(
                "base segment generation {generation} contains a deletion"
            )));
        }
        previous = Some(mutation.event_id);
        mutations.push(mutation);
    }
    Ok(mutations)
}

pub(super) fn validate_metadata_payload(
    mapping: &Mmap,
    header: &SegmentHeader,
    mutations: &[EventMutation],
    generation: u64,
) -> FlatResult<()> {
    let mutation_kinds = mutations
        .iter()
        .map(|mutation| (mutation.event_id, mutation.kind))
        .collect::<HashMap<_, _>>();
    let count = usize_from_u64(header.record_count, "metadata count")?;
    let mut current_event = None::<Uuid>;
    let mut previous_chunk_index = None::<u32>;
    let mut completed_events = HashSet::<Uuid>::new();
    let mut event_evidence = HashMap::<Uuid, (u64, FlatSourceHash)>::new();
    let mut metadata_events = HashSet::<Uuid>::new();
    for ordinal in 0..count {
        let start = HEADER_BYTES + ordinal * METADATA_RECORD_BYTES;
        let record = mapping
            .get(start..start + METADATA_RECORD_BYTES)
            .ok_or_else(|| {
                FlatStoreError::Corrupt("metadata payload is shorter than declared".to_owned())
            })?;
        let metadata = decode_metadata_record_checked(record)?;
        if metadata.start_char > metadata.end_char {
            return Err(FlatStoreError::Corrupt(format!(
                "segment generation {generation} has an inverted character range"
            )));
        }
        if current_event == Some(metadata.event_id) {
            if previous_chunk_index.is_some_and(|index| index >= metadata.chunk_index) {
                return Err(FlatStoreError::Corrupt(format!(
                    "segment generation {generation} repeats or reorders an event chunk"
                )));
            }
        } else {
            if let Some(event_id) = current_event {
                completed_events.insert(event_id);
            }
            if completed_events.contains(&metadata.event_id) {
                return Err(FlatStoreError::Corrupt(format!(
                    "segment generation {generation} splits one event across metadata ranges"
                )));
            }
            current_event = Some(metadata.event_id);
        }
        if mutation_kinds.get(&metadata.event_id) != Some(&MutationKind::Replace) {
            return Err(FlatStoreError::Corrupt(format!(
                "segment generation {generation} metadata has no replacement mutation"
            )));
        }
        if event_evidence
            .insert(metadata.event_id, (metadata.seq, metadata.source_text_hash))
            .is_some_and(|evidence| evidence != (metadata.seq, metadata.source_text_hash))
        {
            return Err(FlatStoreError::Corrupt(format!(
                "segment generation {generation} has inconsistent event sequence or hash"
            )));
        }
        metadata_events.insert(metadata.event_id);
        previous_chunk_index = Some(metadata.chunk_index);
    }
    for mutation in mutations {
        if mutation.kind == MutationKind::Replace && !metadata_events.contains(&mutation.event_id) {
            return Err(FlatStoreError::Corrupt(format!(
                "segment generation {generation} replacement has no chunks"
            )));
        }
        if mutation.kind == MutationKind::Delete && metadata_events.contains(&mutation.event_id) {
            return Err(FlatStoreError::Corrupt(format!(
                "segment generation {generation} deletion also has chunks"
            )));
        }
    }
    Ok(())
}

pub(super) fn validate_manifest(
    envelope: &ManifestEnvelope,
    filename_generation: u64,
    filename_digest: &str,
) -> FlatResult<()> {
    if envelope.format != STORE_FORMAT
        || envelope.envelope_version != MANIFEST_ENVELOPE_VERSION
        || envelope.manifest.schema_version != MANIFEST_SCHEMA_VERSION
    {
        return Err(FlatStoreError::Incompatible(
            "manifest format or schema version is unsupported".to_owned(),
        ));
    }
    validate_model_contract(&envelope.manifest.model).map_err(|error| {
        FlatStoreError::Corrupt(format!("manifest has an invalid model contract: {error}"))
    })?;
    let manifest_bytes = serde_json::to_vec(&envelope.manifest)?;
    let actual_digest = encode_hex(Sha256::digest(&manifest_bytes).as_slice());
    if envelope.manifest_sha256 != actual_digest || filename_digest != actual_digest {
        return Err(FlatStoreError::Corrupt(
            "manifest checksum does not match its payload and filename".to_owned(),
        ));
    }
    if envelope.manifest.generation == 0
        || envelope.manifest.generation != filename_generation
        || envelope.manifest.segments.is_empty()
    {
        return Err(FlatStoreError::Corrupt(
            "manifest generation or segment set is invalid".to_owned(),
        ));
    }
    let mut prior_generation = 0_u64;
    let mut saw_base = false;
    for (index, segment) in envelope.manifest.segments.iter().enumerate() {
        if segment.format_version != SEGMENT_FORMAT_VERSION
            || segment.generation <= prior_generation
            || segment.generation > envelope.manifest.generation
        {
            return Err(FlatStoreError::Corrupt(
                "manifest segment generations are invalid".to_owned(),
            ));
        }
        match segment.kind {
            SegmentKind::Base if index != 0 || saw_base => {
                return Err(FlatStoreError::Corrupt(
                    "base segment must be the first and only base".to_owned(),
                ));
            }
            SegmentKind::Base => saw_base = true,
            SegmentKind::Delta => {}
        }
        validate_artifact_descriptor(segment, &segment.vectors, ArtifactRole::Vectors)?;
        validate_artifact_descriptor(segment, &segment.metadata, ArtifactRole::Metadata)?;
        validate_artifact_descriptor(segment, &segment.mutations, ArtifactRole::Mutations)?;
        prior_generation = segment.generation;
    }
    if prior_generation != envelope.manifest.generation {
        return Err(FlatStoreError::Corrupt(
            "manifest has no segment for its publication generation".to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn validate_artifact_descriptor(
    segment: &SegmentDescriptor,
    artifact: &ArtifactDescriptor,
    role: ArtifactRole,
) -> FlatResult<()> {
    validate_artifact_name(&artifact.file, segment.generation, role)?;
    if decode_sha256(&artifact.payload_sha256).is_none() {
        return Err(FlatStoreError::Corrupt(format!(
            "{} has a malformed checksum",
            artifact.file
        )));
    }
    let record_count = match role {
        ArtifactRole::Vectors | ArtifactRole::Metadata => segment.vector_count,
        ArtifactRole::Mutations => segment.mutation_count,
    };
    let minimum_record_bytes = match role {
        ArtifactRole::Vectors => 4_u64,
        ArtifactRole::Metadata => METADATA_RECORD_BYTES as u64,
        ArtifactRole::Mutations => MUTATION_RECORD_BYTES as u64,
    };
    let minimum = HEADER_BYTES_U64
        .checked_add(
            record_count
                .checked_mul(minimum_record_bytes)
                .ok_or_else(|| {
                    FlatStoreError::Corrupt("artifact byte length overflow".to_owned())
                })?,
        )
        .ok_or_else(|| FlatStoreError::Corrupt("artifact byte length overflow".to_owned()))?;
    if artifact.file_bytes < minimum {
        return Err(FlatStoreError::Corrupt(format!(
            "{} is too short for its record count",
            artifact.file
        )));
    }
    Ok(())
}

pub(super) fn validate_publication_input(
    contract: &FlatModelContract,
    replacements: &[FlatEventReplacement],
    tombstones: &[Uuid],
) -> FlatResult<()> {
    let dimensions = usize_from_u32(contract.dimensions, "dimensions")?;
    let mut event_ids = HashSet::with_capacity(replacements.len() + tombstones.len());
    for replacement in replacements {
        if replacement.event_id.is_nil() {
            return Err(FlatStoreError::InvalidInput(
                "replacement event id must not be nil".to_owned(),
            ));
        }
        if !event_ids.insert(replacement.event_id) {
            return Err(FlatStoreError::InvalidInput(format!(
                "event {} appears more than once in one publication",
                replacement.event_id
            )));
        }
        if replacement.chunks.is_empty() {
            return Err(FlatStoreError::InvalidInput(format!(
                "replacement event {} has no chunks; use a tombstone",
                replacement.event_id
            )));
        }
        let mut chunk_indexes = HashSet::with_capacity(replacement.chunks.len());
        for chunk in &replacement.chunks {
            if !chunk_indexes.insert(chunk.chunk_index) {
                return Err(FlatStoreError::InvalidInput(format!(
                    "event {} repeats chunk index {}",
                    replacement.event_id, chunk.chunk_index
                )));
            }
            if chunk.start_char > chunk.end_char {
                return Err(FlatStoreError::InvalidInput(format!(
                    "event {} chunk {} has an inverted character range",
                    replacement.event_id, chunk.chunk_index
                )));
            }
            validate_vector(&chunk.vector, dimensions)?;
        }
    }
    for event_id in tombstones {
        if event_id.is_nil() {
            return Err(FlatStoreError::InvalidInput(
                "tombstone event id must not be nil".to_owned(),
            ));
        }
        if !event_ids.insert(*event_id) {
            return Err(FlatStoreError::InvalidInput(format!(
                "event {event_id} is both replaced and tombstoned"
            )));
        }
    }
    Ok(())
}

pub(super) fn validate_vector(vector: &[f32], dimensions: usize) -> FlatResult<()> {
    if vector.len() != dimensions {
        return Err(FlatStoreError::InvalidInput(format!(
            "vector has {} dimensions, expected {dimensions}",
            vector.len()
        )));
    }
    let mut norm_squared = 0.0_f64;
    for value in vector {
        if !value.is_finite() {
            return Err(FlatStoreError::InvalidInput(
                "vector contains a non-finite component".to_owned(),
            ));
        }
        norm_squared += f64::from(*value) * f64::from(*value);
    }
    if (norm_squared - 1.0).abs() > NORMALIZED_NORM_SQUARED_TOLERANCE {
        return Err(FlatStoreError::InvalidInput(format!(
            "vector is not L2-normalized (norm squared {norm_squared})"
        )));
    }
    Ok(())
}

pub(super) fn validate_model_contract(contract: &FlatModelContract) -> FlatResult<()> {
    if contract.contract_version == 0
        || contract.dimensions == 0
        || contract.dimensions > MAX_DIMENSIONS
        || !contract.normalization.eq_ignore_ascii_case("l2")
    {
        return Err(FlatStoreError::InvalidInput(
            "model contract version/dimensions/normalization are invalid".to_owned(),
        ));
    }
    for (name, value) in [
        ("model id", &contract.model_id),
        ("model revision", &contract.model_revision),
        ("tokenizer", &contract.tokenizer),
        ("pooling", &contract.pooling),
        ("normalization", &contract.normalization),
    ] {
        if value.is_empty()
            || value.len() > MAX_CONTRACT_FIELD_BYTES
            || value.chars().any(char::is_control)
        {
            return Err(FlatStoreError::InvalidInput(format!(
                "{name} is empty, oversized, or contains control characters"
            )));
        }
    }
    let _ = vector_stride(contract.dimensions)?;
    Ok(())
}

pub(super) fn vector_stride(dimensions: u32) -> FlatResult<u32> {
    let bytes = dimensions.checked_mul(4).ok_or_else(|| {
        FlatStoreError::InvalidInput("model vector byte length overflow".to_owned())
    })?;
    let alignment = VECTOR_ALIGNMENT as u32;
    bytes
        .checked_add(alignment - 1)
        .map(|value| value / alignment * alignment)
        .ok_or_else(|| FlatStoreError::InvalidInput("vector stride overflow".to_owned()))
}

pub(super) fn validate_artifact_name(
    name: &str,
    generation: u64,
    role: ArtifactRole,
) -> FlatResult<()> {
    if Path::new(name).file_name().and_then(|value| value.to_str()) != Some(name)
        || name.contains('/')
        || name.contains('\\')
    {
        return Err(FlatStoreError::Corrupt(
            "segment artifact name is not a safe leaf name".to_owned(),
        ));
    }
    let prefix = format!("{SEGMENT_PREFIX}{generation:020}-{}-", role.name());
    let Some(digest) = name
        .strip_prefix(&prefix)
        .and_then(|value| value.strip_suffix(".bin"))
    else {
        return Err(FlatStoreError::Corrupt(format!(
            "segment artifact {name:?} does not match its generation/role"
        )));
    };
    if decode_sha256(digest).is_none() {
        return Err(FlatStoreError::Corrupt(format!(
            "segment artifact {name:?} has an invalid checksum suffix"
        )));
    }
    Ok(())
}

pub(super) fn ensure_little_endian() -> FlatResult<()> {
    if cfg!(target_endian = "little") {
        Ok(())
    } else {
        Err(FlatStoreError::Unsupported(
            "memory-mapped f32 slices require a little-endian target".to_owned(),
        ))
    }
}

pub(super) fn usize_from_u64(value: u64, name: &'static str) -> FlatResult<usize> {
    usize::try_from(value)
        .map_err(|_| FlatStoreError::Corrupt(format!("{name} does not fit this platform")))
}

pub(super) fn usize_from_u32(value: u32, name: &'static str) -> FlatResult<usize> {
    usize::try_from(value)
        .map_err(|_| FlatStoreError::Corrupt(format!("{name} does not fit this platform")))
}
