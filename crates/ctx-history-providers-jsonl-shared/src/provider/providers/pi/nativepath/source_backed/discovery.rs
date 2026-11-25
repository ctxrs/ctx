//! Pi file discovery, header admission, and rejected-member diagnostics.

use super::*;
use ctx_history_capture_runtime::{SourceBackedRouteError, SourceBackedRouteErrorKind};
use ctx_history_jsonl::{
    observe_opened_file_allow_append, probe_records_until_with_mutable_first_record,
    JsonlFamilyError, JsonlFamilyPendingLeaf, JsonlFamilyRejectedLeaf, JsonlProbeOutcome,
};
use ctx_history_source_io::visit_bounded_tree_files_isolating_selected;
use std::{
    collections::BTreeSet,
    fs,
    io::{self, Read},
};

struct DiscoveredPiSource {
    path: PathBuf,
    relative_path: PathBuf,
    cached: CachedBinding,
}

pub(super) fn discover<R>(
    adapter: &PiJsonlAdapter<R>,
    root: &Path,
) -> Result<JsonlFamilyInventory> {
    match fs::symlink_metadata(root) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return JsonlFamilyInventory::missing(CaptureProvider::Pi, root);
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
            let path = candidate.path();
            path.extension().is_some_and(|ext| ext == "jsonl") && !is_omp_edit_journal(root, path)
        },
        &mut |file| {
            paths.insert(fs::canonicalize(file.path())?);
            Ok(())
        },
        &mut |path, error: CaptureError| {
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
    let dispositions = repeated_identity_dispositions(&authority, &discovered);
    let mut leaves = Vec::with_capacity(discovered.len());
    for (discovered, disposition) in discovered.into_iter().zip(dispositions) {
        let DiscoveredPiSource {
            path,
            relative_path,
            cached,
        } = discovered;
        match disposition {
            IdentityDisposition::Admit => {}
            IdentityDisposition::CoveredCopy => {
                next.insert(path, cached);
                continue;
            }
            IdentityDisposition::Conflicting { diagnostic_owner } => {
                let source = cached.source.clone();
                let mut leaf = JsonlFamilyRejectedLeaf::bind_observed(
                    path.clone(),
                    relative_path,
                    cached.observation.clone(),
                    cached.binding_key.clone(),
                    0,
                )
                .with_quarantined_source(source.clone());
                if diagnostic_owner {
                    leaf = leaf.with_logical_source_failure(
                        source,
                        format!(
                            "Pi transcript {} repeats a native session identity claimed by another transcript with different content",
                            path.display()
                        ),
                    );
                }
                rejected.push(leaf);
                next.insert(path, cached);
                continue;
            }
            IdentityDisposition::Unreadable(error) => {
                rejected.push(reject_unreadable_file(
                    &authority,
                    &path,
                    error,
                    adapter.source_anchor_scope,
                )?);
                continue;
            }
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
        next.insert(path, cached);
    }
    *adapter
        .bindings
        .lock()
        .map_err(|_| contract("Pi binding catalog lock was poisoned"))? = next;
    JsonlFamilyInventory::present_multi_with_dispositions(
        CaptureProvider::Pi,
        root,
        vec![authority],
        leaves,
        rejected,
        pending,
    )
}

/// How discovery treats one physical member of a Pi native session identity.
#[derive(Debug)]
enum IdentityDisposition {
    /// The only member of its identity, or the most complete one.
    Admit,
    /// An exact record prefix of the admitted member. OMP copies a session's
    /// subagent transcripts into sessions continued or forked from it, keeping
    /// the original header, so these copies repeat the identity by design.
    /// Every record is already in the admitted member, so no second leaf is
    /// bound and nothing is lost.
    CoveredCopy,
    /// Members of one identity whose records diverge. None is admitted; one
    /// deterministic member carries the diagnostic, as Claude discovery does.
    Conflicting { diagnostic_owner: bool },
    /// The member could not be read while comparing it with its peers.
    Unreadable(CaptureError),
}

/// Resolves repeated native session identities member by member, so one
/// repeated header no longer fails the whole Pi inventory.
fn repeated_identity_dispositions(
    authority: &ProviderSourceRoot,
    discovered: &[DiscoveredPiSource],
) -> Vec<IdentityDisposition> {
    let mut members = HashMap::<[u8; 32], Vec<usize>>::new();
    for (index, member) in discovered.iter().enumerate() {
        members
            .entry(member.cached.source.exact_descriptor_digest())
            .or_default()
            .push(index);
    }
    let mut dispositions = discovered
        .iter()
        .map(|_| IdentityDisposition::Admit)
        .collect::<Vec<_>>();
    for indexes in members.values().filter(|indexes| indexes.len() > 1) {
        // Discovery visits paths in sorted order, so the first longest member
        // is a deterministic choice among equally long copies.
        let admitted = indexes.iter().copied().fold(indexes[0], |best, index| {
            if discovered[index].cached.observation.length()
                > discovered[best].cached.observation.length()
            {
                index
            } else {
                best
            }
        });
        let mut conflicting = false;
        for &index in indexes {
            if index == admitted {
                continue;
            }
            let member = &discovered[index];
            let covered = if member.cached.observation == discovered[admitted].cached.observation {
                Ok(true)
            } else {
                is_record_prefix_of(authority, member, &discovered[admitted])
            };
            match covered {
                Ok(true) => dispositions[index] = IdentityDisposition::CoveredCopy,
                Ok(false) => conflicting = true,
                Err(error) => dispositions[index] = IdentityDisposition::Unreadable(error),
            }
        }
        if conflicting {
            for (position, &index) in indexes.iter().enumerate() {
                if !matches!(dispositions[index], IdentityDisposition::Unreadable(_)) {
                    dispositions[index] = IdentityDisposition::Conflicting {
                        diagnostic_owner: position == 0,
                    };
                }
            }
        }
    }
    dispositions
}

/// Whether `copy` holds exactly a leading prefix of `admitted`'s bytes, at the
/// lengths observed during discovery. A leading OMP title slot is metadata
/// OMP rewrites in place, so it is skipped when both members start with one.
fn is_record_prefix_of(
    authority: &ProviderSourceRoot,
    copy: &DiscoveredPiSource,
    admitted: &DiscoveredPiSource,
) -> Result<bool> {
    const CHUNK: usize = 64 * 1024;
    let copy_len = copy.cached.observation.length();
    if copy_len > admitted.cached.observation.length() {
        return Ok(false);
    }
    let mut copy_reader = authority
        .open_file(&copy.relative_path)?
        .reopen_same_object()?
        .take(copy_len);
    let mut admitted_reader = authority
        .open_file(&admitted.relative_path)?
        .reopen_same_object()?
        .take(copy_len);
    let mut remaining = copy_len;
    let slot = JsonlMutableFirstRecord::WIRE_BYTES;
    if copy_len >= slot as u64 {
        let mut copy_slot = [0_u8; JsonlMutableFirstRecord::WIRE_BYTES];
        let mut admitted_slot = [0_u8; JsonlMutableFirstRecord::WIRE_BYTES];
        read_observed(&mut copy_reader, &mut copy_slot)?;
        read_observed(&mut admitted_reader, &mut admitted_slot)?;
        if !(is_omp_title_slot_wire(&copy_slot) && is_omp_title_slot_wire(&admitted_slot))
            && copy_slot != admitted_slot
        {
            return Ok(false);
        }
        remaining -= slot as u64;
    }
    let mut copy_chunk = vec![0_u8; CHUNK];
    let mut admitted_chunk = vec![0_u8; CHUNK];
    while remaining > 0 {
        let take = usize::try_from(remaining.min(CHUNK as u64))
            .map_err(|_| CaptureError::SystemInvariant("Pi comparison chunk does not fit usize"))?;
        read_observed(&mut copy_reader, &mut copy_chunk[..take])?;
        read_observed(&mut admitted_reader, &mut admitted_chunk[..take])?;
        if copy_chunk[..take] != admitted_chunk[..take] {
            return Ok(false);
        }
        remaining -= take as u64;
    }
    Ok(true)
}

/// Reads bytes that discovery already observed; a short read means the file
/// shrank after its observation.
fn read_observed(reader: &mut impl Read, bytes: &mut [u8]) -> Result<()> {
    reader.read_exact(bytes).map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            CaptureError::SourceChangedDuringCapture
        } else {
            CaptureError::Io(error)
        }
    })
}

fn is_omp_title_slot_wire(bytes: &[u8; JsonlMutableFirstRecord::WIRE_BYTES]) -> bool {
    let (record, terminator) = bytes.split_at(JsonlMutableFirstRecord::WIRE_BYTES - 1);
    terminator == b"\n"
        && !record.contains(&b'\n')
        && !record.contains(&b'\r')
        && JsonlMutableFirstRecord::OmpTitleSlotV1.validates_record(record)
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

pub(super) fn observe_terminal_membership(
    root: &Path,
    opening: &JsonlFamilyInventory,
) -> Result<JsonlFamilyMembershipObservation<CaptureError>> {
    if opening.root_missing() {
        return JsonlFamilyMembershipObservation::observe(root, opening);
    }
    let mut selected = BTreeSet::new();
    let mut isolated = Vec::new();
    visit_bounded_tree_files_isolating_selected(
        root,
        &mut |candidate| {
            let path = candidate.path();
            path.extension().is_some_and(|ext| ext == "jsonl") && !is_omp_edit_journal(root, path)
        },
        &mut |file| {
            selected.insert(fs::canonicalize(file.path())?);
            Ok(())
        },
        &mut |path, error: CaptureError| {
            if !(error.is_ignorable_membership_entry()
                || error.is_source_unavailable()
                || matches!(&error, CaptureError::Io(source) if source.kind() == io::ErrorKind::PermissionDenied))
            {
                return Err(error);
            }
            if fs::symlink_metadata(path)?.is_dir() {
                return Err(error);
            }
            isolated.push(std::path::absolute(path)?);
            Ok(())
        },
    )?;
    selected.extend(isolated);
    JsonlFamilyMembershipObservation::observe_selected_paths(opening, selected)
}

fn is_omp_edit_journal(root: &Path, path: &Path) -> bool {
    path != root
        && path.file_name().and_then(|name| name.to_str()) == Some("changes.jsonl")
        && path
            .parent()
            .is_some_and(|parent| parent.strip_prefix(root).is_ok())
}

#[cfg(test)]
mod tests;
