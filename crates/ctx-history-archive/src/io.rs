use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    path::Path,
};

use ctx_history_core::MAX_ENCODED_CORE_RECORD_BYTES;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

use crate::{checked_add, hex, invalid, Manifest, Result, SessionMember};

// A valid 64 KiB Core source key can expand sixfold under JSON escaping.
const MAX_METADATA_BYTES: usize = 512 * 1024;

/// Verifies the entire closed inventory and every complete Core record without
/// retaining the corpus. Input directories must remain stable for the call.
pub fn verify(archive: &Path) -> Result<Manifest> {
    let manifest = read_manifest(archive)?;
    visit_members(archive, |member| {
        visit_records(archive, &member, |_| Ok(()))
    })?;
    let mut count = 0;
    for entry in fs::read_dir(archive.join("members"))? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err(invalid("non-UTF-8 archive member filename"));
        };
        let Some(key) = name.strip_suffix(".jsonl") else {
            return Err(invalid("unexpected archive member"));
        };
        if !crate::is_digest(key) {
            return Err(invalid("unexpected archive member"));
        }
        regular_metadata(&entry.path(), false)?;
        checked_add(&mut count, 1)?;
    }
    if count != manifest.members {
        return Err(invalid("unlisted files in archive members directory"));
    }
    Ok(manifest)
}

/// Streams inventory metadata. Callbacks are provisional until this function
/// succeeds (the final inventory digest/count is checked at EOF).
pub fn visit_members(
    archive: &Path,
    mut visit: impl FnMut(SessionMember) -> Result<()>,
) -> Result<()> {
    let manifest = read_manifest(archive)?;
    let mut reader = BufReader::new(open_regular(&archive.join("inventory.jsonl"))?);
    let mut hash = Sha256::new();
    let mut line = Vec::new();
    let mut previous = String::new();
    let (mut members, mut records) = (0, 0);
    while read_line(&mut reader, &mut line, MAX_METADATA_BYTES)? {
        hash.update(&line);
        let member: SessionMember = serde_json::from_slice(&line)?;
        member.validate()?;
        if serde_json::to_vec(&member)? != line[..line.len() - 1] {
            return Err(invalid("inventory is not canonical archive metadata"));
        }
        if member.path <= previous {
            return Err(invalid(
                "inventory must be unique and ordered by member path",
            ));
        }
        previous.clone_from(&member.path);
        checked_add(&mut members, 1)?;
        checked_add(&mut records, member.records)?;
        visit(member)?;
    }
    if hex(&hash.finalize()) != manifest.inventory_sha256
        || members != manifest.members
        || records != manifest.records
    {
        return Err(invalid("inventory checksum/count mismatch"));
    }
    Ok(())
}

/// Streams original, unmodified Core records. Callbacks must only stage work:
/// checksum/count validation finishes at EOF, and failure must abort staging.
pub fn visit_records(
    archive: &Path,
    member: &SessionMember,
    visit: impl FnMut(crate::CoreRecord) -> Result<()>,
) -> Result<()> {
    member.validate()?;
    regular_metadata(archive, true)?;
    regular_metadata(&archive.join("members"), true)?;
    visit_member_records(&archive.join(&member.path), member, visit)
}

/// Validate one normalized upload member without a whole snapshot. The file is
/// a server-owned staged regular file; its transport filename is not identity.
pub fn verify_member(file: &Path, member: &SessionMember) -> Result<()> {
    visit_member_records(file, member, |_| Ok(()))
}

/// Stream an independently staged member. The callback is provisional until
/// EOF checksum validation succeeds; never durably acknowledge inside it.
pub fn visit_member_records(
    file: &Path,
    member: &SessionMember,
    mut visit: impl FnMut(crate::CoreRecord) -> Result<()>,
) -> Result<()> {
    member.validate()?;
    let file = open_regular(file)?;
    if file.metadata()?.len() != member.bytes {
        return Err(invalid("member byte count mismatch"));
    }
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut hash = Sha256::new();
    let (mut records, mut bytes) = (0, 0);
    let mut previous = None;
    while read_line(&mut reader, &mut line, MAX_ENCODED_CORE_RECORD_BYTES + 1)? {
        checked_add(&mut bytes, line.len() as u64)?;
        if bytes > member.bytes {
            return Err(invalid("member grew during verification"));
        }
        hash.update(&line);
        let encoded = &line[..line.len() - 1];
        let record = crate::CoreRecord::decode_stored(encoded)?;
        if record.normalization_revision != ctx_history_core::CORE_NORMALIZATION_REVISION
            || record.content.policy_revision != ctx_history_core::CORE_CONTENT_POLICY_REVISION
        {
            return Err(invalid(
                "record uses an unsupported archive normalization/content mapping",
            ));
        }
        // A frozen mapping must never silently discard unknown nested fields.
        if record.encode_stored()? != encoded {
            return Err(invalid("record is not canonical archive Core JSON"));
        }
        let event = record.event_id.encode_canonical()?;
        if previous.is_some_and(|p| p >= event)
            || !record.source.exact_descriptor_eq(&member.source)
            || record.session_id.encode_canonical()? != member.session_id.encode_canonical()?
        {
            return Err(invalid(
                "member has duplicate/unordered events or mismatched ownership",
            ));
        }
        previous = Some(event);
        checked_add(&mut records, 1)?;
        visit(record)?;
    }
    if records != member.records || bytes != member.bytes || hex(&hash.finalize()) != member.sha256
    {
        return Err(invalid("member checksum/count mismatch"));
    }
    Ok(())
}

pub(crate) fn read_manifest(archive: &Path) -> Result<Manifest> {
    regular_metadata(archive, true)?;
    regular_metadata(&archive.join("members"), true)?;
    for entry in fs::read_dir(archive)? {
        let entry = entry?;
        if !matches!(
            entry.file_name().to_str(),
            Some("manifest.json" | "inventory.jsonl" | "members")
        ) {
            return Err(invalid("archive contains unsupported payloads"));
        }
    }
    let manifest: Manifest = read_json(&archive.join("manifest.json"))?;
    manifest.validate()?;
    Ok(manifest)
}

pub(crate) fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let mut bytes = Vec::new();
    open_regular(path)?
        .take((MAX_METADATA_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_METADATA_BYTES {
        return Err(invalid("archive metadata exceeds per-record bound"));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

pub(crate) fn read_line(reader: &mut impl BufRead, line: &mut Vec<u8>, max: usize) -> Result<bool> {
    line.clear();
    reader.take((max + 1) as u64).read_until(b'\n', line)?;
    if line.is_empty() {
        return Ok(false);
    }
    if line.len() > max || line.last() != Some(&b'\n') {
        return Err(invalid("oversized or incomplete JSONL record"));
    }
    Ok(true)
}

pub(crate) fn regular_metadata(path: &Path, directory: bool) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return Err(invalid("archive reparse points are unsupported"));
        }
    }
    if meta.file_type().is_symlink()
        || if directory {
            !meta.is_dir()
        } else {
            !meta.is_file()
        }
    {
        return Err(invalid("archive links and special files are unsupported"));
    }
    Ok(())
}

pub(crate) fn open_regular(path: &Path) -> Result<File> {
    regular_metadata(path, false)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x00200000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(invalid("archive reparse points are unsupported"));
        }
    }
    if !metadata.is_file() {
        return Err(invalid("archive input is not a regular file"));
    }
    Ok(file)
}

#[cfg(test)]
pub(crate) fn hash_file(path: &Path) -> Result<(String, u64)> {
    hash_file_with_control(path, &mut || Ok(()))
}

pub(crate) fn hash_file_with_control(
    path: &Path,
    control: &mut impl FnMut() -> Result<()>,
) -> Result<(String, u64)> {
    let mut reader = open_regular(path)?;
    let mut hash = Sha256::new();
    let mut total = 0;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        control()?;
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        checked_add(&mut total, n as u64)?;
        hash.update(&buffer[..n]);
    }
    Ok((hex(&hash.finalize()), total))
}

pub(crate) fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    let mut file = ctx_history_platform::platform_security::create_private_file_new(path)?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

/// Copy only the closed normalized inventory, then verify the owned bytes.
/// Never retain a reference to the caller's temporary upload/restore directory.
pub(crate) fn copy_owned(archive: &Path, destination: &Path, manifest: &Manifest) -> Result<()> {
    use ctx_history_platform::platform_security::{
        create_private_directory_all, create_private_file_new,
    };
    create_private_directory_all(&destination.join("members"))?;
    visit_members(archive, |member| {
        let mut input =
            open_regular(&archive.join(&member.path))?.take(member.bytes.saturating_add(1));
        let mut output = create_private_file_new(&destination.join(&member.path))?;
        if std::io::copy(&mut input, &mut output)? != member.bytes {
            return Err(invalid("member changed while copying"));
        }
        output.sync_all()?;
        Ok(())
    })?;
    let mut input = open_regular(&archive.join("inventory.jsonl"))?;
    let mut output = create_private_file_new(&destination.join("inventory.jsonl"))?;
    std::io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    write_json(&destination.join("manifest.json"), manifest)?;
    if verify(destination)? != *manifest {
        return Err(invalid("archive changed while copying"));
    }
    ctx_history_index_generation::sync_directory(&destination.join("members"))?;
    ctx_history_index_generation::sync_directory(destination)?;
    Ok(())
}
