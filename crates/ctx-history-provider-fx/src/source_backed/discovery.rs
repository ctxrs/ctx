use std::{path::Path, sync::Arc};

use ctx_history_core::{SourceKey, TypedKey};
use ctx_history_jsonl::JsonlFamilyError;
use ctx_history_provider_runtime::{
    source_io::{OpenedProviderSourceFile, ProviderSourceRoot},
    CaptureError, JsonlFamilyRejectedLeaf,
};

pub(super) fn pending_exists(root: &ProviderSourceRoot, path: &Path) -> Result<bool, CaptureError> {
    match root.open_path(path) {
        Ok(_) => Ok(true),
        Err(error) if error.is_not_found() => Ok(false),
        Err(error) => Err(error),
    }
}

pub(super) fn has_markerless_v3_evidence(
    root: &ProviderSourceRoot,
    session: &Path,
    metadata_entries_remaining: &mut usize,
) -> Result<bool, CaptureError> {
    match root.open_path(&session.join("events.jsonl")) {
        Ok(_) => return Ok(true),
        Err(error) if error.is_not_found() => {}
        Err(error) => return Err(error),
    }
    let entries = root
        .open_directory(session)?
        .entries(*metadata_entries_remaining)?;
    *metadata_entries_remaining = metadata_entries_remaining
        .checked_sub(entries.len())
        .ok_or(CaptureError::SystemInvariant(
            "fx markerless metadata budget accounting underflowed",
        ))?;
    for entry in entries {
        let name = entry.to_string_lossy();
        if name.starts_with("commit.") && name.ends_with(".json") {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn safe_directory_name(name: &Path) -> Option<&str> {
    let name = name.to_str()?;
    (name != "."
        && name != ".."
        && !name.is_empty()
        && name.len() <= 255
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')))
    .then_some(name)
}

pub(super) fn read_limited(
    root: &ProviderSourceRoot,
    path: &Path,
    maximum: usize,
) -> Result<(OpenedProviderSourceFile, Vec<u8>), CaptureError> {
    let opened = root.open_file(path)?;
    let bytes = opened.read_all_bounded(maximum)?;
    Ok((opened, bytes))
}

pub(super) fn reject_path(
    authority: &Arc<ProviderSourceRoot>,
    relative: &Path,
    rejected: &mut Vec<JsonlFamilyRejectedLeaf>,
    source: Option<SourceKey>,
    detail: String,
) -> Result<(), CaptureError> {
    let source_path = authority.named_path().join(relative);
    let proof =
        TypedKey::utf8(detail).map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let leaf = match authority.open_file(relative) {
        Ok(opened) => JsonlFamilyRejectedLeaf::bind_observed(
            source_path.clone(),
            relative.to_path_buf(),
            match ctx_history_provider_runtime::observe_opened_file(&source_path, &opened) {
                Ok(observation) => observation,
                Err(_) => {
                    let leaf = JsonlFamilyRejectedLeaf::bind_unobserved(
                        source_path,
                        relative.to_path_buf(),
                        proof,
                        1,
                    );
                    if let Some(source) = source {
                        rejected.push(leaf.with_quarantined_source(source));
                    } else {
                        rejected.push(leaf);
                    }
                    return Ok(());
                }
            },
            proof,
            1,
        ),
        Err(_) => {
            JsonlFamilyRejectedLeaf::bind_unobserved(source_path, relative.to_path_buf(), proof, 1)
        }
    };
    if let Some(source) = source {
        rejected.push(leaf.with_quarantined_source(source));
    } else {
        rejected.push(leaf);
    }
    Ok(())
}
