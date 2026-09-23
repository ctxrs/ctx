// Adapted from Sift, MIT; source revision and license are in this crate’s NOTICE.
//! Flat Tape presentation based on TAP13 and Tape 5.9.0 emitter behavior.
//! https://testanything.org/tap-version-13-specification.html
//! https://github.com/tape-testing/tape/blob/v5.9.0/lib/results.js (MIT)
//! None means lossless fallback. Callers own capture, opt-outs, exit/stderr,
//! and exact-token selection; passing names may be omitted explicitly.
use std::ffi::OsString;
use std::fmt::Write;

const MAX_BYTES: usize = 8 * 1024 * 1024;

pub(crate) fn eligible(argv: &[OsString]) -> bool {
    let Some(node) = argv.first().and_then(|s| s.to_str()) else {
        return false;
    };
    if node.contains('\0') || !matches!(node.rsplit(['/', '\\']).next(), Some("node" | "node.exe"))
    {
        return false;
    }
    let Some(entry) = argv.get(1).and_then(|s| s.to_str()) else {
        return false;
    };
    let parts: Vec<_> = entry.split(['/', '\\']).collect();
    if !parts.ends_with(&["node_modules", "tape", "bin", "tape"])
        || entry.starts_with('-')
        || entry.contains('\0')
    {
        return false;
    }
    let mut files = &argv[2..];
    if files.first().is_some_and(|s| s == "--") {
        files = &files[1..];
    }
    !files.is_empty()
        && files.iter().all(|s| {
            s.to_str()
                .is_some_and(|s| !s.is_empty() && !s.starts_with('-') && !s.contains('\0'))
        })
}

fn number(s: &str) -> Option<usize> {
    if s.is_empty() || (s.len() > 1 && s.starts_with('0')) {
        return None;
    }
    s.bytes().try_fold(0usize, |n, b| {
        if !b.is_ascii_digit() {
            return None;
        }
        n.checked_mul(10)?.checked_add(usize::from(b - b'0'))
    })
}

fn directive(s: &str) -> Option<()> {
    let word = s.trim_start_matches(' ').split(' ').next()?;
    (word.eq_ignore_ascii_case("skip") || word.eq_ignore_ascii_case("todo")).then_some(())
}

pub(crate) fn candidate(argv: &[OsString], text: &str) -> Option<String> {
    render(argv, text, false)
}

/// A validated terminal Tape footer ends the initial presentation. All later
/// bytes are opaque delivered text; no child-status interpretation is made.
pub(crate) fn delivered_candidate(argv: &[OsString], text: &str) -> Option<String> {
    render(argv, text, true)
}

fn structural_line(line: &str) -> bool {
    line.ends_with('\n') && !line.chars().any(|c| c.is_control() && c != '\n')
}

fn render(argv: &[OsString], text: &str, delivered: bool) -> Option<String> {
    if !eligible(argv) || text.len() > MAX_BYTES || (!delivered && !structural_line(text)) {
        return None;
    }
    // LF-only splitting preserves Unicode separators and every retained byte.
    let lines: Vec<_> = text.split_inclusive('\n').collect();
    if lines.first().copied()? != "TAP version 13\n" {
        return None;
    }
    let mut omit = vec![None; lines.len()];
    let (mut count, mut passes, mut failures) = (0usize, 0usize, 0usize);
    let (mut plan, mut trailing_plan) = (None, false);
    let mut owner = None;
    let mut has_directives = false;
    let mut footer = [None; 3]; // tests, pass, fail
    let (mut footer_started, mut footer_ok) = (false, false);
    let mut i = 1;
    while i < lines.len() {
        if delivered && !structural_line(lines[i]) {
            return None;
        }
        let line = lines[i].strip_suffix('\n')?;
        if line == "  ---" {
            let row: usize = owner?;
            if row.checked_add(1)? != i {
                return None;
            }
            omit[row] = None;
            i = i.checked_add(1)?;
            while i < lines.len() && lines[i] != "  ...\n" {
                if delivered && !structural_line(lines[i]) {
                    return None;
                }
                let body = lines[i].strip_suffix('\n')?;
                // Opaque framing only: embedded result-looking text is data.
                if !body.is_empty() && (!body.starts_with("  ") || body == "  ---") {
                    return None;
                }
                i = i.checked_add(1)?;
            }
            if i == lines.len() {
                return None;
            }
            i = i.checked_add(1)?;
            continue;
        }
        if line.is_empty() {
            i = i.checked_add(1)?;
            continue;
        }
        if let Some(comment) = line.strip_prefix('#') {
            if let Some(row) = owner {
                omit[row] = None;
            }
            let body = comment.trim_start_matches(' ');
            if body.to_ascii_lowercase().starts_with("subtest:") {
                return None;
            }
            let word = body.split(' ').next()?;
            if let Some(slot) = ["tests", "pass", "fail"].iter().position(|s| *s == word) {
                if footer[slot].is_some() {
                    return None;
                }
                footer[slot] = Some(number(body.strip_prefix(word)?.trim_start_matches(' '))?);
                footer_started = true;
            } else if word == "ok" {
                if body != "ok" || footer_ok {
                    return None;
                }
                footer_ok = true;
                footer_started = true;
            } else if word == "todo" {
                return None;
            }
            if delivered && trailing_plan && !has_directives && (word == "ok" || word == "fail") {
                // Only the exact canonical terminal footer opens the opaque tail.
                // Counts and all earlier structure still pass the shared checks below.
                let ending = if failures > 0 {
                    format!("# fail  {failures}\n")
                } else {
                    "\n# ok\n".to_owned()
                };
                let canonical =
                    format!("\n1..{count}\n# tests {count}\n# pass  {passes}\n{ending}");
                let mut preceding = lines[..=i].iter().rev();
                if canonical
                    .split_inclusive('\n')
                    .rev()
                    .all(|expected| preceding.next().copied() == Some(expected))
                {
                    break;
                }
            }
            i = i.checked_add(1)?;
            continue;
        }
        if let Some(rest) = line.strip_prefix("1..") {
            if plan.is_some() {
                return None;
            }
            let n = if let Some((n, reason)) = rest.split_once(" # ") {
                if number(n)? != 0 || !reason.to_ascii_lowercase().starts_with("skip") {
                    return None;
                }
                0
            } else {
                number(rest)?
            };
            plan = Some(n);
            trailing_plan = count > 0;
            owner = None;
            i = i.checked_add(1)?;
            continue;
        }
        if trailing_plan || footer_started {
            return None;
        }
        let (pass, rest) = if let Some(s) = line.strip_prefix("ok ") {
            (true, s)
        } else {
            (false, line.strip_prefix("not ok ")?)
        };
        let (digits, description) = rest.split_once(' ').unwrap_or((rest, ""));
        count = count.checked_add(1)?;
        if number(digits)? != count {
            return None;
        }
        let directed = if let Some((name, suffix)) = description.split_once('#') {
            if name.ends_with('\\') {
                return None;
            }
            directive(suffix)?;
            true
        } else {
            false
        };
        has_directives |= directed;
        if pass {
            passes = passes.checked_add(1)?;
        } else {
            failures = failures.checked_add(1)?;
        }
        if pass && !directed {
            omit[i] = Some(count);
        }
        owner = Some(i);
        i = i.checked_add(1)?;
    }
    if plan? != count
        || footer[0].is_some_and(|n| n != count)
        || footer[1].is_some_and(|n| n != passes)
        || footer[2].is_some_and(|n| n != failures)
        || (footer_ok && failures != 0)
        || (has_directives && (footer[1].is_some() || footer[2].is_some() || footer_ok))
    {
        return None;
    }
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < lines.len() {
        if let Some(first) = omit[i] {
            let mut last = first;
            let start = i;
            i = i.checked_add(1)?;
            while let Some(Some(n)) = omit.get(i) {
                last = *n;
                i = i.checked_add(1)?;
            }
            let mut marker = String::new();
            writeln!(
                marker,
                "# omitted {} ordinary passing names/rows: tests {}..{} (human view, not TAP)",
                last.checked_sub(first)?.checked_add(1)?,
                first,
                last
            )
            .ok()?;
            let original_bytes: usize = lines[start..i].iter().map(|s| s.len()).sum();
            if marker.len() < original_bytes {
                out.push_str(&marker);
            } else {
                for line in &lines[start..i] {
                    out.push_str(line);
                }
            }
        } else {
            out.push_str(lines[i]);
            i = i.checked_add(1)?;
        }
    }
    (out.len() < text.len()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn argv() -> Vec<OsString> {
        ["node", "./node_modules/tape/bin/tape", "test.js"]
            .map(Into::into)
            .to_vec()
    }
    const ROWS: &str = "ok 1 first independently authored ordinary assertion with a deliberately descriptive name\nok 2 second independently authored ordinary assertion with a deliberately descriptive name\nok 3 third independently authored ordinary assertion with a deliberately descriptive name\n";
    const MARK: &str =
        "# omitted 3 ordinary passing names/rows: tests 1..3 (human view, not TAP)\n";
    fn baseline() -> String {
        format!("TAP version 13\n# sample group\n{ROWS}\n1..3\n# tests 3\n# pass  3\n\n# ok\n\n")
    }

    #[test]
    fn whole_canonical_tape_field_and_leading_plan() {
        assert_eq!(
            candidate(&argv(), &baseline()).as_deref(),
            Some(
                "TAP version 13\n# sample group\n# omitted 3 ordinary passing names/rows: tests 1..3 (human view, not TAP)\n\n1..3\n# tests 3\n# pass  3\n\n# ok\n\n"
            )
        );
        assert_eq!(
            candidate(&argv(), &format!("TAP version 13\n1..3\n{ROWS}")),
            Some(format!("TAP version 13\n1..3\n{MARK}"))
        );
    }

    #[test]
    fn failure_yaml_directives_and_blank_bytes_stay_in_place() {
        let retained = "not ok 4 - retains path C:\\fake\\a.js:9\n  ---\n    expected: 17\n    actual: 19\n    stack: |-\n      ok 982 is opaque data\n      not ok 983 is opaque data\n\n  ...\nok 5 skipped name # sKiP platform reason\nnot ok 6 expected failure # ToDo ticket A\nok 7 surprising success # TODO ticket B\n\n1..7\n# final note\n\n";
        let input = format!("TAP version 13\n{ROWS}{retained}");
        let expected = format!("TAP version 13\n{MARK}{retained}");
        assert_eq!(candidate(&argv(), &input), Some(expected));
        let ordinary = format!(
            "TAP version 13\n{ROWS}not ok 4 failure\n\n1..4\n# tests 4\n# pass  3\n# fail  1\n\n"
        );
        assert_eq!(
            candidate(&argv(), &ordinary),
            Some(format!(
                "TAP version 13\n{MARK}not ok 4 failure\n\n1..4\n# tests 4\n# pass  3\n# fail  1\n\n"
            ))
        );
    }

    #[test]
    fn passing_owners_survive_comments_blanks_and_opaque_yaml() {
        for suffix in [
            "# owned note\n",
            "\n# owned note\n\n",
            "  ---\n    actual: |-\n      ok 200 fake\n  ...\n",
        ] {
            let input = format!("TAP version 13\n{ROWS}{suffix}1..3\n");
            let expected = format!(
                "TAP version 13\n# omitted 2 ordinary passing names/rows: tests 1..2 (human view, not TAP)\nok 3 third independently authored ordinary assertion with a deliberately descriptive name\n{suffix}1..3\n"
            );
            assert_eq!(candidate(&argv(), &input), Some(expected));
        }
    }

    #[test]
    fn disjoint_ranges_and_unicode_description() {
        let input = "TAP version 13\n1..5\nok 1 a long ordinary name that contains a literal Unicode separator \u{2028} and must split only on LF\nok 2 another long ordinary passing name that provides enough bytes for an omission\nok 3 named # SKIP exact reason\nok 4 - another long passing name after the skip, with a separator hyphen\nok 5 final independently authored long ordinary passing name after the skip\n";
        assert_eq!(
            candidate(&argv(), input).as_deref(),
            Some(
                "TAP version 13\n1..5\n# omitted 2 ordinary passing names/rows: tests 1..2 (human view, not TAP)\nok 3 named # SKIP exact reason\n# omitted 2 ordinary passing names/rows: tests 4..5 (human view, not TAP)\n"
            )
        );
    }

    #[test]
    fn entire_stream_declines_on_harmful_changes() {
        let good = baseline();
        for (old, new) in [
            ("1..3", "1..4"),
            ("1..3", "1..3\n1..3"),
            ("1..3\n", ""),
            ("ok 2 ", "ok 4 "),
            ("ok 2 ", "ok 1 "),
            ("ok 2 ", "ok "),
            ("ok 2 ", "ok 02 "),
            ("ok 2 ", "ok 99999999999999999999999999999999 "),
            ("ok 2 ", "ok 2adjacent "),
            ("# tests 3", "# tests 2"),
            ("# pass  3", "# pass  2"),
            ("# pass  3", "# pass  3\n# pass  3"),
            ("# pass  3", "# pass three"),
            ("# ok", "# fail  1"),
            ("# sample group", "# Subtest: nested"),
            ("ok 2 ", "    ok 2 "),
            ("# sample group", "arbitrary stdout"),
            ("# sample group", "Bail out! incomplete"),
            ("# sample group", "  ---\n    actual: orphan\n  ..."),
            ("ok 2 ", "1..3\nok 2 "),
            ("TAP version 13", "TAP version 14"),
            ("# ok", "# todo 1"),
            ("# ok", "# ok unexpected"),
            ("# sample group", "# sample\u{001b}[31mgroup"),
        ] {
            let bad = good.replacen(old, new, 1);
            assert_eq!(
                candidate(&argv(), &bad),
                None,
                "mutation {old:?} -> {new:?}"
            );
        }
        for bad in [
            good.trim_end_matches('\n').to_owned(),
            good.replace('\n', "\r\n"),
            format!("{good}{good}"),
            format!("{good}exit status 0\n"),
        ] {
            assert_eq!(candidate(&argv(), &bad), None);
        }
        assert!(candidate(&argv(), &good).is_some());
    }

    #[test]
    fn diagnostics_framing_and_directive_ambiguity_controls() {
        for suffix in [
            "  ---\n    actual: unfinished\n",
            "  ---\nnot ok 90 escape\n  ...\n",
            "  ---\n  ---\n  ...\n",
            "  ...\n",
            "\n  ---\n    actual: unowned\n  ...\n",
            "# tests 3\n# pass 3\n# fail 1\n",
        ] {
            let bad = format!("TAP version 13\n{ROWS}{suffix}1..3\n");
            assert_eq!(candidate(&argv(), &bad), None, "{suffix:?}");
        }
        for row in [
            "ok 4 x # UNKNOWN reason",
            "ok 4 x # TODOish reason",
            "ok 4 x\\# TODO reason",
        ] {
            assert_eq!(
                candidate(&argv(), &format!("TAP version 13\n{ROWS}{row}\n1..4\n")),
                None
            );
        }
        let directed = format!("TAP version 13\n{ROWS}ok 4 skip name # SKIP why\n1..4\n");
        assert!(candidate(&argv(), &directed).is_some());
        for footer in ["# pass  4\n", "# fail  0\n", "# ok\n", "# todo  0\n"] {
            assert_eq!(candidate(&argv(), &(directed.clone() + footer)), None);
        }
    }

    #[test]
    fn tiny_zero_failure_and_size_controls() {
        for unchanged in [
            "TAP version 13\n1..0 # skip none\n",
            "TAP version 13\nok 1 tiny\n1..1\n",
            "TAP version 13\nnot ok 1 fail\n1..1\n",
            "TAP version 13\nok 1 only # TODO why\n1..1\n",
        ] {
            assert_eq!(candidate(&argv(), unchanged), None);
        }
        let prefix = "TAP version 13\n# ";
        let suffix = format!("\n{ROWS}1..3\n");
        let at_limit = format!(
            "{prefix}{}{suffix}",
            "x".repeat(MAX_BYTES - prefix.len() - suffix.len())
        );
        assert_eq!(at_limit.len(), MAX_BYTES);
        assert!(candidate(&argv(), &at_limit).is_some());
        assert_eq!(candidate(&argv(), &(at_limit + "\n")), None);
    }

    #[test]
    fn literal_argv_does_not_execute_or_broaden_routing() {
        for args in [
            vec![
                "/usr/bin/node",
                "/app/node_modules/tape/bin/tape",
                "--",
                "test.js",
            ],
            vec![
                "C:\\bin\\node.exe",
                "C:\\app\\node_modules\\tape\\bin\\tape",
                "one.js",
                "two.js",
            ],
            vec![
                "node",
                "node_modules/tape/bin/tape",
                "literal $(no-execution); file.js",
            ],
        ] {
            assert!(eligible(
                &args.iter().map(OsString::from).collect::<Vec<_>>()
            ));
        }
        for args in [
            vec![],
            vec!["node"],
            vec!["bad\0/node", "node_modules/tape/bin/tape", "test.js"],
            vec!["node", "bad\0/node_modules/tape/bin/tape", "test.js"],
            vec!["node", "--test", "test.js"],
            vec!["node", "other.js", "test.js"],
            vec!["npm", "test"],
            vec!["node", "--require", "node_modules/tape/bin/tape", "test.js"],
            vec!["node", "node_modules/tape/bin/tape"],
            vec!["node", "node_modules/tape/bin/tape", "--"],
            vec!["node", "node_modules/tape/bin/tape", "--watch", "test.js"],
            vec!["node", "node_modules/tape/bin/tape", "--", "-option.js"],
            vec!["node", "node_modules/tape/bin/tape-extra", "test.js"],
            vec!["node", "other_node_modules/tape/bin/tape", "test.js"],
            vec!["node", "node_modules/tape/bin/tape", ""],
        ] {
            assert!(
                !eligible(&args.iter().map(OsString::from).collect::<Vec<_>>()),
                "{args:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_argv_declines_without_panicking() {
        use std::os::unix::ffi::OsStringExt;
        let mut args = argv();
        args[2] = OsString::from_vec(vec![0xff]);
        assert!(!eligible(&args));
    }

    #[test]
    fn delivered_tail_is_opaque_after_validated_footer() {
        let raw = baseline();
        let view = candidate(&argv(), &raw).unwrap();
        for tail in [
            "\n\nCommand exited with code 1",
            "\n\nCommand timed out after 7 seconds",
            "arbitrary partial",
            "\x1b[31m\r\0opaque",
            "not ok 999 later text\n1..999\n",
            "TAP version 13\npartial second presentation",
        ] {
            let full = format!("{raw}{tail}");
            assert_eq!(
                delivered_candidate(&argv(), &full),
                Some(format!("{view}{tail}"))
            );
            assert_eq!(candidate(&argv(), &full), None);
        }
        assert_eq!(delivered_candidate(&argv(), &raw), Some(view));
    }

    #[test]
    fn delivered_failure_footer_and_lf_boundary() {
        let raw = format!(
            "TAP version 13\n{ROWS}not ok 4 retains failure\n\n1..4\n# tests 4\n# pass  3\n# fail  1\n"
        );
        let expected = format!(
            "TAP version 13\n{MARK}not ok 4 retains failure\n\n1..4\n# tests 4\n# pass  3\n# fail  1\n\n\nCommand exited with code 1"
        );
        assert_eq!(
            delivered_candidate(&argv(), &(raw.clone() + "\n\nCommand exited with code 1")),
            Some(expected)
        );
        assert_eq!(
            delivered_candidate(&argv(), raw.trim_end_matches('\n')),
            None
        );
        // A footer-looking line alone cannot establish a boundary.
        for bad in [
            raw.replace("# tests 4\n", ""),
            raw.replace("# pass  3", "# pass  2"),
            raw.replace("# fail  1", "# fail  0"),
            raw.replace("1..4", "1..5"),
            raw.replace("\n\n1..4", "\n1..4"),
            raw.replace("# pass  3\n", "# pass  3\n# interruption\n"),
        ] {
            assert_eq!(
                delivered_candidate(&argv(), &(bad + "partial host tail")),
                None
            );
        }
    }

    #[test]
    fn delivered_cannot_resync_or_relax_ownership_and_directives() {
        let raw = baseline();
        for bad in [
            format!("notice\n{raw}"),
            raw.replace("ok 2 ", "unknown\nok 2 "),
            raw.replace("ok 2 ", "ok 4 "),
            raw.replace("# sample group", "# Subtest: nested"),
            raw.replace("ok 2 ", "  ---\n    incomplete\nok 2 "),
            raw.replace("ok 2 ", "Bail out!\nok 2 "),
            raw.replace("ok 2 ", "ok 2 # SKIP reason "),
            raw.replace("# pass  3\n", "# pass  3\n# todo  0\n"),
            raw.replace("# sample group", "# bad\x1b[31m"),
        ] {
            assert_eq!(
                delivered_candidate(&argv(), &(bad + "\n\nCommand exited with code 1")),
                None
            );
        }
        let owned = raw.replace("\n\n1..3", "\n# owned final row\n\n1..3");
        let expected = "TAP version 13\n# sample group\n# omitted 2 ordinary passing names/rows: tests 1..2 (human view, not TAP)\nok 3 third independently authored ordinary assertion with a deliberately descriptive name\n# owned final row\n\n1..3\n# tests 3\n# pass  3\n\n# ok\n\npartial";
        assert_eq!(
            delivered_candidate(&argv(), &(owned + "partial")),
            Some(expected.to_owned())
        );
        let minimal = format!("TAP version 13\n1..3\n{ROWS}");
        assert_eq!(
            delivered_candidate(&argv(), &minimal),
            candidate(&argv(), &minimal)
        );
        assert_eq!(delivered_candidate(&argv(), &(minimal + "partial")), None);
    }
}
