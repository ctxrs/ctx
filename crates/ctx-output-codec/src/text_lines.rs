//! Shared prefixes with literal lines, avoiding JSON string framing per line.
use anyhow::{Context, Result, ensure};

const HEADER: &str = "sift:lines-v1 [N,prefix] then N lines; prepend prefix\n";
const LIMIT: usize = 64 * 1024 * 1024;

fn emit(output: &mut String, lines: &[&str], prefix: &str) -> Option<()> {
    output.push_str(&serde_json::to_string(&(lines.len(), prefix)).ok()?);
    output.push('\n');
    for line in lines {
        output.push_str(&line[prefix.len()..]);
    }
    Some(())
}

pub(crate) fn candidate(input: &str) -> Option<String> {
    if input.len() > LIMIT {
        return None;
    }
    let lines: Vec<&str> = input.split_inclusive('\n').collect();
    let mut output = String::with_capacity(HEADER.len().saturating_add(input.len()));
    output.push_str(HEADER);
    let (mut cursor, mut pending) = (0, 0);
    let mut factored = false;
    while cursor + 1 < lines.len() {
        let prefix = crate::text_codec::common_prefix(lines[cursor], lines[cursor + 1]);
        let prefix = prefix.split('\n').next()?;
        if prefix.is_empty() {
            cursor += 1;
            continue;
        }
        let mut end = cursor + 2;
        while end < lines.len() && lines[end].starts_with(prefix) {
            end += 1;
        }
        let count = end - cursor;
        let prefix_header = crate::json_length::serialized(&(count, prefix))?;
        let literal_header = crate::json_length::serialized(&(count, ""))?;
        if prefix.len().checked_mul(count)? + literal_header > prefix_header {
            if pending < cursor {
                emit(&mut output, &lines[pending..cursor], "")?;
            }
            emit(&mut output, &lines[cursor..end], prefix)?;
            pending = end;
            factored = true;
        }
        // Consume an unprofitable group once, avoiding repeated suffix scans.
        cursor = end;
    }
    if !factored {
        return None;
    }
    if pending < lines.len() {
        emit(&mut output, &lines[pending..], "")?;
    }
    (output.len() <= LIMIT).then_some(output)
}

pub(crate) fn restore(input: &str) -> Result<String> {
    ensure!(
        input.len() <= LIMIT,
        "literal lines exceed the 64 MiB input limit"
    );
    let mut remaining =
        crate::strip_product_header(input, HEADER).context("invalid literal-lines header")?;
    let mut blocks = Vec::new();
    let mut total = 0usize;
    while !remaining.is_empty() {
        let (header, rest) = remaining
            .split_once('\n')
            .context("missing line-block header newline")?;
        let (count, prefix): (u64, String) =
            serde_json::from_str(header).context("invalid line-block header")?;
        ensure!(
            count > 0 && !prefix.contains('\n'),
            "line blocks need a positive count and a prefix without LF"
        );
        let count = usize::try_from(count).context("line count is too large")?;
        // Each line needs at least a newline, or the one final unterminated
        // line. Reject impossible counts before iterating attacker input.
        ensure!(
            count <= rest.len().saturating_add(1),
            "missing literal lines"
        );
        let prefix_bytes = prefix
            .len()
            .checked_mul(count)
            .context("line prefix size overflow")?;
        total = total
            .checked_add(prefix_bytes)
            .context("line size overflow")?;
        ensure!(
            total <= LIMIT,
            "literal lines exceed the 64 MiB restoration limit"
        );
        remaining = rest;
        let mut suffix_bytes = 0usize;
        for index in 0..count {
            let length = if let Some(end) = remaining.find('\n') {
                end + 1
            } else {
                ensure!(index + 1 == count, "missing literal lines");
                remaining.len()
            };
            suffix_bytes = suffix_bytes
                .checked_add(length)
                .context("line size overflow")?;
            remaining = &remaining[length..];
        }
        total = total
            .checked_add(suffix_bytes)
            .context("line size overflow")?;
        ensure!(
            total <= LIMIT,
            "literal lines exceed the 64 MiB restoration limit"
        );
        blocks.push((count, prefix, &rest[..suffix_bytes]));
    }
    let mut output = String::new();
    output
        .try_reserve_exact(total)
        .context("cannot allocate restored lines")?;
    for (count, prefix, mut suffixes) in blocks {
        for _ in 0..count {
            output.push_str(&prefix);
            let length = suffixes.find('\n').map_or(suffixes.len(), |end| end + 1);
            output.push_str(&suffixes[..length]);
            suffixes = &suffixes[length..];
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn independently_specified_blocks_preserve_literal_data() {
        let encoded = format!(
            "{HEADER}[2,\"src/\"]\na.rs\r\nb.rs\n[1,\"\"]\n[3,\"looks like a header\"]\n[1,\"last\"]\n"
        );
        assert_eq!(
            restore(&encoded).unwrap(),
            "src/a.rs\r\nsrc/b.rs\n[3,\"looks like a header\"]\nlast"
        );
    }

    #[test]
    fn encoder_retains_newlines_unicode_control_characters_and_empty_suffix() {
        for input in [
            "src/a.rs\nsrc/b.rs\n",
            "~/项目/🦀/a\r\n~/项目/🦀/b\r\nlast",
            "x\nx",
            "\r\n\r\n",
            "\n\n",
            "a\u{2028}x\na\u{2028}y",
            "same\"\t\0a\nsame\"\t\0b\n",
            "first\ncommon long prefix a\ncommon long prefix b\nlast",
        ] {
            if let Some(encoded) = candidate(input) {
                assert_eq!(restore(&encoded).unwrap(), input);
            }
        }
        assert!(candidate("").is_none());
        assert!(candidate("one line").is_none());
    }

    #[test]
    fn rejects_malformed_or_excessive_expansion() {
        for body in [
            "[0,\"x\"]\n",
            "[-1,\"x\"]\n",
            "[-0,\"x\"]\n",
            "[1.0,\"x\"]\n",
            "[1e0,\"x\"]\n",
            "[2,1]\na\nb",
            "[1,\"x\",0]\na",
            "[2,\"x\"]\na",
            "[1,\"a\\nb\"]\nx",
            "[18446744073709551616,\"x\"]\n",
        ] {
            assert!(restore(&format!("{HEADER}{body}")).is_err(), "{body}");
        }
        let prefix = "x".repeat(LIMIT / 2 + 1);
        let encoded = format!(
            "{HEADER}{}\n\n",
            serde_json::to_string(&(2, prefix)).unwrap()
        );
        assert!(restore(&encoded).is_err());
    }

    #[test]
    fn repeated_prefix_uses_literal_suffix_lines() {
        let input = "src/components/a.rs\nsrc/components/b.rs\n";
        let expected = format!("{HEADER}[2,\"src/components/\"]\na.rs\nb.rs\n");
        assert_eq!(candidate(input).unwrap(), expected);
        assert_eq!(restore(&expected).unwrap(), input);
    }
}
