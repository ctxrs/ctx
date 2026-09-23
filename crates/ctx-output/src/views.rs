// Adapted from Sift, MIT; source revision and license are in this crate’s NOTICE.
//! Explicit, potentially lossy views. These never select a view automatically,
//! execute a child, infer a test status, or replace the lossless compact path.
use anyhow::{Context, Result, bail, ensure};
use serde::de::{self, MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::value::RawValue;
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, Read, Write};

pub const HELP: &str = "Explicit views (selection can omit information):
  ctx output read [FILE|-] [--from N] [--lines N] [--grep LITERAL]
  ctx output json [FILE|-] [--pointer POINTER] [--field KEY]... [--limit N]
  ctx output summary [--lines N] -- COMMAND [ARG...]
  ctx output err [--context N] -- COMMAND [ARG...]
  ctx output test [--context N] -- COMMAND [ARG...]

Read selects literal, case-sensitive matching lines at/after source line N
(1-based); --lines limits matches. Selected lines preserve their original bytes.
Without selection options, read uses the normal lossless compact path.
JSON emits one JSON value plus LF; formatting may change. It validates the
entire document, selects an RFC 6901 pointer, limits the selected array, then
projects repeated literal --field keys from an object or each remaining row.
Missing keys, duplicate keys, invalid pointers/types and invalid JSON fail.
Numbers retain their original spelling; no depth/string/array elision is implicit.
Input files/stdin and render buffers are limited to 16 MiB; excess is an error.
JSON nesting deeper than 64 levels is rejected, never elided.
Summary keeps the first and last N lines per stream (default 20 each).
Err/test show case-insensitive diagnostic keyword lines and N context lines
(default 2), with explicit omission labels; they are heuristic, not test parsers.
Err matches error/warning/fatal/panic/fail; test matches fail/error/panic/not ok/fatal.
These are substring matches and can include lines such as '0 failures'.
No keyword matches or non-text input passes through unchanged.
Command views require the runner's bounded complete capture; progress is delayed.
Capture overflow passes through raw. Child argv and exit status stay unchanged.
Use ctx output run -- test ... to invoke the native test utility.
Use '--' before a filename starting '-'. FILE defaults to stdin.
";

const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_DEPTH: usize = 64;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReadOptions {
    /// One-based source line; zero is rejected.
    pub from: Option<usize>,
    pub lines: Option<usize>,
    pub grep: Option<String>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct JsonOptions {
    pub pointer: Option<String>,
    pub fields: Vec<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum View {
    Read(ReadOptions),
    Json(JsonOptions),
    Summary { lines: usize },
    Errors { context: usize },
    Test { context: usize },
}

impl View {
    /// Main retains `read`'s existing compact alias unless a selection was asked
    /// for, including an explicitly supplied --from 1 or --grep ''.
    pub fn is_read_selection(&self) -> bool {
        matches!(self, Self::Read(opts) if opts.from.is_some() || opts.lines.is_some() || opts.grep.is_some())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Help,
    Input { path: Option<OsString>, view: View },
    Command { argv: Vec<OsString>, view: View },
}

/// Parse only view options. Once a command begins every remaining argument is
/// forwarded verbatim, including options, shell-looking strings and '--'.
pub fn parse(name: &str, args: &[OsString]) -> Result<Action> {
    let mut view = match name {
        "read" => View::Read(ReadOptions::default()),
        "json" => View::Json(JsonOptions::default()),
        "summary" => View::Summary { lines: 20 },
        "err" => View::Errors { context: 2 },
        "test" => View::Test { context: 2 },
        _ => bail!("unknown view"),
    };
    let command = matches!(name, "summary" | "err" | "test");
    let mut path = None;
    let mut seen = HashSet::new();
    let mut i = 0;
    let mut positional = false;
    while i < args.len() {
        let arg = &args[i];
        let text = arg.to_str();
        if !positional && text == Some("--") {
            positional = true;
            i += 1;
            continue;
        }
        if !positional && matches!(text, Some("--help" | "-h")) {
            return Ok(Action::Help);
        }
        if !positional && text.is_some_and(|s| s.starts_with('-') && s != "-") {
            let (key, inline) = text
                .unwrap()
                .split_once('=')
                .map_or((text.unwrap(), None), |(k, v)| (k, Some(v)));
            ensure!(
                key == "--field" || seen.insert(key.to_owned()),
                "duplicate option {key}"
            );
            let value = match inline {
                Some(value) => value,
                None => {
                    i += 1;
                    args.get(i)
                        .and_then(|s| s.to_str())
                        .context("option requires a UTF-8 value")?
                }
            };
            match (&mut view, key) {
                (View::Read(opts), "--from") => {
                    let n = number(value)?;
                    ensure!(n > 0, "--from must be at least 1");
                    opts.from = Some(n);
                }
                (View::Read(opts), "--lines") => opts.lines = Some(number(value)?),
                (View::Read(opts), "--grep") => opts.grep = Some(value.into()),
                (View::Json(opts), "--pointer") => {
                    pointer_tokens(value)?;
                    opts.pointer = Some(value.into());
                }
                (View::Json(opts), "--field") => {
                    ensure!(
                        !opts.fields.iter().any(|k| k == value),
                        "duplicate field selection"
                    );
                    opts.fields.push(value.into());
                }
                (View::Json(opts), "--limit") => opts.limit = Some(number(value)?),
                (View::Summary { lines }, "--lines") => *lines = number(value)?,
                (View::Errors { context } | View::Test { context }, "--context") => {
                    *context = number(value)?
                }
                _ => bail!("unsupported {name} option {key}; use 'ctx output {name} --help'"),
            }
        } else if command {
            ensure!(!arg.is_empty(), "missing command");
            return Ok(Action::Command {
                argv: args[i..].to_vec(),
                view,
            });
        } else {
            ensure!(path.is_none(), "expected at most one input file");
            path = Some(arg.clone());
        }
        i += 1;
    }
    ensure!(
        !command,
        "{name} requires COMMAND; use 'ctx output {name} --help'"
    );
    Ok(Action::Input { path, view })
}

fn number(value: &str) -> Result<usize> {
    ensure!(
        !value.is_empty() && value.bytes().all(|c| c.is_ascii_digit()),
        "expected a nonnegative integer"
    );
    value.parse().context("integer is too large")
}

/// File/stdin path, independent of execution and usage accounting.
pub fn run_input(path: Option<&OsStr>, view: &View) -> Result<()> {
    let input: Box<dyn Read> = match path {
        Some(path) if path != "-" => {
            Box::new(std::fs::File::open(path).context("cannot open input file")?)
        }
        _ => Box::new(io::stdin().lock()),
    };
    let mut bytes = Vec::new();
    input
        .take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .context("cannot read input")?;
    let output = render(view, &bytes)?;
    io::stdout().lock().write_all(&output)?;
    Ok(())
}

/// Transform one complete stream, without inferring anything about its process.
/// The runner must bypass this on partial/over-cap captures and preserve status.
pub fn render(view: &View, input: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        input.len() <= MAX_BYTES,
        "view input exceeds 16 MiB; use a native streaming filter"
    );
    match view {
        View::Read(opts) => {
            ensure!(opts.from != Some(0), "--from must be at least 1");
            let needle = opts.grep.as_deref().unwrap_or("").as_bytes();
            Ok(input
                .split_inclusive(|&b| b == b'\n')
                .skip(opts.from.unwrap_or(1) - 1)
                .filter(|line| needle.is_empty() || line.windows(needle.len()).any(|w| w == needle))
                .take(opts.lines.unwrap_or(usize::MAX))
                .flatten()
                .copied()
                .collect())
        }
        View::Json(opts) => json(input, opts),
        _ => Ok(text_view(view, input)),
    }
}

// RawValue avoids serde_json::Value's reserved arbitrary-precision number key
// and never round-trips an unknown number through a machine numeric type.
struct Object<'a>(Vec<(String, &'a RawValue)>);

impl<'de> Deserialize<'de> for Object<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct ObjectVisitor;
        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = Object<'de>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an object with unique keys")
            }
            fn visit_map<M: MapAccess<'de>>(
                self,
                mut map: M,
            ) -> std::result::Result<Self::Value, M::Error> {
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

impl Serialize for Object<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in &self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

fn validate(value: &RawValue, depth: usize) -> Result<()> {
    ensure!(depth <= MAX_DEPTH, "JSON exceeds nesting limit of 64");
    match value.get().as_bytes()[0] {
        b'{' => {
            let object: Object<'_> = serde_json::from_str(value.get())?;
            for (_, child) in object.0 {
                validate(child, depth + 1)?;
            }
        }
        b'[' => {
            let children: Vec<&RawValue> = serde_json::from_str(value.get())?;
            for child in children {
                validate(child, depth + 1)?;
            }
        }
        b'"' => {
            let _: String = serde_json::from_str(value.get())?;
        }
        _ => {}
    }
    Ok(())
}

fn pointer_tokens(pointer: &str) -> Result<Vec<String>> {
    if pointer.is_empty() {
        return Ok(Vec::new());
    }
    ensure!(
        pointer.starts_with('/'),
        "JSON pointer must be empty or begin with '/'"
    );
    pointer[1..]
        .split('/')
        .map(|token| {
            let mut out = String::new();
            let mut chars = token.chars();
            while let Some(c) = chars.next() {
                out.push(if c == '~' {
                    match chars.next() {
                        Some('0') => '~',
                        Some('1') => '/',
                        _ => bail!("invalid JSON pointer escape; use ~0 or ~1"),
                    }
                } else {
                    c
                });
            }
            Ok(out)
        })
        .collect()
}

fn select<'a>(mut value: &'a RawValue, pointer: &str) -> Result<&'a RawValue> {
    for token in pointer_tokens(pointer)? {
        value = match value.get().as_bytes()[0] {
            b'{' => {
                let object: Object<'a> = serde_json::from_str(value.get())?;
                object
                    .0
                    .into_iter()
                    .find(|(key, _)| key == &token)
                    .map(|(_, v)| v)
                    .context("JSON pointer key is missing")?
            }
            b'[' => {
                ensure!(
                    token == "0" || !token.starts_with('0'),
                    "JSON array index has a leading zero"
                );
                let index = number(&token).context("invalid JSON array index")?;
                let array: Vec<&'a RawValue> = serde_json::from_str(value.get())?;
                *array
                    .get(index)
                    .context("JSON array index is out of bounds")?
            }
            _ => bail!("JSON pointer cannot descend into a scalar"),
        };
    }
    Ok(value)
}

fn project<'a>(value: &'a RawValue, fields: &[String]) -> Result<Object<'a>> {
    let object: Object<'a> = serde_json::from_str(value.get())
        .context("--field requires an object or array of objects")?;
    let mut result = Vec::new();
    for key in fields {
        let value = object
            .0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| *v)
            .context("selected JSON field is missing")?;
        result.push((key.clone(), value));
    }
    Ok(Object(result))
}

fn json(input: &[u8], opts: &JsonOptions) -> Result<Vec<u8>> {
    let root: &RawValue =
        serde_json::from_slice(input).context("expected exactly one JSON value")?;
    validate(root, 0)?;
    let value = select(root, opts.pointer.as_deref().unwrap_or(""))?;
    let mut seen = HashSet::new();
    ensure!(
        opts.fields.iter().all(|key| seen.insert(key)),
        "duplicate field selection"
    );
    let mut out =
        if value.get().starts_with('[') && (opts.limit.is_some() || !opts.fields.is_empty()) {
            let mut array: Vec<&RawValue> = serde_json::from_str(value.get())?;
            array.truncate(opts.limit.unwrap_or(usize::MAX));
            if opts.fields.is_empty() {
                serde_json::to_vec(&array)?
            } else {
                let rows = array
                    .into_iter()
                    .map(|v| project(v, &opts.fields))
                    .collect::<Result<Vec<_>>>()?;
                serde_json::to_vec(&rows)?
            }
        } else {
            ensure!(
                opts.limit.is_none(),
                "--limit requires a selected JSON array"
            );
            if opts.fields.is_empty() {
                value.get().as_bytes().to_vec()
            } else {
                serde_json::to_vec(&project(value, &opts.fields)?)?
            }
        };
    out.push(b'\n');
    Ok(out)
}

fn text_view(view: &View, input: &[u8]) -> Vec<u8> {
    if input.contains(&0) || std::str::from_utf8(input).is_err() {
        return input.to_vec();
    }
    let lines: Vec<_> = input.split_inclusive(|&b| b == b'\n').collect();
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let label = match view {
        View::Summary { lines: count } => {
            if lines.len() <= count.saturating_mul(2) {
                return input.to_vec();
            }
            ranges.push((0, *count));
            ranges.push((lines.len() - count, lines.len()));
            "summary head/tail"
        }
        View::Errors { context } | View::Test { context } => {
            // ponytail: literal diagnostics work across tools, but aren't parsers.
            // Use native structured output plus `json` for authoritative results.
            let words: &[&str] = if matches!(view, View::Test { .. }) {
                &["fail", "error", "panic", "not ok", "fatal"]
            } else {
                &["error", "warning", "fatal", "panic", "fail"]
            };
            for (i, line) in lines.iter().enumerate() {
                let lower = String::from_utf8_lossy(line).to_ascii_lowercase();
                if words.iter().any(|word| lower.contains(word)) {
                    let start = i.saturating_sub(*context);
                    let end = i
                        .saturating_add(*context)
                        .saturating_add(1)
                        .min(lines.len());
                    if let Some(last) = ranges.last_mut().filter(|last| start <= last.1) {
                        last.1 = end;
                    } else {
                        ranges.push((start, end));
                    }
                }
            }
            if ranges.is_empty() {
                return input.to_vec();
            }
            if matches!(view, View::Test { .. }) {
                "test keyword/context (not a test result parser)"
            } else {
                "diagnostic keyword/context"
            }
        }
        _ => return input.to_vec(),
    };
    let kept: usize = ranges.iter().map(|(start, end)| end - start).sum();
    let mut out = format!(
        "Sift {label} view: {kept}/{} source lines; {} omitted.\n",
        lines.len(),
        lines.len() - kept
    )
    .into_bytes();
    let mut previous = 0;
    for (start, end) in ranges {
        if start > previous {
            out.extend_from_slice(
                format!("[ctx output: {} lines omitted]\n", start - previous).as_bytes(),
            );
        }
        for line in &lines[start..end] {
            out.extend_from_slice(line);
        }
        previous = end;
    }
    if previous < lines.len() {
        if !out.ends_with(b"\n") {
            out.push(b'\n');
        }
        out.extend_from_slice(
            format!("[ctx output: {} lines omitted]\n", lines.len() - previous).as_bytes(),
        );
    }
    out
}
