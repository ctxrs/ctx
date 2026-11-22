//! Reclaim archive-owned copies only after verified replacement and retention checks.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::BufReader,
    path::Path,
};

use ctx_history_index_generation::{
    load_active_generation_pointer, load_generation_retention_lease, sync_directory,
    try_generation_directory_reclaim_authority, INDEX_GENERATIONS_DIRECTORY,
};
use ctx_history_platform::platform_security::ensure_private_directory;
use serde::Serialize;

use crate::{
    checked_add, hex, invalid, io, is_archive_root, is_digest, prepare_restore_root,
    retained::OwnedMember, ArchiveIdentity, CoreRecord, Result, SessionMember, VerifiedIndex,
};

const PRUNE_DIRECTORY: &str = ".prune";

#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub struct PruneReceipt {
    pub eligible_snapshots: u64,
    pub eligible_bytes: u64,
    pub reclaimed_snapshots: u64,
    pub reclaimed_bytes: u64,
    pub deferred_generations: bool,
}

struct LiveMember {
    snapshot: String,
    digest: String,
}

fn live_members(
    index: &VerifiedIndex,
    keep: &mut BTreeSet<String>,
) -> Result<BTreeMap<String, Vec<LiveMember>>> {
    let mut members = BTreeMap::<_, Vec<_>>::new();
    for certificate in &index.try_manifest()?.sources {
        if certificate.observation().source().source_format() != "ctx_archive_v1" {
            return Err(invalid(
                "archive pruning requires an archive-owned generation",
            ));
        }
        let reference = OwnedMember::decode(certificate.observation())?;
        if !is_digest(&reference.snapshot) {
            return Err(invalid("invalid retained archive snapshot reference"));
        }
        keep.insert(reference.snapshot.clone());
        members
            .entry(reference.member)
            .or_default()
            .push(LiveMember {
                snapshot: reference.snapshot,
                digest: hex(certificate.content_digest()),
            });
    }
    Ok(members)
}

fn matching_inventory(
    directory: &Path,
    identity: &ArchiveIdentity,
    path: &str,
    digest: &str,
) -> Result<Option<SessionMember>> {
    let manifest = io::read_manifest(directory)?;
    if manifest.identity != *identity
        || directory.file_name().and_then(|name| name.to_str())
            != Some(manifest.snapshot_id()?.as_str())
    {
        return Ok(None);
    }
    let mut selected = None;
    io::visit_members(directory, |member| {
        if member.path == path && member.sha256 == digest {
            selected = Some(member);
        }
        Ok(())
    })?;
    Ok(selected)
}

fn covered_records(
    old: &Path,
    member: &SessionMember,
    newer: &Path,
    replacement: &SessionMember,
) -> Result<bool> {
    if member.records > replacement.records {
        return Ok(false);
    }
    if member.sha256 == replacement.sha256 {
        io::verify_member(&newer.join(&replacement.path), replacement)?;
        return Ok(true);
    }
    let mut reader = BufReader::new(io::open_regular(&old.join(&member.path))?);
    let mut line = Vec::new();
    let mut next = next_record(&mut reader, &mut line)?;
    let mut missing = false;
    io::visit_records(newer, replacement, |record| {
        if let Some(expected) = &next {
            let event = record.event_id.encode_canonical()?;
            let expected_event = expected.event_id.encode_canonical()?;
            if event > expected_event {
                missing = true;
            } else if event == expected_event {
                if expected != &record {
                    missing = true;
                }
                next = next_record(&mut reader, &mut line)?;
            }
        }
        Ok(())
    })?;
    Ok(!missing && next.is_none())
}

fn next_record(
    reader: &mut impl std::io::BufRead,
    line: &mut Vec<u8>,
) -> Result<Option<CoreRecord>> {
    if !io::read_line(
        reader,
        line,
        ctx_history_core::MAX_ENCODED_CORE_RECORD_BYTES + 1,
    )? {
        return Ok(None);
    }
    Ok(Some(CoreRecord::decode_stored(&line[..line.len() - 1])?))
}

fn snapshot_bytes(directory: &Path) -> Result<u64> {
    let mut bytes = 0;
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            checked_add(&mut bytes, snapshot_bytes(&entry.path())?)?;
        } else if kind.is_file() {
            checked_add(&mut bytes, entry.metadata()?.len())?;
        } else {
            return Err(invalid("archive pruning refuses non-regular payloads"));
        }
    }
    Ok(bytes)
}

/// Preview or reclaim verified, superseded copies in an already archive-owned root.
pub fn prune(root: &Path, apply: bool) -> Result<PruneReceipt> {
    if !is_archive_root(root)? {
        return Err(invalid("archive pruning requires an archive-owned root"));
    }
    let _restore_lock = prepare_restore_root(root)?;
    let lexical = root.join("search/lexical");
    let Some(pointer) =
        load_active_generation_pointer(&lexical).map_err(ctx_history_index::IndexError::from)?
    else {
        return Ok(PruneReceipt::default());
    };
    let retention =
        load_generation_retention_lease(&lexical).map_err(ctx_history_index::IndexError::from)?;
    let retained = std::iter::once(pointer.active())
        .chain(pointer.previous())
        .chain(retention.as_ref().map(|lease| lease.target()))
        .collect::<Vec<_>>();
    let named = retained
        .iter()
        .map(|slot| slot.directory())
        .collect::<BTreeSet<_>>();
    let mut inactive_guards = Vec::new();
    for entry in fs::read_dir(lexical.join(INDEX_GENERATIONS_DIRECTORY))? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str().filter(|name| name.starts_with("generation-")) else {
            return Err(invalid("unrecognized lexical generation directory"));
        };
        if !entry.file_type()?.is_dir() {
            return Err(invalid("lexical generation is not an owned directory"));
        }
        if !named.contains(name) {
            let Some(guard) = try_generation_directory_reclaim_authority(&lexical, name)
                .map_err(ctx_history_index::IndexError::from)?
            else {
                // A reader can keep a generation beyond the pointer's recovery slot. Preserve
                // possible archive references until that exact generation can be reclaimed.
                return Ok(PruneReceipt {
                    deferred_generations: true,
                    ..PruneReceipt::default()
                });
            };
            inactive_guards.push(guard);
        }
    }
    let mut keep = BTreeSet::new();
    let active = VerifiedIndex::open_pinned_generation(&lexical, pointer.active().generation_id())?;
    let latest = live_members(&active, &mut keep)?;
    for slot in retained.iter().skip(1) {
        let generation = VerifiedIndex::open_pinned_generation(&lexical, slot.generation_id())?;
        live_members(&generation, &mut keep)?;
    }
    let archives = root.join("archives");
    let mut candidates = Vec::new();
    let mut receipt = PruneReceipt::default();
    for entry in fs::read_dir(&archives)? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !is_digest(&name) || keep.contains(&name) || !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path();
        let manifest = io::verify(&path)?;
        if manifest.snapshot_id()? != name {
            return Err(invalid(
                "owned archive name differs from its verified identity",
            ));
        }
        let mut covered = true;
        io::visit_members(&path, |member| {
            if !covered {
                return Ok(());
            }
            for live in latest.get(&member.path).into_iter().flatten() {
                let newer = archives.join(&live.snapshot);
                if let Some(replacement) =
                    matching_inventory(&newer, &manifest.identity, &member.path, &live.digest)?
                {
                    if covered_records(&path, &member, &newer, &replacement)? {
                        return Ok(());
                    }
                }
            }
            covered = false;
            Ok(())
        })?;
        if covered {
            let bytes = snapshot_bytes(&path)?;
            checked_add(&mut receipt.eligible_snapshots, 1)?;
            checked_add(&mut receipt.eligible_bytes, bytes)?;
            candidates.push((name, path));
        }
    }
    if apply {
        let staged = archives.join(PRUNE_DIRECTORY);
        if !candidates.is_empty() || staged.try_exists()? {
            ensure_private_directory(&staged)?;
            sweep(&staged, &mut receipt)?;
            for (name, path) in candidates {
                fs::rename(path, staged.join(name))?;
                sync_directory(&archives)?;
                sync_directory(&staged)?;
            }
            sweep(&staged, &mut receipt)?;
            fs::remove_dir(&staged)?;
            sync_directory(&archives)?;
        }
    }
    Ok(receipt)
}

fn sweep(staged: &Path, receipt: &mut PruneReceipt) -> Result<()> {
    let pending = fs::read_dir(staged)?
        .map(|entry| {
            let entry = entry?;
            if !entry.file_type()?.is_dir() || !entry.file_name().to_str().is_some_and(is_digest) {
                return Err(invalid("invalid staged archive pruning payload"));
            }
            let path = entry.path();
            Ok((snapshot_bytes(&path)?, path))
        })
        .collect::<Result<Vec<_>>>()?;
    for (bytes, path) in pending {
        fs::remove_dir_all(path)?;
        sync_directory(staged)?;
        checked_add(&mut receipt.reclaimed_snapshots, 1)?;
        checked_add(&mut receipt.reclaimed_bytes, bytes)?;
    }
    Ok(())
}
