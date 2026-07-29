use super::*;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(super) fn load_and_validate_segment(
    root: &Path,
    contract: &FlatModelContract,
    descriptor: &SegmentDescriptor,
) -> FlatResult<LoadedSegment> {
    let vectors = map_artifact(
        root,
        descriptor,
        &descriptor.vectors,
        ArtifactRole::Vectors,
        contract,
    )?;
    let metadata = map_artifact(
        root,
        descriptor,
        &descriptor.metadata,
        ArtifactRole::Metadata,
        contract,
    )?;
    let mutation_map = map_artifact(
        root,
        descriptor,
        &descriptor.mutations,
        ArtifactRole::Mutations,
        contract,
    )?;
    let vector_header = decode_header(&vectors)?;
    let metadata_header = decode_header(&metadata)?;
    let mutation_header = decode_header(&mutation_map)?;
    let stride_bytes = usize_from_u32(vector_header.record_bytes, "vector stride")?;

    validate_vector_payload(&vectors, &vector_header, contract)?;
    let mutations = validate_mutation_payload(
        &mutation_map,
        &mutation_header,
        descriptor.kind,
        descriptor.generation,
    )?;
    validate_metadata_payload(
        &metadata,
        &metadata_header,
        &mutations,
        descriptor.generation,
    )?;
    Ok(LoadedSegment {
        descriptor: descriptor.clone(),
        vectors,
        metadata,
        mutations,
        stride_bytes,
    })
}

pub(super) fn validate_staged_segment(
    root: &Path,
    contract: &FlatModelContract,
    descriptor: &SegmentDescriptor,
) -> FlatResult<()> {
    let _ = load_and_validate_segment(root, contract, descriptor)?;
    Ok(())
}

pub(super) fn map_artifact(
    root: &Path,
    segment: &SegmentDescriptor,
    artifact: &ArtifactDescriptor,
    role: ArtifactRole,
    contract: &FlatModelContract,
) -> FlatResult<Mmap> {
    validate_artifact_name(&artifact.file, segment.generation, role)?;
    let path = segments_directory(root).join(&artifact.file);
    let metadata = symlink_metadata_file(&path)?;
    if metadata.len() != artifact.file_bytes {
        return Err(FlatStoreError::Corrupt(format!(
            "{} has {} bytes, manifest requires {}",
            artifact.file,
            metadata.len(),
            artifact.file_bytes
        )));
    }
    if metadata.len() < HEADER_BYTES_U64 {
        return Err(FlatStoreError::Corrupt(format!(
            "{} is shorter than its header",
            artifact.file
        )));
    }
    let file = File::open(&path).map_err(|source| io_error("open flat segment", &path, source))?;
    // The map is read-only and the file length/type were checked immediately
    // before mapping. All offsets are checked again before typed access.
    let mapping = unsafe {
        MmapOptions::new()
            .map(&file)
            .map_err(|source| io_error("mmap flat segment", &path, source))?
    };
    let header = decode_header(&mapping)?;
    validate_header(&header, segment, artifact, role, contract)?;
    let payload = &mapping[HEADER_BYTES..];
    let actual = Sha256::digest(payload);
    if actual.as_slice() != header.payload_sha256
        || encode_hex(actual.as_slice()) != artifact.payload_sha256
    {
        return Err(FlatStoreError::Corrupt(format!(
            "{} payload checksum mismatch",
            artifact.file
        )));
    }
    Ok(mapping)
}

pub(super) fn metadata_at(mapping: &Mmap, ordinal: usize) -> FlatChunkMetadata {
    let start = HEADER_BYTES + ordinal * METADATA_RECORD_BYTES;
    decode_metadata_record(&mapping[start..start + METADATA_RECORD_BYTES])
}

pub(super) fn publish_manifest(root: &Path, manifest: Manifest) -> FlatResult<SelectedManifest> {
    let manifest_bytes = serde_json::to_vec(&manifest)?;
    let digest = encode_hex(Sha256::digest(&manifest_bytes).as_slice());
    let envelope = ManifestEnvelope {
        format: STORE_FORMAT.to_owned(),
        envelope_version: MANIFEST_ENVELOPE_VERSION,
        manifest,
        manifest_sha256: digest.clone(),
    };
    let bytes = serde_json::to_vec(&envelope)?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(FlatStoreError::InvalidInput(
            "manifest exceeds the safe size limit; compact first".to_owned(),
        ));
    }
    let directory = manifests_directory(root);
    let final_path = directory.join(manifest_name(envelope.manifest.generation, &digest));
    let temporary = unique_temporary_path(&directory, "manifest");
    let mut file = create_new_file(&temporary)?;
    file.write_all(&bytes)
        .map_err(|source| io_error("write flat manifest", &temporary, source))?;
    file.sync_all()
        .map_err(|source| io_error("sync flat manifest", &temporary, source))?;
    drop(file);
    commit_unique_file(&temporary, &final_path)?;
    sync_directory(&directory)?;
    Ok(SelectedManifest {
        envelope,
        generation_hash: digest,
        path: final_path,
    })
}

pub(super) fn write_replacement_segment(
    root: &Path,
    contract: &FlatModelContract,
    generation: u64,
    replacements: &[FlatEventReplacement],
    tombstones: &[Uuid],
) -> FlatResult<StagedSegment> {
    let mut ordered_replacements = replacements.iter().collect::<Vec<_>>();
    ordered_replacements.sort_by_key(|replacement| replacement.event_id);
    let mut ordered_tombstones = tombstones.to_vec();
    ordered_tombstones.sort_unstable();

    let vector_count = ordered_replacements
        .iter()
        .try_fold(0_u64, |total, replacement| {
            let chunks = u64::try_from(replacement.chunks.len()).map_err(|_| {
                FlatStoreError::InvalidInput("replacement chunk count is too large".to_owned())
            })?;
            total.checked_add(chunks).ok_or_else(|| {
                FlatStoreError::InvalidInput("publication vector count overflow".to_owned())
            })
        })?;
    let mutation_count = u64::try_from(ordered_replacements.len())
        .ok()
        .zip(u64::try_from(ordered_tombstones.len()).ok())
        .and_then(|(replacements, tombstones)| replacements.checked_add(tombstones))
        .ok_or_else(|| FlatStoreError::InvalidInput("mutation count overflow".to_owned()))?;

    let directory = segments_directory(root);
    let stride = usize_from_u32(vector_stride(contract.dimensions)?, "vector stride")?;
    let mut vectors = StagedArtifactWriter::new(&directory, generation, ArtifactRole::Vectors)?;
    let mut metadata = StagedArtifactWriter::new(&directory, generation, ArtifactRole::Metadata)?;
    let mut vector_scratch = vec![0_u8; stride];
    for replacement in ordered_replacements {
        let mut chunks = replacement.chunks.iter().collect::<Vec<_>>();
        chunks.sort_by_key(|chunk| chunk.chunk_index);
        for chunk in chunks {
            encode_vector(&chunk.vector, &mut vector_scratch)?;
            vectors.write_payload(&vector_scratch)?;
            metadata.write_payload(&encode_metadata_record(FlatChunkMetadata {
                event_id: replacement.event_id,
                seq: replacement.seq,
                source_text_hash: replacement.source_text_hash,
                chunk_index: chunk.chunk_index,
                start_char: chunk.start_char,
                end_char: chunk.end_char,
            }))?;
        }
    }

    let mut mutations = StagedArtifactWriter::new(&directory, generation, ArtifactRole::Mutations)?;
    let mut ordered_mutations = replacements
        .iter()
        .map(|replacement| EventMutation {
            event_id: replacement.event_id,
            kind: MutationKind::Replace,
        })
        .chain(
            ordered_tombstones
                .into_iter()
                .map(|event_id| EventMutation {
                    event_id,
                    kind: MutationKind::Delete,
                }),
        )
        .collect::<Vec<_>>();
    ordered_mutations.sort_by_key(|mutation| mutation.event_id);
    for mutation in ordered_mutations {
        mutations.write_payload(&encode_mutation_record(mutation))?;
    }

    let vectors = vectors.finalize(vector_count, stride as u32, contract.dimensions)?;
    let metadata = metadata.finalize(vector_count, METADATA_RECORD_BYTES as u32, 0)?;
    let mutations = mutations.finalize(mutation_count, MUTATION_RECORD_BYTES as u32, 0)?;
    Ok(StagedSegment {
        descriptor: SegmentDescriptor {
            format_version: SEGMENT_FORMAT_VERSION,
            generation,
            kind: SegmentKind::Delta,
            vector_count,
            mutation_count,
            vectors,
            metadata,
            mutations,
        },
    })
}

pub(super) struct StagedArtifactWriter {
    directory: PathBuf,
    temporary: PathBuf,
    generation: u64,
    role: ArtifactRole,
    writer: BufWriter<File>,
    hasher: Sha256,
    payload_bytes: u64,
}

impl StagedArtifactWriter {
    pub(super) fn new(directory: &Path, generation: u64, role: ArtifactRole) -> FlatResult<Self> {
        let temporary = unique_temporary_path(directory, role.name());
        let file = create_new_file(&temporary)?;
        let mut writer = BufWriter::new(file);
        writer
            .write_all(&[0_u8; HEADER_BYTES])
            .map_err(|source| io_error("write flat segment header", &temporary, source))?;
        Ok(Self {
            directory: directory.to_path_buf(),
            temporary,
            generation,
            role,
            writer,
            hasher: Sha256::new(),
            payload_bytes: 0,
        })
    }

    pub(super) fn write_payload(&mut self, bytes: &[u8]) -> FlatResult<()> {
        self.writer
            .write_all(bytes)
            .map_err(|source| io_error("write flat segment payload", &self.temporary, source))?;
        self.hasher.update(bytes);
        self.payload_bytes = self
            .payload_bytes
            .checked_add(u64::try_from(bytes.len()).map_err(|_| {
                FlatStoreError::InvalidInput("segment write length is too large".to_owned())
            })?)
            .ok_or_else(|| {
                FlatStoreError::InvalidInput("segment payload length overflow".to_owned())
            })?;
        Ok(())
    }

    pub(super) fn finalize(
        mut self,
        record_count: u64,
        record_bytes: u32,
        dimensions: u32,
    ) -> FlatResult<ArtifactDescriptor> {
        let expected_payload = record_count
            .checked_mul(u64::from(record_bytes))
            .ok_or_else(|| {
                FlatStoreError::InvalidInput("segment payload length overflow".to_owned())
            })?;
        if self.payload_bytes != expected_payload {
            return Err(FlatStoreError::Corrupt(format!(
                "staged {} payload length does not match its records",
                self.role.name()
            )));
        }
        self.writer
            .flush()
            .map_err(|source| io_error("flush flat segment", &self.temporary, source))?;
        let digest = self.hasher.finalize();
        let digest_bytes: [u8; 32] = digest.into();
        let header = encode_header(SegmentHeader {
            magic: self.role.magic(),
            format_version: SEGMENT_FORMAT_VERSION,
            header_bytes: HEADER_BYTES as u32,
            generation: self.generation,
            record_count,
            record_bytes,
            dimensions,
            payload_bytes: self.payload_bytes,
            payload_sha256: digest_bytes,
        });
        self.writer
            .seek(SeekFrom::Start(0))
            .map_err(|source| io_error("seek flat segment", &self.temporary, source))?;
        self.writer
            .write_all(&header)
            .map_err(|source| io_error("finalize flat segment header", &self.temporary, source))?;
        self.writer
            .flush()
            .map_err(|source| io_error("flush flat segment header", &self.temporary, source))?;
        let file = self.writer.into_inner().map_err(|error| {
            io_error("finish flat segment", &self.temporary, error.into_error())
        })?;
        file.sync_all()
            .map_err(|source| io_error("sync flat segment", &self.temporary, source))?;
        drop(file);
        let digest_hex = encode_hex(&digest_bytes);
        let final_name = segment_name(self.generation, self.role, &digest_hex);
        let final_path = self.directory.join(&final_name);
        commit_unique_file(&self.temporary, &final_path)?;
        Ok(ArtifactDescriptor {
            file: final_name,
            file_bytes: HEADER_BYTES_U64 + self.payload_bytes,
            payload_sha256: digest_hex,
        })
    }
}

pub(super) fn encode_vector(vector: &[f32], scratch: &mut [u8]) -> FlatResult<()> {
    let vector_bytes = vector
        .len()
        .checked_mul(4)
        .ok_or_else(|| FlatStoreError::InvalidInput("vector byte length overflow".to_owned()))?;
    if vector_bytes > scratch.len() {
        return Err(FlatStoreError::InvalidInput(
            "vector exceeds its aligned row".to_owned(),
        ));
    }
    scratch.fill(0);
    for (value, destination) in vector
        .iter()
        .zip(scratch[..vector_bytes].chunks_exact_mut(4))
    {
        destination.copy_from_slice(&value.to_le_bytes());
    }
    Ok(())
}

pub(super) fn encode_metadata_record(metadata: FlatChunkMetadata) -> [u8; METADATA_RECORD_BYTES] {
    let mut record = [0_u8; METADATA_RECORD_BYTES];
    record[..16].copy_from_slice(metadata.event_id.as_bytes());
    record[16..48].copy_from_slice(metadata.source_text_hash.as_bytes());
    record[48..56].copy_from_slice(&metadata.seq.to_le_bytes());
    record[56..60].copy_from_slice(&metadata.chunk_index.to_le_bytes());
    record[60..64].copy_from_slice(&metadata.start_char.to_le_bytes());
    record[64..68].copy_from_slice(&metadata.end_char.to_le_bytes());
    record
}

pub(super) fn decode_metadata_record(record: &[u8]) -> FlatChunkMetadata {
    let mut event_id = [0_u8; 16];
    event_id.copy_from_slice(&record[..16]);
    let mut hash = [0_u8; 32];
    hash.copy_from_slice(&record[16..48]);
    FlatChunkMetadata {
        event_id: Uuid::from_bytes(event_id),
        source_text_hash: FlatSourceHash::from_bytes(hash),
        seq: u64::from_le_bytes([
            record[48], record[49], record[50], record[51], record[52], record[53], record[54],
            record[55],
        ]),
        chunk_index: u32::from_le_bytes([record[56], record[57], record[58], record[59]]),
        start_char: u32::from_le_bytes([record[60], record[61], record[62], record[63]]),
        end_char: u32::from_le_bytes([record[64], record[65], record[66], record[67]]),
    }
}

pub(super) fn decode_metadata_record_checked(record: &[u8]) -> FlatResult<FlatChunkMetadata> {
    let metadata = decode_metadata_record(record);
    if metadata.event_id.is_nil() || record[68..].iter().any(|byte| *byte != 0) {
        return Err(FlatStoreError::Corrupt(
            "metadata record has invalid identity or reserved bytes".to_owned(),
        ));
    }
    Ok(metadata)
}

pub(super) fn encode_mutation_record(mutation: EventMutation) -> [u8; MUTATION_RECORD_BYTES] {
    let mut record = [0_u8; MUTATION_RECORD_BYTES];
    record[..16].copy_from_slice(mutation.event_id.as_bytes());
    record[16] = mutation.kind as u8;
    record
}

pub(super) fn decode_mutation_record(record: &[u8]) -> FlatResult<EventMutation> {
    let mut event_id = [0_u8; 16];
    event_id.copy_from_slice(&record[..16]);
    let event_id = Uuid::from_bytes(event_id);
    if event_id.is_nil() || record[17..].iter().any(|byte| *byte != 0) {
        return Err(FlatStoreError::Corrupt(
            "mutation record has invalid identity or reserved bytes".to_owned(),
        ));
    }
    let kind = match record[16] {
        1 => MutationKind::Replace,
        2 => MutationKind::Delete,
        value => {
            return Err(FlatStoreError::Corrupt(format!(
                "mutation record has unknown kind {value}"
            )));
        }
    };
    Ok(EventMutation { event_id, kind })
}

pub(super) fn encode_header(header: SegmentHeader) -> [u8; HEADER_BYTES] {
    let mut bytes = [0_u8; HEADER_BYTES];
    bytes[..8].copy_from_slice(&header.magic);
    bytes[8..12].copy_from_slice(&header.format_version.to_le_bytes());
    bytes[12..16].copy_from_slice(&header.header_bytes.to_le_bytes());
    bytes[16..24].copy_from_slice(&header.generation.to_le_bytes());
    bytes[24..32].copy_from_slice(&header.record_count.to_le_bytes());
    bytes[32..36].copy_from_slice(&header.record_bytes.to_le_bytes());
    bytes[36..40].copy_from_slice(&header.dimensions.to_le_bytes());
    bytes[40..48].copy_from_slice(&header.payload_bytes.to_le_bytes());
    bytes[48..80].copy_from_slice(&header.payload_sha256);
    bytes
}

pub(super) fn decode_header(mapping: &[u8]) -> FlatResult<SegmentHeader> {
    let bytes = mapping.get(..HEADER_BYTES).ok_or_else(|| {
        FlatStoreError::Corrupt("segment is shorter than its fixed header".to_owned())
    })?;
    if bytes[80..].iter().any(|byte| *byte != 0) {
        return Err(FlatStoreError::Corrupt(
            "segment header has non-zero reserved bytes".to_owned(),
        ));
    }
    let mut magic = [0_u8; 8];
    magic.copy_from_slice(&bytes[..8]);
    let mut digest = [0_u8; 32];
    digest.copy_from_slice(&bytes[48..80]);
    Ok(SegmentHeader {
        magic,
        format_version: read_u32(bytes, 8),
        header_bytes: read_u32(bytes, 12),
        generation: read_u64(bytes, 16),
        record_count: read_u64(bytes, 24),
        record_bytes: read_u32(bytes, 32),
        dimensions: read_u32(bytes, 36),
        payload_bytes: read_u64(bytes, 40),
        payload_sha256: digest,
    })
}

pub(super) fn read_u32(bytes: &[u8], start: usize) -> u32 {
    u32::from_le_bytes([
        bytes[start],
        bytes[start + 1],
        bytes[start + 2],
        bytes[start + 3],
    ])
}

pub(super) fn read_u64(bytes: &[u8], start: usize) -> u64 {
    u64::from_le_bytes([
        bytes[start],
        bytes[start + 1],
        bytes[start + 2],
        bytes[start + 3],
        bytes[start + 4],
        bytes[start + 5],
        bytes[start + 6],
        bytes[start + 7],
    ])
}

pub(super) fn segment_name(generation: u64, role: ArtifactRole, digest: &str) -> String {
    format!(
        "{SEGMENT_PREFIX}{generation:020}-{}-{digest}.bin",
        role.name()
    )
}

pub(super) fn ensure_store_directories(root: &Path) -> FlatResult<()> {
    fs::create_dir_all(root).map_err(|source| io_error("create flat store root", root, source))?;
    ensure_real_directory(root)?;
    for directory in [manifests_directory(root), segments_directory(root)] {
        fs::create_dir_all(&directory)
            .map_err(|source| io_error("create flat store directory", &directory, source))?;
        ensure_real_directory(&directory)?;
    }
    let lock = lock_path(root);
    let file = open_lock(&lock, true)?;
    file.sync_all()
        .map_err(|source| io_error("sync flat writer lock", &lock, source))?;
    sync_directory(root)
}

pub(super) fn validate_existing_store_directories(root: &Path) -> FlatResult<()> {
    ensure_real_directory(root)?;
    ensure_real_directory(&manifests_directory(root))?;
    ensure_real_directory(&segments_directory(root))?;
    let lock = lock_path(root);
    let _ = symlink_metadata_file(&lock)?;
    Ok(())
}

pub(super) fn ensure_real_directory(path: &Path) -> FlatResult<()> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| io_error("stat flat store directory", path, source))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(FlatStoreError::Corrupt(format!(
            "{} is not a real directory",
            path.display()
        )));
    }
    Ok(())
}

pub(super) fn symlink_metadata_file(path: &Path) -> FlatResult<fs::Metadata> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| io_error("stat flat store file", path, source))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(FlatStoreError::Corrupt(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    Ok(metadata)
}

pub(super) fn manifests_directory(root: &Path) -> PathBuf {
    root.join(MANIFESTS_DIRECTORY)
}

pub(super) fn segments_directory(root: &Path) -> PathBuf {
    root.join(SEGMENTS_DIRECTORY)
}

pub(super) fn lock_path(root: &Path) -> PathBuf {
    root.join(WRITER_LOCK_FILE)
}

pub(super) fn create_new_file(path: &Path) -> FlatResult<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| io_error("create flat temporary file", path, source))
}

pub(super) fn unique_temporary_path(directory: &Path, purpose: &str) -> PathBuf {
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    directory.join(format!(
        "{TEMP_PREFIX}{purpose}-{}-{}-{sequence}",
        std::process::id(),
        unix_nanos()
    ))
}

pub(super) fn commit_unique_file(temporary: &Path, final_path: &Path) -> FlatResult<()> {
    if final_path.exists() {
        return Err(FlatStoreError::Corrupt(format!(
            "immutable flat artifact already exists: {}",
            final_path.display()
        )));
    }
    fs::rename(temporary, final_path)
        .map_err(|source| io_error("publish immutable flat artifact", final_path, source))
}

#[cfg(unix)]
pub(super) fn sync_directory(path: &Path) -> FlatResult<()> {
    File::open(path)
        .map_err(|source| io_error("open flat directory for sync", path, source))?
        .sync_all()
        .map_err(|source| io_error("sync flat directory", path, source))
}

#[cfg(not(unix))]
pub(super) fn sync_directory(_path: &Path) -> FlatResult<()> {
    Ok(())
}

pub(super) fn unix_nanos() -> u128 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_nanos(),
        Err(_) => 0,
    }
}

pub(super) fn io_error(
    operation: &'static str,
    path: &Path,
    source: std::io::Error,
) -> FlatStoreError {
    FlatStoreError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}
