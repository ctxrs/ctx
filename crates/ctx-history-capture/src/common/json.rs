use std::{collections::BTreeSet, fmt};

use serde::{
    de::{DeserializeSeed, MapAccess, SeqAccess, Visitor},
    Deserializer,
};
use serde_json::{json, Value};

/// Provider envelopes and tool payloads are far smaller than 65,536 object
/// members. Keeping this limit high avoids excluding legitimate shapes while
/// bounding the decoded keys retained by the structural-authority pass across
/// every nested object in one record.
const MAX_TOTAL_OBJECT_MEMBERS: usize = 65_536;

/// Returns true only when `input` is one complete JSON value whose object keys
/// are unique at every depth. The visitor retains no payload, so callers can
/// establish structural authority without allocating a second value tree.
/// Member-budget exhaustion returns false, leaving callers to ordinary
/// discovery instead of rejecting the provider record.
pub(crate) fn raw_object_keys_are_unique(input: &[u8]) -> bool {
    let mut deserializer = serde_json::Deserializer::from_slice(input);
    let mut remaining_object_members = MAX_TOTAL_OBJECT_MEMBERS;
    if (UniqueJsonShapeSeed {
        remaining_object_members: &mut remaining_object_members,
    })
    .deserialize(&mut deserializer)
    .is_err()
    {
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

struct UniqueJsonShapeSeed<'a> {
    remaining_object_members: &'a mut usize,
}

impl<'de> DeserializeSeed<'de> for UniqueJsonShapeSeed<'_> {
    type Value = UniqueJsonShape;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(UniqueJsonShapeVisitor {
            remaining_object_members: self.remaining_object_members,
        })
    }
}

struct UniqueJsonShapeVisitor<'a> {
    remaining_object_members: &'a mut usize,
}

impl<'de> Visitor<'de> for UniqueJsonShapeVisitor<'_> {
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
        while sequence
            .next_element_seed(UniqueJsonShapeSeed {
                remaining_object_members: &mut *self.remaining_object_members,
            })?
            .is_some()
        {}
        Ok(UniqueJsonShape)
    }

    fn visit_map<A>(self, mut object: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut keys = BTreeSet::new();
        while let Some(key) = object.next_key::<String>()? {
            let Some(remaining_object_members) = self.remaining_object_members.checked_sub(1)
            else {
                return Err(serde::de::Error::custom(
                    "JSON object member limit exceeded",
                ));
            };
            *self.remaining_object_members = remaining_object_members;
            if !keys.insert(key) {
                return Err(serde::de::Error::custom("duplicate JSON object key"));
            }
            object.next_value_seed(UniqueJsonShapeSeed {
                remaining_object_members: &mut *self.remaining_object_members,
            })?;
        }
        Ok(UniqueJsonShape)
    }
}

pub(crate) fn default_metadata() -> Value {
    json!({})
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::{exact_value, raw_object_keys_are_unique, MAX_TOTAL_OBJECT_MEMBERS};

    fn nested_objects_with_total_members(total_member_count: usize) -> String {
        const MEMBERS_PER_NESTED_OBJECT: usize = 256;

        assert!(total_member_count >= 1);
        let nested_member_count = total_member_count - 1;
        let mut input =
            String::with_capacity(total_member_count.saturating_mul(14).saturating_add(16));
        input.push_str("{\"groups\":[");
        let mut written_members = 0;
        while written_members < nested_member_count {
            if written_members != 0 {
                input.push(',');
            }
            input.push('{');
            let members_in_object =
                (nested_member_count - written_members).min(MEMBERS_PER_NESTED_OBJECT);
            for member in 0..members_in_object {
                if member != 0 {
                    input.push(',');
                }
                write!(input, "\"key{member}\":null").unwrap();
            }
            input.push('}');
            written_members += members_in_object;
        }
        input.push_str("]}");
        input
    }

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

    #[test]
    fn exact_json_authority_accepts_the_nested_total_object_member_limit() {
        let input = nested_objects_with_total_members(MAX_TOTAL_OBJECT_MEMBERS);
        assert!(raw_object_keys_are_unique(input.as_bytes()));
    }

    #[test]
    fn exact_json_authority_abstains_when_nested_maps_exceed_the_total_member_limit() {
        let input = nested_objects_with_total_members(MAX_TOTAL_OBJECT_MEMBERS + 1);
        assert!(!raw_object_keys_are_unique(input.as_bytes()));
        assert!(exact_value(&input).is_none());
    }
}
