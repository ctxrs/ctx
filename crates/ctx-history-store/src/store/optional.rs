#[allow(unused_imports)]
use super::*;

pub(crate) fn optional_uuid_string(id: Option<Uuid>) -> Option<String> {
    id.map(|id| id.to_string())
}
