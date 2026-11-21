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
        Err(error) => return classify_open_root_error(&error),
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

fn classify_open_root_error(error: &SourceIoError) -> BoundedProbe {
    match error {
        SourceIoError::Io(io_error) | SourceIoError::SystemIo { source: io_error, .. }
            if io_error.kind() == ErrorKind::NotFound =>
        {
            BoundedProbe::NotFound
        }
        _ => BoundedProbe::IoError,
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;

    #[test]
    fn classify_open_root_error_maps_io_and_system_io_not_found() {
        let plain = SourceIoError::Io(io::Error::new(ErrorKind::NotFound, "missing"));
        assert_eq!(classify_open_root_error(&plain), BoundedProbe::NotFound);

        let system_not_found = SourceIoError::SystemIo {
            operation: "provider source open",
            source: io::Error::new(ErrorKind::NotFound, "missing on windows"),
        };
        assert_eq!(
            classify_open_root_error(&system_not_found),
            BoundedProbe::NotFound
        );

        let system_denied = SourceIoError::SystemIo {
            operation: "provider source open",
            source: io::Error::new(ErrorKind::PermissionDenied, "denied"),
        };
        assert_eq!(
            classify_open_root_error(&system_denied),
            BoundedProbe::IoError
        );
    }
}
