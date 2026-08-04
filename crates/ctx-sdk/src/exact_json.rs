use std::collections::BTreeSet;

use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::Deserializer;
use serde_json::Value;

use super::{AgentHistoryError, AgentHistoryErrorCode};

struct NoDuplicateJsonMembers;

impl<'de> DeserializeSeed<'de> for NoDuplicateJsonMembers {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for NoDuplicateJsonMembers {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON value without duplicate object members")
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_string<E>(self, _value: String) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence
            .next_element_seed(NoDuplicateJsonMembers)?
            .is_some()
        {}
        Ok(())
    }

    fn visit_map<A>(self, mut object: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut members = BTreeSet::new();
        while let Some(member) = object.next_key::<String>()? {
            if !members.insert(member.clone()) {
                return Err(de::Error::custom(format!(
                    "duplicate JSON object member {member:?}"
                )));
            }
            object.next_value_seed(NoDuplicateJsonMembers)?;
        }
        Ok(())
    }
}

#[cfg(test)]
pub(super) fn decode_json_value_exact(
    bytes: &[u8],
    message: &str,
) -> Result<Value, AgentHistoryError> {
    parse_json_value_exact(bytes).map_err(|err| {
        AgentHistoryError::new(AgentHistoryErrorCode::DecodeError, message, false)
            .with_cause(err.to_string())
    })
}

pub(super) fn parse_json_value_exact(bytes: &[u8]) -> serde_json::Result<Value> {
    let mut duplicate_check = serde_json::Deserializer::from_slice(bytes);
    NoDuplicateJsonMembers
        .deserialize(&mut duplicate_check)
        .and_then(|()| duplicate_check.end())?;
    serde_json::from_slice(bytes)
}
