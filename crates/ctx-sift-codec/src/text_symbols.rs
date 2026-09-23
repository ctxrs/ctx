//! Lossless, literal text with a JSON dictionary of single-character symbols.
//! Substitution visits only original body characters, never inserted values.

use crate::text_refs::{Entry, Plan};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde::de::{MapAccess, Visitor};
use std::collections::HashMap;
use std::fmt;

const HEADER: &str = "sift:symbols-v1 substitute each character using this JSON dictionary:\n";
const LIMIT: usize = 64 * 1024 * 1024;
const SYMBOLS: &str = "§¶¤†‡°ªºµ½¼¾¿¡¢£¥©®™±÷×•–—αβγδελπΩΔΣθσφψω✓★☆♦●○■□→←↑↓∞";

pub(crate) fn candidate(plan: &Plan<'_>, count: impl Fn(&str) -> usize) -> Option<String> {
    let original = plan.original();
    if plan.references().len() > LIMIT {
        return None;
    }
    let entries = plan.entries();
    let mut uses = vec![0usize; entries.len()];
    for entry in entries {
        if let Entry::Reference(index) = entry {
            uses[usize::try_from(*index).ok()?] += 1;
        }
    }
    let mut ranked = Vec::new();
    for (index, &references) in uses.iter().enumerate() {
        if references == 0 {
            continue;
        }
        let Entry::Literal(literal) = &entries[index] else {
            return None;
        };
        let tokens = count(literal) as i128;
        let score = (tokens - 1) * (references as i128 + 1) - tokens - 5;
        if score > 0 {
            ranked.push((score, index));
        }
    }
    ranked.sort_unstable_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let available = SYMBOLS.chars().filter(|&symbol| !original.contains(symbol));
    let selected: Vec<_> = ranked.into_iter().take(32).zip(available).collect();
    if selected.is_empty() {
        return None;
    }

    let mut keys = HashMap::with_capacity(selected.len());
    let mut output = String::with_capacity(HEADER.len().saturating_add(plan.references().len()));
    output.push_str(HEADER);
    output.push('{');
    for ((_, index), symbol) in selected {
        let Entry::Literal(literal) = &entries[index] else {
            return None;
        };
        if !keys.is_empty() {
            output.push(',');
        }
        output.push_str(&serde_json::to_string(&symbol).ok()?);
        output.push(':');
        output.push_str(&serde_json::to_string(literal).ok()?);
        keys.insert(index, symbol);
    }
    output.push_str("}\n");
    for (position, entry) in entries.iter().enumerate() {
        let index = match entry {
            Entry::Literal(_) => position,
            Entry::Reference(index) => usize::try_from(*index).ok()?,
        };
        if let Some(&symbol) = keys.get(&index) {
            output.push(symbol);
        } else if let Entry::Literal(literal) = &entries[index] {
            output.push_str(literal);
        }
        if output.len() > LIMIT {
            return None;
        }
    }
    Some(output)
}

struct Dictionary(HashMap<char, String>);

impl<'de> Deserialize<'de> for Dictionary {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct DictionaryVisitor;

        impl<'de> Visitor<'de> for DictionaryVisitor {
            type Value = Dictionary;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .write_str("a dictionary of unique single-character keys and string values")
            }

            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Dictionary, M::Error> {
                let mut entries = HashMap::new();
                while let Some(key) = map.next_key::<String>()? {
                    let mut chars = key.chars();
                    let (Some(symbol), None) = (chars.next(), chars.next()) else {
                        return Err(serde::de::Error::custom(
                            "symbol key must be one Unicode scalar",
                        ));
                    };
                    if entries.contains_key(&symbol) {
                        return Err(serde::de::Error::custom("duplicate symbol key"));
                    }
                    entries.insert(symbol, map.next_value::<String>()?);
                }
                Ok(Dictionary(entries))
            }
        }

        deserializer.deserialize_map(DictionaryVisitor)
    }
}

pub(crate) fn restore(input: &str) -> Result<String> {
    ensure!(
        input.len() <= LIMIT,
        "symbols exceed the 64 MiB input limit"
    );
    let (dictionary, body) = crate::strip_product_header(input, HEADER)
        .context("invalid symbols-v1 header")?
        .split_once('\n')
        .context("missing symbol dictionary newline")?;
    let Dictionary(dictionary) =
        serde_json::from_str(dictionary).context("invalid symbol dictionary")?;
    let mut total = 0usize;
    for symbol in body.chars() {
        let bytes = dictionary
            .get(&symbol)
            .map_or(symbol.len_utf8(), String::len);
        total = total.checked_add(bytes).context("symbol size overflow")?;
        ensure!(
            total <= LIMIT,
            "symbols exceed the 64 MiB restoration limit"
        );
    }
    let mut output = String::new();
    output
        .try_reserve_exact(total)
        .context("cannot allocate restored symbols")?;
    // The first pass checks size; only this pass substitutes, without recursion.
    for symbol in body.chars() {
        if let Some(literal) = dictionary.get(&symbol) {
            output.push_str(literal);
        } else {
            output.push(symbol);
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text_refs;

    fn scalar_count(text: &str) -> usize {
        text.chars().count()
    }

    #[test]
    fn independently_written_frames_preserve_literal_bytes() {
        for (frame, expected) in [
            ("{}\n", ""),
            ("{}\nraw\r\n\0雪\\n\"tail", "raw\r\n\0雪\\n\"tail"),
            (
                "{\"§\":\"src/🦀/\"}\n§a.rs\r\n§b.rs",
                "src/🦀/a.rs\r\nsrc/🦀/b.rs",
            ),
            ("{\"x\":\"\"}\nxax", "a"),
            ("{\"x\":\"unused\"}\n", ""),
            (
                "{\"\\n\":\"LF\",\"\\u0000\":\"NUL\",\"🦀\":\"crab\"}\n\n\0🦀",
                "LFNULcrab",
            ),
            ("{\"\\uD83E\\uDD80\":\"雪\"}\n🦀", "雪"),
        ] {
            assert_eq!(restore(&format!("{HEADER}{frame}")).unwrap(), expected);
        }
    }

    #[test]
    fn replacement_values_are_never_substituted_again() {
        assert_eq!(
            restore(&format!("{HEADER}{{\"x\":\"y\",\"y\":\"xx\"}}\nxy")).unwrap(),
            "yxx"
        );
        let frame = format!(
            "{HEADER}{{\"x\":{},\"r\":\"stop\"}}\nx{{\"x\":0}}\r\n",
            serde_json::to_string(HEADER).unwrap()
        );
        assert_eq!(
            restore(&frame).unwrap(),
            format!("{HEADER}{{\"{HEADER}\":0}}\r\n")
        );
    }

    #[test]
    fn rejects_bad_dictionary_keys_values_unicode_and_frames() {
        for dictionary in [
            "[]",
            "null",
            "1",
            "\"text\"",
            "{1:\"x\"}",
            "{\"\":\"x\"}",
            "{\"ab\":\"x\"}",
            "{\"é\":\"x\"}",
            "{\"x\":1}",
            "{\"x\":null}",
            "{\"x\":true}",
            "{\"x\":[]}",
            "{\"x\":{}}",
            r#"{"x":"a","x":"b"}"#,
            r#"{"x":"a","\u0078":"b"}"#,
            r#"{"🦀":"a","\uD83E\uDD80":"b"}"#,
            r#"{"\uD800":"x"}"#,
            r#"{"x":"\uDC00"}"#,
            r#"{"x":{"$serde_json::private::Number":"0"}}"#,
            "{\"x\":\"a\",}",
            "{} trailing",
            "{\"x\":\"a\nb\"}",
        ] {
            assert!(
                restore(&format!("{HEADER}{dictionary}\nx")).is_err(),
                "{dictionary:?}"
            );
        }
        for input in [
            "{}\nx",
            HEADER,
            &format!("{HEADER}{{}}"),
            &format!(" {HEADER}{{}}\n"),
        ] {
            assert!(restore(input).is_err(), "{input:?}");
        }
    }

    #[test]
    fn candidate_ranks_scores_then_original_indices() {
        let original = "first literal\r\nsecond literal\nfirst literal\r\nsecond literal\n";
        let plan = text_refs::candidate(original).unwrap();
        assert_eq!(
            plan.references(),
            format!(
                "{}[\"first literal\\r\\n\",\"second literal\\n\",0,1]",
                text_refs::HEADER
            )
        );
        let ranked = candidate(&plan, |text| match text {
            "first literal\r\n" => 10,
            "second literal\n" => 20,
            _ => scalar_count(text),
        })
        .unwrap();
        assert_eq!(
            ranked,
            format!("{HEADER}{{\"§\":\"second literal\\n\",\"¶\":\"first literal\\r\\n\"}}\n¶§¶§")
        );
        let tied = candidate(&plan, |text| {
            if text.len() > 4 {
                10
            } else {
                scalar_count(text)
            }
        })
        .unwrap();
        assert_eq!(
            tied,
            format!("{HEADER}{{\"§\":\"first literal\\r\\n\",\"¶\":\"second literal\\n\"}}\n§¶§¶")
        );
        assert_eq!(restore(&ranked).unwrap(), original);
        assert_eq!(restore(&tied).unwrap(), original);
    }

    #[test]
    fn candidate_keeps_source_syntax_literal() {
        let original = "long repeated literal\nx§\0\"\\\r\nlong repeated literal\n";
        let plan = text_refs::candidate(original).unwrap();
        let encoded = candidate(&plan, scalar_count).unwrap();
        assert_eq!(
            encoded,
            format!("{HEADER}{{\"¶\":\"long repeated literal\\n\"}}\n¶x§\0\"\\\r\n¶")
        );
        assert_eq!(restore(&encoded).unwrap(), original);
        for literal in [
            text_refs::HEADER,
            HEADER,
            "[\"literal\",0,1]\r\n",
            "{\"§\":\"injection\"}\n",
            "雪🦀é\0 quoted \\\"\r\n",
        ] {
            let original = literal.repeat(3);
            let plan = text_refs::candidate(&original).unwrap();
            assert_eq!(
                restore(&candidate(&plan, scalar_count).unwrap()).unwrap(),
                original
            );
        }
    }

    #[test]
    fn declines_exhausted_symbols_and_nonpositive_scores() {
        for original in ["", "aa", "unique"] {
            assert!(text_refs::candidate(original).is_none());
        }
        let original = format!("long literal {SYMBOLS}\n").repeat(2);
        assert!(candidate(&text_refs::candidate(&original).unwrap(), scalar_count).is_none());
        let original = "long literal\n".repeat(2);
        let plan = text_refs::candidate(&original).unwrap();
        assert!(candidate(&plan, |s| if s.len() > 4 { 7 } else { 1 }).is_none());
    }

    #[test]
    fn reserved_symbols_are_single_ordinary_o200k_tokens() {
        let tokenizer = crate::tokenizer::o200k_base();
        for symbol in SYMBOLS.chars() {
            assert_eq!(tokenizer.count(symbol.encode_utf8(&mut [0; 4])), 1);
        }
    }

    #[test]
    fn candidate_has_only_a_thirty_two_symbol_budget() {
        let literals = (0..40)
            .map(|i| format!("literal number {i:02}\n"))
            .collect::<Vec<_>>();
        let original = literals.concat().repeat(2);
        let plan = text_refs::candidate(&original).unwrap();
        let encoded = candidate(&plan, scalar_count).unwrap();
        let (dictionary, body) = encoded
            .strip_prefix(HEADER)
            .unwrap()
            .split_once('\n')
            .unwrap();
        let Dictionary(dictionary) = serde_json::from_str(dictionary).unwrap();
        assert_eq!(dictionary.len(), 32);
        assert_eq!(dictionary[&'§'], literals[0]);
        assert!(body.contains(&literals[32]));
        assert_eq!(restore(&encoded).unwrap(), original);
    }

    #[test]
    fn oversized_serialized_reference_plan_declines_before_counting() {
        let literal = format!("{}\n", "\"".repeat(LIMIT / 2 - 1));
        let original = literal.repeat(2);
        assert_eq!(original.len(), LIMIT);
        let plan = text_refs::candidate(&original).unwrap();
        assert!(plan.references().len() > LIMIT);
        assert!(candidate(&plan, |_| panic!("must decline before counting")).is_none());
    }

    #[test]
    fn bounds_restored_utf8_bytes_before_allocating_without_recursion() {
        let literal = format!("y{}", "雪".repeat(5461));
        assert_eq!(literal.len(), 16 * 1024);
        let dictionary = serde_json::json!({"x": literal, "y": "x".repeat(16 * 1024)});
        let frame = format!(
            "{HEADER}{dictionary}\n{}",
            "x".repeat(LIMIT / literal.len())
        );
        let restored = restore(&frame).unwrap();
        assert_eq!(restored.len(), LIMIT);
        assert!(restored.starts_with(&literal) && restored.ends_with(&literal));
        drop(restored);
        assert!(restore(&format!("{frame}z")).is_err());
    }

    #[test]
    fn accepts_exact_input_limit_and_rejects_larger_frames_and_originals() {
        let mut frame = format!("{HEADER}{{}}\n");
        frame.push_str(&"x".repeat(LIMIT - frame.len()));
        assert_eq!(restore(&frame).unwrap().len(), LIMIT - HEADER.len() - 3);
        frame.push('x');
        assert!(restore(&frame).is_err());
        assert!(text_refs::candidate(&frame).is_none());
    }

    #[test]
    fn encoder_accepts_an_original_at_the_restoration_limit() {
        let literal = format!("{}\n", "x".repeat(16 * 1024 - 1));
        let original = literal.repeat(LIMIT / literal.len());
        let plan = text_refs::candidate(&original).unwrap();
        let encoded = candidate(&plan, scalar_count).unwrap();
        assert_eq!(restore(&encoded).unwrap(), original);
    }
}
