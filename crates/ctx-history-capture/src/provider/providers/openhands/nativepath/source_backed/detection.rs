use std::{io, path::Path};

use crate::{
    common::io::{open_provider_source_path, OpenedProviderSourcePath, ProviderSourceDirectory},
    provider::providers::openhands::source::normalized_openhands_authority_path,
    CaptureError,
};

use super::OpenHandsSourceBackedResultV2;

const OPENHANDS_CURRENT_CLI_MAX_ENTRIES: usize = 16_384;

pub(super) fn detects_current_cli_format(path: &Path) -> OpenHandsSourceBackedResultV2<bool> {
    let path = normalized_openhands_authority_path(path)?;
    let opened = match open_provider_source_path(&path) {
        Ok(opened) => opened,
        Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(false);
        }
        Err(error) => return Err(error.into()),
    };
    if let OpenedProviderSourcePath::File(file) = opened {
        let detected = current_cli_event_file(&path)
            && path
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|name| name == "events");
        file.revalidate()?;
        return Ok(detected);
    }
    let OpenedProviderSourcePath::Directory(directory) = opened else {
        return Err(CaptureError::SystemInvariant(
            "OpenHands CLI format root classification is incomplete",
        )
        .into());
    };
    if path.file_name().is_some_and(|name| name == "events")
        && directory_has_current_cli_event(&directory)?
    {
        return Ok(true);
    }
    let entries = directory.entries(OPENHANDS_CURRENT_CLI_MAX_ENTRIES.saturating_add(1))?;
    for name in &entries {
        if name == "events" {
            if let OpenedProviderSourcePath::Directory(events) = directory.open_child(name)? {
                if directory_has_current_cli_event(&events)? {
                    return Ok(true);
                }
            }
        }
    }
    for name in entries {
        let OpenedProviderSourcePath::Directory(child) = directory.open_child(&name)? else {
            continue;
        };
        match child.open_child(std::ffi::OsStr::new("events")) {
            Ok(OpenedProviderSourcePath::Directory(events))
                if directory_has_current_cli_event(&events)? =>
            {
                return Ok(true);
            }
            Ok(_) => {}
            Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        child.revalidate()?;
    }
    directory.revalidate()?;
    Ok(false)
}

fn directory_has_current_cli_event(
    directory: &ProviderSourceDirectory,
) -> OpenHandsSourceBackedResultV2<bool> {
    let names = directory.entries(OPENHANDS_CURRENT_CLI_MAX_ENTRIES.saturating_add(1))?;
    if names.len() > OPENHANDS_CURRENT_CLI_MAX_ENTRIES {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: directory.relative_path().to_path_buf(),
            reason: "OpenHands CLI history selector exceeds its bounded entry limit",
        }
        .into());
    }
    for name in names {
        if !current_cli_event_file(Path::new(&name)) {
            continue;
        }
        if let OpenedProviderSourcePath::File(file) = directory.open_child(&name)? {
            file.revalidate()?;
            directory.revalidate()?;
            return Ok(true);
        }
    }
    directory.revalidate()?;
    Ok(false)
}

fn current_cli_event_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("event-") && name.ends_with(".json"))
}
