use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
};

use super::{
    load_active_generation_pointer, CloneMode, GenerationSlot, INDEX_GENERATIONS_DIRECTORY,
};

const MANAGED_FILE: &str = ".managed.json";
const META_FILE: &str = "meta.json";

#[derive(Debug, Clone)]
pub(super) struct CloneTopologyProof {
    pub(super) payload_files: u64,
    pub(super) payload_bytes: u64,
    pub(super) shared_payload_files: u64,
    pub(super) shared_payload_bytes: u64,
}

fn generation_path(root: &Path, slot: &GenerationSlot) -> PathBuf {
    root.join(INDEX_GENERATIONS_DIRECTORY)
        .join(slot.directory())
}

fn payload_paths(generation: &Path) -> Result<BTreeSet<PathBuf>, String> {
    let managed = fs::read(generation.join(MANAGED_FILE)).map_err(|error| error.to_string())?;
    let declared: Vec<PathBuf> =
        serde_json::from_slice(&managed).map_err(|error| error.to_string())?;
    let mut payload = BTreeSet::new();
    for relative in declared {
        let mut components = relative.components();
        if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
            return Err(format!(
                "managed payload path is not one relative component: {}",
                relative.display()
            ));
        }
        if relative != Path::new(META_FILE) && !payload.insert(relative.clone()) {
            return Err(format!(
                "managed payload path is duplicated: {}",
                relative.display()
            ));
        }
    }
    if payload.is_empty() {
        return Err("generation has no immutable payload files to qualify".to_owned());
    }
    Ok(payload)
}

fn regular_metadata(path: &Path) -> Result<fs::Metadata, String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(format!(
            "payload path is not a regular file: {}",
            path.display()
        ));
    }
    Ok(metadata)
}

fn topology_paths(root: &Path) -> Result<(PathBuf, PathBuf, BTreeSet<PathBuf>), String> {
    let pointer = load_active_generation_pointer(root)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "qualification root has no active generation".to_owned())?;
    let previous = pointer
        .previous()
        .ok_or_else(|| "qualification publication did not retain its predecessor".to_owned())?;
    let predecessor = generation_path(root, previous);
    let candidate = generation_path(root, pointer.active());
    let predecessor_payload = payload_paths(&predecessor)?;
    let candidate_payload = payload_paths(&candidate)?;
    if predecessor_payload != candidate_payload {
        return Err(format!(
            "candidate payload paths differ from predecessor: predecessor={predecessor_payload:?} candidate={candidate_payload:?}"
        ));
    }
    Ok((predecessor, candidate, predecessor_payload))
}

pub(super) fn first_payload_pair(root: &Path) -> Result<(PathBuf, PathBuf), String> {
    let (predecessor, candidate, payload) = topology_paths(root)?;
    let relative = payload
        .first()
        .ok_or_else(|| "generation has no payload file".to_owned())?;
    Ok((predecessor.join(relative), candidate.join(relative)))
}

pub(super) fn verify_clone_topology(
    root: &Path,
    mode: CloneMode,
) -> Result<CloneTopologyProof, String> {
    let (predecessor, candidate, payload) = topology_paths(root)?;
    let mut predecessor_inodes = BTreeMap::new();
    let mut candidate_inodes = BTreeMap::new();
    let mut payload_bytes = 0_u64;
    let mut shared_payload_files = 0_u64;
    let mut shared_payload_bytes = 0_u64;

    for relative in &payload {
        let predecessor_metadata = regular_metadata(&predecessor.join(relative))?;
        let candidate_metadata = regular_metadata(&candidate.join(relative))?;
        if predecessor_metadata.len() != candidate_metadata.len() {
            return Err(format!(
                "payload length differs for {}: predecessor={} candidate={}",
                relative.display(),
                predecessor_metadata.len(),
                candidate_metadata.len()
            ));
        }
        let predecessor_inode = (predecessor_metadata.dev(), predecessor_metadata.ino());
        let candidate_inode = (candidate_metadata.dev(), candidate_metadata.ino());
        predecessor_inodes.insert(predecessor_inode, relative);
        candidate_inodes.insert(candidate_inode, relative);
        payload_bytes = payload_bytes.saturating_add(predecessor_metadata.len());
        if predecessor_inode == candidate_inode {
            if predecessor_metadata.nlink() < 2 || candidate_metadata.nlink() < 2 {
                return Err(format!(
                    "shared payload inode lacks two links: {}",
                    relative.display()
                ));
            }
            shared_payload_files = shared_payload_files.saturating_add(1);
            shared_payload_bytes = shared_payload_bytes.saturating_add(predecessor_metadata.len());
        } else if mode == CloneMode::HardLink {
            return Err(format!(
                "requested hard-link republish silently copied payload {}",
                relative.display()
            ));
        }
    }

    if mode == CloneMode::CopyFallback {
        if let Some((inode, predecessor_relative)) = predecessor_inodes
            .iter()
            .find(|(inode, _)| candidate_inodes.contains_key(inode))
        {
            let candidate_relative = candidate_inodes[inode];
            return Err(format!(
                "forced-copy republish shares payload inode {inode:?}: predecessor={} candidate={}",
                predecessor_relative.display(),
                candidate_relative.display()
            ));
        }
        if shared_payload_files != 0 {
            return Err("forced-copy republish retained shared payload inodes".to_owned());
        }
    }

    Ok(CloneTopologyProof {
        payload_files: payload.len() as u64,
        payload_bytes,
        shared_payload_files,
        shared_payload_bytes,
    })
}
