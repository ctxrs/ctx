use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use crate::graph::segment::SegmentManifest;

use super::super::SegmentMaterializerError;

pub(crate) fn remove_unreferenced_segments(
    root: &Path,
    active: &SegmentManifest,
    previous: Option<&SegmentManifest>,
) -> Result<(), SegmentMaterializerError> {
    active.validate()?;
    super::super::locking::verify_private_root(root)?;
    let mut retained = active
        .segments
        .iter()
        .map(|reference| reference.file_name.as_str())
        .collect::<BTreeSet<_>>();
    retained.extend(
        active
            .predecessor_segments
            .iter()
            .map(|reference| reference.file_name.as_str()),
    );
    match previous {
        Some(previous) => {
            previous.validate()?;
            if active.prior_generation_id.as_deref() != Some(previous.generation_id.as_str())
                || previous.graph_generation.checked_add(1) != Some(active.graph_generation)
                || active.predecessor_segments != previous.segments
            {
                return Err(SegmentMaterializerError::Corrupt(
                    "retained manifest is not the active manifest predecessor",
                ));
            }
        }
        None if active.prior_generation_id.is_some() && active.predecessor_segments.is_empty() => {
            return Err(SegmentMaterializerError::Corrupt(
                "active manifest has no checked predecessor reachability",
            ));
        }
        None => {}
    }
    let entries = fs::read_dir(root).map_err(|source| SegmentMaterializerError::Io {
        operation: "scan segment root",
        path: root.to_owned(),
        source,
    })?;
    let mut stale = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| SegmentMaterializerError::Io {
            operation: "read segment root entry",
            path: root.to_owned(),
            source,
        })?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if is_canonical_segment_name(name) && !retained.contains(name) {
            stale.push(root.join(name));
        }
    }
    stale.sort();
    // Validate the complete deletion set before the first unlink so one bad
    // file cannot cause partial cleanup.
    for path in &stale {
        let file = super::super::locking::open_private_file(path, false)?;
        file.verify_identity()?;
    }
    for path in stale {
        super::super::locking::remove_private_file_if_exists(&path)?;
    }
    super::super::locking::sync_private_root(root)
}

/// Removes canonical segment files not reachable from the checked active
/// manifest. Inactive publication files are disposable work, never authority.
pub(crate) fn cleanup_candidate_orphans(
    root: &Path,
    active: Option<&SegmentManifest>,
) -> Result<(), SegmentMaterializerError> {
    super::super::locking::verify_private_root(root)?;
    let mut retained = BTreeSet::new();
    if let Some(active) = active {
        active.validate()?;
        if active.prior_generation_id.is_some() && active.predecessor_segments.is_empty() {
            return Err(SegmentMaterializerError::Corrupt(
                "active manifest has no checked predecessor reachability",
            ));
        }
        retained.extend(
            active
                .segments
                .iter()
                .chain(&active.predecessor_segments)
                .map(|reference| reference.file_name.as_str()),
        );
    }
    let entries = fs::read_dir(root).map_err(|source| SegmentMaterializerError::Io {
        operation: "scan candidate segment root",
        path: root.to_owned(),
        source,
    })?;
    let mut stale = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| SegmentMaterializerError::Io {
            operation: "read candidate segment root entry",
            path: root.to_owned(),
            source,
        })?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !is_canonical_segment_name(name) || retained.contains(name) {
            continue;
        }
        stale.push(entry.path());
    }
    stale.sort();
    for path in &stale {
        let file = super::super::locking::open_private_file(path, false)?;
        file.verify_identity()?;
    }
    for path in stale {
        super::super::locking::remove_private_file_if_exists(&path)?;
    }
    super::super::locking::sync_private_root(root)
}

pub(crate) fn is_canonical_segment_name(name: &str) -> bool {
    crate::graph::segment::parse_segment_file_name(name).is_some()
}
