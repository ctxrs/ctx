use std::{io::ErrorKind, path::Path};

use ctx_history_source_io::{open_provider_source_path, OpenedProviderSourcePath, SourceIoError};

use super::{path_metadata_probe, BoundedProbe, PathProbe};

pub(super) fn has_antigravity_transcript(root: &Path, max_entries: usize) -> BoundedProbe {
    let is_transcript_name = |path: &Path| {
        matches!(
            path.file_name().and_then(|name| name.to_str()),
            Some("transcript_full.jsonl" | "transcript.jsonl")
        )
    };
    let dir_has_transcript = |dir: &Path| {
        for leaf in ["transcript_full.jsonl", "transcript.jsonl"] {
            match path_metadata_probe(&dir.join(leaf)) {
                PathProbe::File => return BoundedProbe::Found,
                PathProbe::IoError => return BoundedProbe::IoError,
                PathProbe::Dir | PathProbe::Other | PathProbe::Missing => {}
            }
        }
        BoundedProbe::NotFound
    };
    match path_metadata_probe(root) {
        PathProbe::File => return BoundedProbe::from_bool(is_transcript_name(root)),
        PathProbe::Dir => {}
        PathProbe::Missing | PathProbe::Other => return BoundedProbe::NotFound,
        PathProbe::IoError => return BoundedProbe::IoError,
    }
    match dir_has_transcript(root) {
        BoundedProbe::NotFound => {}
        outcome => return outcome,
    }
    match dir_has_transcript(&root.join(".system_generated").join("logs")) {
        BoundedProbe::NotFound => {}
        outcome => return outcome,
    }
    let directory = match open_provider_source_path(root) {
        Ok(OpenedProviderSourcePath::Directory(directory)) => directory,
        Ok(OpenedProviderSourcePath::File(_)) => return BoundedProbe::NotFound,
        Err(SourceIoError::Io(error)) if error.kind() == ErrorKind::NotFound => {
            return BoundedProbe::NotFound;
        }
        Err(_) => return BoundedProbe::IoError,
    };
    let entries = match directory.entries(max_entries.saturating_add(1)) {
        Ok(entries) => entries,
        Err(_) => return BoundedProbe::IoError,
    };
    if entries.len() > max_entries {
        return BoundedProbe::BudgetExhausted;
    }
    for name in entries {
        let logs = root.join(name).join(".system_generated").join("logs");
        match dir_has_transcript(&logs) {
            BoundedProbe::NotFound => {}
            outcome => return outcome,
        }
    }
    BoundedProbe::NotFound
}
