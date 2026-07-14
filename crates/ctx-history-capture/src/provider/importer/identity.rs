use std::collections::BTreeMap;

use ctx_history_core::{CaptureProvider, Event};
use ctx_history_store::{Store, StoreError};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use uuid::Uuid;

use crate::common::scratch::CaptureScratchSpace;
use crate::{CaptureError, Result};

use super::ids::{
    provider_event_seq, provider_event_uuid, provider_file_touch_uuid, provider_source_event_seq,
    provider_source_event_uuid, provider_source_file_touch_uuid,
};
use super::ProviderImportCaches;

pub(crate) fn provider_event_exists(store: &Store, dedupe_key: &str) -> Result<bool> {
    match store.event_id_by_dedupe_key(dedupe_key) {
        Ok(_) => Ok(true),
        Err(StoreError::Sql(rusqlite::Error::QueryReturnedNoRows)) => Ok(false),
        Err(err) => Err(CaptureError::Store(err)),
    }
}

#[derive(Clone)]
pub(crate) struct ProviderEventImportIdentity {
    pub(crate) id: Uuid,
    pub(crate) seq: u64,
    pub(crate) dedupe_key: String,
    pub(crate) run_source_id: Option<Uuid>,
}

const PI_IDENTITY_LOAD_BATCH: usize = 128;

pub(crate) struct ProviderPiEventIdentityInventory {
    connection: Connection,
    _scratch: CaptureScratchSpace,
    max_load_batch: usize,
}

impl ProviderPiEventIdentityInventory {
    fn new() -> Result<Self> {
        let scratch = CaptureScratchSpace::create("pi-event-identities")?;
        drop(scratch.create_file("identities.sqlite")?);
        let connection = Connection::open(scratch.path().join("identities.sqlite"))?;
        connection.execute_batch(
            "CREATE TABLE loaded_sessions (session_id TEXT PRIMARY KEY NOT NULL) WITHOUT ROWID;
             CREATE TABLE identities (
                 session_id TEXT NOT NULL,
                 entry_id TEXT NOT NULL,
                 event_id TEXT NOT NULL,
                 seq INTEGER NOT NULL,
                 dedupe_key TEXT NOT NULL,
                 run_source_id TEXT,
                 PRIMARY KEY (session_id, entry_id)
             ) WITHOUT ROWID;",
        )?;
        Ok(Self {
            connection,
            _scratch: scratch,
            max_load_batch: 0,
        })
    }

    pub(crate) fn max_load_batch(&self) -> usize {
        self.max_load_batch
    }

    fn lookup(
        &mut self,
        store: &Store,
        session_id: Uuid,
        entry_id: &str,
    ) -> Result<Option<ProviderEventImportIdentity>> {
        let session_key = session_id.to_string();
        let loaded = self
            .connection
            .query_row(
                "SELECT 1 FROM loaded_sessions WHERE session_id = ?1",
                params![&session_key],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !loaded {
            self.load_session(store, session_id, &session_key)?;
        }
        self.connection
            .query_row(
                "SELECT event_id, seq, dedupe_key, run_source_id
                 FROM identities WHERE session_id = ?1 AND entry_id = ?2",
                params![session_key, entry_id],
                |row| {
                    let event_id = row.get::<_, String>(0)?;
                    let seq = row.get::<_, i64>(1)?;
                    let run_source_id = row.get::<_, Option<String>>(3)?;
                    Ok((event_id, seq, row.get::<_, String>(2)?, run_source_id))
                },
            )
            .optional()?
            .map(|(event_id, seq, dedupe_key, run_source_id)| {
                Ok(ProviderEventImportIdentity {
                    id: Uuid::parse_str(&event_id).map_err(|_| {
                        CaptureError::SystemInvariant(
                            "Pi identity inventory contains an invalid event ID",
                        )
                    })?,
                    seq: u64::try_from(seq).map_err(|_| {
                        CaptureError::SystemInvariant(
                            "Pi identity inventory contains an invalid sequence",
                        )
                    })?,
                    dedupe_key,
                    run_source_id: run_source_id
                        .map(|id| Uuid::parse_str(&id))
                        .transpose()
                        .map_err(|_| {
                            CaptureError::SystemInvariant(
                                "Pi identity inventory contains an invalid run source ID",
                            )
                        })?,
                })
            })
            .transpose()
    }

    fn load_session(&mut self, store: &Store, session_id: Uuid, session_key: &str) -> Result<()> {
        let mut events = store.events_for_session_limited(session_id, PI_IDENTITY_LOAD_BATCH)?;
        loop {
            self.max_load_batch = self.max_load_batch.max(events.len());
            for event in &events {
                let Some(entry_id) = pi_stored_event_entry_id(event) else {
                    continue;
                };
                let Some(dedupe_key) = event.dedupe_key.as_deref() else {
                    continue;
                };
                self.connection.execute(
                    "INSERT OR IGNORE INTO identities
                     (session_id, entry_id, event_id, seq, dedupe_key, run_source_id)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        session_key,
                        entry_id,
                        event.id.to_string(),
                        i64::try_from(event.seq).unwrap_or(i64::MAX),
                        dedupe_key,
                        event.capture_source_id.map(|id| id.to_string()),
                    ],
                )?;
            }
            let Some(anchor) = events.last() else {
                break;
            };
            let mut next = store.events_for_session_window(anchor, 0, PI_IDENTITY_LOAD_BATCH)?;
            if !next.is_empty() {
                next.remove(0);
            }
            if next.is_empty() {
                break;
            }
            events = next;
        }
        self.connection.execute(
            "INSERT OR IGNORE INTO loaded_sessions (session_id) VALUES (?1)",
            params![session_key],
        )?;
        Ok(())
    }
}

pub(crate) fn pi_existing_event_identity_by_entry_id(
    store: &Store,
    provider: CaptureProvider,
    session_id: Uuid,
    entry_id: Option<&str>,
    caches: &mut ProviderImportCaches,
) -> Result<Option<ProviderEventImportIdentity>> {
    if provider != CaptureProvider::Pi {
        return Ok(None);
    }
    let Some(entry_id) = entry_id.filter(|id| !id.trim().is_empty()) else {
        return Ok(None);
    };
    if caches.pi_event_identities.is_none() {
        caches.pi_event_identities = Some(ProviderPiEventIdentityInventory::new()?);
    }
    caches
        .pi_event_identities
        .as_mut()
        .expect("Pi identity inventory was initialized")
        .lookup(store, session_id, entry_id)
}

pub(crate) fn pi_stored_event_entry_id(event: &Event) -> Option<&str> {
    event
        .payload
        .pointer("/body/entry_id")
        .and_then(Value::as_str)
        .or_else(|| {
            event
                .payload
                .pointer("/body/body/id")
                .and_then(Value::as_str)
        })
        .or_else(|| {
            event
                .sync
                .metadata
                .pointer("/metadata/entry_id")
                .and_then(Value::as_str)
        })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn provider_event_import_identity(
    store: &Store,
    provider: CaptureProvider,
    provider_session_id: &str,
    source_id: Uuid,
    provider_event_index: u64,
    provider_event_sequence_index: u64,
    event_hash: &str,
    legacy_provider_event_index: Option<u64>,
    allow_legacy_provider_identity: bool,
) -> Result<ProviderEventImportIdentity> {
    let source_identity = provider_source_event_import_identity_with_seq(
        source_id,
        provider_event_index,
        provider_event_sequence_index,
        event_hash,
    );
    let source_identity = avoid_provider_source_event_seq_collision(
        store,
        source_identity,
        source_id,
        provider_event_index,
        provider_event_sequence_index,
    )?;
    if provider_event_exists(store, &source_identity.dedupe_key)?
        || provider_event_id_exists(store, source_identity.id)?
    {
        return Ok(source_identity);
    }

    if allow_legacy_provider_identity {
        if let Some(legacy_index) = legacy_provider_event_index {
            let legacy_source_identity =
                provider_source_event_import_identity(source_id, legacy_index, event_hash);
            if provider_event_exists(store, &legacy_source_identity.dedupe_key)?
                || provider_event_id_exists(store, legacy_source_identity.id)?
            {
                return Ok(legacy_source_identity);
            }

            let legacy_provider_identity = provider_legacy_event_import_identity(
                provider,
                provider_session_id,
                legacy_index,
                event_hash,
            );
            if provider_event_exists(store, &legacy_provider_identity.dedupe_key)?
                || provider_event_id_exists(store, legacy_provider_identity.id)?
            {
                return Ok(legacy_provider_identity);
            }
        }
    }

    if allow_legacy_provider_identity {
        let legacy_identity = provider_legacy_event_import_identity(
            provider,
            provider_session_id,
            provider_event_index,
            event_hash,
        );
        if provider_event_exists(store, &legacy_identity.dedupe_key)?
            || provider_event_id_exists(store, legacy_identity.id)?
        {
            return Ok(legacy_identity);
        }
    }

    Ok(source_identity)
}

pub(crate) fn provider_source_event_import_identity(
    source_id: Uuid,
    provider_event_index: u64,
    event_hash: &str,
) -> ProviderEventImportIdentity {
    provider_source_event_import_identity_with_seq(
        source_id,
        provider_event_index,
        provider_event_index,
        event_hash,
    )
}

pub(crate) fn provider_source_event_import_identity_with_seq(
    source_id: Uuid,
    provider_event_index: u64,
    provider_event_sequence_index: u64,
    event_hash: &str,
) -> ProviderEventImportIdentity {
    ProviderEventImportIdentity {
        id: provider_source_event_uuid(source_id, provider_event_index),
        seq: provider_source_event_seq(source_id, provider_event_sequence_index),
        dedupe_key: Store::provider_source_event_dedupe_key(
            source_id,
            provider_event_index,
            event_hash,
        ),
        run_source_id: Some(source_id),
    }
}

pub(crate) fn avoid_provider_source_event_seq_collision(
    store: &Store,
    mut identity: ProviderEventImportIdentity,
    source_id: Uuid,
    provider_event_index: u64,
    provider_event_sequence_index: u64,
) -> Result<ProviderEventImportIdentity> {
    if provider_event_seq_available(store, identity.seq, identity.id)? {
        return Ok(identity);
    }

    for candidate in [
        provider_event_sequence_index ^ 0x0008_0000,
        provider_event_index,
        provider_event_index ^ 0x0008_0000,
    ] {
        let seq = provider_source_event_seq(source_id, candidate);
        if provider_event_seq_available(store, seq, identity.id)? {
            identity.seq = seq;
            return Ok(identity);
        }
    }

    for salt in 1..1024 {
        let candidate = provider_event_sequence_index.wrapping_add(salt) & 0x000f_ffff;
        let seq = provider_source_event_seq(source_id, candidate);
        if provider_event_seq_available(store, seq, identity.id)? {
            identity.seq = seq;
            return Ok(identity);
        }
    }

    Ok(identity)
}

pub(crate) fn provider_event_seq_available(
    store: &Store,
    seq: u64,
    event_id: Uuid,
) -> Result<bool> {
    match store.event_id_by_seq(seq) {
        Ok(existing_id) => Ok(existing_id == event_id),
        Err(StoreError::Sql(rusqlite::Error::QueryReturnedNoRows)) => Ok(true),
        Err(err) => Err(CaptureError::Store(err)),
    }
}

pub(crate) fn provider_legacy_event_import_identity(
    provider: CaptureProvider,
    provider_session_id: &str,
    provider_event_index: u64,
    event_hash: &str,
) -> ProviderEventImportIdentity {
    ProviderEventImportIdentity {
        id: provider_event_uuid(provider, provider_session_id, provider_event_index),
        seq: provider_event_seq(provider, provider_session_id, provider_event_index),
        dedupe_key: Store::provider_event_dedupe_key(
            provider,
            provider_session_id,
            provider_event_index,
            event_hash,
        ),
        run_source_id: None,
    }
}

pub(crate) fn provider_file_touch_event_id(
    store: &Store,
    provider: CaptureProvider,
    provider_session_id: &str,
    source_id: Uuid,
    provider_event_index: u64,
    allow_legacy_provider_identity: bool,
) -> Result<Option<Uuid>> {
    let source_event_id = provider_source_event_uuid(source_id, provider_event_index);
    if provider_event_id_exists(store, source_event_id)? {
        return Ok(Some(source_event_id));
    }

    if !allow_legacy_provider_identity {
        return Ok(None);
    }
    let legacy_event_id = provider_event_uuid(provider, provider_session_id, provider_event_index);
    if provider_event_id_exists(store, legacy_event_id)? {
        Ok(Some(legacy_event_id))
    } else {
        Ok(None)
    }
}

pub(crate) fn provider_file_touch_import_id(
    store: &Store,
    provider: CaptureProvider,
    provider_session_id: &str,
    source_id: Uuid,
    provider_touch_index: u64,
    allow_legacy_provider_identity: bool,
) -> Result<Uuid> {
    let source_touch_id = provider_source_file_touch_uuid(source_id, provider_touch_index);
    if store.file_touched_exists(source_touch_id)? {
        return Ok(source_touch_id);
    }

    if !allow_legacy_provider_identity {
        return Ok(source_touch_id);
    }
    let legacy_touch_id =
        provider_file_touch_uuid(provider, provider_session_id, provider_touch_index);
    if store.file_touched_exists(legacy_touch_id)? {
        Ok(legacy_touch_id)
    } else {
        Ok(source_touch_id)
    }
}

pub(crate) fn provider_event_id_exists(store: &Store, id: Uuid) -> Result<bool> {
    match store.get_event(id) {
        Ok(_) => Ok(true),
        Err(StoreError::NotFound(_)) => Ok(false),
        Err(err) => Err(CaptureError::Store(err)),
    }
}

pub(crate) fn provider_session_exists(store: &Store, session_id: Uuid) -> Result<bool> {
    match store.get_session(session_id) {
        Ok(_) => Ok(true),
        Err(StoreError::NotFound(_)) => Ok(false),
        Err(err) => Err(CaptureError::Store(err)),
    }
}

pub(crate) fn provider_session_exists_cached(
    store: &Store,
    session_id: Uuid,
    cache: &mut BTreeMap<Uuid, bool>,
) -> Result<bool> {
    if let Some(exists) = cache.get(&session_id) {
        return Ok(*exists);
    }
    let exists = provider_session_exists(store, session_id)?;
    cache.insert(session_id, exists);
    Ok(exists)
}
