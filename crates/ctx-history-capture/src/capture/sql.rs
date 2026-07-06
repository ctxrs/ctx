#[allow(unused_imports)]
use super::*;

pub(crate) fn provider_event_exists(store: &Store, dedupe_key: &str) -> Result<bool> {
    match store.event_id_by_dedupe_key(dedupe_key) {
        Ok(_) => Ok(true),
        Err(StoreError::Sql(rusqlite::Error::QueryReturnedNoRows)) => Ok(false),
        Err(err) => Err(CaptureError::Store(err)),
    }
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
