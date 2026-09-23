use super::*;

pub(super) fn command_words(value: &Value) -> Vec<String> {
    if let Some(s) = value.as_str() {
        return shell_words(s);
    }
    let Some(items) = value.as_array() else {
        return Vec::new();
    };
    let Some(words): Option<Vec<String>> = items
        .iter()
        .map(|v| v.as_str().map(str::to_owned))
        .collect()
    else {
        return Vec::new();
    };
    if words.len() == 3
        && matches!(basename(&words[0]), "sh" | "bash" | "zsh")
        && matches!(words[1].as_str(), "-c" | "-lc")
    {
        shell_words(&words[2])
    } else {
        words
    }
}

// Reuse the rewriter's bounded literal grammar. A compound command counts as
// Sift use only if removing its wrappers and regenerating it gives exactly the
// original text. This rejects quoted mentions, altered guards and extra syntax.
pub(super) fn shell_words(input: &str) -> Vec<String> {
    use crate::rewrite::{self, Shell};
    if input.len() > 64 * 1024 {
        return Vec::new();
    }
    let Some(tokens) = rewrite::lex(input, Shell::Posix) else {
        return Vec::new();
    };
    if tokens
        .iter()
        .any(|t| t.word.is_none() && t.operator.is_none())
    {
        return Vec::new();
    }
    if tokens.iter().all(|t| t.operator.is_none()) {
        return tokens.into_iter().filter_map(|t| t.word).collect();
    }
    let word = |index: usize| tokens.get(index).and_then(|t| t.word.as_deref());
    let mut start = 0;
    let mut body_start = 0;
    while word(start) == Some("command")
        && word(start + 1) == Some("true")
        && tokens
            .get(start + 2)
            .is_some_and(|t| t.operator == Some("||"))
    {
        let Some(end) = (start + 3..tokens.len()).find(|&i| tokens[i].operator.is_some()) else {
            return Vec::new();
        };
        if end == start + 3
            || tokens[end].operator != Some(";")
            || !input[tokens[end].start..].starts_with("; ")
        {
            return Vec::new();
        }
        // The generator appends "; "; retain the original body's own whitespace.
        body_start = tokens[end].start + 2;
        start = end + 1;
    }
    if body_start == 0 || body_start > input.len() {
        return Vec::new();
    }
    let mut executable = None;
    let mut removals = Vec::new();
    let mut exclusions = Vec::new();
    let body_tokens = start;
    for end in body_tokens..=tokens.len() {
        if end < tokens.len() && tokens[end].operator.is_none() {
            continue;
        }
        if start < end {
            let candidate = word(start + 1)
                .filter(|w| matches!(basename(w), "sift" | "sift.exe" | "ctx" | "ctx.exe"));
            let namespaced = candidate.is_some_and(|w| matches!(basename(w), "ctx" | "ctx.exe"));
            let run = start + 2 + usize::from(namespaced);
            if word(start) == Some("command")
                && candidate.is_some()
                && (!namespaced || word(start + 2) == Some("output"))
                && word(run) == Some("run")
            {
                let payload = run
                    + if word(run + 1) == Some("--capture") {
                        3
                    } else {
                        2
                    };
                if payload >= end || word(payload - 1) != Some("--") {
                    return Vec::new();
                }
                if executable.is_some() && executable != candidate {
                    return Vec::new();
                }
                executable = candidate;
                removals.push((
                    tokens[start].start - body_start,
                    tokens[payload].start - body_start,
                ));
            } else if let Some(name) = word(start) {
                // The original rewrite may exclude some otherwise supported commands.
                let name = basename(name);
                exclusions.push(name.strip_suffix(".exe").unwrap_or(name).to_owned());
            }
        }
        start = end + 1;
    }
    let Some(executable) = executable else {
        return Vec::new();
    };
    let mut original = input[body_start..].to_owned();
    for (from, to) in removals.into_iter().rev() {
        original.replace_range(from..to, "");
    }
    let namespaced = matches!(basename(executable), "ctx" | "ctx.exe");
    if rewrite::command_with_namespace(
        &original,
        Path::new(executable),
        Shell::Posix,
        &exclusions,
        namespaced,
    )
    .as_deref()
        == Some(input)
    {
        let mut words = vec![executable.to_owned()];
        if namespaced {
            words.push("output".into());
        }
        words.push("run".into());
        words
    } else {
        Vec::new()
    }
}

pub(super) fn basename(word: &str) -> &str {
    word.rsplit(['/', '\\']).next().unwrap_or(word)
}

pub(super) fn command_label(words: &[String]) -> (String, bool) {
    // `command -v/-V` queries a name; it does not invoke Sift.
    let words = if words.first().is_some_and(|word| word == "command")
        && words.get(1).is_some_and(|word| !word.starts_with('-'))
    {
        &words[1..]
    } else {
        words
    };
    let Some(first) = words.first() else {
        return ("unknown".into(), false);
    };
    let first = basename(first);
    let ctx_output = matches!(first, "ctx" | "ctx.exe")
        && (words
            .get(1)
            .is_some_and(|s| matches!(s.as_str(), "run" | "compact" | "restore" | "recall"))
            || (words.get(1).is_some_and(|s| s == "output")
                && words.get(2).is_some_and(|s| {
                    matches!(
                        s.as_str(),
                        "run"
                            | "compact"
                            | "restore"
                            | "recall"
                            | "proxy"
                            | "filter"
                            | "read"
                            | "json"
                            | "summary"
                            | "err"
                            | "test"
                            | "gain"
                            | "config"
                            | "hook"
                            | "rewrite"
                            | "discover"
                            | "ccusage"
                            | "semantic"
                    )
                })));
    let sift = matches!(first, "sift" | "sift.exe") || ctx_output;
    let safe = match first {
        _ if ctx_output => "ctx sift",
        "git" | "cargo" | "npm" | "pnpm" | "yarn" | "python" | "python3" | "node" | "go"
        | "rustc" | "rg" | "grep" | "ls" | "cat" | "find" | "make" | "pytest" | "docker"
        | "kubectl" | "echo" | "printf" | "sh" | "bash" | "zsh" => first,
        _ if sift => "sift",
        _ => "other",
    };
    let mut label = safe.to_owned();
    if matches!(
        safe,
        "git" | "cargo" | "npm" | "pnpm" | "yarn" | "go" | "sift" | "ctx sift"
    ) && let Some(sub) = words.get(
        if ctx_output && words.get(1).is_some_and(|s| s == "output") {
            2
        } else {
            1
        },
    ) && matches!(
        sub.as_str(),
        "status"
            | "diff"
            | "log"
            | "show"
            | "test"
            | "build"
            | "check"
            | "run"
            | "install"
            | "fmt"
            | "clippy"
            | "compact"
            | "proxy"
            | "gain"
            | "discover"
            | "recall"
    ) {
        label.push(' ');
        label.push_str(sub);
    }
    (label, sift)
}
