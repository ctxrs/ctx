use super::*;

pub(super) struct AuditedValue {
    pub(super) value: Value,
    pub(super) duplicate_key: bool,
    pub(super) forbidden_output: bool,
}

pub(super) fn audit_json(raw: &str) -> std::result::Result<AuditedValue, ()> {
    let mut deserializer = serde_json::Deserializer::from_str(raw);
    let audited = AuditedSeed::ROOT
        .deserialize(&mut deserializer)
        .map_err(|_| ())?;
    deserializer.end().map_err(|_| ())?;
    Ok(audited)
}

#[derive(Clone, Copy)]
struct AuditedSeed {
    inside_tokens: bool,
}

impl AuditedSeed {
    const ROOT: Self = Self {
        inside_tokens: false,
    };
}

impl<'de> DeserializeSeed<'de> for AuditedSeed {
    type Value = AuditedValue;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(AuditedVisitor {
            inside_tokens: self.inside_tokens,
        })
    }
}

struct AuditedVisitor {
    inside_tokens: bool,
}

impl<'de> Visitor<'de> for AuditedVisitor {
    type Value = AuditedValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded JSON value")
    }

    fn visit_bool<E>(self, value: bool) -> std::result::Result<Self::Value, E> {
        Ok(audited_scalar(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> std::result::Result<Self::Value, E> {
        Ok(audited_scalar(Value::Number(Number::from(value))))
    }

    fn visit_u64<E>(self, value: u64) -> std::result::Result<Self::Value, E> {
        Ok(audited_scalar(Value::Number(Number::from(value))))
    }

    fn visit_f64<E>(self, value: f64) -> std::result::Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Number::from_f64(value)
            .map(Value::Number)
            .map(audited_scalar)
            .ok_or_else(|| E::custom("non-finite JSON number"))
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E> {
        Ok(audited_scalar(Value::String(value.to_owned())))
    }

    fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E> {
        Ok(audited_scalar(Value::String(value)))
    }

    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(audited_scalar(Value::Null))
    }

    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(audited_scalar(Value::Null))
    }

    fn visit_some<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        AuditedSeed {
            inside_tokens: self.inside_tokens,
        }
        .deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        let mut duplicate_key = false;
        let mut forbidden_output = false;
        while let Some(value) = sequence.next_element_seed(AuditedSeed {
            inside_tokens: self.inside_tokens,
        })? {
            duplicate_key |= value.duplicate_key;
            forbidden_output |= value.forbidden_output;
            values.push(value.value);
        }
        Ok(AuditedValue {
            value: Value::Array(values),
            duplicate_key,
            forbidden_output,
        })
    }

    fn visit_map<A>(self, mut entries: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut object = Map::new();
        let mut seen = BTreeSet::new();
        let mut duplicate_key = false;
        let mut forbidden_output = false;
        while let Some(key) = entries.next_key::<String>()? {
            let value = entries.next_value_seed(AuditedSeed {
                inside_tokens: normalize_token(&key) == "tokens",
            })?;
            duplicate_key |= value.duplicate_key || !seen.insert(key.clone());
            forbidden_output |=
                value.forbidden_output || is_output_key(&key, &value.value, self.inside_tokens);
            object.insert(key, value.value);
        }
        forbidden_output |= object_is_forbidden_output(&object);
        Ok(AuditedValue {
            value: Value::Object(object),
            duplicate_key,
            forbidden_output,
        })
    }
}

fn audited_scalar(value: Value) -> AuditedValue {
    AuditedValue {
        value,
        duplicate_key: false,
        forbidden_output: false,
    }
}

fn object_is_forbidden_output(object: &Map<String, Value>) -> bool {
    if object
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(is_direct_output_token)
        || object
            .get("role")
            .and_then(Value::as_str)
            .is_some_and(|role| normalize_token(role) == "tool")
    {
        return true;
    }
    let object_type = object
        .get("type")
        .and_then(Value::as_str)
        .map(normalize_token);
    if !object_type
        .as_deref()
        .is_some_and(|value| matches!(value, "tool" | "shell"))
    {
        return false;
    }
    let state = object.get("state").and_then(Value::as_object);
    let status = state
        .and_then(|state| state.get("status").or_else(|| state.get("outcome")))
        .or_else(|| object.get("status"))
        .or_else(|| object.get("outcome"))
        .and_then(Value::as_str);
    if status.is_some_and(is_terminal_status) {
        return true;
    }
    let has_output = object.contains_key("content")
        || object.contains_key("structured")
        || state
            .is_some_and(|state| state.contains_key("content") || state.contains_key("structured"));
    status.is_some_and(|status| normalize_token(status) == "running") && has_output
}
