#[allow(unused_imports)]
use super::*;

pub(crate) fn indexed_history_item_count(store: &Store) -> Result<usize> {
    Ok(store.indexed_history_item_count()?)
}
