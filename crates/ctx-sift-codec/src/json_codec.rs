//! Complete JSON representations. Numbers remain raw JSON tokens throughout.
//!
//! Legacy `json-v1` decoding accepts JSON_HEADER followed by one JSON value.
//! `json-min-v1` is one minified JSON value without a header.
//! `json-rows-v1` is ROWS_HEADER followed by an object containing `columns`
//! (unique strings) and `rows` (arrays with exactly that many JSON values).
//! `json-columns-v1` is COLUMNS_HEADER followed by a row count and an object
//! of scalar columns: arrays supply per-row values, scalars repeat in every row.
//! Restoration preserves values and numeric lexemes, not source whitespace.

use crate::Encoding;
use anyhow::{Result, bail, ensure};
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::value::RawValue;
use std::collections::{HashMap, HashSet};
use std::fmt;

const JSON_HEADER: &str = "JSON v1 (all values):\n";
const ROWS_HEADER: &str = "JSON rows v1 (each row maps to the columns in order):\n";
const COLUMNS_HEADER: &str = "JSON columns v1: arrays are columns; scalars repeat for all rows\n";
const MAX_JSON_BYTES: usize = 16 * 1024 * 1024;
const MAX_DEPTH: usize = 64;

/// A map without serde_json::Value's number handling or reserved-key behavior.
struct Object<'a>(Vec<(String, &'a RawValue)>);

impl<'de> Deserialize<'de> for Object<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor;

        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = Object<'de>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON object with unique keys")
            }

            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut fields = Vec::new();
                let mut seen = HashSet::new();
                while let Some((key, value)) = map.next_entry::<String, &'de RawValue>()? {
                    if !seen.insert(key.clone()) {
                        return Err(de::Error::custom("duplicate JSON object key"));
                    }
                    fields.push((key, value));
                }
                Ok(Object(fields))
            }
        }

        deserializer.deserialize_map(ObjectVisitor)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Table<'a> {
    columns: Vec<String>,
    #[serde(borrow)]
    rows: Vec<Vec<&'a RawValue>>,
}

fn parse(input: &str) -> Result<&RawValue> {
    ensure!(input.len() <= MAX_JSON_BYTES, "JSON exceeds size limit");
    let value: &RawValue = serde_json::from_str(input)?;
    validate(value, 0)?;
    Ok(value)
}

/// RawValue checks syntax, while this walk rejects duplicate keys at every
/// depth and validates decoded strings (including Unicode surrogate pairs).
fn validate(value: &RawValue, depth: usize) -> Result<()> {
    ensure!(depth <= MAX_DEPTH, "JSON exceeds nesting limit");
    match value.get().as_bytes().first() {
        Some(b'{') => {
            let object: Object<'_> = serde_json::from_str(value.get())?;
            for (_, child) in object.0 {
                validate(child, depth + 1)?;
            }
        }
        Some(b'[') => {
            let children: Vec<&RawValue> = serde_json::from_str(value.get())?;
            for child in children {
                validate(child, depth + 1)?;
            }
        }
        Some(b'"') => {
            let _: String = serde_json::from_str(value.get())?;
        }
        _ => {}
    }
    Ok(())
}

/// Called only on validated JSON. Remove only JSON whitespace outside strings;
/// retain every number lexeme and every escaped or literal string character.
fn minify(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut in_string = false;
    let mut escaped = false;
    for ch in input.chars() {
        if in_string {
            output.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
        } else if ch == '"' {
            in_string = true;
            output.push(ch);
        } else if !matches!(ch, ' ' | '\t' | '\r' | '\n') {
            output.push(ch);
        }
    }
    output
}

fn table(value: &RawValue) -> Option<Table<'_>> {
    if !value.get().starts_with('[') {
        return None;
    }
    let objects: Vec<&RawValue> = serde_json::from_str(value.get()).ok()?;
    let first: Object<'_> = serde_json::from_str(objects.first()?.get()).ok()?;
    let columns: Vec<String> = first.0.into_iter().map(|(key, _)| key).collect();
    let mut rows = Vec::with_capacity(objects.len());
    for raw in objects {
        let object: Object<'_> = serde_json::from_str(raw.get()).ok()?;
        if object.0.len() != columns.len() {
            return None;
        }
        let fields: HashMap<String, &RawValue> = object.0.into_iter().collect();
        let row: Option<Vec<&RawValue>> = columns
            .iter()
            .map(|column| fields.get(column).copied())
            .collect();
        rows.push(row?);
    }
    Some(Table { columns, rows })
}

fn rows_candidate(table: &Table<'_>) -> Option<String> {
    let encoded = serde_json::to_string(table).ok()?;
    let payload = minify(&encoded);
    // The table adds two container levels around nested values. Apply the same
    // limits as restore so every emitted candidate remains independently usable.
    parse(&payload).ok()?;
    Some(format!("{ROWS_HEADER}{payload}"))
}

fn is_scalar(value: &RawValue) -> bool {
    !matches!(value.get().as_bytes().first(), Some(b'{' | b'['))
}

fn columns_candidate(table: &Table<'_>) -> Result<String> {
    ensure!(
        table.rows.iter().flatten().all(|value| is_scalar(value)),
        "JSON columns require scalar values"
    );
    let mut payload = format!("{{\"rows\":{},\"columns\":{{", table.rows.len());
    for (index, key) in table.columns.iter().enumerate() {
        if index > 0 {
            append(&mut payload, ",")?;
        }
        append(&mut payload, &serde_json::to_string(key)?)?;
        append(&mut payload, ":")?;
        let first = table.rows[0][index].get();
        if table.rows.iter().all(|row| row[index].get() == first) {
            append(&mut payload, first)?;
        } else {
            append(&mut payload, "[")?;
            for (row_index, row) in table.rows.iter().enumerate() {
                if row_index > 0 {
                    append(&mut payload, ",")?;
                }
                append(&mut payload, row[index].get())?;
            }
            append(&mut payload, "]")?;
        }
    }
    append(&mut payload, "}}")?;
    Ok(format!("{COLUMNS_HEADER}{payload}"))
}

pub(crate) fn candidates(input: &str) -> Vec<(Encoding, String)> {
    let Ok(value) = parse(input) else {
        return Vec::new();
    };
    let compact = minify(value.get());
    // The fixed o200k pre-tokenizer separates JSON_HEADER from minified JSON,
    // so the legacy header only adds tokens. Keep its decoder for old frames.
    let mut candidates = vec![(Encoding::JsonMinV1, compact)];
    if let Some(table) = table(value) {
        if let Some(encoded) = rows_candidate(&table) {
            candidates.push((Encoding::JsonRowsV1, encoded));
        }
        if let Ok(encoded) = columns_candidate(&table) {
            candidates.push((Encoding::JsonColumnsV1, encoded));
        }
    }
    candidates
}

fn append(output: &mut String, text: &str) -> Result<()> {
    ensure!(
        text.len() <= MAX_JSON_BYTES - output.len(),
        "restored JSON exceeds size limit"
    );
    output.push_str(text);
    Ok(())
}

fn add_restored_size(size: &mut usize, bytes: usize, copies: usize) -> Result<()> {
    *size = bytes
        .checked_mul(copies)
        .and_then(|bytes| size.checked_add(bytes))
        .filter(|size| *size <= MAX_JSON_BYTES)
        .ok_or_else(|| anyhow::anyhow!("restored JSON exceeds size limit"))?;
    Ok(())
}

fn restore_columns(value: &RawValue) -> Result<String> {
    let envelope: Object<'_> = serde_json::from_str(value.get())?;
    ensure!(
        envelope.0.len() == 2,
        "JSON columns require rows and columns"
    );
    let field = |name| {
        envelope
            .0
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| *value)
            .ok_or_else(|| anyhow::anyhow!("missing JSON columns field: {name}"))
    };
    let count = field("rows")?.get();
    let rows: u64 = count.parse()?;
    ensure!(rows.to_string() == count, "row count must be canonical u64");
    let rows =
        usize::try_from(rows).map_err(|_| anyhow::anyhow!("restored JSON exceeds size limit"))?;
    // Even empty objects cost three bytes per row, except the last comma.
    // Check this before reserving anything or iterating over the claimed rows.
    let mut size = if rows == 0 { 2 } else { 1 };
    add_restored_size(&mut size, 3, rows)?;
    let columns: Object<'_> = serde_json::from_str(field("columns")?.get())?;
    ensure!(
        rows != 0 || columns.0.is_empty(),
        "zero rows require empty columns"
    );
    let mut decoded = Vec::with_capacity(columns.0.len());
    for (index, (key, raw)) in columns.0.into_iter().enumerate() {
        let key = serde_json::to_string(&key)?;
        add_restored_size(&mut size, key.len() + 1 + usize::from(index > 0), rows)?;
        // A singleton represents a repeated scalar; actual arrays still must
        // have exactly `rows` values, including when rows is one.
        let values = if raw.get().starts_with('[') {
            let values: Vec<&RawValue> = serde_json::from_str(raw.get())?;
            ensure!(values.len() == rows, "column length differs from row count");
            for value in &values {
                ensure!(is_scalar(value), "column arrays require scalar values");
                add_restored_size(&mut size, value.get().len(), 1)?;
            }
            values
        } else {
            ensure!(is_scalar(raw), "columns require scalar values or arrays");
            add_restored_size(&mut size, raw.get().len(), rows)?;
            vec![raw]
        };
        decoded.push((key, values));
    }
    let mut output = String::with_capacity(size);
    output.push('[');
    for row in 0..rows {
        if row > 0 {
            output.push(',');
        }
        output.push('{');
        for (index, (key, values)) in decoded.iter().enumerate() {
            if index > 0 {
                output.push(',');
            }
            output.push_str(key);
            output.push(':');
            output.push_str(values[if values.len() == 1 { 0 } else { row }].get());
        }
        output.push('}');
    }
    output.push(']');
    debug_assert_eq!(output.len(), size);
    Ok(output)
}

pub(crate) fn restore(encoding: Encoding, input: &str) -> Result<String> {
    let header = match encoding {
        Encoding::JsonV1 => JSON_HEADER,
        Encoding::JsonRowsV1 => ROWS_HEADER,
        Encoding::JsonMinV1 => "",
        Encoding::JsonColumnsV1 => COLUMNS_HEADER,
        _ => bail!("not a JSON encoding"),
    };
    let payload = input
        .strip_prefix(header)
        .ok_or_else(|| anyhow::anyhow!("missing or incorrect JSON encoding header"))?;
    let value = parse(payload)?;
    if matches!(encoding, Encoding::JsonV1 | Encoding::JsonMinV1) {
        return Ok(minify(value.get()));
    }
    if matches!(encoding, Encoding::JsonColumnsV1) {
        return restore_columns(value);
    }

    ensure!(
        value.get().starts_with('{'),
        "JSON table envelope must be an object"
    );
    let table: Table<'_> = serde_json::from_str(value.get())?;
    let mut seen = HashSet::new();
    ensure!(
        table.columns.iter().all(|key| seen.insert(key)),
        "duplicate table column"
    );
    let keys: Vec<String> = table
        .columns
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<_, _>>()?;
    let mut output = String::new();
    append(&mut output, "[")?;
    for (row_index, row) in table.rows.iter().enumerate() {
        ensure!(
            row.len() == keys.len(),
            "table row width differs from columns"
        );
        if row_index > 0 {
            append(&mut output, ",")?;
        }
        append(&mut output, "{")?;
        for (column_index, (key, value)) in keys.iter().zip(row).enumerate() {
            if column_index > 0 {
                append(&mut output, ",")?;
            }
            append(&mut output, key)?;
            append(&mut output, ":")?;
            append(&mut output, value.get())?;
        }
        append(&mut output, "}")?;
    }
    append(&mut output, "]")?;
    Ok(minify(&output))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(input: &str, encoding: Encoding) -> String {
        candidates(input)
            .into_iter()
            .find(|(kind, _)| *kind == encoding)
            .expect("expected encoding candidate")
            .1
    }

    #[test]
    fn minification_preserves_numbers_and_string_lexemes() {
        let input = r#" {
          "big": 1234567890123456789012345678901234567890,
          "decimal": 0.123456789012345678901234567890,
          "exp": 1e+9999, "negative_zero": -0,
          "text": " space  \t\n\"\\ café \uD83D\uDE80 "
        } "#;
        let expected = r#"{"big":1234567890123456789012345678901234567890,"decimal":0.123456789012345678901234567890,"exp":1e+9999,"negative_zero":-0,"text":" space  \t\n\"\\ café \uD83D\uDE80 "}"#;
        let compact = encoded(input, Encoding::JsonMinV1);
        assert_eq!(compact, expected);
        assert_eq!(restore(Encoding::JsonMinV1, &compact).unwrap(), expected);
    }

    #[test]
    fn legacy_json_frames_decode_without_an_encoder_candidate() {
        for (frame, expected) in [
            (
                concat!(
                    "JSON v1 (all values):\n",
                    r#" { "n": -0, "exp": 1e+999999999999999999999999, "s": "/雪\n\uD83D\uDE80" } "#
                ),
                r#"{"n":-0,"exp":1e+999999999999999999999999,"s":"/雪\n\uD83D\uDE80"}"#,
            ),
            (
                "JSON v1 (all values):\n [null, true, false] \n",
                "[null,true,false]",
            ),
            (
                "JSON v1 (all values):\n -18446744073709551617 ",
                "-18446744073709551617",
            ),
            ("JSON v1 (all values):\n \"/雪\\n\" ", r#""/雪\n""#),
        ] {
            assert_eq!(restore(Encoding::JsonV1, frame).unwrap(), expected);
            assert_eq!(crate::restore(Encoding::JsonV1, frame).unwrap(), expected);
        }
    }

    #[test]
    fn complete_counts_prove_legacy_header_dominance_for_every_json_root() {
        let counter = crate::Compactor::new().unwrap();
        let independent = tiktoken_rs::o200k_base().unwrap();
        let header = "JSON v1 (all values):\n";
        let header_tokens = counter.count_tokens(header);
        assert_eq!(header_tokens, 7);
        assert_eq!(header_tokens, independent.encode_ordinary(header).len());
        for body in [
            "null",
            "true",
            "false",
            "0",
            "1",
            "2",
            "3",
            "4",
            "5",
            "6",
            "7",
            "8",
            "9",
            "-0",
            "-0.0",
            "-18446744073709551617",
            "1e+999999999999999999999999",
            "1e-999999999999999999999999",
            "0.123456789012345678901234567890",
            "1.2300E+0042",
            r#""""#,
            r#""/雪\n🦀""#,
            r#""\/escaped\nnewline""#,
            r#""\uD83D\uDE80 café""#,
            r#""<|endoftext|>""#,
            "[]",
            "{}",
            "[null,true,false,-0]",
            r#"{"n":1e9999,"s":"雪"}"#,
            r#"[{"x":0},{"x":1}]"#,
        ] {
            // These are explicit wire bodies, independent of the encoder/minifier.
            let legacy = format!("{header}{body}");
            let body_tokens = counter.count_tokens(body);
            let legacy_tokens = counter.count_tokens(&legacy);
            assert_eq!(
                body_tokens,
                independent.encode_ordinary(body).len(),
                "{body}"
            );
            assert_eq!(
                legacy_tokens,
                independent.encode_ordinary(&legacy).len(),
                "{body}"
            );
            assert_eq!(legacy_tokens, header_tokens + body_tokens, "{body}");
            assert!(legacy_tokens > body_tokens, "{body}");
            assert_eq!(restore(Encoding::JsonV1, &legacy).unwrap(), body);
            assert_eq!(restore(Encoding::JsonMinV1, body).unwrap(), body);
            assert!(
                candidates(body)
                    .iter()
                    .all(|(kind, _)| *kind != Encoding::JsonV1)
            );

            // The identical headerless candidate ties raw; raw must still win.
            let tied = counter.compact(body);
            assert_eq!(tied.encoding, Encoding::Raw, "{body}");
            assert_eq!(tied.text, body);
            assert_eq!(
                (tied.input_tokens, tied.output_tokens),
                (body_tokens, body_tokens)
            );

            // With removable whitespace, that same explicit body must win instead.
            let padded = format!("\n    {body}\n");
            let input_tokens = independent.encode_ordinary(&padded).len();
            assert!(input_tokens > body_tokens, "{body}");
            let winner = counter.compact(&padded);
            assert_eq!(winner.encoding, Encoding::JsonMinV1, "{body}");
            assert_eq!(winner.text, body);
            assert_eq!(
                (winner.input_tokens, winner.output_tokens),
                (input_tokens, body_tokens)
            );
        }
    }

    #[test]
    fn columns_preserve_raw_scalars_and_first_object_key_order() {
        let input = r#"[
          {"n":-0,"same":1.2300E+0042,"text":"\u0061","flag":true,"none":null},
          {"none":null,"flag":false,"text":"a","same":1.2300E+0042,"n":0},
          {"text":"\u0061","n":0.0,"none":null,"same":1.2300E+0042,"flag":true}
        ]"#;
        assert_eq!(
            candidates(input)
                .into_iter()
                .map(|(kind, _)| kind)
                .collect::<Vec<_>>(),
            [
                Encoding::JsonMinV1,
                Encoding::JsonRowsV1,
                Encoding::JsonColumnsV1
            ]
        );
        let compact = encoded(input, Encoding::JsonColumnsV1);
        assert_eq!(
            compact,
            concat!(
                "JSON columns v1: arrays are columns; scalars repeat for all rows\n",
                r#"{"rows":3,"columns":{"n":[-0,0,0.0],"same":1.2300E+0042,"text":["\u0061","a","\u0061"],"flag":[true,false,true],"none":null}}"#
            )
        );
        assert_eq!(
            restore(Encoding::JsonColumnsV1, &compact).unwrap(),
            r#"[{"n":-0,"same":1.2300E+0042,"text":"\u0061","flag":true,"none":null},{"n":0,"same":1.2300E+0042,"text":"a","flag":false,"none":null},{"n":0.0,"same":1.2300E+0042,"text":"\u0061","flag":true,"none":null}]"#
        );
    }

    #[test]
    fn columns_preserve_numeric_tokens_without_semantic_equality() {
        let input = r#"[{"n":9007199254740992},{"n":9007199254740993},{"n":1e999999999999999999999999},{"n":1e-999999999999999999999999},{"n":-18446744073709551617},{"n":0.123456789012345678901234567890},{"n":1.00},{"n":1e0},{"n":1}]"#;
        let compact = encoded(input, Encoding::JsonColumnsV1);
        assert_eq!(
            compact,
            format!(
                "{COLUMNS_HEADER}{}",
                r#"{"rows":9,"columns":{"n":[9007199254740992,9007199254740993,1e999999999999999999999999,1e-999999999999999999999999,-18446744073709551617,0.123456789012345678901234567890,1.00,1e0,1]}}"#
            )
        );
        assert_eq!(restore(Encoding::JsonColumnsV1, &compact).unwrap(), input);
    }

    #[test]
    fn columns_require_homogeneous_scalar_objects() {
        for input in [
            "[]",
            "null",
            "{}",
            "[0,1]",
            "[{},null]",
            r#"[{"x":0},{}]"#,
            r#"[{"x":0},{"y":0}]"#,
            r#"[{"x":[]}]"#,
            r#"[{"x":{}}]"#,
        ] {
            assert!(
                candidates(input)
                    .iter()
                    .all(|(kind, _)| *kind != Encoding::JsonColumnsV1)
            );
            let compact = encoded(input, Encoding::JsonMinV1);
            assert_eq!(restore(Encoding::JsonMinV1, &compact).unwrap(), input);
        }
        for input in ["[{}]", "[{},{}]", r#"[{"x":true}]"#] {
            let compact = encoded(input, Encoding::JsonColumnsV1);
            assert_eq!(restore(Encoding::JsonColumnsV1, &compact).unwrap(), input);
        }
        assert_eq!(
            encoded("[{},{}]", Encoding::JsonColumnsV1),
            format!("{COLUMNS_HEADER}{{\"rows\":2,\"columns\":{{}}}}")
        );
    }

    #[test]
    fn columns_decode_valid_envelopes_and_literal_marker_keys() {
        for (payload, expected) in [
            (r#"{"rows":0,"columns":{}}"#, "[]"),
            (r#"{"rows":2,"columns":{}}"#, "[{},{}]"),
            (r#"{"columns":{"a":[-0]},"rows":1}"#, r#"[{"a":-0}]"#),
            (
                r#"{"rows":2,"columns":{"a":[true,null],"b":"\uD83D\uDE80"}}"#,
                r#"[{"a":true,"b":"\uD83D\uDE80"},{"a":null,"b":"\uD83D\uDE80"}]"#,
            ),
            (
                r#"{"rows":1,"columns":{"\u0061":"\u0062"}}"#,
                r#"[{"a":"\u0062"}]"#,
            ),
        ] {
            let framed = format!("{COLUMNS_HEADER} \t\n{payload}\r\n ");
            assert_eq!(restore(Encoding::JsonColumnsV1, &framed).unwrap(), expected);
        }
        let input = r#"[{"$serde_json::private::Number":"1e9999","$serde_json::private::RawValue":"{}","rows":-0,"columns":"JSON columns v1: arrays are columns; scalars repeat for all rows\n","":null,"\"\\\n":"雪"}]"#;
        let compact = encoded(input, Encoding::JsonColumnsV1);
        assert_eq!(restore(Encoding::JsonColumnsV1, &compact).unwrap(), input);
    }

    #[test]
    fn columns_reject_invalid_envelope_grammar() {
        for payload in [
            "[]",
            "null",
            "{}",
            r#"[1,{"a":0}]"#,
            r#"{"rows":1}"#,
            r#"{"columns":{}}"#,
            r#"{"rows":1,"columns":{},"extra":0}"#,
            r#"{"rows":1,"rows":1,"columns":{}}"#,
            r#"{"rows":1,"\u0072ows":1,"columns":{}}"#,
            r#"{"rows":1,"columns":{"a":0,"\u0061":0}}"#,
            r#"{"rows":1,"columns":[]}"#,
            r#"{"rows":1,"columns":null}"#,
            r#"{"rows":1,"columns":{"a":[]}}"#,
            r#"{"rows":1,"columns":{"a":[0,1]}}"#,
            r#"{"rows":2,"columns":{"a":[0]}}"#,
            r#"{"rows":1,"columns":{"a":{}}}"#,
            r#"{"rows":1,"columns":{"a":[[]]}}"#,
            r#"{"rows":1,"columns":{"a":[{}]}}"#,
            r#"{"rows":1,"columns":{"a":[{"x":0,"\u0078":1}]}}"#,
            r#"{"rows":1,"columns":{"a":"\uD800"}}"#,
            r#"{"rows":1,"columns":{"\uDC00":0}}"#,
            r#"{"rows":1,"columns":{"a":["\uDC00"]}}"#,
            r#"{"rows":0,"columns":{"a":false}}"#,
            r#"{"rows":0,"columns":{"a":[]}}"#,
        ] {
            assert!(
                restore(
                    Encoding::JsonColumnsV1,
                    &format!("{COLUMNS_HEADER}{payload}")
                )
                .is_err(),
                "{payload}"
            );
        }
        for count in [
            "-0",
            "-1",
            "1.0",
            "1e0",
            "1E+0",
            "01",
            "+1",
            "18446744073709551616",
            "null",
            "true",
            "\"1\"",
            "[]",
            "{}",
        ] {
            let payload = format!("{COLUMNS_HEADER}{{\"rows\":{count},\"columns\":{{}}}}");
            assert!(
                restore(Encoding::JsonColumnsV1, &payload).is_err(),
                "{count}"
            );
        }
    }

    #[test]
    fn columns_bound_expansion_before_iterating_rows() {
        let long = "k".repeat(4096);
        let fitting_rows = (MAX_JSON_BYTES - 1) / (long.len() + 9);
        let fitting =
            format!("{COLUMNS_HEADER}{{\"rows\":{fitting_rows},\"columns\":{{\"x\":\"{long}\"}}}}");
        let restored = restore(Encoding::JsonColumnsV1, &fitting).unwrap();
        assert_eq!(restored.len(), 1 + fitting_rows * (long.len() + 9));
        assert!(restored.len() <= MAX_JSON_BYTES);
        assert_eq!(restored.matches("\"x\":").count(), fitting_rows);
        for payload in [
            r#"{"rows":18446744073709551615,"columns":{}}"#.to_owned(),
            format!("{{\"rows\":{},\"columns\":{{}}}}", MAX_JSON_BYTES / 3 + 1),
            format!("{{\"rows\":4096,\"columns\":{{\"{long}\":0}}}}"),
            format!(
                "{{\"rows\":{},\"columns\":{{\"x\":\"{long}\"}}}}",
                fitting_rows + 1
            ),
        ] {
            assert!(payload.len() < MAX_JSON_BYTES);
            let error = restore(
                Encoding::JsonColumnsV1,
                &format!("{COLUMNS_HEADER}{payload}"),
            )
            .unwrap_err();
            assert!(error.to_string().contains("size limit"), "{error}");
        }
        let mut size = 1;
        add_restored_size(&mut size, MAX_JSON_BYTES - 1, 1).unwrap();
        assert_eq!(size, MAX_JSON_BYTES);
        assert!(add_restored_size(&mut size, 1, 1).is_err());
        assert!(add_restored_size(&mut 0, usize::MAX, 2).is_err());
    }

    #[test]
    fn new_codecs_enforce_source_payload_and_depth_limits() {
        let padded = format!("{}0", " ".repeat(MAX_JSON_BYTES - 1));
        assert_eq!(restore(Encoding::JsonMinV1, &padded).unwrap(), "0");
        let oversized = format!(" {padded}");
        assert!(candidates(&oversized).is_empty());
        assert!(restore(Encoding::JsonMinV1, &oversized).is_err());
        let oversized = format!(
            "{}{{\"rows\":0,\"columns\":{{}}}}",
            " ".repeat(MAX_JSON_BYTES)
        );
        assert!(
            restore(
                Encoding::JsonColumnsV1,
                &format!("{COLUMNS_HEADER}{oversized}")
            )
            .is_err()
        );
        let at_limit = format!("{}0{}", "[".repeat(MAX_DEPTH), "]".repeat(MAX_DEPTH));
        assert_eq!(restore(Encoding::JsonMinV1, &at_limit).unwrap(), at_limit);
        let too_deep = format!("[{at_limit}]");
        assert!(restore(Encoding::JsonMinV1, &too_deep).is_err());
        assert!(candidates(&too_deep).is_empty());
    }

    #[test]
    fn explicit_encoding_preserves_legacy_headers_and_json_data() {
        let object = r#"{"rows":1,"columns":{"x":0}}"#;
        assert_eq!(restore(Encoding::JsonMinV1, object).unwrap(), object);
        assert_eq!(
            restore(Encoding::JsonV1, &format!("{JSON_HEADER}{object}")).unwrap(),
            object
        );
        for input in [
            object.to_owned(),
            format!("{ROWS_HEADER}{object}"),
            format!("{JSON_HEADER}{object}"),
            format!("{}\\n{object}", COLUMNS_HEADER.trim_end()),
        ] {
            assert!(restore(Encoding::JsonColumnsV1, &input).is_err());
        }
        assert!(restore(Encoding::JsonMinV1, &format!("{JSON_HEADER}{object}")).is_err());
        for input in [
            r#"{"a":0,"\u0061":1}"#,
            r#"{"x":[{"a":0,"a":1}]}"#,
            r#""\uD800""#,
        ] {
            assert!(restore(Encoding::JsonMinV1, input).is_err());
        }
    }

    #[test]
    fn rows_preserve_all_values_and_map_reordered_keys() {
        let input = r#"[
          {"id":9007199254740993,"item":{"x":[null,true]},"note":"a\"b"},
          {"note":"rare\nvalue","id":9007199254740995,"item":false}
        ]"#;
        let compact = encoded(input, Encoding::JsonRowsV1);
        let expected_table = r#"{"columns":["id","item","note"],"rows":[[9007199254740993,{"x":[null,true]},"a\"b"],[9007199254740995,false,"rare\nvalue"]]}"#;
        assert_eq!(compact, format!("{ROWS_HEADER}{expected_table}"));
        assert_eq!(
            restore(Encoding::JsonRowsV1, &compact).unwrap(),
            r#"[{"id":9007199254740993,"item":{"x":[null,true]},"note":"a\"b"},{"id":9007199254740995,"item":false,"note":"rare\nvalue"}]"#
        );
    }

    #[test]
    fn current_and_legacy_encodings_preserve_numeric_spellings_without_float_conversion() {
        let numbers = [
            "-0",
            "-0.0",
            "-0e+000",
            "-0.000E-99999",
            "18446744073709551616",
            "-18446744073709551617",
            "1e999999999999999999999999999999999999",
            "1e-999999999999999999999999999999999999",
            "1.2300E+0042",
            "0.1234567890123456789012345678901234567890",
        ];
        for number in numbers {
            let legacy = format!("JSON v1 (all values):\n{number}");
            assert_eq!(restore(Encoding::JsonV1, &legacy).unwrap(), number);
            assert_eq!(encoded(number, Encoding::JsonMinV1), number);
            let input = format!("[{{\"number\":{number}}},{{\"number\":{number}}}]");
            for (encoding, compact) in candidates(&input) {
                assert_eq!(restore(encoding, &compact).unwrap(), input);
            }
            // Ensure table coverage isn't silently replaced by plain minification.
            let rows = encoded(&input, Encoding::JsonRowsV1);
            assert_eq!(restore(Encoding::JsonRowsV1, &rows).unwrap(), input);
        }
    }

    #[test]
    fn table_columns_escape_keys_and_preserve_null_associations() {
        let input = r#"[{"":null,"\"\\\n":false,"雪":[]},{"雪":{},"":true,"\"\\\n":"null"}]"#;
        let compact = encoded(input, Encoding::JsonRowsV1);
        assert_eq!(
            restore(Encoding::JsonRowsV1, &compact).unwrap(),
            r#"[{"":null,"\"\\\n":false,"雪":[]},{"":true,"\"\\\n":"null","雪":{}}]"#
        );
    }

    #[test]
    fn scalar_empty_and_heterogeneous_json_remain_complete() {
        for input in ["null", "true", "false", "0", "-0.00e+5", "\"\"", "[]", "{}"] {
            let compact = encoded(input, Encoding::JsonMinV1);
            assert_eq!(restore(Encoding::JsonMinV1, &compact).unwrap(), input);
        }
        for input in [r#"[{"x":null},{}]"#, r#"[{"x":0},{"y":0}]"#, "[{},null]"] {
            assert_eq!(candidates(input).len(), 1);
            let compact = encoded(input, Encoding::JsonMinV1);
            assert_eq!(restore(Encoding::JsonMinV1, &compact).unwrap(), input);
        }
        let compact = encoded("[{},{}]", Encoding::JsonRowsV1);
        assert_eq!(restore(Encoding::JsonRowsV1, &compact).unwrap(), "[{},{}]");
    }

    #[test]
    fn duplicate_keys_are_rejected_at_every_depth() {
        for input in [
            r#"{"a":1,"a":2}"#,
            r#"{"a":1,"\u0061":2}"#,
            r#"[{"outer":{"a":1,"a":2}}]"#,
            r#"{"list":[{"a":1,"a":2}]}"#,
        ] {
            assert!(candidates(input).is_empty(), "{input}");
            assert!(restore(Encoding::JsonV1, &format!("{JSON_HEADER}{input}")).is_err());
        }
    }

    #[test]
    fn reserved_keys_and_format_markers_are_ordinary_data() {
        let input = r#"[{"$serde_json::private::Number":"1e9999","columns":["x"],"rows":"JSON v1 (all values):\n"}]"#;
        for (encoding, compact) in candidates(input) {
            assert_eq!(restore(encoding, &compact).unwrap(), input);
        }
        assert!(candidates(&format!("{JSON_HEADER}{{}}")).is_empty());
        assert!(restore(Encoding::JsonV1, "{}").is_err());
        assert!(restore(Encoding::JsonRowsV1, &format!("{JSON_HEADER}{{}}")).is_err());
        assert!(restore(Encoding::Raw, "{}").is_err());
    }

    #[test]
    fn malformed_json_and_invalid_unicode_fail_closed() {
        for input in [
            "",
            " ",
            "NaN",
            "Infinity",
            "01",
            "1.",
            "1e",
            "{} []",
            "[1,]",
            r#"{"a":}"#,
            r#""\uD800""#,
            r#""\uDC00""#,
            r#"{"\uD800":0}"#,
            "\"literal\nnewline\"",
        ] {
            assert!(candidates(input).is_empty(), "{input:?}");
        }
    }

    #[test]
    fn table_envelopes_require_objects_and_allow_surrounding_whitespace() {
        for payload in [r#"[["a"],[[1]]]"#, "[[],[[]]]"] {
            for framed in [payload.to_owned(), format!(" \t\n{payload}\r\n ")] {
                assert!(restore(Encoding::JsonRowsV1, &format!("{ROWS_HEADER}{framed}")).is_err());
                assert_eq!(
                    restore(Encoding::JsonV1, &format!("{JSON_HEADER}{framed}")).unwrap(),
                    payload
                );
            }
        }
        for payload in [
            r#"{"columns":["a"],"rows":[[1]]}"#,
            " \t\n{\"rows\": [[1]], \"columns\": [\"a\"]}\r\n ",
        ] {
            assert_eq!(
                restore(Encoding::JsonRowsV1, &format!("{ROWS_HEADER}{payload}")).unwrap(),
                r#"[{"a":1}]"#
            );
        }
    }

    #[test]
    fn malformed_tables_are_rejected() {
        for payload in [
            r#"{"columns":["a"],"rows":[[]]}"#,
            r#"{"columns":[],"rows":[[1]]}"#,
            r#"{"columns":["a","\u0061"],"rows":[[1,2]]}"#,
            r#"{"columns":[0],"rows":[[1]]}"#,
            r#"{"columns":[],"rows":{},"extra":0}"#,
            r#"{"columns":[],"rows":[],"extra":0}"#,
            r#"{"columns":[],"columns":[],"rows":[]}"#,
            r#"{"columns":["a"],"rows":[[{"x":1,"x":2}]]}"#,
            r#"{"columns":[]}"#,
        ] {
            assert!(
                restore(Encoding::JsonRowsV1, &format!("{ROWS_HEADER}{payload}")).is_err(),
                "{payload}"
            );
        }
        let empty = format!("{ROWS_HEADER}{{\"columns\":[],\"rows\":[]}}");
        assert_eq!(restore(Encoding::JsonRowsV1, &empty).unwrap(), "[]");
    }

    #[test]
    fn nesting_and_restore_amplification_are_bounded() {
        let nested = format!(
            "{}0{}",
            "[".repeat(MAX_DEPTH + 1),
            "]".repeat(MAX_DEPTH + 1)
        );
        assert!(candidates(&nested).is_empty());
        let column = "k".repeat(4096);
        let payload = format!(
            "{{\"columns\":[\"{column}\"],\"rows\":[{}]}}",
            vec!["[0]"; MAX_JSON_BYTES / column.len() + 1].join(",")
        );
        assert!(payload.len() < MAX_JSON_BYTES);
        assert!(restore(Encoding::JsonRowsV1, &format!("{ROWS_HEADER}{payload}")).is_err());
    }
}
