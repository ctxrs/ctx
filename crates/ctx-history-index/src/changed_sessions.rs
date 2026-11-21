use std::{
    collections::HashMap,
    fs::File,
    io,
    path::{Path, PathBuf},
};

use ctx_history_core::{ProviderNativeSessionRelationship, StableEntityId};
use uuid::Uuid;

use crate::{
    preparation::{PreparedSessionIdentityFacts, PreparedSessionRelationship},
    writer_options::CHANGED_SESSION_REGISTRY_ENTRY_CHARGE_BYTES,
    IndexError, Result,
};

const SLOT_EMPTY: u8 = 0;
const SLOT_OCCUPIED: u8 = 1;
const SLOT_TOMBSTONE: u8 = 2;

const FLAG_HAS_PARENT: u8 = 1 << 0;
const FLAG_HAS_ROOT: u8 = 1 << 1;
const FLAG_KIND_SHIFT: u8 = 2;
const FLAG_KIND_MASK: u8 = 0b111;
const FLAG_VALID_MASK: u8 = FLAG_HAS_PARENT | FLAG_HAS_ROOT | (FLAG_KIND_MASK << FLAG_KIND_SHIFT);

const OFFSET_STATE: usize = 0;
const OFFSET_UUID: usize = OFFSET_STATE + 1;
const OFFSET_SOURCE_OWNER: usize = OFFSET_UUID + 16;
const OFFSET_FLAGS: usize = OFFSET_SOURCE_OWNER + 32;
const OFFSET_SESSION_ID: usize = OFFSET_FLAGS + 1;
const OFFSET_PARENT_SESSION_ID: usize = OFFSET_SESSION_ID + StableEntityId::CANONICAL_LEN;
const OFFSET_ROOT_SESSION_ID: usize = OFFSET_PARENT_SESSION_ID + StableEntityId::CANONICAL_LEN;
const SLOT_BYTES: usize = OFFSET_ROOT_SESSION_ID + StableEntityId::CANONICAL_LEN;

const MIN_SPILL_CAPACITY: usize = 64;
const REHASH_CHUNK_SLOTS: usize = 160;

/// Bounded-memory registry of distinct sessions touched by a candidate writer.
///
/// Recent entries stay in `hot` up to `hot_max_entries` (sized from the
/// configured indexing memory budget). When full, `hot` flushes into a private
/// unlinked spill hash table so large cold or upgrade rebuilds keep constant
/// RAM regardless of total session count.
pub(crate) struct ChangedSessionRegistry {
    root: PathBuf,
    hot_max_entries: usize,
    hot: HashMap<Uuid, PreparedSessionIdentityFacts>,
    spill: Option<SpilledSessionTable>,
    total_len: usize,
}

impl ChangedSessionRegistry {
    pub(crate) fn new(root: PathBuf, hot_memory_bytes: usize) -> Self {
        let hot_max_entries =
            (hot_memory_bytes / CHANGED_SESSION_REGISTRY_ENTRY_CHARGE_BYTES).max(1);
        Self::with_hot_capacity(root, hot_max_entries)
    }

    pub(crate) fn with_hot_capacity(root: PathBuf, hot_max_entries: usize) -> Self {
        Self {
            root,
            hot_max_entries: hot_max_entries.max(1),
            hot: HashMap::new(),
            spill: None,
            total_len: 0,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.total_len
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.total_len == 0
    }

    #[cfg(test)]
    pub(crate) fn hot_len(&self) -> usize {
        self.hot.len()
    }

    #[cfg(test)]
    pub(crate) fn has_spilled(&self) -> bool {
        self.spill.is_some()
    }

    #[cfg(test)]
    pub(crate) fn contains_key(&self, uuid: &Uuid) -> bool {
        self.get(uuid)
            .expect("changed-session registry test lookup failed")
            .is_some()
    }

    #[cfg(test)]
    pub(crate) fn keys(&self) -> std::vec::IntoIter<Uuid> {
        let mut keys = std::collections::HashSet::with_capacity(self.total_len);
        keys.extend(self.hot.keys().copied());
        if let Some(spill) = self.spill.as_ref() {
            spill
                .for_each_key(|uuid| {
                    keys.insert(uuid);
                    Ok(())
                })
                .expect("changed-session spill key scan failed");
        }
        keys.into_iter().collect::<Vec<_>>().into_iter()
    }

    pub(crate) fn get(&self, uuid: &Uuid) -> Result<Option<PreparedSessionIdentityFacts>> {
        if let Some(facts) = self.hot.get(uuid) {
            return Ok(Some(*facts));
        }
        match self.spill.as_ref() {
            Some(spill) => spill.get(uuid),
            None => Ok(None),
        }
    }

    pub(crate) fn insert_new(
        &mut self,
        uuid: Uuid,
        facts: PreparedSessionIdentityFacts,
    ) -> Result<()> {
        if self.hot.len() >= self.hot_max_entries {
            self.flush_hot_to_spill()?;
        }
        self.hot.insert(uuid, facts);
        self.total_len = self
            .total_len
            .checked_add(1)
            .ok_or(IndexError::CountOverflow)?;
        Ok(())
    }

    pub(crate) fn update_existing(
        &mut self,
        uuid: Uuid,
        facts: PreparedSessionIdentityFacts,
    ) -> Result<PreparedSessionIdentityFacts> {
        if let Some(slot) = self.hot.get_mut(&uuid) {
            return Ok(std::mem::replace(slot, facts));
        }
        let previous = self
            .spill
            .as_ref()
            .and_then(|spill| spill.get(&uuid).transpose())
            .transpose()?
            .ok_or(IndexError::WriterInvariant(
                "changed-session merge lost its prior registry entry",
            ))?;
        if self.hot.len() >= self.hot_max_entries {
            self.flush_hot_to_spill()?;
        }
        self.hot.insert(uuid, facts);
        Ok(previous)
    }

    pub(crate) fn restore(
        &mut self,
        uuid: Uuid,
        prior: PreparedSessionIdentityFacts,
    ) -> Result<()> {
        if !self.hot.contains_key(&uuid) && self.hot.len() >= self.hot_max_entries {
            self.flush_hot_to_spill()?;
        }
        self.hot.insert(uuid, prior);
        Ok(())
    }

    pub(crate) fn remove(&mut self, uuid: &Uuid) -> Result<bool> {
        let removed_hot = self.hot.remove(uuid).is_some();
        let removed_spill = match self.spill.as_mut() {
            Some(spill) => spill.remove(uuid)?,
            None => false,
        };
        if removed_hot || removed_spill {
            self.total_len = self.total_len.saturating_sub(1);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn flush_hot_to_spill(&mut self) -> Result<()> {
        if self.hot.is_empty() {
            return Ok(());
        }
        if self.spill.is_none() {
            let initial_capacity = self
                .hot_max_entries
                .saturating_mul(4)
                .max(MIN_SPILL_CAPACITY)
                .next_power_of_two();
            self.spill = Some(SpilledSessionTable::create(&self.root, initial_capacity)?);
        }
        let spill = self.spill.as_mut().expect("spill table just initialized");
        let mut entries = std::mem::take(&mut self.hot).into_iter();
        while let Some((uuid, facts)) = entries.next() {
            if let Err(error) = spill.upsert(uuid, facts) {
                self.hot.insert(uuid, facts);
                self.hot.extend(entries);
                return Err(error);
            }
        }
        Ok(())
    }
}

struct SpilledSessionTable {
    root: PathBuf,
    file: File,
    capacity: usize,
    occupied: usize,
    tombstones: usize,
}

impl SpilledSessionTable {
    fn create(root: &Path, capacity: usize) -> Result<Self> {
        let capacity = capacity.max(MIN_SPILL_CAPACITY).next_power_of_two();
        let file = create_spill_file(root, capacity)?;
        Ok(Self {
            root: root.to_path_buf(),
            file,
            capacity,
            occupied: 0,
            tombstones: 0,
        })
    }

    fn get(&self, uuid: &Uuid) -> Result<Option<PreparedSessionIdentityFacts>> {
        let mask = self.capacity - 1;
        let mut index = slot_hash(uuid) & mask;
        let mut slot = [0_u8; SLOT_BYTES];
        let target = uuid.as_bytes();
        for _ in 0..self.capacity {
            read_exact_at(&self.file, &mut slot, slot_offset(index)?)?;
            match slot[OFFSET_STATE] {
                SLOT_EMPTY => return Ok(None),
                SLOT_TOMBSTONE => {}
                SLOT_OCCUPIED => {
                    if &slot[OFFSET_UUID..OFFSET_SOURCE_OWNER] == target {
                        let (_, facts) = decode_slot(&slot)?;
                        return Ok(Some(facts));
                    }
                }
                _ => {
                    return Err(IndexError::WriterInvariant(
                        "changed-session spill slot has invalid state",
                    ));
                }
            }
            index = (index + 1) & mask;
        }
        Ok(None)
    }

    fn upsert(&mut self, uuid: Uuid, facts: PreparedSessionIdentityFacts) -> Result<()> {
        if self.needs_rehash_for_insert() {
            let next_capacity = if self.occupied.saturating_add(1).saturating_mul(4) > self.capacity
            {
                self.capacity
                    .checked_mul(2)
                    .ok_or(IndexError::CountOverflow)?
            } else {
                self.capacity
            };
            self.rehash(next_capacity)?;
        }
        let encoded = encode_slot(uuid, facts)?;
        let (occupied_delta, tombstone_delta) =
            insert_encoded_slot(&self.file, self.capacity, &uuid, &encoded)?;
        self.occupied = self.occupied.saturating_add(occupied_delta);
        self.tombstones = self.tombstones.saturating_sub(tombstone_delta);
        Ok(())
    }

    fn remove(&mut self, uuid: &Uuid) -> Result<bool> {
        let mask = self.capacity - 1;
        let mut index = slot_hash(uuid) & mask;
        let mut header = [0_u8; 1 + 16];
        let target = uuid.as_bytes();
        for _ in 0..self.capacity {
            let offset = slot_offset(index)?;
            read_exact_at(&self.file, &mut header, offset)?;
            match header[OFFSET_STATE] {
                SLOT_EMPTY => return Ok(false),
                SLOT_TOMBSTONE => {}
                SLOT_OCCUPIED => {
                    if &header[OFFSET_UUID..OFFSET_UUID + 16] == target {
                        write_all_at(&self.file, &[SLOT_TOMBSTONE], offset)?;
                        self.occupied = self.occupied.saturating_sub(1);
                        self.tombstones = self.tombstones.saturating_add(1);
                        return Ok(true);
                    }
                }
                _ => {
                    return Err(IndexError::WriterInvariant(
                        "changed-session spill slot has invalid state",
                    ));
                }
            }
            index = (index + 1) & mask;
        }
        Ok(false)
    }

    #[cfg(test)]
    fn for_each_key(&self, mut visit: impl FnMut(Uuid) -> Result<()>) -> Result<()> {
        let mut buffer = vec![0_u8; REHASH_CHUNK_SLOTS * SLOT_BYTES];
        let mut processed = 0_usize;
        while processed < self.capacity {
            let batch_slots = (self.capacity - processed).min(REHASH_CHUNK_SLOTS);
            let slice = &mut buffer[..batch_slots * SLOT_BYTES];
            read_exact_at(&self.file, slice, slot_offset(processed)?)?;
            for chunk in slice.chunks_exact(SLOT_BYTES) {
                if chunk[OFFSET_STATE] == SLOT_OCCUPIED {
                    let mut bytes = [0_u8; 16];
                    bytes.copy_from_slice(&chunk[OFFSET_UUID..OFFSET_SOURCE_OWNER]);
                    visit(Uuid::from_bytes(bytes))?;
                }
            }
            processed += batch_slots;
        }
        Ok(())
    }

    fn needs_rehash_for_insert(&self) -> bool {
        self.occupied
            .saturating_add(self.tombstones)
            .saturating_add(1)
            .saturating_mul(2)
            > self.capacity
    }

    fn rehash(&mut self, next_capacity: usize) -> Result<()> {
        let next_file = create_spill_file(&self.root, next_capacity)?;
        let mut buffer = vec![0_u8; REHASH_CHUNK_SLOTS * SLOT_BYTES];
        let mut processed = 0_usize;
        let mut migrated = 0_usize;
        while processed < self.capacity {
            let batch_slots = (self.capacity - processed).min(REHASH_CHUNK_SLOTS);
            let slice = &mut buffer[..batch_slots * SLOT_BYTES];
            read_exact_at(&self.file, slice, slot_offset(processed)?)?;
            for chunk in slice.chunks_exact(SLOT_BYTES) {
                if chunk[OFFSET_STATE] == SLOT_OCCUPIED {
                    let mut uuid_bytes = [0_u8; 16];
                    uuid_bytes.copy_from_slice(&chunk[OFFSET_UUID..OFFSET_SOURCE_OWNER]);
                    let uuid = Uuid::from_bytes(uuid_bytes);
                    let (delta, _) = insert_encoded_slot(&next_file, next_capacity, &uuid, chunk)?;
                    migrated = migrated.saturating_add(delta);
                }
            }
            processed += batch_slots;
        }
        self.file = next_file;
        self.capacity = next_capacity;
        self.occupied = migrated;
        self.tombstones = 0;
        Ok(())
    }
}

fn create_spill_file(root: &Path, capacity: usize) -> Result<File> {
    #[cfg(unix)]
    let file = tempfile::tempfile_in(root)?;
    #[cfg(not(unix))]
    let file = {
        let _ = root;
        tempfile::tempfile()?
    };
    let total_bytes = u64::try_from(capacity)
        .ok()
        .and_then(|slots| slots.checked_mul(SLOT_BYTES as u64))
        .ok_or(IndexError::CountOverflow)?;
    file.set_len(total_bytes)?;
    Ok(file)
}

fn insert_encoded_slot(
    file: &File,
    capacity: usize,
    uuid: &Uuid,
    encoded: &[u8],
) -> Result<(usize, usize)> {
    let mask = capacity - 1;
    let mut index = slot_hash(uuid) & mask;
    let mut header = [0_u8; 1 + 16];
    let target = uuid.as_bytes();
    let mut first_tombstone = None;
    for _ in 0..capacity {
        let offset = slot_offset(index)?;
        read_exact_at(file, &mut header, offset)?;
        match header[OFFSET_STATE] {
            SLOT_EMPTY => {
                if let Some(tombstone_index) = first_tombstone {
                    write_all_at(file, encoded, slot_offset(tombstone_index)?)?;
                    return Ok((1, 1));
                }
                write_all_at(file, encoded, offset)?;
                return Ok((1, 0));
            }
            SLOT_TOMBSTONE => {
                if first_tombstone.is_none() {
                    first_tombstone = Some(index);
                }
            }
            SLOT_OCCUPIED => {
                if &header[OFFSET_UUID..OFFSET_UUID + 16] == target {
                    write_all_at(file, encoded, offset)?;
                    return Ok((0, 0));
                }
            }
            _ => {
                return Err(IndexError::WriterInvariant(
                    "changed-session spill slot has invalid state",
                ));
            }
        }
        index = (index + 1) & mask;
    }
    if let Some(tombstone_index) = first_tombstone {
        write_all_at(file, encoded, slot_offset(tombstone_index)?)?;
        return Ok((1, 1));
    }
    Err(IndexError::WriterInvariant(
        "changed-session spill hash table exhausted capacity",
    ))
}

fn slot_offset(index: usize) -> Result<u64> {
    u64::try_from(index)
        .ok()
        .and_then(|slot| slot.checked_mul(SLOT_BYTES as u64))
        .ok_or(IndexError::CountOverflow)
}

fn slot_hash(uuid: &Uuid) -> usize {
    let bytes = uuid.as_bytes();
    let lo = u64::from_le_bytes(bytes[..8].try_into().expect("uuid low bytes"));
    let hi = u64::from_le_bytes(bytes[8..].try_into().expect("uuid high bytes"));
    let mut x = lo ^ hi.rotate_left(31);
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^= x >> 31;
    x as usize
}

fn encode_slot(uuid: Uuid, facts: PreparedSessionIdentityFacts) -> Result<[u8; SLOT_BYTES]> {
    let mut slot = [0_u8; SLOT_BYTES];
    slot[OFFSET_STATE] = SLOT_OCCUPIED;
    slot[OFFSET_UUID..OFFSET_SOURCE_OWNER].copy_from_slice(uuid.as_bytes());
    slot[OFFSET_SOURCE_OWNER..OFFSET_FLAGS].copy_from_slice(&facts.source_owner);

    let mut flags = encode_relationship_kind(facts.relationship.kind) << FLAG_KIND_SHIFT;
    let session_bytes = facts.session_id.encode_canonical()?;
    slot[OFFSET_SESSION_ID..OFFSET_PARENT_SESSION_ID].copy_from_slice(&session_bytes);
    if let Some(parent) = facts.relationship.parent_session_id {
        flags |= FLAG_HAS_PARENT;
        let parent_bytes = parent.encode_canonical()?;
        slot[OFFSET_PARENT_SESSION_ID..OFFSET_ROOT_SESSION_ID].copy_from_slice(&parent_bytes);
    }
    if let Some(root) = facts.relationship.root_session_id {
        flags |= FLAG_HAS_ROOT;
        let root_bytes = root.encode_canonical()?;
        slot[OFFSET_ROOT_SESSION_ID..SLOT_BYTES].copy_from_slice(&root_bytes);
    }
    slot[OFFSET_FLAGS] = flags;
    Ok(slot)
}

fn decode_slot(slot: &[u8; SLOT_BYTES]) -> Result<(Uuid, PreparedSessionIdentityFacts)> {
    if slot[OFFSET_STATE] != SLOT_OCCUPIED {
        return Err(IndexError::WriterInvariant(
            "decoded empty changed-session spill slot",
        ));
    }
    let flags = slot[OFFSET_FLAGS];
    if flags & !FLAG_VALID_MASK != 0 {
        return Err(IndexError::WriterInvariant(
            "changed-session spill slot has invalid flags",
        ));
    }
    let mut uuid_bytes = [0_u8; 16];
    uuid_bytes.copy_from_slice(&slot[OFFSET_UUID..OFFSET_SOURCE_OWNER]);
    let uuid = Uuid::from_bytes(uuid_bytes);

    let mut source_owner = [0_u8; 32];
    source_owner.copy_from_slice(&slot[OFFSET_SOURCE_OWNER..OFFSET_FLAGS]);

    let session_id =
        StableEntityId::decode_canonical(&slot[OFFSET_SESSION_ID..OFFSET_PARENT_SESSION_ID])?;
    let parent_slice = &slot[OFFSET_PARENT_SESSION_ID..OFFSET_ROOT_SESSION_ID];
    let parent_session_id = if flags & FLAG_HAS_PARENT != 0 {
        Some(StableEntityId::decode_canonical(parent_slice)?)
    } else if parent_slice.iter().all(|byte| *byte == 0) {
        None
    } else {
        return Err(IndexError::WriterInvariant(
            "changed-session spill slot has non-zero absent parent",
        ));
    };
    let root_slice = &slot[OFFSET_ROOT_SESSION_ID..SLOT_BYTES];
    let root_session_id = if flags & FLAG_HAS_ROOT != 0 {
        Some(StableEntityId::decode_canonical(root_slice)?)
    } else if root_slice.iter().all(|byte| *byte == 0) {
        None
    } else {
        return Err(IndexError::WriterInvariant(
            "changed-session spill slot has non-zero absent root",
        ));
    };
    let kind = decode_relationship_kind((flags >> FLAG_KIND_SHIFT) & FLAG_KIND_MASK)?;
    Ok((
        uuid,
        PreparedSessionIdentityFacts {
            session_id,
            source_owner,
            relationship: PreparedSessionRelationship {
                parent_session_id,
                root_session_id,
                kind,
            },
        },
    ))
}

fn encode_relationship_kind(kind: Option<ProviderNativeSessionRelationship>) -> u8 {
    match kind {
        None => 0,
        Some(ProviderNativeSessionRelationship::Root) => 1,
        Some(ProviderNativeSessionRelationship::Delegated) => 2,
        Some(ProviderNativeSessionRelationship::Forked) => 3,
        Some(ProviderNativeSessionRelationship::ResumedFrom) => 4,
        Some(ProviderNativeSessionRelationship::WorkflowChild) => 5,
    }
}

fn decode_relationship_kind(tag: u8) -> Result<Option<ProviderNativeSessionRelationship>> {
    match tag {
        0 => Ok(None),
        1 => Ok(Some(ProviderNativeSessionRelationship::Root)),
        2 => Ok(Some(ProviderNativeSessionRelationship::Delegated)),
        3 => Ok(Some(ProviderNativeSessionRelationship::Forked)),
        4 => Ok(Some(ProviderNativeSessionRelationship::ResumedFrom)),
        5 => Ok(Some(ProviderNativeSessionRelationship::WorkflowChild)),
        _ => Err(IndexError::WriterInvariant(
            "changed-session spill slot has invalid relationship kind",
        )),
    }
}

#[cfg(unix)]
fn read_exact_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    let mut filled = 0;
    while filled < buffer.len() {
        let read = file.read_at(&mut buffer[filled..], offset + filled as u64)?;
        if read == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        filled += read;
    }
    Ok(())
}

#[cfg(windows)]
fn read_exact_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    let mut filled = 0;
    while filled < buffer.len() {
        let read = file.seek_read(&mut buffer[filled..], offset + filled as u64)?;
        if read == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        filled += read;
    }
    Ok(())
}

#[cfg(unix)]
fn write_at(file: &File, buffer: &[u8], offset: u64) -> io::Result<usize> {
    use std::os::unix::fs::FileExt;
    file.write_at(buffer, offset)
}

#[cfg(windows)]
fn write_at(file: &File, buffer: &[u8], offset: u64) -> io::Result<usize> {
    use std::os::windows::fs::FileExt;
    file.seek_write(buffer, offset)
}

fn write_all_at(file: &File, mut buffer: &[u8], mut offset: u64) -> io::Result<()> {
    while !buffer.is_empty() {
        let written = write_at(file, buffer, offset)?;
        if written == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        buffer = &buffer[written..];
        offset = offset
            .checked_add(written as u64)
            .ok_or_else(|| io::Error::other("changed-session spill offset overflow"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use ctx_history_core::{
        derive_session_id, NativeSessionKey, SessionIdentityInput, SourceKey, TypedKey,
    };
    use tempfile::tempdir;

    use super::*;

    fn test_source() -> SourceKey {
        SourceKey::derive_provider_native(
            "test-provider",
            "jsonl",
            "v1",
            1,
            "local",
            TypedKey::utf8("sessions.jsonl").unwrap(),
        )
        .unwrap()
    }

    fn test_session(source: &SourceKey, label: &str) -> StableEntityId {
        let key = NativeSessionKey::native_id("session", TypedKey::utf8(label).unwrap()).unwrap();
        derive_session_id(SessionIdentityInput {
            source,
            logical_session_kind: "thread",
            native_session_key: &key,
        })
        .unwrap()
    }

    fn test_facts(
        source: &SourceKey,
        index: usize,
        kind: Option<ProviderNativeSessionRelationship>,
        with_lineage: bool,
    ) -> PreparedSessionIdentityFacts {
        let session_id = test_session(source, &format!("session-{index}"));
        let parent_session_id =
            with_lineage.then(|| test_session(source, &format!("parent-{index}")));
        let root_session_id = with_lineage.then(|| test_session(source, &format!("root-{index}")));
        PreparedSessionIdentityFacts {
            session_id,
            source_owner: source.identity().digest(),
            relationship: PreparedSessionRelationship {
                parent_session_id,
                root_session_id,
                kind,
            },
        }
    }

    #[test]
    fn spill_preserves_roundtrip_rehash_updates_and_rollback_across_boundaries() {
        let temp = tempdir().unwrap();
        let source = test_source();
        let mut registry = ChangedSessionRegistry::with_hot_capacity(temp.path().to_path_buf(), 8);
        let kinds = [
            None,
            Some(ProviderNativeSessionRelationship::Root),
            Some(ProviderNativeSessionRelationship::Delegated),
            Some(ProviderNativeSessionRelationship::Forked),
            Some(ProviderNativeSessionRelationship::ResumedFrom),
            Some(ProviderNativeSessionRelationship::WorkflowChild),
        ];

        let mut expected = HashMap::new();
        for ordinal in 0..320 {
            let facts = test_facts(
                &source,
                ordinal,
                kinds[ordinal % kinds.len()],
                ordinal % 3 == 0,
            );
            let uuid = facts.session_id.as_uuid();
            registry.insert_new(uuid, facts).unwrap();
            expected.insert(uuid, facts);
            assert!(
                registry.hot_len() <= 8,
                "hot map must remain bounded at hot_max_entries"
            );
        }
        assert!(registry.has_spilled());
        assert_eq!(registry.len(), expected.len());
        assert_eq!(
            std::fs::read_dir(temp.path()).unwrap().count(),
            0,
            "anonymous spill file must not leave visible entries in index root"
        );

        for (uuid, facts) in &expected {
            assert_eq!(registry.get(uuid).unwrap(), Some(*facts));
        }

        // Update a spilled entry, spill again, then roll it back via restore().
        let first_uuid = *expected.keys().next().unwrap();
        let original = expected[&first_uuid];
        let mut upgraded = original;
        upgraded.relationship.kind = Some(ProviderNativeSessionRelationship::WorkflowChild);
        upgraded.relationship.parent_session_id = Some(test_session(&source, "upgraded-parent"));
        let prior = registry.update_existing(first_uuid, upgraded).unwrap();
        assert_eq!(prior, original);
        assert_eq!(registry.get(&first_uuid).unwrap(), Some(upgraded));
        assert_eq!(registry.len(), expected.len());

        // Insert more entries to force the updated entry to flush into spill,
        // then restore and remove it as a route rollback would.
        let mut rollback_insertions = Vec::new();
        for ordinal in 320..340 {
            let facts = test_facts(&source, ordinal, None, false);
            let uuid = facts.session_id.as_uuid();
            registry.insert_new(uuid, facts).unwrap();
            rollback_insertions.push(uuid);
        }
        registry.restore(first_uuid, prior).unwrap();
        assert_eq!(registry.get(&first_uuid).unwrap(), Some(original));
        for uuid in rollback_insertions {
            assert!(registry.remove(&uuid).unwrap());
            assert_eq!(registry.get(&uuid).unwrap(), None);
        }
        assert_eq!(registry.len(), expected.len());
        assert_eq!(registry.get(&first_uuid).unwrap(), Some(original));
    }
}
