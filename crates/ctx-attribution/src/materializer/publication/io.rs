use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::{Component, Path};

use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use std::io::{Read as _, Seek as _};

#[cfg(test)]
use crate::graph::segment::IndexedCoreEventState;
use crate::graph::segment::{
    CompactIndexedCoreEventState, EventIndexReader, EventIndexSource, EventIndexWriter,
    EventLineageTables, FLAT_CHUNK_BYTES, FlatSegmentWriter, IndexedCoreEventTombstone,
    SegmentFile, SegmentRef, SegmentWriter, segment_file_name,
};

use super::super::SegmentMaterializerError;
use super::super::locking::{
    open_private_file, remove_private_file_if_exists, sync_private_root, verify_private_root,
};
use super::super::model::{MAX_METADATA_SEGMENT_BYTES, SEGMENT_CHUNK_BYTES};
#[derive(Clone)]
pub(super) struct PlannedSegmentWrite {
    reference: SegmentRef,
    generation: [u8; 32],
    chunk_bytes: u32,
}

impl PlannedSegmentWrite {
    pub(super) fn placeholder(&self) -> SegmentRef {
        self.reference.clone()
    }

    pub(super) const fn ordinal(&self) -> u32 {
        self.reference.ordinal
    }

    pub(super) fn file_name(&self) -> &str {
        &self.reference.file_name
    }
}

pub(super) struct CompletedSegmentPublication {
    pub(super) reference: SegmentRef,
    pub(super) checked_readbacks: u64,
}

#[cfg(test)]
pub(super) fn write_json_segment<T: Serialize>(
    root: &Path,

    publication_generation: u64,
    role: u32,
    ordinal: u32,
    value: &T,
) -> Result<SegmentRef, SegmentMaterializerError> {
    let plan = plan_json_segment(&"0".repeat(64), publication_generation, role, ordinal)?;
    Ok(write_planned_json_segment(root, plan, value)?.reference)
}

pub(super) fn plan_json_segment(
    materialization_id: &str,
    publication_generation: u64,
    role: u32,
    ordinal: u32,
) -> Result<PlannedSegmentWrite, SegmentMaterializerError> {
    plan_segment(
        materialization_id,
        publication_generation,
        role,
        SEGMENT_CHUNK_BYTES,
        ordinal,
    )
}

pub(super) fn write_planned_json_segment<T: Serialize>(
    root: &Path,

    plan: PlannedSegmentWrite,
    value: &T,
) -> Result<CompletedSegmentPublication, SegmentMaterializerError> {
    if plan.chunk_bytes != SEGMENT_CHUNK_BYTES {
        return Err(SegmentMaterializerError::Corrupt(
            "JSON publication plan has the wrong chunk policy",
        ));
    }
    let encoded = serde_json::to_vec(value).map_err(|_| SegmentMaterializerError::Encoding)?;
    if encoded.is_empty() || encoded.len() > MAX_METADATA_SEGMENT_BYTES {
        return Err(SegmentMaterializerError::Bounds);
    }
    let plaintext_bytes =
        u64::try_from(encoded.len()).map_err(|_| SegmentMaterializerError::Bounds)?;
    write_planned_segment(root, plan, |writer| {
        let mut writer = writer;
        writer
            .write_all(encoded.as_slice())
            .map_err(|source| io_error(root, source))?;
        writer.finish()?;
        Ok(CompletedSegmentWrite { plaintext_bytes })
    })
}

#[cfg(test)]
pub(super) fn write_flat_segment(
    root: &Path,

    publication_generation: u64,
    records: Vec<crate::graph::segment::ServingRecord>,
    tombstones: Vec<crate::graph::segment::EventTombstone>,
) -> Result<SegmentRef, SegmentMaterializerError> {
    let plan = plan_flat_segment(&"0".repeat(64), publication_generation, 0)?;
    Ok(write_planned_flat_segment(root, plan, records, tombstones)?.reference)
}

pub(super) fn plan_flat_segment(
    materialization_id: &str,
    publication_generation: u64,
    ordinal: u32,
) -> Result<PlannedSegmentWrite, SegmentMaterializerError> {
    plan_segment(
        materialization_id,
        publication_generation,
        crate::graph::segment::FLAT_SERVING_ROLE,
        FLAT_CHUNK_BYTES,
        ordinal,
    )
}

pub(super) fn write_planned_flat_segment(
    root: &Path,

    plan: PlannedSegmentWrite,
    records: Vec<crate::graph::segment::ServingRecord>,
    tombstones: Vec<crate::graph::segment::EventTombstone>,
) -> Result<CompletedSegmentPublication, SegmentMaterializerError> {
    if plan.reference.role != crate::graph::segment::FLAT_SERVING_ROLE
        || plan.chunk_bytes != FLAT_CHUNK_BYTES
    {
        return Err(SegmentMaterializerError::Corrupt(
            "Flat publication plan has the wrong role or chunk policy",
        ));
    }
    write_planned_segment(root, plan, |writer| {
        let stats = FlatSegmentWriter::write(writer, records, tombstones)?;
        Ok(CompletedSegmentWrite {
            plaintext_bytes: stats.plaintext_bytes,
        })
    })
}

pub(super) fn plan_event_index_segment(
    materialization_id: &str,
    publication_generation: u64,
    ordinal: u32,
) -> Result<PlannedSegmentWrite, SegmentMaterializerError> {
    plan_segment(
        materialization_id,
        publication_generation,
        crate::graph::segment::EVENT_STATE_INDEX_ROLE,
        SEGMENT_CHUNK_BYTES,
        ordinal,
    )
}

pub(super) fn write_planned_event_index_segment(
    root: &Path,

    plan: PlannedSegmentWrite,
    sources: Vec<EventIndexSource>,
    records: Vec<CompactIndexedCoreEventState>,
    tombstones: Vec<IndexedCoreEventTombstone>,
    lineage: EventLineageTables,
) -> Result<CompletedSegmentPublication, SegmentMaterializerError> {
    if plan.reference.role != crate::graph::segment::EVENT_STATE_INDEX_ROLE
        || plan.chunk_bytes != SEGMENT_CHUNK_BYTES
    {
        return Err(SegmentMaterializerError::Corrupt(
            "event-index publication plan has the wrong role or chunk policy",
        ));
    }
    write_planned_segment(root, plan, |writer| {
        let stats = EventIndexWriter::write_compact(writer, sources, records, tombstones, lineage)?;
        Ok(CompletedSegmentWrite {
            plaintext_bytes: stats.plaintext_bytes,
        })
    })
}

#[cfg(test)]
pub(crate) fn write_event_index_segment_for_test(
    root: &Path,

    publication_generation: u64,
    ordinal: u32,
    sources: Vec<EventIndexSource>,
    records: Vec<IndexedCoreEventState>,
) -> Result<SegmentRef, SegmentMaterializerError> {
    let plan = plan_event_index_segment(&"0".repeat(64), publication_generation, ordinal)?;
    Ok(write_planned_segment(root, plan, |writer| {
        let stats = EventIndexWriter::write(writer, sources, records, Vec::new())?;
        Ok(CompletedSegmentWrite {
            plaintext_bytes: stats.plaintext_bytes,
        })
    })?
    .reference)
}

pub(super) fn read_json_segment<T: DeserializeOwned>(
    root: &Path,

    reference: &SegmentRef,
) -> Result<T, SegmentMaterializerError> {
    let (mut segment, pinned) = open_segment(root, reference)?;
    if segment.plaintext_len() > MAX_METADATA_SEGMENT_BYTES as u64 {
        return Err(SegmentMaterializerError::Bounds);
    }
    let bytes = segment.read_all()?;
    pinned.verify_identity()?;
    serde_json::from_slice(bytes.as_slice()).map_err(|_| SegmentMaterializerError::Encoding)
}

pub(super) fn open_event_index(
    root: &Path,

    reference: &SegmentRef,
) -> Result<EventIndexReader, SegmentMaterializerError> {
    // Event indexes can hold millions of rows. Opening checks only the
    // segment header and bounded source dictionary; fixed rows are decrypted
    // lazily by lookup/page and checked by their containing chunks.
    let (segment, pinned) = open_segment_pinned(root, reference)?;
    let reader = EventIndexReader::open(segment)?;
    pinned.verify_identity()?;
    Ok(reader)
}

pub(crate) fn open_segment_pinned(
    root: &Path,

    reference: &SegmentRef,
) -> Result<(SegmentFile, super::super::locking::VerifiedFile), SegmentMaterializerError> {
    let path = root.join(&reference.file_name);
    let pinned = open_private_file(&path, false)?;
    let container_length = pinned.len()?;
    let generation = decode_generation(&reference.generation_id)?;
    let segment = SegmentFile::open_file_region(
        pinned
            .file()
            .try_clone()
            .map_err(|source| io_error(&path, source))?,
        &path,
        0,
        container_length,
        generation,
        reference.role,
    )?;
    if segment.plaintext_len() != reference.plaintext_bytes {
        return Err(SegmentMaterializerError::Corrupt(
            "segment length does not match manifest",
        ));
    }
    Ok((segment, pinned))
}

fn plan_segment(
    materialization_id: &str,
    publication_generation: u64,
    role: u32,
    chunk_bytes: u32,
    ordinal: u32,
) -> Result<PlannedSegmentWrite, SegmentMaterializerError> {
    let generation =
        publication_segment_generation(materialization_id, publication_generation, ordinal, role)?;
    let generation_id = hex::encode(generation);
    let file_name = segment_file_name(&generation_id, role);
    Ok(PlannedSegmentWrite {
        reference: SegmentRef {
            ordinal,
            publication_generation,
            generation_id,
            role,
            file_name,
            plaintext_bytes: 0,
            file_sha256: String::new(),
        },
        generation,
        chunk_bytes,
    })
}

fn publication_segment_generation(
    materialization_id: &str,
    publication_generation: u64,
    ordinal: u32,
    role: u32,
) -> Result<[u8; 32], SegmentMaterializerError> {
    if !materialization_id
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || materialization_id.len() != 64
    {
        return Err(SegmentMaterializerError::Corrupt(
            "publication materialization identity is invalid",
        ));
    }
    let mut digest = Sha256::new();
    digest.update(b"ctx-attribution-publication-segment-v1\0");
    digest.update(materialization_id.as_bytes());
    digest.update(publication_generation.to_be_bytes());
    let owner = digest.finalize();
    let mut generation = [0_u8; 32];
    generation[..24].copy_from_slice(&owner[..24]);
    generation[24..28].copy_from_slice(&ordinal.to_be_bytes());
    generation[28..32].copy_from_slice(&role.to_be_bytes());
    Ok(generation)
}

fn write_planned_segment(
    root: &Path,

    plan: PlannedSegmentWrite,
    operation: impl FnOnce(SegmentWriter) -> Result<CompletedSegmentWrite, SegmentMaterializerError>,
) -> Result<CompletedSegmentPublication, SegmentMaterializerError> {
    let path = root.join(&plan.reference.file_name);
    let writer = SegmentWriter::create(
        &path,
        plan.generation,
        plan.reference.role,
        plan.chunk_bytes,
    )?;
    let write = operation(writer)?;
    sync_private_root(root)?;
    let mut reference = plan.reference;
    reference.plaintext_bytes = write.plaintext_bytes;
    let file_sha256 = readback_written_segment(root, &reference)?;
    reference.file_sha256 = hex::encode(file_sha256);
    Ok(CompletedSegmentPublication {
        reference,
        checked_readbacks: 1,
    })
}

pub(super) fn remove_publication_attempt_files(
    root: &Path,

    attempt_files: &BTreeSet<String>,
) -> Result<(), SegmentMaterializerError> {
    verify_private_root(root)?;
    let active = crate::graph::segment::SegmentStore::new(root).load_active()?;
    let active_files = active
        .iter()
        .flat_map(|manifest| manifest.segments.iter())
        .map(|reference| reference.file_name.as_str())
        .collect::<BTreeSet<_>>();
    for file_name in attempt_files {
        let mut components = Path::new(file_name).components();
        if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
            return Err(SegmentMaterializerError::Corrupt(
                "publication cleanup target is not a direct file name",
            ));
        }
    }
    let mut removal_error = None;
    for file_name in attempt_files {
        // A manifest rename can succeed before its directory fsync reports an
        // error. In that case the transaction failed from the caller's point
        // of view, but its segments are already active and must be retained.
        if active_files.contains(file_name.as_str()) {
            continue;
        }
        if let Err(error) = remove_private_file_if_exists(&root.join(file_name)) {
            removal_error.get_or_insert(error);
        }
    }
    let sync = sync_private_root(root);
    match (removal_error, sync) {
        (Some(error), _) => Err(error),
        (None, Err(error)) => Err(error),
        (None, Ok(())) => Ok(()),
    }
}

struct CompletedSegmentWrite {
    plaintext_bytes: u64,
}

fn readback_written_segment(
    root: &Path,
    reference: &SegmentRef,
) -> Result<[u8; 32], SegmentMaterializerError> {
    let (mut segment, pinned) = open_segment_pinned(root, reference)?;
    // Publication checks each newly written block once. Query reads stay lazy.
    let mut offset = 0;
    while offset < segment.plaintext_len() {
        let length = (segment.plaintext_len() - offset).min(u64::from(segment.chunk_bytes()));
        segment.read_range(offset, length as usize)?;
        offset += length;
    }
    let mut file = pinned
        .file()
        .try_clone()
        .map_err(|source| io_error(root, source))?;
    file.rewind().map_err(|source| io_error(root, source))?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|source| io_error(root, source))?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    pinned.verify_identity()?;
    Ok(digest.finalize().into())
}

fn open_segment(
    root: &Path,

    reference: &SegmentRef,
) -> Result<(SegmentFile, super::super::locking::VerifiedFile), SegmentMaterializerError> {
    open_segment_pinned(root, reference)
}

fn decode_generation(value: &str) -> Result<[u8; 32], SegmentMaterializerError> {
    hex::decode(value)
        .map_err(|_| SegmentMaterializerError::Corrupt("segment generation is invalid"))?
        .try_into()
        .map_err(|_| SegmentMaterializerError::Corrupt("segment generation is invalid"))
}

fn io_error(path: &Path, source: std::io::Error) -> SegmentMaterializerError {
    SegmentMaterializerError::Io {
        operation: "access segment",
        path: path.to_owned(),
        source,
    }
}

#[cfg(test)]
#[path = "io_tests.rs"]
mod tests;
