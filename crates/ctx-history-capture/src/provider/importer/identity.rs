use std::collections::BTreeMap;

use ctx_history_core::{CaptureProvider, Event};
use ctx_history_store::{Store, StoreError};
use serde_json::Value;
use uuid::Uuid;

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
    if !caches
        .pi_event_identities_by_entry_id
        .contains_key(&session_id)
    {
        let mut identities = BTreeMap::new();
        for event in store.events_for_session(session_id)? {
            let Some(existing_entry_id) = pi_stored_event_entry_id(&event) else {
                continue;
            };
            let Some(dedupe_key) = event.dedupe_key.clone() else {
                continue;
            };
            identities
                .entry(existing_entry_id.to_owned())
                .or_insert(ProviderEventImportIdentity {
                    id: event.id,
                    seq: event.seq,
                    dedupe_key,
                    run_source_id: event.capture_source_id,
                });
        }
        caches
            .pi_event_identities_by_entry_id
            .insert(session_id, identities);
    }
    Ok(caches
        .pi_event_identities_by_entry_id
        .get(&session_id)
        .and_then(|identities| identities.get(entry_id).cloned()))
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

pub(crate) fn provider_event_import_identity(
    store: &Store,
    provider: CaptureProvider,
    provider_session_id: &str,
    source_id: Uuid,
    provider_event_index: u64,
    provider_event_sequence_index: u64,
    event_hash: &str,
    legacy_provider_event_index: Option<u64>,
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

    let legacy_identity = provider_legacy_event_import_identity(
        provider,
        provider_session_id,
        provider_event_index,
        event_hash,
    );
    if provider_event_exists(store, &legacy_identity.dedupe_key)?
        || provider_event_id_exists(store, legacy_identity.id)?
    {
        Ok(legacy_identity)
    } else {
        Ok(source_identity)
    }
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
) -> Result<Option<Uuid>> {
    let source_event_id = provider_source_event_uuid(source_id, provider_event_index);
    if provider_event_id_exists(store, source_event_id)? {
        return Ok(Some(source_event_id));
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
) -> Result<Uuid> {
    let source_touch_id = provider_source_file_touch_uuid(source_id, provider_touch_index);
    if store.file_touched_exists(source_touch_id)? {
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
