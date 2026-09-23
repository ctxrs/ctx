// Adapted from Sift, MIT; source revision and license are in this crate’s NOTICE.
//! Conservative presentation candidates for human-readable captures.
//!
//! These are not reversible codecs. `candidate` requires the caller's complete
//! capture/stream contract; `delivered_candidate` makes only a text-row contract.
//! Both callers own exact-token selection against the original delivered field.
use std::ffi::OsString;

#[path = "tap_view.rs"]
mod tap_view;

/// Return a strictly smaller (in bytes) presentation, or decline recognition.
/// Supports Git long status, captured Cargo libtest output and strict flat Tape.
pub(crate) fn candidate(argv: &[OsString], text: &str, stderr: bool) -> Option<String> {
    if stderr || !text.ends_with('\n') {
        return None;
    }
    presentation(argv, text, false)
}

/// Pi's delivered merged text, not authenticated stdout or proof of test success.
/// Complete Cargo blocks may omit passing rows; all other delivered bytes remain.
/// A perfectly matching unmarked printed transcript is inherently indistinguishable.
pub(crate) fn delivered_candidate(argv: &[OsString], text: &str) -> Option<String> {
    presentation(argv, text, true)
}

fn presentation(argv: &[OsString], text: &str, delivered: bool) -> Option<String> {
    if text.is_empty() {
        return None;
    }
    // Tape validates its own structural controls and preserves opaque delivered
    // tails. Git/Cargo retain their existing whole-field control guards below.
    if tap_view::eligible(argv) {
        return if delivered {
            tap_view::delivered_candidate(argv, text)
        } else {
            tap_view::candidate(argv, text)
        };
    }
    // Terminal control sequences and CR progress displays need another grammar.
    if text
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return None;
    }
    let args: Vec<&str> = argv.iter().map(|a| a.to_str()).collect::<Option<_>>()?;
    let program = args.first()?.rsplit(['/', '\\']).next()?;
    let view = match program {
        "git" | "git.exe" if text.ends_with('\n') && git_args(&args[1..]) => git_status(text),
        "cargo" | "cargo.exe" if cargo_args(&args[1..]) => cargo_tests(text, delivered),
        _ => None,
    }?;
    (view.len() < text.len()).then_some(view)
}

fn git_args(mut args: &[&str]) -> bool {
    while args.first() == Some(&"-C") && args.len() >= 2 {
        args = &args[2..];
    }
    if args.first() != Some(&"status") {
        return false;
    }
    for arg in &args[1..] {
        if *arg == "--" {
            return true; // The remainder is a literal pathspec, not options.
        }
        if !matches!(
            *arg,
            "--long"
                | "-b"
                | "--branch"
                | "--show-stash"
                | "--ahead-behind"
                | "--no-ahead-behind"
                | "-u"
                | "-uno"
                | "-unormal"
                | "-uall"
                | "--untracked-files"
                | "--untracked-files=no"
                | "--untracked-files=normal"
                | "--untracked-files=all"
                | "--ignored"
                | "--ignored=traditional"
                | "--ignored=matching"
                | "--ignored=no"
        ) {
            return false;
        }
    }
    true
}

fn cargo_args(args: &[&str]) -> bool {
    if args.first() != Some(&"test") {
        return false;
    }
    // An allowlist keeps explicit machine output, custom harness arguments,
    // nocapture/show-output, and unstable output modes out of this grammar.
    let mut args = args[1..].iter().copied();
    let mut harness = false;
    while let Some(arg) = args.next() {
        if arg == "--" && !harness {
            harness = true;
        } else if !arg.starts_with('-') {
            continue; // Test-name filter.
        } else if harness {
            match arg {
                "--ignored" | "--include-ignored" | "--exact" => {}
                "--skip" | "--test-threads" => {
                    if args.next().is_none() {
                        return false;
                    }
                }
                _ if arg.starts_with("--skip=") || arg.starts_with("--test-threads=") => {}
                _ => return false,
            }
        } else {
            match arg {
                "--lib"
                | "--bins"
                | "--tests"
                | "--examples"
                | "--doc"
                | "--workspace"
                | "--all"
                | "--all-targets"
                | "--all-features"
                | "--no-default-features"
                | "--release"
                | "--locked"
                | "--offline"
                | "--frozen"
                | "--no-fail-fast" => {}
                "-p" | "--package" | "--exclude" | "--test" | "--bin" | "--example"
                | "--features" | "--manifest-path" | "--target-dir" | "--target" | "--profile"
                | "-j" | "--jobs" => {
                    if args.next().is_none() {
                        return false;
                    }
                }
                _ => return false,
            }
        }
    }
    true
}

fn git_status(text: &str) -> Option<String> {
    let lines: Vec<_> = text.split_inclusive('\n').collect();
    let first = lines.first()?.trim_end_matches('\n');
    let mut out = String::with_capacity(text.len());
    if let Some(branch) = first.strip_prefix("On branch ").filter(|s| !s.is_empty()) {
        out.push_str("## ");
        out.push_str(branch);
        out.push('\n');
    } else if ["HEAD detached at ", "HEAD detached from "]
        .iter()
        .any(|prefix| first.strip_prefix(prefix).is_some_and(|s| !s.is_empty()))
        || first == "Not currently on any branch."
    {
        out.push_str(lines[0]);
    } else {
        return None;
    }
    let mut i = 1;
    let mut sections = false;
    while i < lines.len() {
        let Some(section) = git_section(lines[i].trim_end_matches('\n')) else {
            if git_ambiguous_line(lines[i]) || lines[i].starts_with('\t') {
                return None;
            }
            out.push_str(lines[i]);
            i += 1;
            continue;
        };
        let start = i;
        i += 1;
        while i < lines.len() && git_section(lines[i].trim_end_matches('\n')).is_none() {
            i += 1;
        }
        let mut converted = String::new();
        let mut entries = 0;
        let mut preserve = section == "Conflicts:";
        for raw in &lines[start + 1..i] {
            let line = raw.trim_end_matches('\n');
            if line.is_empty() || (entries == 0 && git_hint(section, line)) {
                continue;
            }
            if let Some(path) = line.strip_prefix('\t') {
                if path.is_empty() {
                    return None;
                }
                entries += 1;
                if matches!(section, "Untracked:" | "Ignored:") {
                    converted.push_str(if section == "Untracked:" {
                        "?? "
                    } else {
                        "!! "
                    });
                    converted.push_str(&raw[1..]);
                    continue;
                }
                let prefix = [
                    ("\tmodified:   ", 'M'),
                    ("\tnew file:   ", 'A'),
                    ("\tdeleted:    ", 'D'),
                    ("\trenamed:    ", 'R'),
                    ("\tcopied:     ", 'C'),
                    ("\ttypechange: ", 'T'),
                ]
                .into_iter()
                .find(|(prefix, _)| line.starts_with(prefix));
                if let Some((prefix, code)) =
                    prefix.filter(|_| matches!(section, "Staged:" | "Unstaged:"))
                {
                    if line.len() == prefix.len() {
                        return None;
                    }
                    // Keep separate rows for each stage of a path. Copy the
                    // exact path suffix, including quotes, spaces and hints.
                    if section == "Staged:" {
                        converted.push(code);
                        converted.push_str("  ");
                    } else {
                        converted.push(' ');
                        converted.push(code);
                        converted.push(' ');
                    }
                    converted.push_str(&raw[prefix.len()..]);
                } else {
                    preserve = true;
                }
            } else {
                if git_ambiguous_line(line) {
                    return None;
                }
                preserve = true;
            }
        }
        if entries == 0 {
            return None;
        }
        // Unknown diagnostics/statuses need their original section owner.
        // Conflict XY columns have different meanings, so keep those intact.
        if preserve {
            for raw in &lines[start..i] {
                out.push_str(raw);
            }
        } else {
            out.push_str(&converted);
        }
        sections = true;
    }
    sections.then_some(out)
}

fn git_section(line: &str) -> Option<&'static str> {
    match line {
        "Changes to be committed:" => Some("Staged:"),
        "Changes not staged for commit:" => Some("Unstaged:"),
        "Untracked files:" => Some("Untracked:"),
        "Unmerged paths:" => Some("Conflicts:"),
        "Ignored files:" => Some("Ignored:"),
        _ => None,
    }
}

fn git_ambiguous_line(line: &str) -> bool {
    if line.starts_with("## ") {
        return true;
    }
    match line.as_bytes() {
        [x, y, b' ', ..] => {
            matches!((*x, *y), (b'?', b'?') | (b'!', b'!'))
                || (b" MADRCT".contains(x) && b" MADRCT".contains(y) && (*x, *y) != (b' ', b' '))
        }
        _ => false,
    }
}

fn git_hint(section: &str, line: &str) -> bool {
    match section {
        "Staged:" => matches!(
            line,
            "  (use \"git restore --staged <file>...\" to unstage)"
                | "  (use \"git rm --cached <file>...\" to unstage)"
        ),
        "Unstaged:" => matches!(
            line,
            "  (use \"git add <file>...\" to update what will be committed)"
                | "  (use \"git add/rm <file>...\" to update what will be committed)"
                | "  (use \"git restore <file>...\" to discard changes in working directory)"
        ),
        "Untracked:" => {
            line == "  (use \"git add <file>...\" to include in what will be committed)"
        }
        _ => false, // Keep conflict/rebase/submodule recovery instructions.
    }
}

fn number(text: &str) -> Option<usize> {
    (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

fn running(line: &str) -> Option<usize> {
    let tail = line.strip_prefix("running ")?;
    let (n, unit) = tail.split_once(' ')?;
    let n = number(n)?;
    ((n == 1 && unit == "test") || (n != 1 && unit == "tests")).then_some(n)
}

fn result_counts(line: &str) -> Option<[usize; 3]> {
    let (status, tail) = line.strip_prefix("test result: ")?.split_once(". ")?;
    let parts: Vec<_> = tail.split("; ").collect();
    if parts.len() != 6 {
        return None;
    }
    let passed = number(parts[0].strip_suffix(" passed")?)?;
    let failed = number(parts[1].strip_suffix(" failed")?)?;
    let ignored = number(parts[2].strip_suffix(" ignored")?)?;
    if number(parts[3].strip_suffix(" measured")?)? != 0 {
        return None;
    }
    number(parts[4].strip_suffix(" filtered out")?)?;
    let duration = parts[5].strip_prefix("finished in ")?.strip_suffix('s')?;
    let (seconds, fraction) = duration.split_once('.')?;
    number(seconds)?;
    number(fraction)?;
    if (failed == 0 && status != "ok") || (failed != 0 && status != "FAILED") {
        return None;
    }
    Some([passed, failed, ignored])
}

fn test_row(line: &str) -> Option<(&str, usize)> {
    let (name, result) = line.strip_prefix("test ")?.rsplit_once(" ... ")?;
    if name.is_empty() {
        return None;
    }
    let kind = match result {
        "ok" => Some(0),
        "FAILED" => Some(1),
        "ignored" => Some(2),
        s if s.starts_with("ignored, ") => Some(2),
        _ => None,
    }?;
    Some((name, kind))
}

fn failure_region(line: &str) -> bool {
    line == "failures:" || (line.starts_with("---- ") && line.ends_with(" stdout ----"))
}

fn cargo_tests(text: &str, delivered: bool) -> Option<String> {
    let lines: Vec<_> = text.split_inclusive('\n').collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut omitted_any = false;
    while i < lines.len() {
        let line = lines[i].trim_end_matches('\n');
        // A native capped tail may begin inside captured failure output. Do not
        // search that unowned region for a quoted report to treat as a suite.
        if delivered && failure_region(line) {
            return None;
        }
        let Some(total) = running(line) else {
            // An orphan summary or malformed start is ambiguous (quiet output,
            // a partial capture, a nested/custom harness, or localization).
            if line.starts_with("test result:") || line.starts_with("running ") {
                return None;
            }
            out.push_str(lines[i]);
            i += 1;
            continue;
        };
        if delivered && !lines[i].ends_with('\n') {
            return None;
        }
        out.push_str(lines[i]);
        i += 1;
        let start = i;
        let mut counts = [0usize; 3];
        let mut names = std::collections::HashSet::new();
        while i < lines.len() {
            let Some((name, kind)) = test_row(lines[i].trim_end_matches('\n')) else {
                break;
            };
            if delivered && (!lines[i].ends_with('\n') || !names.insert(name)) {
                return None;
            }
            counts[kind] += 1;
            i += 1;
        }
        if counts
            .iter()
            .try_fold(0usize, |sum, n| sum.checked_add(*n))?
            != total
        {
            return None;
        }
        let rows_end = i;
        while i < lines.len() && !lines[i].starts_with("test result:") {
            if lines[i].starts_with("running ")
                || (delivered && counts[1] == 0 && failure_region(lines[i].trim_end_matches('\n')))
            {
                return None;
            }
            i += 1;
        }
        if (delivered && !lines.get(i)?.ends_with('\n'))
            || result_counts(lines.get(i)?.trim_end_matches('\n'))? != counts
        {
            return None;
        }
        // Only rows immediately following the validated start can be omitted.
        // Diagnostics (even lines that look exactly like passing rows) survive.
        if counts[0] > 0 {
            out.push_str(&format!("[{} passing test lines omitted]\n", counts[0]));
            omitted_any = true;
        }
        for raw in &lines[start..rows_end] {
            if !test_row(raw.trim_end_matches('\n')).is_some_and(|(_, kind)| kind == 0) {
                out.push_str(raw);
            }
        }
        for raw in &lines[rows_end..=i] {
            out.push_str(raw);
        }
        i += 1;
    }
    omitted_any.then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("tests/fixtures/pi_delivered.rs");

    #[test]
    fn delivered_cargo_matches_authored_byte_removal_with_opaque_tails() {
        let argv = ["cargo".into(), "test".into()];
        for failed in [false, true] {
            for tail in [
                "",
                "\nCommand exited with code 101",
                "\nCommand aborted",
                "\nCommand timed out after 2 seconds",
                "\n[Showing lines 8-40 of 40. Full output: /tmp/synthetic.log]",
                "\npartial multibyte 🦀",
                "\nunknown stderr diagnostic\n",
            ] {
                let (raw, expected) = cargo_fixture(failed, tail);
                assert_eq!(delivered_candidate(&argv, &raw), Some(expected));
                if !raw.ends_with('\n') {
                    assert!(candidate(&argv, &raw, false).is_none());
                }
            }
        }
    }

    #[test]
    fn delivered_cargo_rejects_structural_ambiguity_without_resynchronizing() {
        let argv = ["cargo".into(), "test".into()];
        let (raw, _) = cargo_fixture(false, "");
        let (failed, _) = cargo_fixture(true, "");
        for text in [
            raw.replace(
                "checks::rejects_stale_generation",
                "checks::accepts_unicode_identifiers",
            ),
            failed.replace(
                "checks::broken_value",
                "checks::accepts_unicode_identifiers",
            ),
            raw.replace(
                "\ntest result:",
                "\nfailures:\nquoted failed diagnostic\ntest result:",
            ),
            raw.replace(
                "\ntest result:",
                "\n---- checks::x stdout ----\nquoted diagnostic\ntest result:",
            ),
            format!("failures:\n{raw}"),
            format!("---- checks::x stdout ----\n{raw}"),
            format!("test result: incomplete\n{raw}"),
            raw.replace(
                "test checks::rejects_stale_generation",
                "stderr interleaves\ntest checks::rejects_stale_generation",
            ),
            raw.replace("12 passed", "11 passed"),
            failed.replace("test result: FAILED.", "test result: ok."),
            raw.trim_end_matches('\n').to_owned(),
            format!("{raw}running 1 test\ntest unfinished ... ok"),
            format!("{raw}running 1 test"),
            raw.replace('\n', "\r\n"),
            format!("\x1b[31m{raw}"),
        ] {
            assert!(delivered_candidate(&argv, &text).is_none(), "{text}");
        }
        let (one, expected) = cargo_fixture(false, "");
        assert_eq!(
            delivered_candidate(&argv, &format!("{one}{one}")),
            Some(format!("{expected}{expected}"))
        );
    }

    #[test]
    fn delivered_git_reuses_whole_field_contract_and_command_allowlists() {
        let argv = ["git".into(), "status".into()];
        assert_eq!(
            delivered_candidate(&argv, STATUS),
            candidate(&argv, STATUS, false)
        );
        for text in [
            STATUS.trim_end_matches('\n').to_owned(),
            format!("{STATUS}Command exited with code 1"),
        ] {
            assert!(delivered_candidate(&argv, &text).is_none());
        }
        let (raw, _) = cargo_fixture(false, "");
        for args in [
            vec!["cargo", "test", "--", "--nocapture"],
            vec!["cargo", "test", "-q"],
            vec!["sh", "-c", "cargo test"],
            vec!["echo", "cargo", "test"],
        ] {
            let argv: Vec<_> = args.iter().map(OsString::from).collect();
            assert!(delivered_candidate(&argv, &raw).is_none());
        }
    }

    fn view(args: &[&str], text: &str) -> Option<String> {
        candidate(
            &args.iter().map(OsString::from).collect::<Vec<_>>(),
            text,
            false,
        )
    }

    const STATUS: &str = "On branch topic\nYour branch and 'origin/topic' have diverged,\nand have 2 and 3 different commits each, respectively.\n\nChanges to be committed:\n  (use \"git restore --staged <file>...\" to unstage)\n\n\trenamed:    \"old\\tname\" -> \"new\\nname\"\n\tmodified:   shared.rs\n\nChanges not staged for commit:\n  (use \"git add <file>...\" to update what will be committed)\n  (use \"git restore <file>...\" to discard changes in working directory)\n\n\tmodified:   shared.rs\n\tdeleted:    leading space \n\nUntracked files:\n  (use \"git add <file>...\" to include in what will be committed)\n\n\t\"quote\\\"\\\\path\"\n\tunicodé.rs\n\t(use \"git add\" is a filename)\n\nUnmerged paths:\n  (use \"git add <file>...\" to mark resolution)\n\tboth modified:   conflict.rs\nUnknown advisory: index scan incomplete\n";

    #[test]
    fn git_keeps_all_paths_states_and_unknowns() {
        let out = view(&["git", "status"], STATUS).unwrap();
        assert!(out.starts_with("## topic\nYour branch and 'origin/topic' have diverged,\nand have 2 and 3 different commits each, respectively.\n"));
        assert!(out.contains(
            "R  \"old\\tname\" -> \"new\\nname\"\nM  shared.rs\n M shared.rs\n D leading space \n"
        ));
        assert!(out.ends_with("?? \"quote\\\"\\\\path\"\n?? unicodé.rs\n?? (use \"git add\" is a filename)\nUnmerged paths:\n  (use \"git add <file>...\" to mark resolution)\n\tboth modified:   conflict.rs\nUnknown advisory: index scan incomplete\n"));
        assert_eq!(
            out.lines()
                .filter(|line| line.ends_with(" shared.rs"))
                .count(),
            2
        );
        assert!(!out.contains("omitted"));
        assert!(out.len() < STATUS.len());
    }

    #[test]
    fn git_declines_machine_short_localized_or_tiny_output() {
        for arg in [
            "--porcelain",
            "--porcelain=v2",
            "-z",
            "--null",
            "-s",
            "--short",
            "-sb",
            "--format=json",
            "-v",
        ] {
            assert!(view(&["git", "status", arg], STATUS).is_none(), "{arg}");
        }
        for text in [
            "## topic\n M src/lib.rs\n",
            "Auf Branch topic\n",
            "On branch topic\nnothing to commit, working tree clean\n",
            "On branch topic\nChanges to be committed:\n",
            "On branch topic\n\tunknown\n",
        ] {
            assert!(view(&["git", "status"], text).is_none());
        }
        assert!(view(&["git", "-c", "status.short=true", "status"], STATUS).is_none());
        assert!(
            view(
                &[
                    "/usr/bin/git",
                    "-C",
                    "a folder",
                    "status",
                    "--long",
                    "--",
                    "--porcelain"
                ],
                STATUS
            )
            .is_some()
        );
    }

    #[test]
    fn git_short_status_rows_keep_all_paths_without_capping() {
        let mut raw = String::from("On branch main\nChanges to be committed:\n");
        for n in 0..250 {
            raw.push_str(&format!("\tmodified:   file {n}.rs\n"));
        }
        raw.push_str("\tmodified:    leading and trailing \n\trenamed:    old -> new\n\trenamed:    \"a -> b\" -> c\nUntracked files:\n\tmodified:   a name\n\tmodified:   another name\n");
        let out = view(&["git", "status"], &raw).unwrap();
        assert!(out.starts_with("## main\nM  file 0.rs\n"));
        for n in 0..250 {
            assert!(out.contains(&format!("M  file {n}.rs\n")));
        }
        assert!(out.contains("M   leading and trailing \nR  old -> new\nR  \"a -> b\" -> c\n"));
        assert!(out.ends_with("?? modified:   a name\n?? modified:   another name\n"));
    }

    #[test]
    fn git_codes_preserve_status_meanings_and_path_boundaries() {
        let raw = "On branch main\nChanges to be committed:\n\n\tnew file:   \"new\\tfile\"\n\tmodified:    space \n\tdeleted:    gone\n\trenamed:    before -> after\n\tcopied:     source -> copy\n\ttypechange: link\nIgnored files:\n\n\tStaged:\n\tmodified:   literal\n\n";
        let out = view(&["git", "status", "--ignored"], raw).unwrap();
        assert_eq!(
            out,
            "## main\nA  \"new\\tfile\"\nM   space \nD  gone\nR  before -> after\nC  source -> copy\nT  link\n!! Staged:\n!! modified:   literal\n"
        );
    }

    #[test]
    fn git_keeps_operation_recovery_and_advisory_layout() {
        let raw = "HEAD detached at abc1234\nYou are currently rebasing branch 'topic'.\n  (use \"git rebase --continue\" once you are satisfied with your changes)\n\nChanges not staged for commit:\n  (use \"git add <file>...\" to update what will be committed)\n\n\tmodified:   sub (new commits, modified content)\n\nUnknown advisory: scan incomplete\n\n  (use \"git restore <file>...\" to discard changes in working directory)\n  inspect this before continuing\n\nUnmerged paths:\n\n\tboth added:      conflict\n\nRecovery details\n\n  pending resolution\n";
        // Every section requires full preservation here, so no smaller view.
        assert!(view(&["git", "status"], raw).is_none());
        let with_staged = raw.replace(
            "Changes not staged for commit:",
            "Changes to be committed:\n\tmodified:   staged.rs\nChanges not staged for commit:",
        );
        let out = view(&["git", "status"], &with_staged).unwrap();
        let (preamble, original_sections) =
            raw.split_once("Changes not staged for commit:").unwrap();
        assert_eq!(
            out,
            format!("{preamble}M  staged.rs\nChanges not staged for commit:{original_sections}")
        );
    }

    #[test]
    fn git_preserves_unknown_section_ownership_and_rejects_ambiguous_rows() {
        let unknown = "Changes to be committed:\n  (use \"git restore --staged <file>...\" to unstage)\n\tmodified:   known.rs\n\tfuture kind:  unknown\n\nUnknown advisory\n\n  continuation\n";
        let raw =
            format!("On branch main\n{unknown}Untracked files:\n\t M literal\n\t## literal\n");
        assert_eq!(
            view(&["git", "status"], &raw).unwrap(),
            format!("## main\n{unknown}??  M literal\n?? ## literal\n")
        );
        for row in [
            "M  advisory",
            " M advisory",
            "?? advisory",
            "!! advisory",
            "## misleading",
            "MM advisory",
        ] {
            let text = format!(
                "On branch main\nChanges to be committed:\n\tmodified:   known.rs\n{row}\n"
            );
            assert!(view(&["git", "status"], &text).is_none(), "{row}");
        }
    }

    #[test]
    fn git_declines_malformed_sections_without_partial_conversion() {
        for suffix in [
            "\t\n",
            "\tmodified:   \n",
            "Untracked files:\n",
            "Unmerged paths:\n  incomplete\n",
        ] {
            let raw = format!(
                "On branch main\nChanges to be committed:\n\tmodified:   known.rs\n{suffix}"
            );
            assert!(view(&["git", "status"], &raw).is_none(), "{suffix:?}");
        }
        for head in [
            "On branch ",
            "HEAD detached at ",
            "HEAD detached from ",
            "On branch main\n\tunowned",
        ] {
            let raw = format!("{head}\nChanges to be committed:\n\tmodified:   known.rs\n");
            assert!(view(&["git", "status"], &raw).is_none(), "{head:?}");
        }
    }

    fn suite(passed: usize, failed: usize, ignored: usize) -> String {
        let total = passed + failed + ignored;
        let mut text = format!(
            "running {total} {}\n",
            if total == 1 { "test" } else { "tests" }
        );
        for i in 0..passed {
            text.push_str(&format!("test module::ordinary_case_{i} ... ok\n"));
        }
        for i in 0..failed {
            text.push_str(&format!("test module::broken_case_{i} ... FAILED\n"));
        }
        for i in 0..ignored {
            text.push_str(&format!(
                "test module::skipped_case_{i} ... ignored, needs a server\n"
            ));
        }
        if failed > 0 {
            text.push_str("\nfailures:\n\n---- module::broken_case_0 stdout ----\nthread 'module::broken_case_0' panicked at src/lib.rs:42:9:\nassertion `left == right` failed\n  left: [1,\n         2]\n right: [3,\n         4]\nstack backtrace:\n   0: crate::call\n      at src/lib.rs:42\nwarning: test output was truncated by an external reporter\ntest printed_a_passing_looking_line ... ok\nunknown diagnostic payload\n\nfailures:\n    module::broken_case_0\n");
        }
        let status = if failed == 0 { "ok" } else { "FAILED" };
        text.push_str(&format!("\ntest result: {status}. {passed} passed; {failed} failed; {ignored} ignored; 0 measured; 7 filtered out; finished in 0.02s\n\n"));
        text
    }

    #[test]
    fn cargo_keeps_failure_body_names_counts_and_unknown_lines() {
        let raw = format!(
            "warning: unusual build configuration\n{}post-run unknown line\n",
            suite(12, 1, 1)
        )
        .replace("test module::broken_case_0 ... FAILED\n", "")
        .replace(
            "test module::ordinary_case_6 ... ok\n",
            "test module::broken_case_0 ... FAILED\ntest module::ordinary_case_6 ... ok\n",
        );
        let out = view(&["cargo", "test"], &raw).unwrap();
        let diagnostic = raw.split_once("\nfailures:\n").unwrap().1;
        assert!(out.ends_with(diagnostic));
        assert!(out.starts_with("warning: unusual build configuration\nrunning 14 tests\n[12 passing test lines omitted]\n"));
        assert!(out.contains("test module::broken_case_0 ... FAILED\n"));
        assert!(out.contains("test module::skipped_case_0 ... ignored, needs a server\n"));
        assert!(!out.contains("ordinary_case_"));
    }

    #[test]
    fn cargo_keeps_suite_boundaries_and_ignored_filtered_counts() {
        let raw = format!("{}{}{}", suite(4, 0, 2), suite(0, 0, 0), suite(3, 0, 0));
        let out = view(
            &["cargo", "test", "--workspace", "--", "--include-ignored"],
            &raw,
        )
        .unwrap();
        assert_eq!(out.matches("test result: ok.").count(), 3);
        assert_eq!(out.matches("7 filtered out").count(), 3);
        assert!(out.contains("[4 passing test lines omitted]"));
        assert!(out.contains("[3 passing test lines omitted]"));
        assert!(out.contains(&suite(0, 0, 0)));
        assert!(out.contains("2 ignored"));
    }

    #[test]
    fn cargo_declines_incomplete_inconsistent_and_uncaptured_modes() {
        let raw = suite(5, 1, 0);
        for args in [
            vec!["cargo", "test", "--", "--nocapture"],
            vec!["cargo", "test", "--", "--show-output"],
            vec!["cargo", "test", "--message-format=json"],
            vec!["cargo", "test", "--", "--format", "json"],
            vec!["cargo", "test", "-q"],
            vec!["cargo", "nextest", "run"],
        ] {
            assert!(view(&args, &raw).is_none(), "{args:?}");
        }
        for invalid in [
            raw.replace("5 passed", "4 passed"),
            raw.replace("1 failed", "0 failed"),
            raw.replace("running 6 tests", "running 7 tests"),
            raw.replace("test result: FAILED.", "test result: ok."),
            raw.replace("0 measured", "1 measured"),
            raw.replace(
                "test module::ordinary_case_1",
                "unexpected line\ntest module::ordinary_case_1",
            ),
            raw.split_once("test result:").unwrap().0.to_owned(),
            format!("{raw}running 2 tests\n"),
            raw.replace("0.02s", "unknown"),
        ] {
            assert!(view(&["cargo", "test"], &invalid).is_none(), "{invalid}");
        }
    }

    #[test]
    fn tiny_stderr_controls_and_other_families_pass_through() {
        assert!(view(&["cargo", "test"], "running 1 test\ntest x ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n").is_none());
        let raw = suite(5, 0, 0);
        for args in [
            vec!["pytest"],
            vec!["go", "test"],
            vec!["jest"],
            vec!["sh", "-c", "cargo test"],
            vec!["echo", "cargo", "test"],
        ] {
            assert!(view(&args, &raw).is_none());
        }
        assert!(candidate(&["cargo".into(), "test".into()], &raw, true).is_none());
        for text in [
            raw.replace('\n', "\r\n"),
            format!("\x1b[31m{raw}"),
            raw.trim_end().to_owned(),
            format!("{raw}\0"),
        ] {
            assert!(view(&["cargo", "test"], &text).is_none());
        }
    }

    include!("tests/fixtures/tape_delivered.rs");

    #[test]
    fn tape_dispatch_preserves_complete_and_delivered_contracts() {
        let argv = ["node", "./node_modules/tape/bin/tape", "fixture.js"].map(OsString::from);
        let (text, expected) = tape_fixture("");
        assert_eq!(candidate(&argv, &text, false), Some(expected.clone()));
        assert_eq!(delivered_candidate(&argv, &text), Some(expected));
        assert_eq!(candidate(&argv, &text, true), None);
        let (text, expected) =
            tape_fixture("\n\nCommand exited with code 1\n\u{001b}[31mopaque partial");
        assert_eq!(candidate(&argv, &text, false), None);
        assert_eq!(delivered_candidate(&argv, &text), Some(expected));
        let cargo = ["cargo", "test"].map(OsString::from);
        assert!(delivered_candidate(&cargo, &format!("{}\u{001b}[31m", suite(5, 0, 0))).is_none());
        let git = ["git", "status"].map(OsString::from);
        assert!(delivered_candidate(&git, &format!("{STATUS}\u{001b}[31m\n")).is_none());
    }
}
