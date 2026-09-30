use std::{
    fs::{self, OpenOptions},
    io::{BufWriter, Write},
    path::Path,
};

use ctx_history_platform::platform_security::{
    create_private_directory_all, create_private_file_new,
};
use rusqlite::{params, Connection};

use crate::{
    checked_add, invalid, io, member_path, parent_dir, ArchiveIdentity, Manifest, Result,
    Selection, SessionMember, VerifiedIndex, ARCHIVE_VERSION, CORE_MAPPING,
};

/// Export an already pinned current Core reader into a new destination.
/// All Core retained fields survive exactly; only whole sessions are selected.
/// Finalized checkpoints are independent and never overwrite an earlier one.
pub fn export(
    index: &VerifiedIndex,
    destination: &Path,
    identity: ArchiveIdentity,
    selection: &Selection,
) -> Result<Manifest> {
    export_with_control(index, destination, identity, selection, || Ok(()))
}

/// Cooperative shutdown/backpressure hook, called between pages and records.
/// An error removes staging and leaves the final destination unpublished.
pub fn export_with_control(
    index: &VerifiedIndex,
    destination: &Path,
    identity: ArchiveIdentity,
    selection: &Selection,
    mut control: impl FnMut() -> Result<()>,
) -> Result<Manifest> {
    control()?;
    identity.validate()?;
    if selection
        .sources
        .iter()
        .chain(selection.sessions.iter())
        .any(|s| !crate::is_digest(s))
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
    let staging = tempfile::Builder::new()
        .prefix(".ctx-archive-")
        .tempdir_in(parent)?;
    create_private_directory_all(&staging.path().join("members"))?;
    // Scratch metadata lives on disk: session cardinality never limits a corpus.
    // The database is not shipped and is not a second retained history store.
    let scratch = tempfile::NamedTempFile::new_in(parent)?;
    let db = Connection::open(scratch.path())?;
    db.execute_batch(
        "PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA cache_size=-2048;
        CREATE TABLE members(path TEXT PRIMARY KEY, source TEXT NOT NULL,
            session TEXT NOT NULL, records INTEGER NOT NULL); BEGIN",
    )?;
    for certificate in &index.manifest().sources {
        control()?;
        let source = certificate.observation().source();
        if !selection.includes_source(source) {
            continue;
        }
        let mut cursor = None;
        loop {
            control()?;
            let page = index.stored_core_source_event_page(source, cursor.as_ref(), 64)?;
            for stored in page.items {
                control()?;
                let record = &stored.core_record;
                if !selection.includes_session(record.session_id) {
                    continue;
                }
                let path = member_path(source, record.session_id);
                let fresh = db.execute(
                    "INSERT OR IGNORE INTO members VALUES (?1,?2,?3,0)",
                    params![
                        path,
                        serde_json::to_string(source)?,
                        serde_json::to_string(&record.session_id)?
                    ],
                )? != 0;
                if fresh {
                    create_private_file_new(&staging.path().join(&path))?;
                }
                let mut file = OpenOptions::new()
                    .append(true)
                    .open(staging.path().join(&path))?;
                file.write_all(stored.stored_json.encoded_core_record()?)?;
                file.write_all(b"\n")?;
                db.execute(
                    "UPDATE members SET records=records+1 WHERE path=?1",
                    params![path],
                )?;
            }
            if page.terminal {
                break;
            }
            cursor = page.next_cursor;
            if cursor.is_none() {
                return Err(invalid("Core source page made no progress"));
            }
        }
    }
    db.execute_batch("COMMIT")?;
    let mut inventory = BufWriter::new(create_private_file_new(
        &staging.path().join("inventory.jsonl"),
    )?);
    let (mut members, mut records) = (0, 0);
    let mut statement =
        db.prepare("SELECT path,source,session,records FROM members ORDER BY path")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        control()?;
        let path: String = row.get(0)?;
        let (sha256, bytes) =
            io::hash_file_with_control(&staging.path().join(&path), &mut control)?;
        OpenOptions::new()
            .write(true)
            .open(staging.path().join(&path))?
            .sync_all()?;
        let member = SessionMember {
            source: serde_json::from_str(&row.get::<_, String>(1)?)?,
            session_id: serde_json::from_str(&row.get::<_, String>(2)?)?,
            path,
            sha256,
            bytes,
            records: row.get(3)?,
        };
        member.validate()?;
        serde_json::to_writer(&mut inventory, &member)?;
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
        selected_subset: !selection.sources.is_empty() || !selection.sessions.is_empty(),
        inventory_sha256: io::hash_file_with_control(
            &staging.path().join("inventory.jsonl"),
            &mut control,
        )?
        .0,
        members,
        records,
    };
    manifest.validate()?;
    io::write_json(&staging.path().join("manifest.json"), &manifest)?;
    control()?;
    ctx_history_index_generation::sync_directory(&staging.path().join("members"))?;
    ctx_history_index_generation::sync_directory(staging.path())?;
    fs::rename(staging.path(), destination)?;
    ctx_history_index_generation::sync_directory(parent)?;
    Ok(manifest)
}

/// Capture just one whole session from a pinned generation. Daemons should
/// first compare `index.generation_id()` with their last successful observation,
/// then capture eligible changed sessions and compare the returned revision to
/// receipts. Retry already captured immutable bytes; never recapture for a
/// transport retry. The callback lets shutdown interrupt long session scans.
pub fn export_session(
    index: &VerifiedIndex,
    source: &crate::SourceKey,
    session: crate::StableEntityId,
    destination: &Path,
    mut control: impl FnMut() -> Result<()>,
) -> Result<SessionMember> {
    use sha2::{Digest, Sha256};
    source.validate_contract()?;
    session.validate_contract()?;
    control()?;
    let parent = parent_dir(destination);
    fs::create_dir_all(parent)?;
    let mut stage = tempfile::NamedTempFile::new_in(parent)?;
    let mut hash = Sha256::new();
    let (mut records, mut bytes) = (0, 0);
    let mut cursor = None;
    loop {
        control()?;
        let page = index.stored_core_source_event_page(source, cursor.as_ref(), 64)?;
        for stored in page.items {
            control()?;
            if stored.core_record.session_id.encode_canonical()? != session.encode_canonical()? {
                continue;
            }
            let encoded = stored.stored_json.encoded_core_record()?;
            stage.write_all(encoded)?;
            stage.write_all(b"\n")?;
            hash.update(encoded);
            hash.update(b"\n");
            checked_add(&mut records, 1)?;
            checked_add(&mut bytes, encoded.len() as u64 + 1)?;
        }
        if page.terminal {
            break;
        }
        cursor = page.next_cursor;
        if cursor.is_none() {
            return Err(invalid("Core source page made no progress"));
        }
    }
    let member = SessionMember {
        source: source.clone(),
        session_id: session,
        path: member_path(source, session),
        sha256: crate::hex(&hash.finalize()),
        bytes,
        records,
    };
    member.validate()?;
    control()?;
    stage.as_file().sync_all()?;
    stage
        .persist_noclobber(destination)
        .map_err(|error| error.error)?;
    ctx_history_index_generation::sync_directory(parent)?;
    Ok(member)
}
