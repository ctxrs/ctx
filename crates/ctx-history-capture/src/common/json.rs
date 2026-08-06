use std::{collections::BTreeSet, fmt};

use serde::{
    de::{MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer,
};
use serde_json::{json, Value};

/// Returns true only when `input` is one complete JSON value whose object keys
/// are unique at every depth. The visitor retains no payload, so callers can
/// establish structural authority without allocating a second value tree.
pub(crate) fn raw_object_keys_are_unique(input: &[u8]) -> bool {
    let mut deserializer = serde_json::Deserializer::from_slice(input);
    if UniqueJsonShape::deserialize(&mut deserializer).is_err() {
        return false;
    }
    deserializer.end().is_ok()
}

/// Parses one exact JSON value after the shared duplicate-key preflight.
pub(crate) fn exact_value(input: &str) -> Option<Value> {
    raw_object_keys_are_unique(input.as_bytes())
        .then(|| serde_json::from_str(input).ok())
        .flatten()
}

struct UniqueJsonShape;

impl<'de> Deserialize<'de> for UniqueJsonShape {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(UniqueJsonShapeVisitor)
    }
}

struct UniqueJsonShapeVisitor;

impl<'de> Visitor<'de> for UniqueJsonShapeVisitor {
    type Value = UniqueJsonShape;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JSON without duplicate object keys")
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
        Ok(UniqueJsonShape)
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
        Ok(UniqueJsonShape)
    }

    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
        Ok(UniqueJsonShape)
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
        Ok(UniqueJsonShape)
    }

    fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E> {
        Ok(UniqueJsonShape)
    }

    fn visit_string<E>(self, _value: String) -> Result<Self::Value, E> {
        Ok(UniqueJsonShape)
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(UniqueJsonShape)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(UniqueJsonShape)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<UniqueJsonShape>()?.is_some() {}
        Ok(UniqueJsonShape)
    }

    fn visit_map<A>(self, mut object: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut keys = BTreeSet::new();
        while let Some(key) = object.next_key::<String>()? {
            if !keys.insert(key) {
                return Err(serde::de::Error::custom("duplicate JSON object key"));
            }
            object.next_value::<UniqueJsonShape>()?;
        }
        Ok(UniqueJsonShape)
    }
}

pub(crate) fn default_metadata() -> Value {
    json!({})
}

#[cfg(test)]
mod tests {
    use super::{exact_value, raw_object_keys_are_unique};

    #[test]
    fn exact_json_authority_rejects_duplicate_escaped_and_incomplete_values() {
        let exact = br#"{"command":"ctx search exact","nested":{"key":1},"items":[null,true]}"#;
        assert!(raw_object_keys_are_unique(exact));
        assert!(exact_value(std::str::from_utf8(exact).unwrap()).is_some());
        assert!(!raw_object_keys_are_unique(
            br#"{"command":"ordinary","command":"ctx search secret"}"#,
        ));
        assert!(!raw_object_keys_are_unique(
            br#"{"input":{"command":"ordinary","comm\u0061nd":"ctx search secret"}}"#,
        ));
        assert!(!raw_object_keys_are_unique(br#"{"command":}"#));
        assert!(!raw_object_keys_are_unique(
            br#"{"command":"ctx search exact"} trailing"#,
        ));
    }
}
