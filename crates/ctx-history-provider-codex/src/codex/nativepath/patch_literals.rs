//! Literal path declarations in complete Codex patch text, never file effects.
//!
//! Tool-input text can contain a patch directly or as a quoted string. Reading
//! that declaration does not evaluate its surrounding program, bind variables,
//! resolve a workdir, or assert that the patch was submitted or applied.

use std::{collections::HashSet, fmt};

use ctx_history_core::{MAX_CORE_CONTENT_BYTES, MAX_PROVIDER_DECLARED_FACTS};
use serde::de::{DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;
use serde_json::value::RawValue;

const BEGIN: &str = "*** Begin Patch";
const END: &str = "*** End Patch";

#[derive(Deserialize)]
struct Envelope<'a> {
    #[serde(rename = "type")]
    kind: String,
    #[serde(borrow)]
    payload: Option<&'a RawValue>,
    #[serde(borrow)]
    arguments: Option<&'a RawValue>,
    #[serde(borrow)]
    input: Option<&'a RawValue>,
    #[serde(borrow)]
    args: Option<&'a RawValue>,
}

pub(super) fn declared_files(bytes: &[u8]) -> Vec<String> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Vec::new();
    };
    if !text.contains(BEGIN) && !text.contains("\\u") {
        return Vec::new();
    }
    let Ok(mut envelope) = serde_json::from_slice::<Envelope<'_>>(bytes) else {
        return Vec::new();
    };
    if envelope.kind == "response_item" {
        let Some(payload) = envelope.payload else {
            return Vec::new();
        };
        let Ok(payload) = serde_json::from_str::<Envelope<'_>>(payload.get()) else {
            return Vec::new();
        };
        envelope = payload;
    }
    let (raw, decode_json) = match envelope.kind.as_str() {
        "function_call" if envelope.input.is_none() && envelope.args.is_none() => {
            (envelope.arguments, true)
        }
        "custom_tool_call" if envelope.arguments.is_none() && envelope.args.is_none() => {
            (envelope.input, false)
        }
        _ => return Vec::new(),
    };
    let Some(raw) = raw.filter(|raw| raw.get().len() <= MAX_CORE_CONTENT_BYTES) else {
        return Vec::new();
    };
    let mut files = Files::default();
    let mut deserializer = serde_json::Deserializer::from_str(raw.get());
    if (Strings {
        files: &mut files,
        decode_json,
    })
    .deserialize(&mut deserializer)
    .and_then(|()| deserializer.end())
    .is_err()
        || files.unavailable
    {
        return Vec::new();
    }
    files.ordered
}

#[derive(Default)]
struct Files {
    ordered: Vec<String>,
    seen: HashSet<String>,
    bytes: usize,
    unavailable: bool,
}

impl Files {
    fn add(&mut self, path: &str) {
        if self.unavailable || self.seen.contains(path) {
            return;
        }
        if self.ordered.len() >= MAX_PROVIDER_DECLARED_FACTS
            || path.len() > MAX_CORE_CONTENT_BYTES.saturating_sub(self.bytes)
        {
            self.unavailable = true;
            self.ordered.clear();
            self.seen.clear();
            return;
        }
        self.bytes += path.len();
        self.seen.insert(path.to_owned());
        self.ordered.push(path.to_owned());
    }

    fn text(&mut self, text: &str) {
        let mut remaining = text;
        while !self.unavailable {
            let Some(start) = remaining.find(BEGIN) else {
                break;
            };
            let after = &remaining[start + BEGIN.len()..];
            if let Some((end, paths)) = block_paths(after) {
                for path in paths {
                    self.add(&path);
                }
                remaining = &after[end..];
                continue;
            }
            if PatchLines::new(after).is_some() {
                // No closing marker in this suffix. Do not scan it again for
                // every nested Begin marker in an incomplete input.
                break;
            }
            remaining = after;
        }
    }
}

// Only the line separators and header values need decoding. In particular a
// JavaScript-only escape in patch body text must not hide an ordinary filename.
// This is not a parser or evaluator for the program surrounding a declaration.
struct PatchLines<'a> {
    remaining: &'a str,
    offset: usize,
    slash_width: usize,
}

impl<'a> PatchLines<'a> {
    fn new(text: &'a str) -> Option<Self> {
        let slash_width = text.bytes().take_while(|&byte| byte == b'\\').count();
        if slash_width == 0 {
            if !text.starts_with('\n') && !text.starts_with("\r\n") {
                return None;
            }
        } else if !slash_width.is_power_of_two()
            || !matches!(text.as_bytes().get(slash_width), Some(b'n' | b'r'))
        {
            return None;
        }
        if slash_width > 0 && text.as_bytes()[slash_width] == b'r' {
            let rest = &text.as_bytes()[slash_width + 1..];
            if rest.iter().take_while(|&&byte| byte == b'\\').count() != slash_width
                || rest.get(slash_width) != Some(&b'n')
            {
                return None;
            }
        }
        Some(Self {
            remaining: text,
            offset: 0,
            slash_width,
        })
    }

    fn next(&mut self) -> Option<(usize, &'a str)> {
        if self.remaining.is_empty() {
            return None;
        }
        let bytes = self.remaining.as_bytes();
        let delimiter = if self.slash_width == 0 {
            self.remaining.find('\n').map(|at| (at, 1))
        } else {
            let mut at = 0;
            let mut found = None;
            while at < bytes.len() {
                if bytes[at] != b'\\' {
                    at += 1;
                    continue;
                }
                let start = at;
                while bytes.get(at) == Some(&b'\\') {
                    at += 1;
                }
                // An escaped backslash followed by n is part of a path/body,
                // not a line break. The same parity rule holds through nested
                // quoted layers (one, two, four ... backslashes).
                if bytes.get(at) == Some(&b'n')
                    && (at - start) % (2 * self.slash_width) == self.slash_width
                {
                    found = Some((at - self.slash_width, self.slash_width + 1));
                    break;
                }
            }
            found
        };
        let (end, separator) = delimiter.unwrap_or((bytes.len(), 0));
        let mut line = &self.remaining[..end];
        if self.slash_width == 0 {
            line = line.strip_suffix('\r').unwrap_or(line);
        } else if line.ends_with('r') {
            let slashes = line.as_bytes()[..line.len() - 1]
                .iter()
                .rev()
                .take_while(|&&byte| byte == b'\\')
                .count();
            if slashes % (2 * self.slash_width) == self.slash_width {
                line = &line[..line.len() - self.slash_width - 1];
            }
        }
        let offset = self.offset;
        self.offset += end + separator;
        self.remaining = &self.remaining[end + separator..];
        Some((offset, line))
    }
}

fn block_paths(block: &str) -> Option<(usize, Vec<String>)> {
    let mut lines = PatchLines::new(block)?;
    let depth = if lines.slash_width == 0 {
        0
    } else {
        lines.slash_width.trailing_zeros() + 1
    };
    let (_, first) = lines.next()?;
    if !first.is_empty() {
        return None;
    }
    let mut paths = Vec::new();
    let mut operation = "";
    let mut move_allowed = false;
    while let Some((offset, line)) = lines.next() {
        if line.strip_prefix(END).is_some_and(|tail| {
            let tail = tail.trim_start_matches(['\r', ' ', '\t']);
            tail.is_empty() || tail.trim_start_matches('\\').starts_with(['"', '\'', '`'])
        }) {
            return Some((offset + END.len(), paths));
        }
        let mut valid = true;
        let header = ["Add File: ", "Update File: ", "Delete File: "]
            .into_iter()
            .find_map(|kind| {
                line.strip_prefix("*** ")?
                    .strip_prefix(kind)
                    .map(|path| (kind, path))
            });
        if let Some((kind, path)) = header {
            valid = add_header(&mut paths, path, depth);
            operation = kind;
            move_allowed = kind == "Update File: ";
        } else if let Some(path) = line.strip_prefix("*** Move to: ") {
            valid = move_allowed && add_header(&mut paths, path, depth);
            move_allowed = false;
        } else {
            let body = match operation {
                "Add File: " => line.starts_with('+'),
                "Update File: " => {
                    line.is_empty()
                        || line.starts_with(['+', '-', ' '])
                        || line == "@@"
                        || line.starts_with("@@ ")
                        || line == "*** End of File"
                }
                _ => false,
            };
            if !body {
                valid = false;
            }
            move_allowed = false;
        }
        if !valid {
            // This candidate is incomplete/malformed. Resume at the first
            // invalid line, which may contain a separate complete declaration.
            // Never restart scans over the already inspected prefix.
            return Some((offset, Vec::new()));
        }
    }
    None
}

fn add_header(paths: &mut Vec<String>, encoded: &str, depth: u32) -> bool {
    if encoded.is_empty() || paths.len() == MAX_PROVIDER_DECLARED_FACTS {
        return false;
    }
    let mut path = encoded.to_owned();
    for _ in 0..depth {
        // A double quote is an ordinary header character in single-quoted or
        // backtick text. Shield only bare delimiters; serde_json still owns
        // escape decoding, including escaped backslashes and Unicode.
        let mut quoted = String::with_capacity(path.len() + 2);
        quoted.push('"');
        let mut escaped = false;
        for ch in path.chars() {
            if ch == '"' && !escaped {
                quoted.push('\\');
            }
            quoted.push(ch);
            escaped = ch == '\\' && !escaped;
        }
        quoted.push('"');
        let Ok(decoded) = serde_json::from_str::<String>(&quoted) else {
            return false;
        };
        path = decoded;
    }
    if path.is_empty() {
        return false;
    }
    paths.push(path);
    true
}

struct Strings<'a> {
    files: &'a mut Files,
    decode_json: bool,
}

impl<'de> DeserializeSeed<'de> for Strings<'_> {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Strings<'_> {
    type Value = ();
    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("literal Codex tool-input strings")
    }

    fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<(), M::Error> {
        while map.next_key::<IgnoredAny>()?.is_some() {
            map.next_value_seed(Strings {
                files: &mut *self.files,
                decode_json: false,
            })?;
        }
        Ok(())
    }

    fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<(), S::Error> {
        while seq
            .next_element_seed(Strings {
                files: &mut *self.files,
                decode_json: false,
            })?
            .is_some()
        {}
        Ok(())
    }

    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<(), E> {
        if self.decode_json && value.trim_start().starts_with(['{', '[']) {
            let mut deserializer = serde_json::Deserializer::from_str(value);
            Strings {
                files: self.files,
                decode_json: false,
            }
            .deserialize(&mut deserializer)
            .and_then(|()| deserializer.end())
            .map_err(E::custom)
        } else {
            self.files.text(value);
            Ok(())
        }
    }

    fn visit_bool<E>(self, _: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
    fn visit_unit<E>(self) -> Result<(), E> {
        Ok(())
    }
}
