use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

const HEADER: &str = "sift:text-runs-v1 counts repeat exact JSON strings; concatenate\n";
const PREFIX_HEADER: &str = "sift:text-prefixes-v1 strings are literal; [prefix,[suffixes]] repeats prefix before each suffix; concatenate\n";
const MAX_RESTORED_BYTES: usize = 64 * 1024 * 1024;

pub(crate) fn candidate(input: &str) -> Option<String> {
    if input.len() > MAX_RESTORED_BYTES {
        return None;
    }
    let mut runs: Vec<(u64, &str)> = Vec::new();
    let mut repeated = false;
    for line in input.split_inclusive('\n') {
        match runs.last_mut() {
            Some((count, previous)) if *previous == line => {
                *count += 1;
                repeated = true;
            }
            _ => runs.push((1, line)),
        }
    }
    if !repeated {
        return None;
    }
    let mut coalesced: Vec<(u64, String)> = Vec::new();
    for (count, text) in runs {
        if count == 1
            && let Some((1, previous)) = coalesced.last_mut()
        {
            previous.push_str(text);
        } else {
            coalesced.push((count, text.to_owned()));
        }
    }
    Some(format!(
        "{HEADER}{}",
        serde_json::to_string(&coalesced).ok()?
    ))
}

// Borrow strings while encoding; own JSON-unescaped strings while restoring.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum PrefixEntry<S> {
    Literal(S),
    Group((S, Vec<S>)),
}

pub(crate) fn common_prefix<'a>(left: &'a str, right: &str) -> &'a str {
    let mut length = left
        .bytes()
        .zip(right.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    while !left.is_char_boundary(length) {
        length -= 1;
    }
    &left[..length]
}

pub(crate) fn prefix_candidate(input: &str) -> Option<String> {
    if input.len() > MAX_RESTORED_BYTES {
        return None;
    }
    let lines: Vec<&str> = input.split_inclusive('\n').collect();
    let mut entries = Vec::with_capacity(lines.len());
    let (mut i, mut offset, mut literal_start) = (0, 0, 0);
    let mut factored = false;
    while i + 1 < lines.len() {
        let prefix = common_prefix(lines[i], lines[i + 1]);
        if prefix.is_empty() {
            offset += lines[i].len();
            i += 1;
            continue;
        }
        let mut end = i + 2;
        while end < lines.len() && lines[end].starts_with(prefix) {
            end += 1;
        }
        let length: usize = lines[i..end].iter().map(|line| line.len()).sum();
        let group = PrefixEntry::Group((
            prefix,
            lines[i..end]
                .iter()
                .map(|line| &line[prefix.len()..])
                .collect(),
        ));
        let literal = &input[offset..offset + length];
        if crate::json_length::serialized(&group)? < crate::json_length::serialized(literal)? {
            if literal_start < offset {
                entries.push(PrefixEntry::Literal(&input[literal_start..offset]));
            }
            entries.push(group);
            literal_start = offset + length;
            factored = true;
        }
        // Consume rejected groups as literal text too. Rescanning every suffix
        // of an unprofitable group would make ordinary shared indentation quadratic.
        offset += length;
        i = end;
    }
    if !factored {
        return None;
    }
    if literal_start < input.len() {
        entries.push(PrefixEntry::Literal(&input[literal_start..]));
    }
    Some(format!(
        "{PREFIX_HEADER}{}",
        serde_json::to_string(&entries).ok()?
    ))
}

pub(crate) fn restore_prefixes(input: &str) -> Result<String> {
    let body = crate::strip_product_header(input, PREFIX_HEADER)
        .context("invalid text-prefixes-v1 header")?;
    let entries: Vec<PrefixEntry<String>> =
        serde_json::from_str(body).context("invalid text prefixes")?;
    let mut total = 0usize;
    for entry in &entries {
        let length = match entry {
            PrefixEntry::Literal(text) => text.len(),
            PrefixEntry::Group((prefix, suffixes)) => {
                let length = prefix
                    .len()
                    .checked_mul(suffixes.len())
                    .context("text prefix size overflow")?;
                suffixes
                    .iter()
                    .try_fold(length, |size, suffix| size.checked_add(suffix.len()))
                    .context("text prefix size overflow")?
            }
        };
        total = total
            .checked_add(length)
            .context("text prefix size overflow")?;
        ensure!(
            total <= MAX_RESTORED_BYTES,
            "text prefixes exceed the 64 MiB restoration limit"
        );
    }
    let mut output = String::new();
    output
        .try_reserve_exact(total)
        .context("cannot allocate restored text")?;
    for entry in entries {
        match entry {
            PrefixEntry::Literal(text) => output.push_str(&text),
            PrefixEntry::Group((prefix, suffixes)) => {
                for suffix in suffixes {
                    output.push_str(&prefix);
                    output.push_str(&suffix);
                }
            }
        }
    }
    Ok(output)
}

pub(crate) fn restore(input: &str) -> Result<String> {
    let body = crate::strip_product_header(input, HEADER).context("invalid text-runs-v1 header")?;
    let runs: Vec<(u64, String)> = serde_json::from_str(body).context("invalid text runs")?;
    let mut total = 0usize;
    for (count, line) in &runs {
        ensure!(
            *count > 0 && !line.is_empty(),
            "text runs require positive counts and nonempty strings"
        );
        let count = usize::try_from(*count).context("text run count is too large")?;
        total = line
            .len()
            .checked_mul(count)
            .and_then(|size| total.checked_add(size))
            .context("text run size overflow")?;
        ensure!(
            total <= MAX_RESTORED_BYTES,
            "text runs exceed the 64 MiB restoration limit"
        );
    }
    let mut output = String::new();
    output
        .try_reserve_exact(total)
        .context("cannot allocate restored text")?;
    for (count, line) in runs {
        for _ in 0..count {
            output.push_str(&line);
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_exact_bytes_and_safe_framing() {
        for input in [
            "x\nx\n",
            "x\r\nx\r\nx",
            "\n\n",
            "\r\n\r\n",
            "\"\\\0🦀\n\"\\\0🦀\nend",
            "x\nx\ny\ny",
            "sift:text-runs-v1\nsift:text-runs-v1\n",
        ] {
            assert_eq!(restore(&candidate(input).unwrap()).unwrap(), input);
        }
        for input in ["", "x", "x\nx", "x\r\nx\n"] {
            assert!(candidate(input).is_none());
        }
    }

    #[test]
    fn rejects_malformed_and_excessive_expansion_before_allocating() {
        for body in [
            "[[0,\"a\"]]",
            "[[1,\"\"]]",
            "[[-1,\"a\"]]",
            "[[1.0,\"a\"]]",
            "[[67108865,\"x\"]]",
            "[[18446744073709551615,\"xx\"]]",
            "[[1,\"a\",0]]",
            "[[1,\"a\"]] trailing",
        ] {
            assert!(restore(&format!("{HEADER}{body}")).is_err(), "{body}");
        }
        assert!(restore("[[1,\"a\"]]").is_err());
        assert_eq!(restore(&format!("{HEADER}[]")).unwrap(), "");
    }

    #[test]
    fn coalesces_literal_spans_around_repeated_lines() {
        let input = "first\r\nsecond\nrepeat\r\nrepeat\r\nthird\nlast 🦀";
        let encoded = candidate(input).unwrap();
        assert_eq!(
            encoded,
            format!(
                "{HEADER}{}",
                r#"[[1,"first\r\nsecond\n"],[2,"repeat\r\n"],[1,"third\nlast 🦀"]]"#
            )
        );
        assert_eq!(restore(&encoded).unwrap(), input);
    }

    #[test]
    fn prefix_grammar_preserves_literals_unicode_and_line_endings() {
        let input = "begin\nsource/components/🦀-a.rs\r\nsource/components/🦀-b.rs\r\nsource/components/🦀-c.rs\nend 🦀";
        let expected = format!(
            "{PREFIX_HEADER}{}",
            r#"["begin\n",["source/components/🦀-",["a.rs\r\n","b.rs\r\n","c.rs\n"]],"end 🦀"]"#
        );
        assert_eq!(prefix_candidate(input).unwrap(), expected);
        assert_eq!(restore_prefixes(&expected).unwrap(), input);
        // These characters share leading UTF-8 bytes, but no complete character.
        assert_eq!(common_prefix("é", "ê"), "");
        assert_eq!(common_prefix("x🦀", "x🦁"), "x");
        let escaped = format!(
            "{PREFIX_HEADER}{}",
            r#"["\u0000",["\"\\🦀",["a\r\n","b"]],"tail"]"#
        );
        assert_eq!(
            restore_prefixes(&escaped).unwrap(),
            "\0\"\\🦀a\r\n\"\\🦀btail"
        );
        for text in ["", "one line", "a\nb\n", " x\n y\n z\n"] {
            assert!(prefix_candidate(text).is_none());
        }
    }

    #[test]
    fn prefix_decoder_rejects_invalid_shapes_and_bounds_expansion() {
        for body in [
            r#"[1]"#,
            r#"[["p","suffix"]]"#,
            r#"[["p",[1]]]"#,
            r#"[["p",[],"extra"]]"#,
            r#"[{"prefix":"x"}]"#,
            r#"[["x"]]"#,
            r#"[] trailing"#,
        ] {
            assert!(
                restore_prefixes(&format!("{PREFIX_HEADER}{body}")).is_err(),
                "{body}"
            );
        }
        assert!(restore_prefixes("[]").is_err());
        assert_eq!(
            restore_prefixes(&format!(
                "{PREFIX_HEADER}{}",
                r#"["",["",["", "x"]],["unused",[]]]"#
            ))
            .unwrap(),
            "x"
        );
        let prefix = "x".repeat(1024);
        let suffixes = vec![""; MAX_RESTORED_BYTES / prefix.len() + 1];
        let payload =
            serde_json::to_string(&vec![PrefixEntry::Group((prefix.as_str(), suffixes))]).unwrap();
        assert!(restore_prefixes(&format!("{PREFIX_HEADER}{payload}")).is_err());
    }
}
