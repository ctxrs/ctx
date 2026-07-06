#[allow(unused_imports)]
use super::*;

pub(crate) fn parse_optional_uuid(value: Option<String>) -> rusqlite::Result<Option<Uuid>> {
    value.map(parse_uuid).transpose()
}
