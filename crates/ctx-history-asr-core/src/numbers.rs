//! Exact native JSON numbers; unsafe integers travel as JSON text, never rounded.
use serde_json::{value::RawValue, Value};
pub const SAFE: u64 = 9_007_199_254_740_991;
// An internal carrier, removed before publication. Native objects containing
// this key are escaped as whole JSON text, so they cannot impersonate a carrier.
const RAW: &str = "$ctx.asr.exactJson";
pub fn safe(v: &Value) -> bool {
    match v {
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.unsigned_abs() <= SAFE
            } else if let Some(i) = n.as_u64() {
                i <= SAFE
            } else {
                n.as_f64()
                    .is_some_and(|f| f.is_finite() && f.abs() <= SAFE as f64)
            }
        }
        Value::Array(a) => a.iter().all(safe),
        Value::Object(o) => !o.contains_key(RAW) && o.values().all(safe),
        _ => true,
    }
}
pub fn parse(raw: &str) -> Option<Value> {
    decode(raw).filter(safe)
}

/// Keep exact values outside serde_json's integer range local to ASR. Enabling
/// arbitrary_precision globally would alter every existing custom JSON visitor.
pub fn decode(raw: &str) -> Option<Value> {
    if let Ok(value) = serde_json::from_str::<Value>(raw) {
        if safe(&value) {
            return ctx_history_capture_model::raw_object_keys_are_unique(raw.as_bytes())
                .then_some(value);
        }
    }
    exact(raw).ok()
}

fn exact(raw: &str) -> serde_json::Result<Value> {
    let raw: &RawValue = serde_json::from_str(raw)?;
    let source = raw.get();
    if source.starts_with('{') {
        let fields: serde_json::Map<String, Value> = serde_json::from_str::<Unique>(source)?
            .0
            .into_iter()
            .map(|(k, v)| Ok((k, exact(v.get())?)))
            .collect::<serde_json::Result<_>>()?;
        if fields.contains_key(RAW) {
            return Ok(serde_json::json!({(RAW):source}));
        }
        Ok(Value::Object(fields))
    } else if source.starts_with('[') {
        let items: Vec<&RawValue> = serde_json::from_str(source)?;
        Ok(Value::Array(
            items
                .into_iter()
                .map(|v| exact(v.get()))
                .collect::<serde_json::Result<_>>()?,
        ))
    } else {
        let value = serde_json::from_str::<Value>(source);
        if let Ok(value) = value {
            // i64/u64 retain their exact spelling as a value. Floats outside the
            // safe range may be rounded integers, so preserve their source token.
            if !value.is_number()
                || value.as_i64().is_some()
                || value.as_u64().is_some()
                || safe(&value)
            {
                return Ok(value);
            }
        }
        Ok(serde_json::json!({(RAW):source}))
    }
}

/// Reconstruct source JSON without changing any protected numeric token.
pub fn text(value: &Value) -> String {
    match value {
        Value::Object(o) if o.len() == 1 && o.get(RAW).is_some_and(Value::is_string) => {
            o[RAW].as_str().unwrap_or_default().to_owned()
        }
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, v)| format!("{}:{}", Value::String(k.clone()), text(v)))
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Array(a) => format!("[{}]", a.iter().map(text).collect::<Vec<_>>().join(",")),
        v => v.to_string(),
    }
}
pub fn write(mut out: impl std::io::Write, value: &Value) -> crate::Result<()> {
    if safe(value) {
        serde_json::to_writer(out, value)?;
    } else {
        out.write_all(text(value).as_bytes())?;
    }
    Ok(())
}

struct Unique<'a>(std::collections::BTreeMap<String, &'a RawValue>);
impl<'de> serde::Deserialize<'de> for Unique<'de> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Unique<'de>;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("unique object keys")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut fields = std::collections::BTreeMap::new();
                while let Some((key, value)) = map.next_entry::<String, &RawValue>()? {
                    if fields.insert(key, value).is_some() {
                        return Err(serde::de::Error::custom("duplicate JSON key"));
                    }
                }
                Ok(Unique(fields))
            }
        }
        d.deserialize_map(Visitor)
    }
}
