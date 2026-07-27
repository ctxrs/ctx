use std::collections::BTreeMap;

use ctx_history_core::{CaptureProvider, Event};
use ctx_history_store::{Store, StoreError};
use serde_json::Value;
use uuid::Uuid;

use crate::{CaptureError, Result};

use super::ids::{
    provider_event_file_touch_uuid, provider_event_seq, provider_event_uuid,
    provider_source_event_file_touch_uuid, provider_source_event_seq, provider_source_event_uuid,
};
use super::ProviderImportCaches;

pub(crate) fn provider_event_exists(store: &Store, dedupe_key: &str) -> Result<bool> {
    match store.event_id_by_dedupe_key(dedupe_key) {
        Ok(_) => Ok(true),
        Err(StoreError::Sql(rusqlite::Error::QueryReturnedNoRows)) => Ok(false),
        Err(err) => Err(CaptureError::Store(err)),
    }
}

fn provider_event_by_id(store: &Store, id: Uuid) -> Result<Option<Event>> {
    match store.get_event(id) {
        Ok(event) => Ok(Some(event)),
        Err(StoreError::NotFound(_)) => Ok(None),
        Err(err) => Err(CaptureError::Store(err)),
    }
}

fn provider_event_identity_by_id(
    store: &Store,
    id: Uuid,
) -> Result<Option<ProviderEventImportIdentity>> {
    Ok(provider_event_by_id(store, id)?.and_then(|event| {
        event
            .dedupe_key
            .map(|dedupe_key| ProviderEventImportIdentity {
                id: event.id,
                seq: event.seq,
                dedupe_key,
                run_source_id: event.capture_source_id,
            })
    }))
}

fn provider_event_identity_by_alias(
    store: &Store,
    alias_id: Uuid,
) -> Result<Option<ProviderEventImportIdentity>> {
    let Some(event_id) = store.event_alias_target_id(alias_id)? else {
        return Ok(None);
    };
    provider_event_identity_by_id(store, event_id)
}

fn provider_event_identity_by_dedupe_key_and_source_path(
    store: &Store,
    dedupe_key: &str,
    source_id: Uuid,
) -> Result<Option<ProviderEventImportIdentity>> {
    let event_id = match store.event_id_by_dedupe_key(dedupe_key) {
        Ok(event_id) => event_id,
        Err(StoreError::Sql(rusqlite::Error::QueryReturnedNoRows)) => return Ok(None),
        Err(err) => return Err(CaptureError::Store(err)),
    };
    let source = store.get_capture_source(source_id)?;
    let Some(incoming_path) = source.descriptor.raw_source_path.as_deref() else {
        return Ok(None);
    };
    let Some(event) = provider_event_by_id(store, event_id)? else {
        return Ok(None);
    };
    if event
        .sync
        .metadata
        .pointer("/metadata/event_path")
        .and_then(Value::as_str)
        != Some(incoming_path)
    {
        return Ok(None);
    }
    Ok(event
        .dedupe_key
        .map(|dedupe_key| ProviderEventImportIdentity {
            id: event.id,
            seq: event.seq,
            dedupe_key,
            run_source_id: event.capture_source_id,
        }))
}

#[derive(Clone)]
pub(crate) struct ProviderEventImportIdentity {
    pub(crate) id: Uuid,
    pub(crate) seq: u64,
    pub(crate) dedupe_key: String,
    pub(crate) run_source_id: Option<Uuid>,
}

#[derive(Clone, Copy)]
pub(crate) struct ExactLegacySourceEventCandidate {
    pub(crate) source_id: Uuid,
    pub(crate) provider_event_index: u64,
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
    if let std::collections::btree_map::Entry::Vacant(entry) =
        caches.pi_event_identities_by_entry_id.entry(session_id)
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
        entry.insert(identities);
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

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
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
    provider_event_import_identity_with_exact_legacy_source(
        store,
        provider,
        provider_session_id,
        source_id,
        provider_event_index,
        provider_event_sequence_index,
        event_hash,
        None,
        legacy_provider_event_index,
        allow_legacy_provider_identity,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn provider_event_import_identity_with_exact_legacy_source(
    store: &Store,
    provider: CaptureProvider,
    provider_session_id: &str,
    source_id: Uuid,
    provider_event_index: u64,
    provider_event_sequence_index: u64,
    event_hash: &str,
    exact_legacy_source: Option<ExactLegacySourceEventCandidate>,
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
    if provider_event_exists(store, &source_identity.dedupe_key)? {
        return Ok(source_identity);
    }
    if let Some(candidate) = exact_legacy_source {
        // The ordinal is advisory. Only an exact old source/index/native-hash
        // dedupe key and canonical event path can prove the same record.
        let legacy_source_identity = provider_source_event_import_identity(
            candidate.source_id,
            candidate.provider_event_index,
            event_hash,
        );
        if let Some(existing) = provider_event_identity_by_dedupe_key_and_source_path(
            store,
            &legacy_source_identity.dedupe_key,
            source_id,
        )? {
            return Ok(existing);
        }
    }
    if let Some(existing) = provider_event_identity_by_alias(store, source_identity.id)? {
        return Ok(existing);
    }
    if provider_event_id_exists(store, source_identity.id)? {
        return Ok(source_identity);
    }

    if allow_legacy_provider_identity {
        if let Some(legacy_index) = legacy_provider_event_index {
            let legacy_source_identity =
                provider_source_event_import_identity(source_id, legacy_index, event_hash);
            if provider_event_exists(store, &legacy_source_identity.dedupe_key)? {
                return Ok(legacy_source_identity);
            }
            if let Some(existing) =
                provider_event_identity_by_alias(store, legacy_source_identity.id)?
            {
                return Ok(existing);
            }
            if provider_event_id_exists(store, legacy_source_identity.id)? {
                return Ok(legacy_source_identity);
            }

            let legacy_provider_identity = provider_legacy_event_import_identity(
                provider,
                provider_session_id,
                legacy_index,
                event_hash,
            );
            if provider_event_exists(store, &legacy_provider_identity.dedupe_key)? {
                return Ok(legacy_provider_identity);
            }
            if let Some(existing) =
                provider_event_identity_by_alias(store, legacy_provider_identity.id)?
            {
                return Ok(existing);
            }
            if provider_event_id_exists(store, legacy_provider_identity.id)? {
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
        if provider_event_exists(store, &legacy_identity.dedupe_key)? {
            return Ok(legacy_identity);
        }
        if let Some(existing) = provider_event_identity_by_alias(store, legacy_identity.id)? {
            return Ok(existing);
        }
        if provider_event_id_exists(store, legacy_identity.id)? {
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
    if let Some(existing) = provider_event_by_id(store, source_event_id)? {
        return Ok(Some(existing.id));
    }

    if !allow_legacy_provider_identity {
        return Ok(None);
    }
    let legacy_event_id = provider_event_uuid(provider, provider_session_id, provider_event_index);
    if let Some(existing) = provider_event_by_id(store, legacy_event_id)? {
        Ok(Some(existing.id))
    } else {
        Ok(None)
    }
}

pub(crate) fn provider_file_touch_import_id(
    store: &Store,
    provider: CaptureProvider,
    provider_session_id: &str,
    source_id: Uuid,
    provider_event_index: Option<u64>,
    provider_touch_index: u64,
    allow_legacy_provider_identity: bool,
) -> Result<Uuid> {
    let source_touch_id = provider_source_event_file_touch_uuid(
        source_id,
        provider_event_index,
        provider_touch_index,
    );
    if store.file_touched_exists(source_touch_id)? {
        return Ok(source_touch_id);
    }

    if !allow_legacy_provider_identity {
        return Ok(source_touch_id);
    }
    let legacy_touch_id = provider_event_file_touch_uuid(
        provider,
        provider_session_id,
        provider_event_index,
        provider_touch_index,
    );
    if store.file_touched_exists(legacy_touch_id)? {
        Ok(legacy_touch_id)
    } else {
        Ok(source_touch_id)
    }
}

pub(crate) fn provider_event_id_exists(store: &Store, id: Uuid) -> Result<bool> {
    Ok(provider_event_by_id(store, id)?.is_some())
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

#[cfg(test)]
mod tests {
    use crate::test_support_paths::tempdir;
    use ctx_history_core::{EventRole, EventType, Fidelity};
    use serde_json::json;

    use super::super::ids::provider_sync_metadata;
    use super::*;

    #[test]
    fn full_width_openhands_touch_finds_its_exact_source_event() {
        let temp = tempdir().unwrap();
        let store = Store::open(temp.path().join("store.sqlite")).unwrap();
        let source_id = Uuid::parse_str("4ea89d63-c113-4fe8-93e5-12859eb2aac7").unwrap();
        let provider_event_index = 0xfedc_ba98_7654_3210;
        let event_id = provider_source_event_uuid(source_id, provider_event_index);
        store
            .upsert_event(&Event {
                id: event_id,
                seq: 1,
                history_record_id: None,
                session_id: None,
                run_id: None,
                event_type: EventType::ToolCall,
                role: Some(EventRole::Assistant),
                occurred_at: "2026-07-18T00:00:00Z".parse().unwrap(),
                capture_source_id: None,
                payload: json!({ "path": ".openhands-hash-event" }),
                payload_blob_id: None,
                dedupe_key: Some("full-width-openhands-event".to_owned()),
                sync: provider_sync_metadata(Fidelity::Imported, json!({})),
            })
            .unwrap();

        assert_eq!(
            provider_file_touch_event_id(
                &store,
                CaptureProvider::OpenHands,
                "hash-indexed-session",
                source_id,
                provider_event_index,
                false,
            )
            .unwrap(),
            Some(event_id)
        );
        assert_eq!(
            provider_file_touch_event_id(
                &store,
                CaptureProvider::OpenHands,
                "hash-indexed-session",
                source_id,
                0,
                false,
            )
            .unwrap(),
            None
        );
    }
}
