//! Lossless text with direct references to earlier literal strings.
//!
//! After HEADER, the JSON array contains strings or unsigned integer indices.
//! A string appends itself; an index appends the exact earlier string at that
//! array position. References to references, self, or future entries are invalid.

use anyhow::{Context, Result, bail, ensure};
use serde::de::{Deserializer, Visitor};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt;

pub(crate) const HEADER: &str = "sift:text-refs-v1 concatenate strings; integer N copies the earlier string at zero-based array index N\n";
const MAX_RESTORED_BYTES: usize = 64 * 1024 * 1024;
const PREFIX_BUDGET: usize = 32;

#[derive(Serialize)]
#[serde(untagged)]
pub(crate) enum Entry {
    Literal(String),
    Reference(u64),
}

impl<'de> Deserialize<'de> for Entry {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct EntryVisitor;

        impl Visitor<'_> for EntryVisitor {
            type Value = Entry;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a literal string or unsigned integer reference")
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Entry, E> {
                Ok(Entry::Literal(value.to_owned()))
            }

            fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Entry, E> {
                Ok(Entry::Literal(value))
            }

            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Entry, E> {
                Ok(Entry::Reference(value))
            }
        }

        // Deliberately omit signed, floating-point, map and sequence visitors.
        // In particular, -0, 0.0 and 0e0 are not unsigned integer index tokens.
        deserializer.deserialize_any(EntryVisitor)
    }
}

/// Generated internally from this source; only this module constructs plans.
pub(crate) struct Plan<'a> {
    original: &'a str,
    entries: Vec<Entry>,
    references: String,
}

impl<'a> Plan<'a> {
    pub(crate) fn original(&self) -> &'a str {
        self.original
    }
    pub(crate) fn entries(&self) -> &[Entry] {
        &self.entries
    }
    pub(crate) fn references(&self) -> &str {
        &self.references
    }
    pub(crate) fn into_references(self) -> String {
        self.references
    }
}

fn flush_literal(entries: &mut Vec<Entry>, pending: &mut String) {
    if !pending.is_empty() {
        entries.push(Entry::Literal(std::mem::take(pending)));
    }
}

pub(crate) fn candidate(input: &str) -> Option<Plan<'_>> {
    if input.len() > MAX_RESTORED_BYTES {
        return None;
    }
    encode_fragments(input, input.split_inclusive('\n'))
}

/// Keep the first of each byte-identical plan. There are at most three plans,
/// so exact comparisons avoid both hashing assumptions and duplicate framing.
pub(crate) fn candidates(input: &str) -> Vec<Plan<'_>> {
    let mut distinct = Vec::new();
    for plan in candidate(input)
        .into_iter()
        .chain(fragment_candidates(input))
    {
        if !distinct
            .iter()
            .any(|earlier: &Plan<'_>| earlier.references == plan.references)
        {
            distinct.push(plan);
        }
    }
    distinct
}

fn encode_fragments<'a>(
    input: &'a str,
    fragments: impl Iterator<Item = &'a str> + Clone,
) -> Option<Plan<'a>> {
    let capacity = fragments.clone().size_hint().1.unwrap_or_default();
    let mut counts: HashMap<&str, usize> = HashMap::with_capacity(capacity);
    for line in fragments.clone() {
        *counts.entry(line).or_default() += 1;
    }
    let mut anchors: HashMap<&str, u64> = HashMap::with_capacity(counts.len());
    let mut entries = Vec::with_capacity(capacity);
    let mut pending = String::new();
    let mut used_reference = false;
    for line in fragments {
        if let Some(&index) = anchors.get(line) {
            flush_literal(&mut entries, &mut pending);
            entries.push(Entry::Reference(index));
            used_reference = true;
            continue;
        }

        let count = counts[line];
        if count > 1 {
            let index = u64::try_from(
                entries
                    .len()
                    .checked_add(usize::from(!pending.is_empty()))?,
            )
            .ok()?;
            if byte_saving(line, count, index.to_string().len())? > 0 {
                flush_literal(&mut entries, &mut pending);
                entries.push(Entry::Literal(line.to_owned()));
                anchors.insert(line, index);
                continue;
            }
        }
        // Only unreferenced text is coalesced. Never extend an anchor literal.
        pending.push_str(line);
    }
    if !used_reference {
        return None;
    }
    flush_literal(&mut entries, &mut pending);
    let references = format!("{HEADER}{}", serde_json::to_string(&entries).ok()?);
    Some(Plan {
        original: input,
        entries,
        references,
    })
}

// A conservative byte prefilter, never a token estimate. Isolating an anchor
// costs at most six framing bytes; allow four plus index digits per reference.
fn byte_saving(text: &str, count: usize, digits: usize) -> Option<usize> {
    let escaped = crate::json_length::serialized(text)?.checked_sub(2)?;
    Some(
        escaped
            .saturating_sub(digits.saturating_add(4))
            .saturating_mul(count.saturating_sub(1))
            .saturating_sub(6),
    )
}

/// Two competing segmentations, with every separator retained verbatim. The
/// second recognizes literal backslash+n bytes; it never interprets JSON/code.
pub(crate) fn fragment_candidates(input: &str) -> impl Iterator<Item = Plan<'_>> + '_ {
    ["\n", "\\n"]
        .into_iter()
        .filter_map(move |separator| fragment_candidate(input, separator))
}

/// One bounded literal segmentation; quotes and JSON syntax are not interpreted.
pub(crate) fn comma_candidate(input: &str) -> Option<Plan<'_>> {
    if input.len() > 64 * 1024 || input.split_inclusive(',').take(4097).count() > 4096 {
        return None;
    }
    fragment_candidate(input, ",")
}

fn fragment_candidate<'a>(input: &'a str, separator: &str) -> Option<Plan<'a>> {
    if input.len() > MAX_RESTORED_BYTES {
        return None;
    }
    let lines: Vec<&str> = input.split_inclusive(separator).collect();
    let mut counts: HashMap<&str, usize> = HashMap::with_capacity(lines.len());
    for &line in &lines {
        *counts.entry(line).or_default() += 1;
    }
    let digits = lines
        .len()
        .saturating_mul(2)
        .saturating_sub(1)
        .to_string()
        .len();
    let mut whole = HashSet::with_capacity(counts.len());
    let mut ordered = Vec::with_capacity(counts.len());
    for (&line, &count) in &counts {
        if count > 1 && byte_saving(line, count, digits)? > 0 {
            whole.insert(line);
        } else {
            ordered.push((line, count));
        }
    }
    ordered.sort_unstable_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut cumulative = vec![0usize];
    for &(_, count) in &ordered {
        cumulative.push(cumulative.last()?.checked_add(count)?);
    }
    let mut prefixes = HashSet::with_capacity(ordered.len());
    for pair in ordered.windows(2) {
        let prefix = crate::text_codec::common_prefix(pair[0].0, pair[1].0);
        if !prefix.is_empty() {
            prefixes.insert(prefix);
        }
    }
    let mut ranked = Vec::with_capacity(prefixes.len());
    for prefix in prefixes {
        let mut upper = prefix.as_bytes().to_vec();
        // Valid UTF-8 never ends in 0xff. This exclusive byte-order sentinel
        // need not itself be UTF-8; it is not part of any emitted string.
        let last = upper.last_mut()?;
        *last = last.checked_add(1)?;
        let start = ordered.partition_point(|(line, _)| line.as_bytes() < prefix.as_bytes());
        let end = ordered.partition_point(|(line, _)| line.as_bytes() < upper.as_slice());
        let count = cumulative[end].checked_sub(cumulative[start])?;
        let score = byte_saving(prefix, count, digits)?;
        if score > 0 {
            ranked.push((score, prefix));
        }
    }
    ranked.sort_unstable_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.as_bytes().cmp(b.1.as_bytes()))
    });
    ranked.truncate(PREFIX_BUDGET);
    // Matching prefixes are nested, so byte length and character length choose
    // the same longest match. All tie ordering uses UTF-8 bytes explicitly.
    ranked.sort_unstable_by(|a, b| {
        b.1.len()
            .cmp(&a.1.len())
            .then_with(|| a.1.as_bytes().cmp(b.1.as_bytes()))
    });
    let mut fragments = Vec::with_capacity(lines.len().saturating_mul(2));
    for line in lines {
        let prefix = if whole.contains(line) {
            None
        } else {
            ranked
                .iter()
                .map(|&(_, prefix)| prefix)
                .find(|prefix| line.starts_with(prefix))
        };
        if let Some(prefix) = prefix {
            fragments.push(prefix);
            let suffix = &line[prefix.len()..];
            if !suffix.is_empty() {
                fragments.push(suffix);
            }
        } else {
            fragments.push(line);
        }
    }
    encode_fragments(input, fragments.iter().copied())
}

fn literal_at(entries: &[Entry], position: usize) -> Result<&str> {
    match &entries[position] {
        Entry::Literal(text) => Ok(text),
        Entry::Reference(index) => {
            let index = usize::try_from(*index).context("text reference index is too large")?;
            ensure!(
                index < position,
                "text reference must point to an earlier string"
            );
            match &entries[index] {
                Entry::Literal(text) => Ok(text),
                Entry::Reference(_) => bail!("text reference cannot point to another reference"),
            }
        }
    }
}

pub(crate) fn restore(input: &str) -> Result<String> {
    let body = crate::strip_product_header(input, HEADER).context("invalid text-refs-v1 header")?;
    let entries: Vec<Entry> = serde_json::from_str(body).context("invalid text references")?;
    let mut total = 0usize;
    for position in 0..entries.len() {
        total = total
            .checked_add(literal_at(&entries, position)?.len())
            .context("text reference size overflow")?;
        ensure!(
            total <= MAX_RESTORED_BYTES,
            "text references exceed the 64 MiB restoration limit"
        );
    }
    let mut output = String::new();
    output
        .try_reserve_exact(total)
        .context("cannot allocate restored text")?;
    for position in 0..entries.len() {
        output.push_str(literal_at(&entries, position)?);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comma_discovery_accepts_limits_and_declines_overflow() {
        let byte_limit = "0123456789abcdefghijklmnopqrstu,".repeat(2048);
        assert_eq!(byte_limit.len(), 64 * 1024);
        assert!(comma_candidate(&byte_limit).is_some());
        assert!(comma_candidate(&(byte_limit + "x")).is_none());

        let fragment_limit = "repeat,".repeat(4096);
        assert!(comma_candidate(&fragment_limit).is_some());
        assert!(comma_candidate(&(fragment_limit + ",")).is_none());
    }

    #[test]
    fn comma_discovery_preserves_opaque_quotes_and_final_bytes() {
        let input = format!(
            "{}unterminated\0",
            "diagnostic: \"https://example.invalid/items?q=a,b\",雪\\n🦀\r\n".repeat(40)
        );
        let plan = comma_candidate(&input).unwrap();
        assert_eq!(
            restore(plan.references()).unwrap().as_bytes(),
            input.as_bytes()
        );
    }

    #[test]
    fn duplicate_plans_are_removed_without_dropping_distinct_segmentations() {
        let repeated =
            "worker completed operation; exact status retained with full details\n".repeat(1000);
        let first = candidate(&repeated).unwrap().into_references();
        let fragments: Vec<_> = fragment_candidates(&repeated)
            .map(Plan::into_references)
            .collect();
        assert_eq!(fragments.as_slice(), std::slice::from_ref(&first));
        assert_eq!(
            candidates(&repeated)
                .into_iter()
                .map(Plan::into_references)
                .collect::<Vec<_>>(),
            [first]
        );

        let mut mixed = String::new();
        for i in 0..40 {
            mixed.push_str("a long repeated warning retaining complete details\n");
            mixed.push_str(&format!("source/generated/components/Widget{i}.rs\n"));
            mixed.push_str(&format!("assets/generated/navigation/Icon{i}.svg\\n\n"));
        }
        let plans = candidates(&mixed);
        assert!(plans.len() >= 2);
        assert_eq!(
            plans[0].references(),
            candidate(&mixed).unwrap().references()
        );
        for (i, plan) in plans.iter().enumerate() {
            assert!(
                !plans[..i]
                    .iter()
                    .any(|old| old.references() == plan.references())
            );
            assert_eq!(restore(plan.references()).unwrap(), mixed);
        }
    }

    #[test]
    fn decodes_independently_written_sequences() {
        for (body, expected) in [
            (
                r#"["left\n","middle\r\n",0,"tail"]"#,
                "left\nmiddle\r\nleft\ntail",
            ),
            (r#"["",0,"A",2]"#, "AA"),
            (r#"["0",0]"#, "00"),
            (
                r#"["\"\\\u0000雪\r\n",0,"last"]"#,
                "\"\\\0雪\r\n\"\\\0雪\r\nlast",
            ),
            ("[]", ""),
            (" \n[\"a\", \"b\", 0]\t", "aba"),
        ] {
            assert_eq!(restore(&format!("{HEADER}{body}")).unwrap(), expected);
        }
    }

    #[test]
    fn anchors_remain_stable_when_unique_lines_are_coalesced() {
        let a = "a sufficiently long repeated line A\n";
        let b = "another sufficiently long repeated line B\n";
        let input = format!("intro a\nintro b\n{a}unique 1\nunique 2\n{b}{a}gap\n{b}tail");
        let expected_body = serde_json::json!([
            "intro a\nintro b\n",
            a,
            "unique 1\nunique 2\n",
            b,
            1,
            "gap\n",
            3,
            "tail"
        ])
        .to_string();
        let encoded = candidate(&input).unwrap();
        assert_eq!(encoded.references(), format!("{HEADER}{expected_body}"));
        assert_eq!(restore(encoded.references()).unwrap(), input);
    }

    #[test]
    fn nonadjacent_prefixes_use_existing_literal_anchors() {
        let first = "資料/🦀/コンポーネント/";
        let second = "assets/generated/vectors/";
        for (separator, ending) in [("\n", "\r\n"), ("\\n", "\\r\\n")] {
            let input = format!(
                "{first}A.rs{ending}{second}A.svg{ending}{first}B.rs{ending}{second}B.svg{ending}{first}C.rs{ending}{second}C.svg{ending}tail 🦀"
            );
            let expected = format!(
                "{HEADER}{}",
                serde_json::json!([
                    first,
                    format!("A.rs{ending}"),
                    second,
                    format!("A.svg{ending}"),
                    0,
                    format!("B.rs{ending}"),
                    2,
                    format!("B.svg{ending}"),
                    0,
                    format!("C.rs{ending}"),
                    2,
                    format!("C.svg{ending}tail 🦀")
                ])
            );
            assert_eq!(
                fragment_candidate(&input, separator).unwrap().references(),
                expected
            );
            assert_eq!(restore(&expected).unwrap().as_bytes(), input.as_bytes());
        }
    }

    #[test]
    fn whole_line_anchors_remain_whole_when_prefixes_are_available() {
        let repeated = "a complete recurring diagnostic that must stay a whole fragment\n";
        let prefix = "packages/generated/components/";
        let input =
            format!("{repeated}{prefix}A.rs\n{repeated}{prefix}B.rs\n{repeated}{prefix}C.rs\n");
        let expected = format!(
            "{HEADER}{}",
            serde_json::json!([repeated, prefix, "A.rs\n", 0, 1, "B.rs\n", 0, 1, "C.rs\n"])
        );
        assert_eq!(
            fragment_candidate(&input, "\n").unwrap().references(),
            expected
        );
        assert_eq!(restore(&expected).unwrap(), input);
    }

    #[test]
    fn fragment_candidates_keep_short_and_large_controls_unchanged() {
        for input in [
            "",
            "one line",
            "a\nb\na\n",
            "  a\n  b\n  c\n",
            "literal \\n with no repeated prefix",
            "é\nê\n🦀\n🦁\n",
        ] {
            assert!(fragment_candidates(input).next().is_none(), "{input:?}");
        }
        let input = "x".repeat(MAX_RESTORED_BYTES + 1);
        assert!(fragment_candidates(&input).next().is_none());
    }

    #[test]
    fn escaped_segments_are_not_interpreted_as_source_escapes() {
        // These are source characters, not a request to interpret a string literal.
        let prefix = r#"C:\\new\\names\\generated\\components\\"#;
        let input = format!("const text = \"{prefix}A.rs\\n{prefix}B.rs\\n{prefix}C.rs\";\r\n");
        for encoded in fragment_candidates(&input) {
            assert_eq!(
                restore(encoded.references()).unwrap().as_bytes(),
                input.as_bytes()
            );
        }
        assert!(fragment_candidate(&input, "\\n").is_some());
    }

    #[test]
    fn preserves_escapes_line_endings_markers_and_final_bytes() {
        for line in [
            "a repeated diagnostic with spaces  \n",
            "a repeated diagnostic with CRLF\r\n",
            "a repeated diagnostic with \"quotes\", \\ and \0\n",
            "a repeated diagnostic with 雪 and 🦀\n",
            HEADER,
        ] {
            for tail in ["", "unterminated", "\r", "\n"] {
                let input = format!("{line}unique\n{line}{tail}");
                assert_eq!(
                    restore(candidate(&input).unwrap().references()).unwrap(),
                    input
                );
            }
        }
    }

    #[test]
    fn skips_inputs_without_worthwhile_exact_repeats() {
        for input in [
            "",
            "unique",
            "a\nb\na\n",
            "\n\n\n\n",
            "a long line\na long line",
            "a long line\r\na long line\n",
        ] {
            assert!(candidate(input).is_none(), "{input:?}");
        }
        let input = "x".repeat(MAX_RESTORED_BYTES + 1);
        assert!(candidate(&input).is_none());
    }

    #[test]
    fn rejects_invalid_reference_targets_and_numeric_spellings() {
        for body in [
            "[0]",
            r#"["x",1]"#,
            r#"["x",2]"#,
            r#"["x",0,1]"#,
            r#"["x",-1]"#,
            r#"["x",-0]"#,
            r#"["x",0.0]"#,
            r#"["x",0e0]"#,
            r#"["x",18446744073709551615]"#,
            r#"["x",18446744073709551616]"#,
        ] {
            assert!(restore(&format!("{HEADER}{body}")).is_err(), "{body}");
        }
    }

    #[test]
    fn rejects_wrong_shapes_invalid_strings_and_framing() {
        for body in [
            "{}",
            "null",
            "\"literal\"",
            "[true]",
            "[null]",
            "[[]]",
            "[{}]",
            r#"[{"$serde_json::private::Number":"0"}]"#,
            r#"["\uD800"]"#,
            r#"["\uDC00"]"#,
            "[\"x\",]",
            "[] trailing",
        ] {
            assert!(restore(&format!("{HEADER}{body}")).is_err(), "{body}");
        }
        assert!(restore("[\"x\",0]").is_err());
        assert!(restore(&format!(" {HEADER}[]")).is_err());
    }

    #[test]
    fn checks_aggregate_expansion_before_allocating_output() {
        let literal = "x".repeat(16 * 1024);
        let repeats = MAX_RESTORED_BYTES / literal.len();
        let body = format!(
            "[{}{}]",
            serde_json::to_string(&literal).unwrap(),
            ",0".repeat(repeats - 1)
        );
        let restored = restore(&format!("{HEADER}{body}")).unwrap();
        assert_eq!(restored.len(), MAX_RESTORED_BYTES);
        assert!(restored.bytes().all(|byte| byte == b'x'));
        drop(restored);
        let excessive = format!("{},\"x\"]", &body[..body.len() - 1]);
        assert!(restore(&format!("{HEADER}{excessive}")).is_err());
    }
}
