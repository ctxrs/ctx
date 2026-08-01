use std::fmt;

use serde::de::{self, DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;

pub(super) const MAX_CURSOR_ATOM_CHARS: usize = 512;
pub(super) const MAX_CURSOR_PATH_CHARS: usize = 4_096;

pub(super) struct CursorPathStringSeed {
    pub(super) max_chars: usize,
}

impl<'de> DeserializeSeed<'de> for CursorPathStringSeed {
    type Value = Option<String>;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(CursorPathStringVisitor {
            max_chars: self.max_chars,
        })
    }
}

struct CursorPathStringVisitor {
    max_chars: usize,
}

impl CursorPathStringVisitor {
    fn exact(&self, value: &str) -> Option<String> {
        (value.chars().count() <= self.max_chars).then(|| value.to_owned())
    }
}

impl<'de> Visitor<'de> for CursorPathStringVisitor {
    type Value = Option<String>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an exact bounded Cursor input path string")
    }

    fn visit_borrowed_str<E>(self, value: &'de str) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(self.exact(value))
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(self.exact(value))
    }

    fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok((value.chars().count() <= self.max_chars).then_some(value))
    }

    fn visit_bool<E>(self, _value: bool) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(None)
    }

    fn visit_i64<E>(self, _value: i64) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(None)
    }

    fn visit_u64<E>(self, _value: u64) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(None)
    }

    fn visit_f64<E>(self, _value: f64) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(None)
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(None)
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(None)
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(None)
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(None)
    }
}

pub(super) struct ExactBoundedStringSeed {
    pub(super) max_chars: usize,
}

impl<'de> DeserializeSeed<'de> for ExactBoundedStringSeed {
    type Value = Option<String>;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok((value.chars().count() <= self.max_chars).then_some(value))
    }
}

pub(super) struct BoundedStringVisitor {
    pub(super) max_chars: usize,
}

impl Visitor<'_> for BoundedStringVisitor {
    type Value = String;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded string")
    }

    fn visit_borrowed_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(value.chars().take(self.max_chars).collect())
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(value.chars().take(self.max_chars).collect())
    }

    fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E>
    where
        E: de::Error,
    {
        if value.chars().count() <= self.max_chars {
            Ok(value)
        } else {
            Ok(value.chars().take(self.max_chars).collect())
        }
    }
}
