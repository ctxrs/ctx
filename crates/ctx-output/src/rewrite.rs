// Adapted from Sift, MIT; source revision and license are in this crate’s NOTICE.
//! Keep original executable prefixes visible to host policy while wrapping
//! their output. The first clauses are short-circuited; only the wrappers run.
//! Unsupported syntax is left to the host shell. Pipeline producers and file writes never
//! receive encoded output.
use anyhow::{Context, Result, ensure};
use std::ffi::OsString;
use std::io::Write;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shell {
    Posix,
    PowerShell,
}

impl Shell {
    pub fn native() -> Self {
        if cfg!(windows) {
            Self::PowerShell
        } else {
            Self::Posix
        }
    }
}

pub fn quote(text: &str, shell: Shell) -> String {
    match shell {
        Shell::Posix => format!("'{}'", text.replace('\'', "'\"'\"'")),
        Shell::PowerShell => format!("'{}'", text.replace('\'', "''")),
    }
}

#[derive(Debug)]
pub(crate) struct Token {
    pub(crate) start: usize,
    pub(crate) word: Option<String>,
    pub(crate) operator: Option<&'static str>,
}

// This is deliberately a lexer for the accepted simple-command grammar, not a
// shell evaluator. Original slices, expansions, quoting and separators survive.
pub(crate) fn lex(input: &str, shell: Shell) -> Option<Vec<Token>> {
    let mut tokens = Vec::new();
    let mut chars = input.char_indices().peekable();
    while let Some((start, ch)) = chars.next() {
        if matches!(ch, ' ' | '\t' | '\r') {
            continue;
        }
        if matches!(ch, '<' | '>' | '(' | ')' | '{' | '}' | '#' | '\0') {
            return None;
        }
        if matches!(ch, '|' | '&' | ';' | '\n') {
            let op = match ch {
                '|' if chars.peek().is_some_and(|(_, c)| *c == '|') => {
                    chars.next();
                    "||"
                }
                '&' if chars.peek().is_some_and(|(_, c)| *c == '&') => {
                    chars.next();
                    "&&"
                }
                '&' => return None,
                '|' => "|",
                ';' => ";",
                _ => "\n",
            };
            tokens.push(Token {
                start,
                word: None,
                operator: Some(op),
            });
            continue;
        }
        let mut word = String::new();
        let mut dynamic = false;
        let mut quote_char = None;
        let mut current = Some(ch);
        while let Some(c) = current {
            match (quote_char, c) {
                (None, '\'' | '"') => quote_char = Some(c),
                (Some(q), c) if q == c => {
                    if shell == Shell::PowerShell
                        && q == '\''
                        && chars.peek().is_some_and(|(_, c)| *c == '\'')
                    {
                        chars.next();
                        word.push('\'');
                    } else {
                        quote_char = None;
                    }
                }
                (q, '\\') if shell == Shell::Posix && q != Some('\'') => {
                    let (_, next) = chars.next()?;
                    if next != '\n' {
                        word.push(next);
                    }
                }
                (q, '`') if shell == Shell::PowerShell && q != Some('\'') => {
                    let (_, next) = chars.next()?;
                    word.push(next);
                }
                (q, '`') if shell == Shell::Posix && q != Some('\'') => return None,
                (q, '$') if q != Some('\'') => {
                    if chars.peek().is_some_and(|(_, c)| matches!(c, '(' | '{')) {
                        return None;
                    }
                    dynamic = true;
                    word.push(c);
                }
                (None, '<' | '>' | '(' | ')' | '{' | '}' | '\0') => return None,
                (None, '*' | '?' | '[' | '~') => {
                    dynamic = true;
                    word.push(c);
                }
                _ => word.push(c),
            }
            if quote_char.is_none()
                && chars
                    .peek()
                    .is_some_and(|(_, c)| matches!(c, ' ' | '\t' | '\r' | '\n' | '|' | '&' | ';'))
            {
                break;
            }
            current = chars.next().map(|(_, c)| c);
        }
        if quote_char.is_some() {
            return None;
        }
        tokens.push(Token {
            start,
            word: (!dynamic).then_some(word),
            operator: None,
        });
    }
    Some(tokens)
}

fn assignment(word: &str) -> bool {
    let Some((key, _)) = word.split_once('=') else {
        return false;
    };
    !key.is_empty()
        && key
            .bytes()
            .enumerate()
            .all(|(i, b)| b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit()))
}

fn supported(name: &str) -> bool {
    matches!(
        name,
        "git"
            | "gh"
            | "rg"
            | "grep"
            | "find"
            | "fd"
            | "ls"
            | "tree"
            | "cat"
            | "head"
            | "tail"
            | "wc"
            | "du"
            | "df"
            | "diff"
            | "sort"
            | "uniq"
            | "jq"
            | "cargo"
            | "rustc"
            | "go"
            | "pytest"
            | "ruff"
            | "mypy"
            | "python"
            | "python3"
            | "node"
            | "npm"
            | "pnpm"
            | "yarn"
            | "bun"
            | "deno"
            | "tsc"
            | "eslint"
            | "vitest"
            | "jest"
            | "make"
            | "cmake"
            | "ninja"
            | "docker"
            | "kubectl"
            | "curl"
            | "wget"
            | "dotnet"
            | "java"
            | "mvn"
            | "gradle"
    )
}

fn finite(name: &str, args: &[Token]) -> bool {
    let words: Vec<_> = args.iter().filter_map(|t| t.word.as_deref()).collect();
    if words.iter().any(|w| {
        matches!(
            *w,
            "--watch" | "-w" | "--follow" | "-f" | "--interactive" | "-i" | "--patch" | "-p"
        )
    }) {
        return false;
    }
    match name {
        "git" => words.first().is_some_and(|w| {
            matches!(
                *w,
                "status"
                    | "diff"
                    | "log"
                    | "show"
                    | "ls-files"
                    | "ls-tree"
                    | "branch"
                    | "rev-parse"
            )
        }),
        "cargo" => words.first().is_some_and(|w| {
            matches!(
                *w,
                "test" | "build" | "check" | "clippy" | "fmt" | "tree" | "metadata"
            )
        }),
        "go" => words
            .first()
            .is_some_and(|w| matches!(*w, "test" | "build" | "vet" | "list")),
        "rg" | "grep" | "find" | "fd" | "ls" | "tree" | "cat" | "head" | "wc" | "du" | "df"
        | "diff" | "sort" | "uniq" | "jq" | "pytest" | "ruff" | "mypy" | "rustc" | "tsc"
        | "eslint" => true,
        _ => false,
    }
}

pub fn command(
    input: &str,
    executable: &Path,
    shell: Shell,
    exclusions: &[String],
) -> Option<String> {
    command_with_namespace(input, executable, shell, exclusions, true)
}

// History discovery also verifies the exact wrappers emitted by released Sift.
pub(crate) fn command_with_namespace(
    input: &str,
    executable: &Path,
    shell: Shell,
    exclusions: &[String],
    namespaced: bool,
) -> Option<String> {
    if input.len() > 64 * 1024 || shell == Shell::PowerShell {
        return None;
    }
    let tokens = lex(input, shell)?;
    if tokens
        .iter()
        .any(|t| t.operator == Some("|") || (t.operator.is_none() && t.word.is_none()))
    {
        return None;
    }
    // Do not reinterpret control-flow grammar as a list of simple commands.
    if tokens.iter().any(|t| {
        t.word.as_deref().is_some_and(|w| {
            matches!(
                w,
                "if" | "then"
                    | "else"
                    | "fi"
                    | "for"
                    | "while"
                    | "until"
                    | "case"
                    | "esac"
                    | "do"
                    | "done"
                    | "function"
            )
        })
    }) {
        return None;
    }
    let executable = executable.to_str()?;
    let prefix = if namespaced { "output run" } else { "run" };
    let wrapper = format!("command {} {prefix}", quote(executable, shell));
    let mut checks = String::new();
    let mut insertions = Vec::new();
    let mut start = 0;
    for end in 0..=tokens.len() {
        if end < tokens.len() && tokens[end].operator.is_none() {
            continue;
        }
        let segment = &tokens[start..end];
        let next = tokens.get(end).and_then(|t| t.operator);
        if next != Some("|") && !segment.is_empty() {
            let word = segment[0].word.as_deref()?;
            // Every segment must remain in the host's plain-command grammar;
            // one declaration or expansion can hide all other policy prefixes.
            if assignment(word) || input[segment[0].start..].starts_with(['\'', '"']) {
                return None;
            }
            let name = word.rsplit(['/', '\\']).next().unwrap_or(word);
            let name = name.strip_suffix(".exe").unwrap_or(name);
            if !supported(name)
                && !matches!(
                    name,
                    "cd" | "pwd" | "echo" | "printf" | "true" | "false" | ":"
                )
            {
                return None;
            }
            if supported(name) && !exclusions.iter().any(|e| e == name) {
                let complete = if finite(name, &segment[1..]) {
                    " --capture"
                } else {
                    ""
                };
                let end_offset = tokens.get(end).map_or(input.len(), |t| t.start);
                checks.push_str("command true || ");
                checks.push_str(&input[segment[0].start..end_offset]);
                checks.push_str("; ");
                insertions.push((segment[0].start, format!("{wrapper}{complete} -- ")));
            }
        }
        start = end + 1;
    }
    if insertions.is_empty() {
        return None;
    }
    let mut output = input.to_owned();
    for (offset, text) in insertions.into_iter().rev() {
        output.insert_str(offset, &text);
    }
    Some(format!("{checks}{output}"))
}

pub fn run(args: &[OsString]) -> Result<i32> {
    let mut shell = Shell::native();
    let mut json = false;
    let mut iter = args.iter();
    let mut input = None;
    while let Some(arg) = iter.next() {
        match arg.to_str() {
            Some("--json") => json = true,
            Some("--shell") => {
                shell = match iter.next().and_then(|s| s.to_str()) {
                    Some("posix" | "bash" | "sh") => Shell::Posix,
                    Some("powershell" | "pwsh") => Shell::PowerShell,
                    _ => anyhow::bail!("--shell requires posix or powershell"),
                }
            }
            Some("--") => {
                input = iter.next().and_then(|s| s.to_str());
                ensure!(
                    iter.next().is_none(),
                    "rewrite expects one shell command string"
                );
                break;
            }
            Some(value) if !value.starts_with('-') && input.is_none() => input = Some(value),
            _ => {
                anyhow::bail!("ctx output rewrite [--json] [--shell posix|powershell] -- 'COMMAND'")
            }
        }
    }
    let input = input.context("rewrite requires one shell command string")?;
    let settings = crate::state::Settings::load()?;
    let rewritten = settings
        .enabled
        .then(|| {
            command(
                input,
                &std::env::current_exe().ok()?,
                shell,
                &settings.exclude_commands,
            )
        })
        .flatten();
    let mut output = std::io::stdout().lock();
    if json {
        writeln!(
            output,
            "{}",
            serde_json::json!({"changed":rewritten.is_some(),"command":rewritten.as_deref().unwrap_or(input)})
        )?;
    } else if let Some(text) = &rewritten {
        writeln!(output, "{text}")?;
    }
    Ok(if rewritten.is_some() { 0 } else { 1 })
}
