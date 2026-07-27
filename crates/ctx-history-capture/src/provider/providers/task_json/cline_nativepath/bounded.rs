use serde::{Deserialize, Deserializer};
use serde_json::value::RawValue;

/// A string whose raw JSON token is bounded before it is unescaped or copied.
///
/// NativePath readers deserialize from one bounded item/control-file backing
/// buffer, so `RawValue` can borrow the token and enforce the bound before an
/// escape-amplified value allocates.
pub(super) struct BoundedString<const MAX_BYTES: usize>(pub(super) Option<String>, pub(super) bool);

impl<'de, const MAX_BYTES: usize> Deserialize<'de> for BoundedString<MAX_BYTES> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = <&RawValue>::deserialize(deserializer)?;
        parse_bounded_raw_string(raw.get())
    }
}

fn parse_bounded_raw_string<const MAX_BYTES: usize, E: serde::de::Error>(
    raw: &str,
) -> Result<BoundedString<MAX_BYTES>, E> {
    if !raw.starts_with('"') {
        return Ok(BoundedString(None, false));
    }
    // A decoded byte can occupy at most six JSON bytes (`\u00xx`), plus the
    // surrounding quotes. This bounds escape expansion before allocation
    // while making MAX_BYTES the actual owned-string limit.
    if raw.len() > MAX_BYTES.saturating_mul(6).saturating_add(2) {
        return Ok(BoundedString(None, true));
    }
    serde_json::from_str::<String>(raw)
        .map(|value| {
            if value.len() > MAX_BYTES {
                BoundedString(None, true)
            } else {
                BoundedString(Some(value), false)
            }
        })
        .map_err(E::custom)
}
