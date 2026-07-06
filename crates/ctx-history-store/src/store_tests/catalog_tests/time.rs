#[allow(unused_imports)]
use super::*;

pub(crate) fn fixed_time() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-06-23T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

pub(crate) fn timestamps() -> EntityTimestamps {
    EntityTimestamps {
        created_at: fixed_time(),
        updated_at: fixed_time(),
    }
}
