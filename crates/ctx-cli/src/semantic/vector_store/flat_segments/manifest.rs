use super::*;

pub(super) const STORE_FORMAT: &str = "ctx-flat-f32";
pub(super) const MANIFEST_ENVELOPE_VERSION: u32 = 1;
pub(super) const MANIFEST_SCHEMA_VERSION: u32 = 1;
pub(super) const SEGMENT_FORMAT_VERSION: u32 = 1;
pub(super) const HEADER_BYTES: usize = 4_096;
pub(super) const HEADER_BYTES_U64: u64 = HEADER_BYTES as u64;
pub(super) const METADATA_RECORD_BYTES: usize = 72;
pub(super) const MUTATION_RECORD_BYTES: usize = 24;
pub(super) const VECTOR_ALIGNMENT: usize = 64;
pub(super) const MAX_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
pub(super) const MAX_DIMENSIONS: u32 = 65_536;
pub(super) const MAX_CONTRACT_FIELD_BYTES: usize = 1_024;
pub(super) const NORMALIZED_NORM_SQUARED_TOLERANCE: f64 = 1.0e-3;

pub(super) const MANIFESTS_DIRECTORY: &str = "flat_manifests";
pub(super) const SEGMENTS_DIRECTORY: &str = "flat_segments";
pub(super) const WRITER_LOCK_FILE: &str = "flat_writer.lock";
pub(super) const MANIFEST_PREFIX: &str = "flat-manifest-";
pub(super) const SEGMENT_PREFIX: &str = "flat-segment-";
pub(super) const TEMP_PREFIX: &str = ".flat-tmp-";

pub(super) const VECTOR_MAGIC: [u8; 8] = *b"CTXF32V\0";
pub(super) const METADATA_MAGIC: [u8; 8] = *b"CTXF32M\0";
pub(super) const MUTATION_MAGIC: [u8; 8] = *b"CTXF32T\0";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ManifestEnvelope {
    pub(super) format: String,
    pub(super) envelope_version: u32,
    pub(super) manifest: Manifest,
    pub(super) manifest_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Manifest {
    pub(super) schema_version: u32,
    pub(super) generation: u64,
    pub(super) created_unix_millis: u64,
    pub(super) model: FlatModelContract,
    pub(super) segments: Vec<SegmentDescriptor>,
}

impl Manifest {
    pub(super) fn new(model: FlatModelContract) -> Self {
        Self {
            schema_version: MANIFEST_SCHEMA_VERSION,
            generation: 0,
            created_unix_millis: 0,
            model,
            segments: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum SegmentKind {
    Base,
    Delta,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SegmentDescriptor {
    pub(super) format_version: u32,
    pub(super) generation: u64,
    pub(super) kind: SegmentKind,
    pub(super) vector_count: u64,
    pub(super) mutation_count: u64,
    pub(super) vectors: ArtifactDescriptor,
    pub(super) metadata: ArtifactDescriptor,
    pub(super) mutations: ArtifactDescriptor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ArtifactDescriptor {
    pub(super) file: String,
    pub(super) file_bytes: u64,
    pub(super) payload_sha256: String,
}

pub(super) struct SelectedManifest {
    pub(super) envelope: ManifestEnvelope,
    pub(super) generation_hash: String,
    pub(super) path: PathBuf,
}

pub(super) struct StagedSegment {
    pub(super) descriptor: SegmentDescriptor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MutationKind {
    Replace = 1,
    Delete = 2,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct EventMutation {
    pub(super) event_id: Uuid,
    pub(super) kind: MutationKind,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct FlatChunkMetadata {
    pub(super) event_id: Uuid,
    pub(super) seq: u64,
    pub(super) source_text_hash: FlatSourceHash,
    pub(super) chunk_index: u32,
    pub(super) start_char: u32,
    pub(super) end_char: u32,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct EventVersion {
    pub(super) generation: u64,
    pub(super) kind: MutationKind,
}

pub(super) struct LoadedSegment {
    pub(super) descriptor: SegmentDescriptor,
    pub(super) vectors: Mmap,
    pub(super) metadata: Mmap,
    pub(super) mutations: Vec<EventMutation>,
    pub(super) stride_bytes: usize,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum ArtifactRole {
    Vectors,
    Metadata,
    Mutations,
}

impl ArtifactRole {
    pub(super) fn magic(self) -> [u8; 8] {
        match self {
            Self::Vectors => VECTOR_MAGIC,
            Self::Metadata => METADATA_MAGIC,
            Self::Mutations => MUTATION_MAGIC,
        }
    }

    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Vectors => "vectors",
            Self::Metadata => "metadata",
            Self::Mutations => "mutations",
        }
    }
}

#[derive(Debug)]
pub(super) struct SegmentHeader {
    pub(super) magic: [u8; 8],
    pub(super) format_version: u32,
    pub(super) header_bytes: u32,
    pub(super) generation: u64,
    pub(super) record_count: u64,
    pub(super) record_bytes: u32,
    pub(super) dimensions: u32,
    pub(super) payload_bytes: u64,
    pub(super) payload_sha256: [u8; 32],
}

pub(super) fn select_manifest(
    root: &Path,
    expected_contract: &FlatModelContract,
) -> FlatResult<Option<SelectedManifest>> {
    let selected = select_manifest_any(root)?;
    if selected
        .as_ref()
        .is_some_and(|selected| &selected.envelope.manifest.model != expected_contract)
    {
        return Err(FlatStoreError::Incompatible(
            "manifest model/tokenizer/pooling/dimension/normalization contract changed".to_owned(),
        ));
    }
    Ok(selected)
}

pub(super) fn select_manifest_any(root: &Path) -> FlatResult<Option<SelectedManifest>> {
    let directory = manifests_directory(root);
    let entries = fs::read_dir(&directory)
        .map_err(|source| io_error("read flat manifest directory", &directory, source))?;
    let mut candidates = Vec::<(u64, String, PathBuf)>::new();
    for entry in entries {
        let entry =
            entry.map_err(|source| io_error("read flat manifest entry", &directory, source))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with(TEMP_PREFIX) {
            continue;
        }
        if !name.starts_with(MANIFEST_PREFIX) {
            continue;
        }
        let (generation, digest) = parse_manifest_name(name).ok_or_else(|| {
            FlatStoreError::Corrupt(format!("malformed committed manifest name {name:?}"))
        })?;
        let metadata = entry
            .metadata()
            .map_err(|source| io_error("stat flat manifest", &entry.path(), source))?;
        if !metadata.is_file() {
            return Err(FlatStoreError::Corrupt(format!(
                "committed manifest {name:?} is not a regular file"
            )));
        }
        candidates.push((generation, digest, entry.path()));
    }
    candidates.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    let Some((highest_generation, _, _)) = candidates.last() else {
        return Ok(None);
    };
    if candidates
        .iter()
        .rev()
        .take_while(|candidate| candidate.0 == *highest_generation)
        .count()
        != 1
    {
        return Err(FlatStoreError::Corrupt(format!(
            "multiple manifests claim generation {highest_generation}"
        )));
    }
    let (filename_generation, filename_digest, path) = candidates
        .pop()
        .ok_or_else(|| FlatStoreError::Corrupt("manifest selection failed".to_owned()))?;
    let envelope = read_manifest(&path)?;
    validate_manifest(&envelope, filename_generation, &filename_digest)?;
    Ok(Some(SelectedManifest {
        envelope,
        generation_hash: filename_digest,
        path,
    }))
}

pub(super) fn read_manifest(path: &Path) -> FlatResult<ManifestEnvelope> {
    let metadata = symlink_metadata_file(path)?;
    if metadata.len() == 0 || metadata.len() > MAX_MANIFEST_BYTES {
        return Err(FlatStoreError::Corrupt(format!(
            "manifest {} has unsafe size {}",
            path.display(),
            metadata.len()
        )));
    }
    let mut file =
        File::open(path).map_err(|source| io_error("open flat manifest", path, source))?;
    let capacity = usize_from_u64(metadata.len(), "manifest size")?;
    let mut bytes = Vec::with_capacity(capacity);
    file.read_to_end(&mut bytes)
        .map_err(|source| io_error("read flat manifest", path, source))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| FlatStoreError::Corrupt(format!("invalid manifest JSON: {error}")))
}

pub(super) fn manifest_name(generation: u64, digest: &str) -> String {
    format!("{MANIFEST_PREFIX}{generation:020}-{digest}.json")
}

pub(super) fn parse_manifest_name(name: &str) -> Option<(u64, String)> {
    let body = name.strip_prefix(MANIFEST_PREFIX)?.strip_suffix(".json")?;
    let (generation, digest) = body.split_once('-')?;
    if generation.len() != 20 || decode_sha256(digest).is_none() {
        return None;
    }
    Some((generation.parse().ok()?, digest.to_owned()))
}

pub(super) fn next_generation(current: Option<&SelectedManifest>) -> FlatResult<u64> {
    current
        .map(|selected| selected.envelope.manifest.generation)
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| FlatStoreError::Corrupt("manifest generation overflow".to_owned()))
}

pub(super) fn noop_outcome(current: Option<&SelectedManifest>) -> FlatPublishOutcome {
    FlatPublishOutcome {
        published: false,
        generation: current
            .map(|selected| selected.envelope.manifest.generation)
            .unwrap_or(0),
        generation_hash: current.map(|selected| selected.generation_hash.clone()),
        replaced_events: 0,
        deleted_events: 0,
    }
}

pub(super) fn decode_sha256(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    let mut bytes = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_nibble(pair[0])?;
        let low = hex_nibble(pair[1])?;
        bytes[index] = high << 4 | low;
    }
    Some(bytes)
}

pub(super) fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

pub(super) fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

pub(super) fn unix_millis() -> u64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
        Err(_) => 0,
    }
}
