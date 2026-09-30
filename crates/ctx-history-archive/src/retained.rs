//! Root-aware backup follows committed original members, never the lossy reader projection.

use std::{
    collections::BTreeSet,
    fs,
    io::{BufWriter, Write},
    path::Path,
};

use ctx_history_platform::platform_security::{
    create_private_directory_all, create_private_file_new,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::{
    checked_add, hex, invalid, io, parent_dir, ArchiveIdentity, Manifest, Result, Selection,
    SessionMember, SourceKey, VerifiedIndex, ARCHIVE_VERSION, CORE_MAPPING,
};

pub(crate) const OBSERVATION_KIND: &str = "archive-owned-member-v1";

/// The source certificate owns this pointer and its content digest atomically.
/// It is bounded metadata; the original identity lives in the retained manifest.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OwnedMember {
    pub snapshot: String,
    pub member: String,
}

/// A single-origin export, including identities intentionally not exported.
#[derive(Debug, Clone, Serialize)]
pub struct ExportReceipt {
    pub manifest: Manifest,
    pub excluded_identities: Vec<ArchiveIdentity>,
}

/// Back up committed history. For a native root, assign `identity` as usual.
/// For an archive-owned root, select that original origin/view without relabeling.
/// `selection` still names local Core source/session IDs. Multiple namespaces
/// coalesce only when the original member bytes agree; conflicting revisions
/// require explicit source selection. No other retained identity is implicit.
pub fn export_data_root(
    root: &Path,
    destination: &Path,
    identity: ArchiveIdentity,
    selection: &Selection,
) -> Result<ExportReceipt> {
    identity.validate()?;
    let owned = crate::is_archive_root(root)?;
    let index = VerifiedIndex::open_pinned(root.join("search/lexical"))?;
    if !owned {
        if index
            .manifest()
            .sources
            .iter()
            .any(|s| s.observation().source().source_format() == "ctx_archive_v1")
        {
            return Err(invalid(
                "restored history requires its archive-owned root for faithful export",
            ));
        }
        return Ok(ExportReceipt {
            manifest: crate::export(&index, destination, identity, selection)?,
            excluded_identities: Vec::new(),
        });
    }
    if selection
        .sources
        .iter()
        .chain(&selection.sessions)
        .any(|id| !crate::is_digest(id))
    {
        return Err(invalid(
            "selection requires full lowercase identity digests",
        ));
    }
    if destination.try_exists()? {
        return Err(invalid("archive destination already exists"));
    }
    let parent = parent_dir(destination);
    fs::create_dir_all(parent)?;
    let stage = tempfile::Builder::new()
        .prefix(".ctx-archive-")
        .tempdir_in(parent)?;
    create_private_directory_all(&stage.path().join("members"))?;
    // Reuse the exporter's disk-backed inventory approach. Neither members nor
    // record bodies accumulate in memory, and scratch never becomes authority.
    let scratch = tempfile::NamedTempFile::new_in(parent)?;
    let db = Connection::open(scratch.path())?;
    db.execute_batch(
        "PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA cache_size=-2048;
        CREATE TABLE imports(snapshot TEXT, path TEXT, source TEXT, revision TEXT,
            selected INTEGER, eligible INTEGER DEFAULT 0, resolved INTEGER DEFAULT 0);
        CREATE INDEX imports_member ON imports(snapshot,path);
        CREATE TABLE members(path TEXT PRIMARY KEY, metadata TEXT NOT NULL); BEGIN;",
    )?;
    for certificate in &index.manifest().sources {
        let observation = certificate.observation();
        if observation.source().source_format() != "ctx_archive_v1"
            || observation.revision_kind() != OBSERVATION_KIND
        {
            return Err(invalid(
                "archive-owned export requires committed original-member references",
            ));
        }
        let owned: OwnedMember = serde_json::from_slice(observation.revision())?;
        if !crate::is_digest(&owned.snapshot)
            || !owned
                .member
                .strip_prefix("members/")
                .and_then(|s| s.strip_suffix(".jsonl"))
                .is_some_and(crate::is_digest)
        {
            return Err(invalid("invalid committed original-member reference"));
        }
        db.execute(
            "INSERT INTO imports(snapshot,path,source,revision,selected) VALUES (?1,?2,?3,?4,?5)",
            params![
                owned.snapshot,
                owned.member,
                serde_json::to_string(observation.source())?,
                hex(certificate.content_digest()),
                selection.includes_source(observation.source())
            ],
        )?;
    }
    let mut identities = BTreeSet::new();
    let mut snapshots = db.prepare("SELECT DISTINCT snapshot FROM imports ORDER BY snapshot")?;
    let mut rows = snapshots.query([])?;
    while let Some(row) = rows.next()? {
        let snapshot: String = row.get(0)?;
        let archive = root.join("archives").join(&snapshot);
        let original = io::read_manifest(&archive)?;
        if original.snapshot_id()? != snapshot {
            return Err(invalid(
                "owned archive manifest differs from committed snapshot",
            ));
        }
        identities.insert((
            original.identity.origin.clone(),
            original.identity.view.clone(),
        ));
        if original.identity != identity {
            continue;
        }
        db.execute(
            "UPDATE imports SET eligible=selected WHERE snapshot=?1",
            [&snapshot],
        )?;
        io::visit_members(&archive, |member| {
            let mut imports = db.prepare(
                "SELECT source,revision FROM imports WHERE snapshot=?1 AND path=?2 AND selected=1",
            )?;
            let mut matches = imports.query(params![snapshot, member.path])?;
            let mut selected = false;
            while let Some(row) = matches.next()? {
                let source: SourceKey = serde_json::from_str(&row.get::<_, String>(0)?)?;
                let revision: String = row.get(1)?;
                if revision != member.sha256 {
                    return Err(invalid("owned member differs from committed revision"));
                }
                selected |= selection.includes_session(crate::restore::session_for_source(
                    &source,
                    member.session_id,
                )?);
            }
            if selected {
                retain_member(&db, &archive, stage.path(), &member)?;
            }
            db.execute(
                "UPDATE imports SET resolved=1 WHERE snapshot=?1 AND path=?2 AND selected=1",
                params![snapshot, member.path],
            )?;
            Ok(())
        })?;
    }
    if !identities.is_empty()
        && !identities.contains(&(identity.origin.clone(), identity.view.clone()))
    {
        return Err(invalid("--origin and --view must select an original identity retained in this archive-owned root; restored history cannot be relabeled"));
    }
    if db.query_row(
        "SELECT EXISTS(SELECT 1 FROM imports WHERE eligible=1 AND resolved=0)",
        [],
        |r| r.get::<_, bool>(0),
    )? {
        return Err(invalid(
            "committed original member is missing from its owned inventory",
        ));
    }
    let excluded_identities: Vec<_> = identities
        .into_iter()
        .filter(|(origin, view)| origin != &identity.origin || view != &identity.view)
        .map(|(origin, view)| ArchiveIdentity { origin, view })
        .collect();
    let manifest = finish(
        &db,
        stage.path(),
        &index,
        identity,
        !selection.sources.is_empty()
            || !selection.sessions.is_empty()
            || !excluded_identities.is_empty(),
    )?;
    ctx_history_index_generation::sync_directory(&stage.path().join("members"))?;
    ctx_history_index_generation::sync_directory(stage.path())?;
    fs::rename(stage.path(), destination)?;
    ctx_history_index_generation::sync_directory(parent)?;
    Ok(ExportReceipt {
        manifest,
        excluded_identities,
    })
}

fn retain_member(
    db: &Connection,
    archive: &Path,
    stage: &Path,
    member: &SessionMember,
) -> Result<()> {
    use std::io::Read;
    let metadata = serde_json::to_string(member)?;
    let existing: Option<String> = db
        .query_row(
            "SELECT metadata FROM members WHERE path=?1",
            [&member.path],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(existing) = existing {
        if existing != metadata {
            return Err(invalid("multiple import bindings retain different revisions of one original member; select one binding with --source"));
        }
        return Ok(());
    }
    let mut input =
        io::open_regular(&archive.join(&member.path))?.take(member.bytes.saturating_add(1));
    let target = stage.join(&member.path);
    let mut output = create_private_file_new(&target)?;
    if std::io::copy(&mut input, &mut output)? != member.bytes {
        return Err(invalid("owned member changed while copying"));
    }
    output.sync_all()?;
    io::verify_member(&target, member)?;
    db.execute(
        "INSERT INTO members VALUES (?1,?2)",
        params![member.path, metadata],
    )?;
    Ok(())
}

fn finish(
    db: &Connection,
    stage: &Path,
    index: &VerifiedIndex,
    identity: ArchiveIdentity,
    selected_subset: bool,
) -> Result<Manifest> {
    let mut inventory = BufWriter::new(create_private_file_new(&stage.join("inventory.jsonl"))?);
    let mut statement = db.prepare("SELECT metadata FROM members ORDER BY path")?;
    let mut rows = statement.query([])?;
    let (mut members, mut records) = (0, 0);
    while let Some(row) = rows.next()? {
        let metadata: String = row.get(0)?;
        let member: SessionMember = serde_json::from_str(&metadata)?;
        inventory.write_all(metadata.as_bytes())?;
        inventory.write_all(b"\n")?;
        checked_add(&mut members, 1)?;
        checked_add(&mut records, member.records)?;
    }
    inventory.flush()?;
    inventory.get_ref().sync_all()?;
    let manifest = Manifest {
        version: ARCHIVE_VERSION,
        identity,
        core_mapping: CORE_MAPPING.into(),
        core_contract: ctx_history_core::core_record_contract_fingerprint(),
        generation: index.generation_id().to_owned(),
        fidelity: "retained_normalized".into(),
        selected_subset,
        inventory_sha256: io::hash_file_with_control(&stage.join("inventory.jsonl"), &mut || {
            Ok(())
        })?
        .0,
        members,
        records,
    };
    manifest.validate()?;
    io::write_json(&stage.join("manifest.json"), &manifest)?;
    Ok(manifest)
}
