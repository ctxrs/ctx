#[allow(unused_imports)]
use super::*;

pub(crate) fn short_uuid(id: Uuid) -> String {
    id.to_string().chars().take(8).collect()
}
