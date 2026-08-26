use std::{
    collections::BTreeMap,
    collections::BTreeSet,
    io::Read,
    path::{Path, PathBuf},
};

use crc32fast::Hasher as Crc32;
use sha2::{Digest, Sha256};
use tantivy::{
    directory::{footer::Footer, Directory as _},
    index::SegmentComponent,
    HasLen,
};

use crate::{
    certification::{
        open_artifact, open_authenticated_artifact, recapture_artifact,
        recapture_authenticated_artifact, ArtifactIdentity,
    },
    hex, ActiveGenerationPointer, DurableMmapDirectory, GenerationError as IndexError, Result,
};

#[cfg(any(test, feature = "test-support"))]
use std::cell::Cell;

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static CHECKSUM_WALKS: Cell<usize> = const { Cell::new(0) };
    static HASHED_ARTIFACT_BYTES: Cell<u64> = const { Cell::new(0) };
}

const PHYSICAL_INTEGRITY_DOMAIN: &[u8] = b"ctx-tantivy-physical-integrity-v1\0";
const PHYSICAL_HASH_BUFFER_BYTES: usize = 64 * 1024;
const TANTIVY_META_FILE: &str = "meta.json";
const MANAGED_FILE: &str = ".managed.json";
pub(crate) const MAX_MANAGED_METADATA_BYTES: u64 = 1024 * 1024;
pub(crate) const MAX_MANAGED_FILE_ENTRIES: usize = 4_096;
pub(crate) const MAX_MANAGED_GENERATION_BYTES: u64 = 1024 * 1024 * 1024 * 1024;

pub(crate) struct ManagedFileTopology {
    retired: BTreeSet<PathBuf>,
}

impl ManagedFileTopology {
    pub(crate) fn retired(&self) -> &BTreeSet<PathBuf> {
        &self.retired
    }
}

#[derive(Debug)]
pub(super) struct PhysicalFileDigest {
    pub(super) artifact: ArtifactIdentity,
    pub(super) sha256: [u8; 32],
}

impl Clone for PhysicalFileDigest {
    fn clone(&self) -> Self {
        Self {
            artifact: self.artifact.clone(),
            sha256: self.sha256,
        }
    }
}

#[derive(Debug)]
struct PhysicalDigestPart {
    path: String,
    length: u64,
    sha256: [u8; 32],
}

#[derive(Debug)]
pub struct PhysicalIntegrityAudit {
    digest: String,
    files: Vec<PhysicalFileDigest>,
}

#[derive(Debug, Clone, Default)]
pub struct CandidatePhysicalProof {
    files: BTreeMap<String, PhysicalFileDigest>,
}

impl CandidatePhysicalProof {
    pub fn clear(&mut self) {
        self.files.clear();
    }

    pub(crate) fn insert(&mut self, file: PhysicalFileDigest) {
        self.files.insert(file.artifact.path.clone(), file);
    }
}

impl PhysicalIntegrityAudit {
    pub fn digest(&self) -> &str {
        &self.digest
    }

    pub(super) fn files(&self) -> &[PhysicalFileDigest] {
        &self.files
    }

    pub(super) fn artifact_paths(&self) -> Vec<String> {
        self.files
            .iter()
            .map(|file| file.artifact.path.clone())
            .collect()
    }
}

/// Computes one physical generation's canonical integrity digest.
///
/// The domain-separated stream contains the exact active file count followed
/// by each sorted UTF-8 relative path, file length, and SHA-256 of the complete
/// file bytes. The sorted path set always includes `meta.json` and every segment
/// file referenced by its active segment metadata. Managed bookkeeping, locks,
/// and temporary files are deliberately excluded because queries do not read them.
/// Segment bytes are streamed once and checked against their Tantivy CRC footer
/// while their stronger SHA-256 is computed.
///
/// `topology_authority` is the caller's already-decoded publication topology.
/// `None` is reserved for a new root or a source-authoritative cold rebuild whose
/// incompatible pointer must remain opaque until the candidate replaces it.
pub fn physical_integrity_digest(
    index: &tantivy::Index,
    generation_path: &Path,
    topology_authority: Option<&ActiveGenerationPointer>,
) -> Result<String> {
    Ok(physical_integrity_audit(index, generation_path, topology_authority)?.digest)
}

pub fn physical_integrity_audit(
    index: &tantivy::Index,
    generation_path: &Path,
    topology_authority: Option<&ActiveGenerationPointer>,
) -> Result<PhysicalIntegrityAudit> {
    physical_integrity_audit_with_candidate_proof(index, generation_path, topology_authority, None)
}

pub fn prime_candidate_physical_proof(
    index: &tantivy::Index,
    generation_path: &Path,
    topology_authority: Option<&ActiveGenerationPointer>,
    proof: &mut CandidatePhysicalProof,
) -> Result<()> {
    let paths = active_paths(index)?;
    proof
        .files
        .retain(|path, _| paths.contains(&PathBuf::from(path.as_str())));
    for path in paths {
        let Some(path_text) = path.to_str() else {
            return Err(IndexError::ChecksumMismatch);
        };
        if let Some(cached) = proof.files.get(path_text) {
            let root = generation_root(generation_path)?;
            let current = recapture_artifact(root, generation_path, &path, topology_authority)?;
            if current == cached.artifact {
                continue;
            }
        }
        let file = hash_physical_file(
            generation_root(generation_path)?,
            &DurableMmapDirectory::open(generation_path)
                .map_err(|_| IndexError::ChecksumMismatch)?,
            generation_path,
            &path,
            path != Path::new(TANTIVY_META_FILE),
            topology_authority,
        )?;
        proof.insert(file);
    }
    Ok(())
}

pub fn physical_integrity_audit_with_candidate_proof(
    index: &tantivy::Index,
    generation_path: &Path,
    topology_authority: Option<&ActiveGenerationPointer>,
    candidate_proof: Option<&CandidatePhysicalProof>,
) -> Result<PhysicalIntegrityAudit> {
    #[cfg(any(test, feature = "test-support"))]
    CHECKSUM_WALKS.with(|count| count.set(count.get() + 1));
    let directory =
        DurableMmapDirectory::open(generation_path).map_err(|_| IndexError::ChecksumMismatch)?;
    let root = generation_root(generation_path)?;
    let paths = active_paths(index)?;
    let entries = paths
        .into_iter()
        .map(|path| {
            match candidate_proof.and_then(|proof| {
                let path_text = path.to_str()?;
                let cached = proof.files.get(path_text)?;
                let current =
                    recapture_artifact(root, generation_path, &path, topology_authority).ok()?;
                (current == cached.artifact).then_some(cached.clone())
            }) {
                Some(cached) => Ok(cached),
                None => hash_physical_file(
                    root,
                    &directory,
                    generation_path,
                    &path,
                    path != Path::new(TANTIVY_META_FILE),
                    topology_authority,
                ),
            }
        })
        .collect::<Result<Vec<_>>>()?;
    let parts = entries
        .iter()
        .map(|entry| PhysicalDigestPart {
            path: entry.artifact.path.clone(),
            length: entry.artifact.identity.length(),
            sha256: entry.sha256,
        })
        .collect::<Vec<_>>();
    let digest = canonical_physical_integrity_digest(&parts)?;
    Ok(PhysicalIntegrityAudit {
        digest,
        files: entries,
    })
}

pub fn verify_candidate_physical_fence(
    index: &tantivy::Index,
    generation_path: &Path,
    topology_authority: Option<&ActiveGenerationPointer>,
    expected: &PhysicalIntegrityAudit,
) -> Result<()> {
    let paths = active_paths(index)?;
    if expected.artifact_paths()
        != paths
            .iter()
            .map(|path| {
                path.to_str()
                    .map(str::to_owned)
                    .ok_or(IndexError::ChecksumMismatch)
            })
            .collect::<Result<Vec<_>>>()?
    {
        return Err(IndexError::ChecksumMismatch);
    }
    let root = generation_root(generation_path)?;
    for expected in &expected.files {
        let current = recapture_artifact(
            root,
            generation_path,
            Path::new(&expected.artifact.path),
            topology_authority,
        )?;
        if current != expected.artifact {
            return if expected.artifact.same_payload_identity_changed(&current) {
                Err(IndexError::ConcurrentGenerationChange)
            } else {
                Err(IndexError::ChecksumMismatch)
            };
        }
    }
    Ok(())
}

pub fn validate_candidate_managed_files(
    index: &tantivy::Index,
    generation_path: &Path,
    topology_authority: Option<&ActiveGenerationPointer>,
) -> Result<()> {
    let root = generation_root(generation_path)?;
    let relative = Path::new(MANAGED_FILE);
    let (mut file, before) =
        open_authenticated_artifact(root, generation_path, relative, topology_authority)?;
    let length = before.identity.length();
    if length > MAX_MANAGED_METADATA_BYTES {
        return Err(IndexError::ChecksumMismatch);
    }
    let capacity = usize::try_from(length).map_err(|_| IndexError::CountOverflow)?;
    let mut bytes = Vec::with_capacity(capacity);
    Read::by_ref(&mut file)
        .take(MAX_MANAGED_METADATA_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| IndexError::ChecksumMismatch)?;
    if bytes.len() != capacity {
        return Err(IndexError::ChecksumMismatch);
    }
    let after = recapture_authenticated_artifact(
        root,
        generation_path,
        relative,
        &file,
        topology_authority,
    )?;
    if after != before {
        return Err(IndexError::ConcurrentGenerationChange);
    }

    let managed =
        serde_json::from_slice::<Vec<PathBuf>>(&bytes).map_err(|_| IndexError::ChecksumMismatch)?;
    let active = active_paths(index)?;
    let topology = managed_file_topology(&managed, &active).ok_or(IndexError::ChecksumMismatch)?;
    validate_retired_managed_files(
        root,
        generation_path,
        topology_authority,
        topology.retired(),
    )?;
    Ok(())
}

/// Classifies Tantivy's managed-file ledger against the searchable topology.
///
/// `meta.json` remains authoritative for the active segment set. The managed
/// ledger must contain every active path, but it may also retain bounded,
/// uniquely named Tantivy segment components that garbage collection could not
/// remove. No other managed namespace is accepted.
pub(crate) fn managed_file_topology(
    managed: &[PathBuf],
    active: &BTreeSet<PathBuf>,
) -> Option<ManagedFileTopology> {
    if managed.len() > MAX_MANAGED_FILE_ENTRIES
        || managed
            .iter()
            .any(|path| !is_single_relative_component(path))
    {
        return None;
    }
    let managed_set = managed.iter().cloned().collect::<BTreeSet<_>>();
    if managed_set.len() != managed.len() || !active.is_subset(&managed_set) {
        return None;
    }
    let retired = managed_set
        .difference(active)
        .cloned()
        .collect::<BTreeSet<_>>();
    if retired.iter().any(|path| !is_tantivy_segment_path(path)) {
        return None;
    }
    Some(ManagedFileTopology { retired })
}

pub(crate) fn canonical_active_managed_bytes(active: &BTreeSet<PathBuf>) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(active)?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAX_MANAGED_METADATA_BYTES {
        return Err(IndexError::ChecksumMismatch);
    }
    Ok(bytes)
}

fn validate_retired_managed_files(
    root: &Path,
    generation_path: &Path,
    topology_authority: Option<&ActiveGenerationPointer>,
    retired: &BTreeSet<PathBuf>,
) -> Result<()> {
    let mut present_bytes = 0_u64;
    for relative in retired {
        let path = generation_path.join(relative);
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err(IndexError::ChecksumMismatch),
        };
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            return Err(IndexError::ChecksumMismatch);
        }
        let (file, before) =
            open_authenticated_artifact(root, generation_path, relative, topology_authority)?;
        present_bytes = present_bytes
            .checked_add(before.identity.length())
            .ok_or(IndexError::CountOverflow)?;
        if present_bytes > MAX_MANAGED_GENERATION_BYTES {
            return Err(IndexError::ChecksumMismatch);
        }
        let after = recapture_authenticated_artifact(
            root,
            generation_path,
            relative,
            &file,
            topology_authority,
        )?;
        if after != before {
            return Err(IndexError::ConcurrentGenerationChange);
        }
    }
    Ok(())
}

fn is_single_relative_component(path: &Path) -> bool {
    let mut components = path.components();
    matches!(components.next(), Some(std::path::Component::Normal(_)))
        && components.next().is_none()
}

fn is_tantivy_segment_path(path: &Path) -> bool {
    let Some(name) = path.to_str() else {
        return false;
    };
    let Some((segment_id, suffix)) = name.get(..32).zip(name.get(32..)) else {
        return false;
    };
    if !segment_id
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return false;
    }
    if matches!(
        suffix,
        ".idx" | ".pos" | ".term" | ".store" | ".fast" | ".fieldnorm"
    ) {
        return true;
    }
    let Some(delete_opstamp) = suffix
        .strip_prefix('.')
        .and_then(|suffix| suffix.strip_suffix(".del"))
    else {
        return false;
    };
    !delete_opstamp.is_empty()
        && (delete_opstamp == "0" || !delete_opstamp.starts_with('0'))
        && delete_opstamp.bytes().all(|byte| byte.is_ascii_digit())
        && delete_opstamp.parse::<u64>().is_ok()
}

fn generation_root(generation_path: &Path) -> Result<&Path> {
    generation_path
        .parent()
        .filter(|parent| {
            parent
                .file_name()
                .is_some_and(|name| name == "index-generations")
        })
        .and_then(Path::parent)
        .ok_or(IndexError::ChecksumMismatch)
}

fn active_paths(index: &tantivy::Index) -> Result<BTreeSet<PathBuf>> {
    let mut paths = active_index_files(index)?;
    paths.insert(PathBuf::from(TANTIVY_META_FILE));
    Ok(paths)
}

/// Verifies a generation against the physical authority in its pointer slot.
pub fn verify_physical_integrity(
    index: &tantivy::Index,
    generation_path: &Path,
    topology_authority: Option<&ActiveGenerationPointer>,
    expected_digest: &str,
) -> Result<()> {
    let audit = physical_integrity_audit(index, generation_path, topology_authority)?;
    if audit.digest != expected_digest {
        return Err(IndexError::ChecksumMismatch);
    }
    Ok(())
}

fn hash_physical_file(
    root: &Path,
    directory: &DurableMmapDirectory,
    generation_path: &Path,
    relative_path: &Path,
    validate_tantivy_footer: bool,
    pointer: Option<&ActiveGenerationPointer>,
) -> Result<PhysicalFileDigest> {
    let (mut file, artifact) = open_artifact(root, generation_path, relative_path, pointer)?;
    let length = artifact.identity.length();
    let footer_contract = if validate_tantivy_footer {
        let slice = directory
            .open_read(relative_path)
            .map_err(|_| IndexError::ChecksumMismatch)?;
        let (footer, body) =
            Footer::extract_footer(slice).map_err(|_| IndexError::ChecksumMismatch)?;
        Some((
            u64::try_from(body.len()).map_err(|_| IndexError::CountOverflow)?,
            footer.crc,
        ))
    } else {
        None
    };

    let mut sha256 = Sha256::new();
    let mut crc32 = footer_contract.map(|_| Crc32::new());
    let mut body_remaining = footer_contract.map_or(0, |(body_length, _)| body_length);
    let mut bytes_read = 0_u64;
    let mut buffer = [0_u8; PHYSICAL_HASH_BUFFER_BYTES];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|_| IndexError::ChecksumMismatch)?;
        if count == 0 {
            break;
        }
        let count_u64 = u64::try_from(count).map_err(|_| IndexError::CountOverflow)?;
        bytes_read = bytes_read
            .checked_add(count_u64)
            .ok_or(IndexError::CountOverflow)?;
        #[cfg(any(test, feature = "test-support"))]
        HASHED_ARTIFACT_BYTES.with(|bytes| bytes.set(bytes.get().saturating_add(count_u64)));
        sha256.update(&buffer[..count]);
        if let Some(crc32) = crc32.as_mut() {
            let body_count = usize::try_from(body_remaining.min(count_u64))
                .map_err(|_| IndexError::CountOverflow)?;
            crc32.update(&buffer[..body_count]);
            body_remaining -= u64::try_from(body_count).map_err(|_| IndexError::CountOverflow)?;
        }
    }
    if bytes_read != length || body_remaining != 0 {
        return Err(IndexError::ChecksumMismatch);
    }
    if let (Some(crc32), Some((_, expected_crc32))) = (crc32, footer_contract) {
        if crc32.finalize() != expected_crc32 {
            return Err(IndexError::ChecksumMismatch);
        }
    }
    let recaptured = recapture_artifact(root, generation_path, relative_path, pointer)?;
    if recaptured != artifact {
        return if artifact.same_payload_identity_changed(&recaptured) {
            Err(IndexError::ConcurrentGenerationChange)
        } else {
            Err(IndexError::ChecksumMismatch)
        };
    }
    Ok(PhysicalFileDigest {
        artifact,
        sha256: sha256.finalize().into(),
    })
}

fn canonical_physical_integrity_digest(entries: &[PhysicalDigestPart]) -> Result<String> {
    physical_integrity_digest_from_parts(
        entries
            .iter()
            .map(|entry| (entry.path.as_str(), entry.length, entry.sha256)),
    )
}

pub(super) fn physical_integrity_digest_from_parts<'a>(
    entries: impl IntoIterator<Item = (&'a str, u64, [u8; 32])>,
) -> Result<String> {
    let entries = entries.into_iter().collect::<Vec<_>>();
    let mut digest = Sha256::new();
    digest.update(PHYSICAL_INTEGRITY_DOMAIN);
    digest.update(
        u64::try_from(entries.len())
            .map_err(|_| IndexError::CountOverflow)?
            .to_be_bytes(),
    );
    for entry in entries {
        let path = entry.0.as_bytes();
        digest.update(
            u64::try_from(path.len())
                .map_err(|_| IndexError::CountOverflow)?
                .to_be_bytes(),
        );
        digest.update(path);
        digest.update(entry.1.to_be_bytes());
        digest.update(entry.2);
    }
    Ok(hex(&digest.finalize()))
}

#[cfg(test)]
#[test]
fn sha256_physical_authority_distinguishes_stubbed_crc_collision() {
    struct StubbedCrcFile<'a> {
        path: &'a str,
        bytes: &'a [u8],
        crc32: u32,
    }

    let first = StubbedCrcFile {
        path: "same.store",
        bytes: b"certificate-A",
        crc32: 0xfeed_beef,
    };
    let second = StubbedCrcFile {
        path: "same.store",
        bytes: b"certificate-B",
        crc32: 0xfeed_beef,
    };
    assert_eq!(first.path, second.path);
    assert_eq!(first.bytes.len(), second.bytes.len());
    assert_eq!(first.crc32, second.crc32);

    let entry = |file: &StubbedCrcFile<'_>| PhysicalDigestPart {
        path: file.path.to_owned(),
        length: u64::try_from(file.bytes.len()).unwrap(),
        sha256: Sha256::digest(file.bytes).into(),
    };
    assert_ne!(
        canonical_physical_integrity_digest(&[entry(&first)]).unwrap(),
        canonical_physical_integrity_digest(&[entry(&second)]).unwrap()
    );
}

#[cfg(test)]
#[test]
fn managed_topology_accepts_only_bounded_retired_tantivy_segments() {
    let active_segment = PathBuf::from("11111111111111111111111111111111.store");
    let active = BTreeSet::from([PathBuf::from(TANTIVY_META_FILE), active_segment.clone()]);
    let retired_store = PathBuf::from("22222222222222222222222222222222.store");
    let retired_delete = PathBuf::from("33333333333333333333333333333333.42.del");
    let managed = vec![
        PathBuf::from(TANTIVY_META_FILE),
        active_segment,
        retired_store.clone(),
        retired_delete.clone(),
    ];

    let topology = managed_file_topology(&managed, &active).unwrap();
    assert_eq!(
        topology.retired(),
        &BTreeSet::from([retired_store, retired_delete])
    );

    for invalid in [
        PathBuf::from("operator-note.txt"),
        PathBuf::from("../22222222222222222222222222222222.store"),
        PathBuf::from("2222222222222222222222222222222.store"),
        PathBuf::from("2222222222222222222222222222222g.store"),
        PathBuf::from("22222222222222222222222222222222.segment"),
        PathBuf::from("22222222222222222222222222222222.01.del"),
    ] {
        let mut invalid_managed = managed.clone();
        invalid_managed.push(invalid);
        assert!(managed_file_topology(&invalid_managed, &active).is_none());
    }

    let mut duplicate = managed.clone();
    duplicate.push(managed[0].clone());
    assert!(managed_file_topology(&duplicate, &active).is_none());
    assert!(managed_file_topology(&managed[1..], &active).is_none());
}

pub fn active_index_files(index: &tantivy::Index) -> Result<BTreeSet<PathBuf>> {
    let directory = index.directory();
    let metas = index.load_metas()?;
    let mut expected_files = BTreeSet::new();
    for segment in &metas.segments {
        for component in [
            SegmentComponent::Postings,
            SegmentComponent::FastFields,
            SegmentComponent::FieldNorms,
            SegmentComponent::Terms,
            SegmentComponent::Store,
        ] {
            expected_files.insert(segment.relative_path(component));
        }
        let positions = segment.relative_path(SegmentComponent::Positions);
        if directory
            .exists(&positions)
            .map_err(|_| IndexError::ChecksumMismatch)?
        {
            expected_files.insert(positions);
        }
        if segment.has_deletes() {
            expected_files.insert(segment.relative_path(SegmentComponent::Delete));
        }
    }
    Ok(expected_files)
}

#[cfg(any(test, feature = "test-support"))]
pub fn reset_physical_verification_activity() {
    CHECKSUM_WALKS.with(|count| count.set(0));
    HASHED_ARTIFACT_BYTES.with(|bytes| bytes.set(0));
}

#[cfg(any(test, feature = "test-support"))]
pub fn checksum_walks() -> usize {
    CHECKSUM_WALKS.with(Cell::get)
}

#[cfg(any(test, feature = "test-support"))]
pub fn hashed_artifact_bytes() -> u64 {
    HASHED_ARTIFACT_BYTES.with(Cell::get)
}
