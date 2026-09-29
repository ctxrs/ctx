use std::{collections::BTreeMap, fs, path::Path};

use ctx_history_core::{
    derive_event_id, derive_session_id, CertifiedSource, EventIdentityInput, NativeItemKey,
    NativeSessionKey, ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceObservation,
    StableEntityKind, TypedKey,
};
use ctx_history_index::{GenerationWriter, WriterOptions};
use ctx_history_platform::platform_security::{create_private_file_new, ensure_private_directory};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    hex, invalid, io, ArchiveError, ArchiveIdentity, CoreRecord, Result, SessionMember, SourceKey,
    StableEntityId,
};

/// Authority supplied by the destination, never inferred from archive fields.
/// Hosted namespaces must include the server-owned collection/publication and
/// publisher binding. Local offline restores can choose a stable local name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportBinding {
    pub namespace: String,
    pub identity: ArchiveIdentity,
}

impl ImportBinding {
    pub fn validate(&self) -> Result<()> {
        self.identity.validate()?;
        if self.namespace.is_empty()
            || self.namespace.len() > 4096
            || self.namespace.chars().any(char::is_control)
        {
            return Err(invalid(
                "destination namespace must contain 1..4096 non-control UTF-8 bytes",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct RestoreOptions {
    pub binding: ImportBinding,
    /// Explicit correction authority: member path -> exact currently imported
    /// SHA-256 revision. An absent member is never a request to remove anything.
    pub expected_predecessors: BTreeMap<String, String>,
    pub writer: WriterOptions,
}

impl RestoreOptions {
    pub fn new(binding: ImportBinding) -> Self {
        Self {
            binding,
            expected_predecessors: BTreeMap::new(),
            writer: WriterOptions::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreReceipt {
    pub snapshot_id: String,
    pub generation_id: String,
    pub imported_members: u64,
    pub unchanged_members: u64,
    pub records: u64,
}

/// The one portable-to-Core source mapping used by offline and hosted import.
/// One whole session is independently replaceable; physical paths never enter
/// identity. The original source descriptor remains in the owned JSONL bytes.
pub fn mapped_source(binding: &ImportBinding, member: &SessionMember) -> Result<SourceKey> {
    member.validate()?;
    source_for_session(binding, member.session_id)
}

fn source_for_session(binding: &ImportBinding, session: StableEntityId) -> Result<SourceKey> {
    binding.validate()?;
    session.validate_contract()?;
    if session.entity_kind() != StableEntityKind::Session {
        return Err(invalid("expected a portable session identity"));
    }
    let mut hash = Sha256::new();
    hash.update(b"ctx-archive-destination-v1\0");
    for value in [
        &binding.namespace,
        &binding.identity.origin,
        &binding.identity.view,
    ] {
        hash.update((value.len() as u64).to_be_bytes());
        hash.update(value.as_bytes());
    }
    hash.update(session.source_digest());
    hash.update(session.digest());
    Ok(SourceKey::derive(
        "archive",
        "ctx_archive_v1",
        "retained_session",
        1,
        SourceAnchor::CatalogLineage(hash.finalize().into()),
    )?)
}

/// Map a portable session/reference entirely inside the trusted destination.
pub fn mapped_session(binding: &ImportBinding, session: StableEntityId) -> Result<StableEntityId> {
    let source = source_for_session(binding, session)?;
    session_for_source(&source, session)
}

pub(crate) fn session_for_source(
    source: &SourceKey,
    session: StableEntityId,
) -> Result<StableEntityId> {
    let key = NativeSessionKey::composite(
        "archive-imported-session",
        vec![TypedKey::bytes(session.digest().to_vec())?],
    )?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "archive_session",
        native_session_key: &key,
    })?)
}

/// Maps event citations without reading their body. Portable full IDs remain in
/// the archive; this is an explicitly imported surrogate, not a native ID.
pub fn mapped_event(
    binding: &ImportBinding,
    session: StableEntityId,
    event: StableEntityId,
) -> Result<StableEntityId> {
    event.validate_contract()?;
    if event.entity_kind() != StableEntityKind::Event
        || event.source_digest() != session.source_digest()
    {
        return Err(invalid(
            "event does not belong to the portable session source",
        ));
    }
    let source = source_for_session(binding, session)?;
    let key = NativeItemKey::composite(
        "archive-imported-event",
        vec![TypedKey::bytes(event.digest().to_vec())?],
    )?;
    Ok(derive_event_id(EventIdentityInput {
        source: &source,
        session_id: mapped_session(binding, session)?,
        logical_item_kind: "archive_event",
        native_item_key: &key,
        subrecord_selector: None,
    })?)
}

/// Preserve content, optional timestamps, native IDs and activity exactly in the
/// projection, while qualifying all identities. Imported lineage is retained
/// only in original archive records: current Core relationship/copy fields mean
/// provider-native evidence and cannot safely represent an uploaded claim.
pub fn map_record(
    binding: &ImportBinding,
    member: &SessionMember,
    mut record: CoreRecord,
) -> Result<CoreRecord> {
    member.validate()?;
    record.validate_contract()?;
    if !record.source.exact_descriptor_eq(&member.source)
        || record.session_id.encode_canonical()? != member.session_id.encode_canonical()?
    {
        return Err(invalid("record is outside the bound archive member"));
    }
    record.event_id = mapped_event(binding, record.session_id, record.event_id)?;
    record.session_id = mapped_session(binding, record.session_id)?;
    record.source = mapped_source(binding, member)?;
    record.parent_session_id = None;
    record.root_session_id = None;
    record.session_relationship = None;
    record.event_copy = None;
    record.validate_contract()?;
    Ok(record)
}

/// Restore to an archive-owned data root (`archives/` plus `search/lexical/`), independent of
/// native providers. This is not ordinary path-based custom-source registration.
/// The existing Core generation owns successful revision state. Immutable owned
/// payloads are persisted first, so an interrupted commit leaves only harmless
/// unreferenced bytes. Full membership is additive; corrections require an exact
/// predecessor. The caller must reserve this root for this adapter.
pub fn restore(
    archive: &Path,
    owned_root: &Path,
    options: &RestoreOptions,
) -> Result<RestoreReceipt> {
    options.binding.validate()?;
    let manifest = io::verify(archive)?;
    manifest.require_identity(&options.binding.identity)?;
    for (path, revision) in &options.expected_predecessors {
        if !crate::is_digest(revision) || !path.starts_with("members/") {
            return Err(invalid("invalid expected predecessor"));
        }
    }
    let _lock = prepare_restore_root(owned_root)?;
    let archives = owned_root.join("archives");
    ensure_private_directory(&archives)?;
    ctx_history_index_generation::sync_directory(owned_root)?;
    ctx_history_index_generation::sync_directory(crate::parent_dir(owned_root))?;
    let snapshot_id = manifest.snapshot_id()?;
    let owned = archives.join(&snapshot_id);
    if owned.try_exists()? {
        if io::verify(&owned)? != manifest {
            return Err(invalid("owned snapshot differs"));
        }
    } else {
        let stage = tempfile::Builder::new()
            .prefix(".restore-")
            .tempdir_in(&archives)?;
        io::copy_owned(archive, stage.path(), &manifest)?;
        fs::rename(stage.path(), &owned)?;
        ctx_history_index_generation::sync_directory(&archives)?;
    }
    let index_root = owned_root.join("search/lexical");
    let mut writer = GenerationWriter::open(&index_root, options.writer.clone())?
        .into_writer()
        .map_err(ArchiveError::MigrationRecovery)?;
    if writer.base_manifest().is_some_and(|base| {
        base.sources
            .iter()
            .any(|source| source.observation().source().source_format() != "ctx_archive_v1")
    }) {
        return Err(invalid(
            "archive restore cannot overwrite a native-owned Core index",
        ));
    }
    let (mut imported_members, mut unchanged_members) = (0, 0);
    let mut used_predecessors = 0;
    io::visit_members(&owned, |member| {
        let source = mapped_source(&options.binding, &member)?;
        let prior = writer.base_manifest().and_then(|manifest| {
            manifest
                .sources
                .binary_search_by_key(&source.identity().digest(), |certificate| {
                    certificate.observation().source().identity().digest()
                })
                .ok()
                .and_then(|position| manifest.sources.get(position))
        });
        let prior = match prior {
            Some(certificate) => {
                certificate
                    .observation()
                    .source()
                    .validate_exact_descriptor(&source)?;
                Some(hex(certificate.content_digest()))
            }
            None => None,
        };
        let expected = options.expected_predecessors.get(&member.path);
        if expected.is_some() {
            used_predecessors += 1;
        }
        if prior.as_deref() == Some(&member.sha256) {
            unchanged_members += 1;
            return Ok(());
        }
        if prior.as_ref() != expected {
            return Err(ArchiveError::Conflict {
                member: member.path,
                current: prior.unwrap_or_else(|| "absent".into()),
            });
        }
        writer.begin_source(source.clone())?;
        io::visit_records(&owned, &member, |record| {
            writer.add_core_record(map_record(&options.binding, &member, record)?)?;
            Ok(())
        })?;
        let digest = digest_bytes(&member.sha256)?;
        let observation = SourceObservation::new(
            source,
            crate::retained::OBSERVATION_KIND,
            serde_json::to_vec(&crate::retained::OwnedMember {
                snapshot: snapshot_id.clone(),
                member: member.path.clone(),
            })?,
        )?;
        writer.certify_source(CertifiedSource::certify(
            observation.clone(),
            observation,
            "ctx-archive-v1",
            digest,
            ScannedSourceCounts {
                complete_records: member.records,
                retained_records: member.records,
                indexed_documents: member.records,
                certified_bytes: member.bytes,
                ..ScannedSourceCounts::default()
            },
        )?)?;
        imported_members += 1;
        Ok(())
    })?;
    if used_predecessors != options.expected_predecessors.len() {
        return Err(invalid("correction authority names an absent member"));
    }
    let generation_id = if imported_members == 0 && writer.base_generation_id().is_some() {
        writer.base_generation_id().unwrap_or_default().to_owned()
    } else {
        writer.commit(|_| true)?.generation_id
    };
    Ok(RestoreReceipt {
        snapshot_id,
        generation_id,
        imported_members,
        unchanged_members,
        records: manifest.records,
    })
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RootMarker {
    archive_root_version: u32,
}

/// Observe archive-root ownership without creating files, locks, or directories.
/// A missing marker is ordinary native ownership; a present invalid marker is an error.
pub fn is_archive_root(root: &Path) -> Result<bool> {
    let marker = root.join("archive-root.json");
    match fs::symlink_metadata(&marker) {
        Ok(_) => {
            validate_root_marker(&marker)?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// Validates and prepares an archive-owned data root, returning its held restore
/// lock. Callers may persist archive-only configuration before publishing Core;
/// drop this file before calling [`restore`], which acquires the same lock.
pub fn prepare_restore_root(root: &Path) -> Result<fs::File> {
    let marker = root.join("archive-root.json");
    if root.try_exists()? {
        io::regular_metadata(root, true)?;
        if marker.try_exists()? {
            validate_root_marker(&marker)?;
        } else {
            validate_initializing_root(root)?;
        }
    }
    ensure_private_directory(root)?;
    let lock_path = root.join("restore.lock");
    let lock = match create_private_file_new(&lock_path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            ctx_history_platform::platform_security::open_verified_private_file(&lock_path)?
        }
        Err(error) => return Err(error.into()),
    };
    fs2::FileExt::try_lock_exclusive(&lock)?;
    if marker.try_exists()? {
        validate_root_marker(&marker)?;
    } else {
        // Recheck under the existing writer lock. Only these two exact private
        // files can precede marker publication; other unmarked data is untouched.
        validate_initializing_root(root)?;
        let staging = root.join("archive-root.json.tmp");
        if staging.try_exists()? {
            fs::remove_file(&staging)?;
        }
        io::write_json(
            &staging,
            &RootMarker {
                archive_root_version: 1,
            },
        )?;
        ctx_history_index_generation::durable_atomic_replace_file(&staging, &marker)?;
    }
    Ok(lock)
}

fn validate_initializing_root(root: &Path) -> Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !matches!(
            entry.file_name().to_str(),
            Some("restore.lock" | "archive-root.json.tmp")
        ) {
            return Err(invalid(
                "restore requires an empty or archive-owned data root",
            ));
        }
        ctx_history_platform::platform_security::verify_private_file(&entry.path())?;
    }
    Ok(())
}

fn validate_root_marker(path: &Path) -> Result<()> {
    let marker: RootMarker = io::read_json(path)?;
    if marker.archive_root_version != 1 {
        return Err(invalid("unsupported archive-owned root version"));
    }
    Ok(())
}

fn digest_bytes(value: &str) -> Result<[u8; 32]> {
    if !crate::is_digest(value) {
        return Err(invalid("invalid SHA-256 digest"));
    }
    let mut digest = [0; 32];
    for (slot, digits) in digest.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
        let nibble = |byte: u8| {
            if byte <= b'9' {
                byte - b'0'
            } else {
                byte - b'a' + 10
            }
        };
        *slot = nibble(digits[0]) * 16 + nibble(digits[1]);
    }
    Ok(digest)
}
