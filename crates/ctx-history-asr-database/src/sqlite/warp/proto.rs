//! Protobuf merge/oneof rules over the shared native wire cursor.
use crate::{Error, Result};
use ctx_history_providers_sqlite_selected::warp_wire::{
    WarpWireCursor, WarpWireField, WarpWireValue,
};
use serde_json::{json, Map, Value};
pub(super) struct Fields<'a>(pub Vec<WarpWireField<'a>>);
impl<'a> Fields<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self> {
        let mut cursor = WarpWireCursor::new(bytes);
        let mut fields = Vec::new();
        while let Some(field) = cursor
            .next_field()
            .map_err(|e| Error::Invalid(e.to_string()))?
        {
            fields.push(field);
        }
        Ok(Self(fields))
    }
    pub fn all(&self, n: u32) -> impl Iterator<Item = &'a [u8]> + '_ {
        self.0.iter().filter_map(move |f| match f.value {
            WarpWireValue::LengthDelimited(b) if f.number == n => Some(b),
            _ => None,
        })
    }
    pub fn text(&self, n: u32) -> Result<Option<&'a str>> {
        self.all(n)
            .last()
            .map(|b| {
                std::str::from_utf8(b)
                    .map_err(|_| Error::Invalid("Warp protobuf string is not UTF-8".into()))
            })
            .transpose()
    }
    pub fn int(&self, n: u32) -> Option<u64> {
        self.0
            .iter()
            .filter_map(|f| match f.value {
                WarpWireValue::Varint(v) if f.number == n => Some(v),
                _ => None,
            })
            .next_back()
    }
    pub fn merged(&self, n: u32) -> Option<Vec<u8>> {
        let mut seen = false;
        let mut bytes = Vec::new();
        for b in self.all(n) {
            seen = true;
            bytes.extend_from_slice(b);
        }
        seen.then_some(bytes)
    }
    pub fn oneof(&self, allowed: impl Fn(u32) -> bool) -> Option<(u32, Vec<u8>)> {
        let mut selected: Option<(u32, Vec<u8>)> = None;
        for f in &self.0 {
            if allowed(f.number) {
                if let WarpWireValue::LengthDelimited(b) = f.value {
                    if let Some((n, bytes)) = &mut selected {
                        if *n == f.number {
                            bytes.extend_from_slice(b);
                            continue;
                        }
                    }
                    selected = Some((f.number, b.to_vec()));
                }
            }
        }
        selected
    }
}
pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
pub(super) fn unhex(s: &str) -> Result<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return Err(Error::Invalid("invalid internal protobuf hex".into()));
    }
    s.as_bytes()
        .chunks_exact(2)
        .map(|b| {
            let s = std::str::from_utf8(b)
                .map_err(|_| Error::Invalid("invalid internal protobuf hex".into()))?;
            u8::from_str_radix(s, 16)
                .map_err(|_| Error::Invalid("invalid internal protobuf hex".into()))
        })
        .collect()
}
pub(super) fn object(bytes: &[u8], depth: usize) -> Result<Value> {
    if depth > 64 {
        return Err(Error::Invalid("Warp Struct nesting exceeds 64".into()));
    }
    let fields = Fields::parse(bytes)?;
    let mut out = Map::new();
    for entry in fields.all(1) {
        let entry = Fields::parse(entry)?;
        let key = entry.text(1)?.unwrap_or_default();
        let value = entry
            .merged(2)
            .ok_or_else(|| Error::Invalid("Warp Struct entry lacks value".into()))?;
        out.insert(key.into(), value_json(&value, depth + 1)?);
    }
    Ok(Value::Object(out))
}
fn value_json(bytes: &[u8], depth: usize) -> Result<Value> {
    if depth > 64 {
        return Err(Error::Invalid("Warp Value nesting exceeds 64".into()));
    }
    let f = Fields::parse(bytes)?;
    let Some((index, selected)) =
        f.0.iter()
            .enumerate()
            .rev()
            .find(|(_, f)| (1..=6).contains(&f.number))
    else {
        return Ok(Value::Null);
    };
    // Repeated occurrences of the same message-valued oneof merge, until another
    // arm replaces them. A scalar occurrence clears the earlier embedded value.
    let mut merged = Vec::new();
    if matches!(selected.number, 5 | 6) {
        let start = f.0[..index]
            .iter()
            .rposition(|p| (1..=6).contains(&p.number) && p.number != selected.number)
            .map_or(0, |i| i + 1);
        for part in &f.0[start..=index] {
            if part.number == selected.number {
                if let WarpWireValue::LengthDelimited(b) = part.value {
                    merged.extend_from_slice(b);
                }
            }
        }
    }
    Ok(match (selected.number, selected.value) {
        (1, _) => Value::Null,
        (2, WarpWireValue::Fixed64(bits)) => {
            let n = f64::from_bits(bits);
            if !n.is_finite() {
                return Err(Error::Invalid("nonfinite Warp Struct number".into()));
            }
            json!(n)
        }
        (3, WarpWireValue::LengthDelimited(b)) => json!(std::str::from_utf8(b)
            .map_err(|_| Error::Invalid("invalid Warp Struct text".into()))?),
        (4, WarpWireValue::Varint(v)) => json!(v != 0),
        (5, WarpWireValue::LengthDelimited(_)) => object(&merged, depth + 1)?,
        (6, WarpWireValue::LengthDelimited(_)) => {
            let list = Fields::parse(&merged)?;
            Value::Array(
                list.all(1)
                    .map(|b| value_json(b, depth + 1))
                    .collect::<Result<_>>()?,
            )
        }
        _ => return Err(Error::Invalid("invalid Warp Struct wire type".into())),
    })
}
