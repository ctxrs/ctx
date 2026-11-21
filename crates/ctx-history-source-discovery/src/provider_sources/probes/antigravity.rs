use std::{ffi::OsStr, io::ErrorKind, path::Path};

use ctx_history_source_io::{
    open_provider_source_path, OpenedProviderSourcePath, ProviderSourceDirectory, SourceIoError,
    NON_REGULAR_PROVIDER_SOURCE_REASON, REPARSE_PROVIDER_SOURCE_REASON,
    SYMLINK_PROVIDER_SOURCE_REASON,
};

use super::BoundedProbe;

pub(super) fn has_antigravity_transcript(root: &Path, max_entries: usize) -> BoundedProbe {
    let is_transcript_name = |path: &Path| {
        matches!(
            path.file_name().and_then(|name| name.to_str()),
            Some("transcript_full.jsonl" | "transcript.jsonl")
        )
    };
    let directory = match open_provider_source_path(root) {
        Ok(OpenedProviderSourcePath::File(_)) => {
            return BoundedProbe::from_bool(is_transcript_name(root));
        }
        Ok(OpenedProviderSourcePath::Directory(directory)) => directory,
        Err(error) => return classify_open_root_error(&error),
    };
    match dir_has_transcript(&directory) {
        BoundedProbe::NotFound => {}
        outcome => return outcome,
    }
    match session_dir_has_transcript(&directory) {
        BoundedProbe::NotFound => {}
        outcome => return outcome,
    }
    let entries = match directory.entries(max_entries.saturating_add(1)) {
        Ok(entries) => entries,
        Err(_) => return BoundedProbe::IoError,
    };
    if entries.len() > max_entries {
        return BoundedProbe::BudgetExhausted;
    }
    for name in entries {
        match directory.open_child(&name) {
            Ok(OpenedProviderSourcePath::Directory(session_dir)) => {
                match session_dir_has_transcript(&session_dir) {
                    BoundedProbe::NotFound => {}
                    outcome => return outcome,
                }
            }
            Ok(OpenedProviderSourcePath::File(_)) => {}
            Err(error) if is_ignorable_child_error(&error) => {}
            Err(_) => return BoundedProbe::IoError,
        }
    }
    BoundedProbe::NotFound
}

fn session_dir_has_transcript(directory: &ProviderSourceDirectory) -> BoundedProbe {
    let system_generated = match directory.open_child(OsStr::new(".system_generated")) {
        Ok(OpenedProviderSourcePath::Directory(dir)) => dir,
        Ok(OpenedProviderSourcePath::File(_)) => return BoundedProbe::NotFound,
        Err(error) if is_ignorable_child_error(&error) => return BoundedProbe::NotFound,
        Err(_) => return BoundedProbe::IoError,
    };
    let logs = match system_generated.open_child(OsStr::new("logs")) {
        Ok(OpenedProviderSourcePath::Directory(dir)) => dir,
        Ok(OpenedProviderSourcePath::File(_)) => return BoundedProbe::NotFound,
        Err(error) if is_ignorable_child_error(&error) => return BoundedProbe::NotFound,
        Err(_) => return BoundedProbe::IoError,
    };
    dir_has_transcript(&logs)
}

fn dir_has_transcript(directory: &ProviderSourceDirectory) -> BoundedProbe {
    for leaf in ["transcript_full.jsonl", "transcript.jsonl"] {
        match directory.open_child(OsStr::new(leaf)) {
            Ok(OpenedProviderSourcePath::File(_)) => return BoundedProbe::Found,
            Ok(OpenedProviderSourcePath::Directory(_)) => {}
            Err(error) if is_ignorable_child_error(&error) => {}
            Err(_) => return BoundedProbe::IoError,
        }
    }
    BoundedProbe::NotFound
}

fn is_not_found_error(error: &SourceIoError) -> bool {
    matches!(
        error,
        SourceIoError::Io(io_error) | SourceIoError::SystemIo { source: io_error, .. }
            if io_error.kind() == ErrorKind::NotFound
    )
}

fn is_ignorable_child_error(error: &SourceIoError) -> bool {
    is_not_found_error(error)
        || matches!(
            error,
            SourceIoError::InvalidProviderTranscriptPath { reason, .. }
                if *reason == SYMLINK_PROVIDER_SOURCE_REASON
                    || *reason == REPARSE_PROVIDER_SOURCE_REASON
                    || *reason == NON_REGULAR_PROVIDER_SOURCE_REASON
        )
}

fn classify_open_root_error(error: &SourceIoError) -> BoundedProbe {
    if is_not_found_error(error) {
        BoundedProbe::NotFound
    } else {
        BoundedProbe::IoError
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
