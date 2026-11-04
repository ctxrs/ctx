//! Pi file discovery, header admission, and rejected-member diagnostics.

use super::*;
use ctx_history_capture_runtime::{SourceBackedRouteError, SourceBackedRouteErrorKind};
use ctx_history_jsonl::{
    observe_opened_file_allow_append, probe_records_until_with_mutable_first_record,
    JsonlFamilyError, JsonlFamilyPendingLeaf, JsonlFamilyRejectedLeaf, JsonlProbeOutcome,
};
use ctx_history_source_io::visit_bounded_tree_files_isolating_selected;
use std::{collections::BTreeSet, fs, io};

struct DiscoveredPiSource {
    path: PathBuf,
    relative_path: PathBuf,
    cached: CachedBinding,
}

pub(super) fn discover<R: JsonlProviderRuntime>(
    adapter: &PiJsonlAdapter<R>,
    root: &Path,
) -> Result<JsonlFamilyInventory> {
    match fs::symlink_metadata(root) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return JsonlFamilyInventory::missing(adapter.provider(), root);
        }
        Err(error) => return Err(error.into()),
    }
    let canonical_root = fs::canonicalize(root)?;
    let authority_path = if fs::symlink_metadata(root)?.is_file() {
        canonical_root
            .parent()
            .ok_or(CaptureError::InvalidProviderTranscriptPath {
                path: canonical_root.clone(),
                reason: "selected Pi transcript has no authority directory",
            })?
            .to_path_buf()
    } else {
        canonical_root
    };
    let authority = Arc::new(ProviderSourceRoot::open(&authority_path)?);
    let mut paths = BTreeSet::new();
    let mut rejected = Vec::new();
    visit_bounded_tree_files_isolating_selected(
        root,
        &mut |candidate| {
            candidate
                .path()
                .extension()
                .is_some_and(|ext| ext == "jsonl")
        },
        &mut |file| {
            paths.insert(fs::canonicalize(file.path())?);
            Ok(())
        },
        &mut |path, error| {
            rejected.push(reject_unreadable_file(
                &authority,
                path,
                error,
                adapter.source_anchor_scope,
            )?);
            Ok(())
        },
    )?;
    // A visitor can succeed before the walker's final file revalidation fails.
    for leaf in &rejected {
        paths.remove(leaf.source_path());
    }

    let previous = adapter
        .bindings
        .lock()
        .map_err(|_| contract("Pi binding catalog lock was poisoned"))?
        .clone();
    let mut next = HashMap::with_capacity(paths.len());
    let mut discovered = Vec::with_capacity(paths.len());
    let mut pending = Vec::new();
    for path in paths {
        let relative_path = relative_to_authority(&authority, &path)?;
        let opened = authority.open_file(&relative_path).and_then(|file| {
            // Cache full metadata, but fence only object identity here: the
            // probe and terminal proof authenticate the bytes, including the
            // validated mutable OMP title slot.
            let observation = observe_opened_file_allow_append(&path, &file)?;
            Ok((Arc::new(file), observation))
        });
        let (opened, observation) = match opened {
            Ok(opened) => opened,
            Err(error) => {
                rejected.push(reject_unreadable_file(
                    &authority,
                    &path,
                    error,
                    adapter.source_anchor_scope,
                )?);
                continue;
            }
        };
        let cached = match previous.get(&path) {
            Some(cached) if cached.observation == observation => cached.clone(),
            _ => {
                let mut leading_rejection_details = Vec::new();
                let parsed = probe_records_until_with_mutable_first_record(
                    &path,
                    &opened,
                    MAX_HEADER_PROBE_RECORDS,
                    Some(JsonlMutableFirstRecord::OmpTitleSlotV1),
                    |record| {
                        let Some(mut binding) =
                            parse_header_binding(record, adapter.source_anchor_scope)?
                        else {
                            if !is_omp_title_slot(record) {
                                leading_rejection_details.push((
                                    record.evidence().physical_ordinal().saturating_add(1),
                                    leading_rejection_detail(record),
                                ));
                            }
                            return Ok(None);
                        };
                        binding.leading_rejected_records =
                            u64::try_from(leading_rejection_details.len()).map_err(|_| {
                                CaptureError::SystemInvariant(
                                    "Pi identity rejection count does not fit u64",
                                )
                            })?;
                        // Admission is part of this file's payload validation.
                        // Cache only headers whose source key and binding fit
                        // the existing Core bounds.
                        let source = source_key_scoped(
                            &binding.native_session_id,
                            adapter.source_anchor_scope,
                        )?;
                        let binding_key =
                            TypedKey::bytes(serde_json::to_vec(&binding)?).map_err(contract)?;
                        Ok(Some((binding, source, binding_key)))
                    },
                );
                let parsed = match parsed {
                    Err(CaptureError::InvalidPayload(detail)) => {
                        rejected.push(rejected_file(
                            path,
                            relative_path,
                            Some(observation),
                            SourceBackedRouteError::new(
                                SourceBackedRouteErrorKind::InvalidSource,
                                detail,
                            ),
                            adapter.source_anchor_scope,
                        )?);
                        continue;
                    }
                    Err(error) => {
                        rejected.push(reject_unreadable_file(
                            &authority,
                            &path,
                            error,
                            adapter.source_anchor_scope,
                        )?);
                        continue;
                    }
                    Ok(parsed) => parsed,
                };
                let (binding, source, binding_key, identity_probe) = match parsed {
                    JsonlProbeOutcome::Matched((binding, source, binding_key), probe) => {
                        (binding, source, binding_key, probe)
                    }
                    JsonlProbeOutcome::Incomplete => {
                        // This can follow complete OMP title records. Keep
                        // the shared pending/retention lifecycle, rather
                        // than diagnosing a still-written header as invalid.
                        pending.push(JsonlFamilyPendingLeaf::bind_observed(
                            path,
                            relative_path,
                            observation,
                            TypedKey::utf8("pi-incomplete-session-header").map_err(contract)?,
                            None,
                        ));
                        continue;
                    }
                    JsonlProbeOutcome::Missing => {
                        rejected.push(rejected_file(
                            path,
                            relative_path,
                            Some(observation),
                            SourceBackedRouteError::new(SourceBackedRouteErrorKind::InvalidSource, "Pi source has no valid session header within the bounded identity probe"),
                            adapter.source_anchor_scope,
                        )?);
                        continue;
                    }
                };
                let leading_rejections =
                    pi_leading_rejection_drafts(&source, &path, leading_rejection_details)?;
                CachedBinding {
                    observation,
                    source,
                    binding,
                    binding_key,
                    identity_probe,
                    leading_rejections,
                }
            }
        };
        discovered.push(DiscoveredPiSource {
            path,
            relative_path,
            cached,
        });
    }
    let mut sources = HashMap::<[u8; 32], JsonlFileObservation>::new();
    let mut leaves = Vec::with_capacity(discovered.len());
    for discovered in discovered {
        let DiscoveredPiSource {
            path,
            relative_path,
            cached,
        } = discovered;
        let source_digest = cached.source.exact_descriptor_digest();
        if let Some(selected) = sources.get(&source_digest) {
            if selected == &cached.observation {
                next.insert(path, cached);
                continue;
            }
            return Err(CaptureError::InvalidPayload(
                "Pi inventory repeats a native session identity".to_owned(),
            ));
        }
        let leaf = JsonlFamilyLeaf::observe_after_identity_probe(
            cached.source.clone(),
            path.clone(),
            Arc::clone(&authority),
            relative_path,
            cached.binding_key.clone(),
            cached.identity_probe.clone(),
            cached.binding.leading_rejected_records,
        );
        match leaf {
            Ok(leaf) => leaves.push(leaf),
            Err(error) => {
                rejected.push(reject_unreadable_file(
                    &authority,
                    &path,
                    error,
                    adapter.source_anchor_scope,
                )?);
                continue;
            }
        }
        sources.insert(source_digest, cached.observation.clone());
        next.insert(path, cached);
    }
    *adapter
        .bindings
        .lock()
        .map_err(|_| contract("Pi binding catalog lock was poisoned"))? = next;
    JsonlFamilyInventory::present_multi_with_dispositions(
        adapter.provider(),
        root,
        vec![authority],
        leaves,
        rejected,
        pending,
    )
}

fn reject_unreadable_file(
    authority: &ProviderSourceRoot,
    path: &Path,
    error: CaptureError,
    scope: SourceAnchorScope,
) -> Result<JsonlFamilyRejectedLeaf> {
    let kind = if error.is_ignorable_membership_entry() {
        SourceBackedRouteErrorKind::InvalidSource
    } else if error.is_source_unavailable()
        || matches!(&error, CaptureError::Io(source) if source.kind() == io::ErrorKind::PermissionDenied)
    {
        SourceBackedRouteErrorKind::Unavailable
    } else {
        return Err(error);
    };
    // A selected name can still be a directory. Refusing its enumeration
    // cannot become a leaf rejection: it would conceal an entire subtree.
    if fs::symlink_metadata(path)?.is_dir() {
        return Err(error);
    }
    // Do not canonicalize a refused symlink or traverse its target.
    let path = std::path::absolute(path)?;
    let relative_path = relative_to_authority(authority, &path)?;
    rejected_file(
        path,
        relative_path,
        None,
        SourceBackedRouteError::from_error(kind, &error),
        scope,
    )
}

fn rejected_file(
    path: PathBuf,
    relative_path: PathBuf,
    observation: Option<JsonlFileObservation>,
    failure: SourceBackedRouteError,
    scope: SourceAnchorScope,
) -> Result<JsonlFamilyRejectedLeaf> {
    let source = SourceKey::derive_provider_native_scoped(
        CaptureProvider::Pi.as_str(),
        PI_SOURCE_FORMAT,
        SOURCE_SCHEMA_VARIANT,
        1,
        "pi.invalid-file",
        TypedKey::bytes(path.as_os_str().as_encoded_bytes().to_vec()).map_err(contract)?,
        scope,
    )
    .map_err(contract)?;
    let proof = TypedKey::utf8(failure.detail.clone()).map_err(contract)?;
    let leaf = match observation {
        Some(observation) => {
            JsonlFamilyRejectedLeaf::bind_observed(path, relative_path, observation, proof, 1)
        }
        None => JsonlFamilyRejectedLeaf::bind_unobserved(path, relative_path, proof, 0),
    };
    Ok(leaf.with_logical_source_error(source, failure))
}

#[cfg(test)]
mod tests;
