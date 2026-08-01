use ctx_history_core::EventRole;
use serde_json::Value;

use crate::provider::file_touches::visit_all_file_touch_drafts;
use crate::provider::normalization::capped_text;
use crate::provider::tool_input;
use crate::PROVIDER_MAX_PREVIEW_CHARS;

pub(crate) fn codex_tool_name(payload: &Value, item_type: &str) -> String {
    payload
        .get("name")
        .or_else(|| payload.get("tool"))
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(item_type)
        .to_owned()
}
pub(crate) use tool_input::is_command_tool as codex_is_command_tool;
pub(crate) fn codex_command_preview(
    tool_name: &str,
    argument_value: Option<&Value>,
) -> Option<String> {
    if !codex_is_command_tool(tool_name) {
        return None;
    }
    let value = argument_value?;
    let command = tool_input::command(value)?;
    Some(codex_local_preview(&command, PROVIDER_MAX_PREVIEW_CHARS).0)
}
pub(crate) fn codex_command_text(
    tool_name: &str,
    argument_value: Option<&Value>,
) -> Option<String> {
    if !codex_is_command_tool(tool_name) {
        return None;
    }
    tool_input::command(argument_value?)
}
pub(crate) fn codex_value_preview(value: &Value, max_chars: usize) -> (String, bool) {
    let rendered = match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        _ => serde_json::to_string(value).unwrap_or_else(|_| value.to_string()),
    };
    codex_local_preview(&rendered, max_chars)
}
pub(crate) fn codex_tool_arguments_preview(value: &Value) -> (String, bool, bool) {
    let parsed = codex_parse_embedded_json(value);
    let parsed = parsed.as_ref().unwrap_or(value);
    let mut retained_paths = Vec::with_capacity(12);
    let mut file_touch_count = 0_usize;
    let visit_result: std::result::Result<(), std::convert::Infallible> =
        visit_all_file_touch_drafts(parsed, |touch| {
            file_touch_count = file_touch_count.saturating_add(1);
            if retained_paths.len() < 12 {
                retained_paths.push(match touch.change_kind {
                    Some(kind) => format!("{}:{}", kind.as_str(), touch.path),
                    None => touch.path,
                });
            }
            Ok(())
        });
    match visit_result {
        Ok(()) => {}
        Err(never) => match never {},
    }
    if file_touch_count != 0 {
        return codex_file_touch_arguments_preview(retained_paths, file_touch_count);
    }
    let (preview, truncated) = codex_value_preview(parsed, PROVIDER_MAX_PREVIEW_CHARS);
    (preview, truncated, false)
}
pub(crate) fn codex_tool_arguments_text(value: &Value) -> (String, bool) {
    let retained = codex_tool_arguments_value(value);
    let rendered = match retained {
        Value::String(text) => text,
        Value::Null => String::new(),
        other => serde_json::to_string(&other).unwrap_or_else(|_| other.to_string()),
    };
    (rendered, false)
}
pub(crate) fn codex_tool_arguments_value(value: &Value) -> Value {
    codex_parse_embedded_json(value).unwrap_or_else(|| value.clone())
}
fn codex_file_touch_arguments_preview(
    retained_paths: Vec<String>,
    file_touch_count: usize,
) -> (String, bool, bool) {
    let paths = retained_paths.join(", ");
    let omitted = file_touch_count.saturating_sub(12);
    let suffix = if omitted == 0 {
        String::new()
    } else {
        format!(", +{omitted} more")
    };
    (format!("file touches: {paths}{suffix}"), omitted > 0, false)
}
pub(crate) fn codex_local_preview(value: &str, max_chars: usize) -> (String, bool) {
    capped_text(value, max_chars)
}
pub(crate) fn codex_parse_embedded_json(value: &Value) -> Option<Value> {
    match value {
        Value::String(text) => serde_json::from_str::<Value>(text).ok(),
        Value::Object(_) | Value::Array(_) => Some(value.clone()),
        _ => None,
    }
}

const CODEX_PROCESS_EXIT_MARKER: &[u8] = b"Process exited with code ";
const CODEX_SCRIPT_COMPLETED: &[u8] = b"Script completed";
const CODEX_SCRIPT_FAILED: &[u8] = b"Script failed";
const CODEX_SCRIPT_PREFIX_BYTES: usize = CODEX_SCRIPT_COMPLETED.len() + 1;

#[derive(Debug, Clone, Copy, Default)]
struct CodexExitCodeNumber {
    found: bool,
    active: bool,
    invalid: bool,
    negative: bool,
    saw_digit: bool,
    magnitude: u32,
}

impl CodexExitCodeNumber {
    fn start(&mut self) {
        self.found = true;
        self.active = true;
    }

    fn feed(&mut self, byte: u8) {
        if !self.active {
            return;
        }
        match byte {
            b'0'..=b'9' => {
                self.saw_digit = true;
                let digit = u32::from(byte - b'0');
                let Some(magnitude) = self
                    .magnitude
                    .checked_mul(10)
                    .and_then(|value| value.checked_add(digit))
                    .filter(|value| *value <= i32::MIN.unsigned_abs())
                else {
                    self.invalid = true;
                    return;
                };
                self.magnitude = magnitude;
            }
            b'-' if !self.negative && !self.saw_digit => self.negative = true,
            b'-' => self.invalid = true,
            _ => self.active = false,
        }
    }

    fn value(self) -> Option<i32> {
        if !self.found || self.invalid || !self.saw_digit {
            return None;
        }
        if self.negative {
            if self.magnitude == i32::MIN.unsigned_abs() {
                Some(i32::MIN)
            } else {
                i32::try_from(self.magnitude).ok().map(|value| -value)
            }
        } else {
            i32::try_from(self.magnitude).ok()
        }
    }
}

/// Canonical, allocation-free parser for Codex textual exit-code outcomes.
///
/// Its integer state admits any number of leading zeroes while the provider
/// record itself remains bounded, and rejects only invalid syntax or a true
/// `i32` overflow. The first process marker has the historical precedence.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CodexExitCodeParser {
    process_marker_len: usize,
    script_prefix: [u8; CODEX_SCRIPT_PREFIX_BYTES],
    script_prefix_len: usize,
    text_len: usize,
    process: CodexExitCodeNumber,
}

impl Default for CodexExitCodeParser {
    fn default() -> Self {
        Self {
            process_marker_len: 0,
            script_prefix: [0; CODEX_SCRIPT_PREFIX_BYTES],
            script_prefix_len: 0,
            text_len: 0,
            process: CodexExitCodeNumber::default(),
        }
    }
}

impl CodexExitCodeParser {
    pub(crate) fn feed_bytes(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.process.feed(byte);
            if self.script_prefix_len < self.script_prefix.len() {
                self.script_prefix[self.script_prefix_len] = byte;
                self.script_prefix_len += 1;
            }
            self.text_len = self.text_len.saturating_add(1);
            if !self.process.found {
                if CODEX_PROCESS_EXIT_MARKER.get(self.process_marker_len) == Some(&byte) {
                    self.process_marker_len += 1;
                    if self.process_marker_len == CODEX_PROCESS_EXIT_MARKER.len() {
                        self.process.start();
                    }
                } else {
                    self.process_marker_len = usize::from(byte == CODEX_PROCESS_EXIT_MARKER[0]);
                }
            }
        }
    }

    pub(crate) fn exit_code(self) -> Option<i32> {
        if self.script_completed() {
            Some(0)
        } else if self.script_failed() {
            Some(1)
        } else {
            self.process.value()
        }
    }

    pub(crate) fn script_completed(self) -> bool {
        self.starts_with_script(CODEX_SCRIPT_COMPLETED)
    }

    fn script_failed(self) -> bool {
        self.starts_with_script(CODEX_SCRIPT_FAILED)
    }

    fn starts_with_script(self, prefix: &[u8]) -> bool {
        (self.text_len == prefix.len() && self.script_prefix.get(..prefix.len()) == Some(prefix))
            || (self.text_len > prefix.len()
                && self.script_prefix.get(..prefix.len()) == Some(prefix)
                && self.script_prefix.get(prefix.len()) == Some(&b'\n'))
    }
}

#[cfg(test)]
pub(crate) fn codex_exit_code(text: &str) -> Option<i32> {
    let mut parser = CodexExitCodeParser::default();
    parser.feed_bytes(text.as_bytes());
    parser.exit_code()
}

const CODEX_WALL_TIME_NUMBER_BYTES: usize = 128;
const CODEX_WALL_TIME_ROLLING_BYTES: usize = b"Wall time: ".len();

#[derive(Debug, Clone, Copy)]
struct CodexWallTimeNumber {
    bytes: [u8; CODEX_WALL_TIME_NUMBER_BYTES],
    len: usize,
    found: bool,
    active: bool,
    overflowed: bool,
}

impl Default for CodexWallTimeNumber {
    fn default() -> Self {
        Self {
            bytes: [0; CODEX_WALL_TIME_NUMBER_BYTES],
            len: 0,
            found: false,
            active: false,
            overflowed: false,
        }
    }
}

impl CodexWallTimeNumber {
    fn start(&mut self) {
        self.found = true;
        self.active = true;
    }

    fn feed(&mut self, byte: u8) {
        if !self.active {
            return;
        }
        if !byte.is_ascii_digit() && byte != b'.' {
            self.active = false;
            return;
        }
        if self.len == self.bytes.len() {
            self.overflowed = true;
            self.active = false;
            return;
        }
        self.bytes[self.len] = byte;
        self.len += 1;
    }

    fn duration_ms(self) -> Option<i64> {
        if !self.found || self.overflowed || self.len == 0 {
            return None;
        }
        let seconds = std::str::from_utf8(&self.bytes[..self.len])
            .ok()?
            .parse::<f64>()
            .ok()?;
        Some((seconds * 1000.0).round() as i64)
    }
}

/// Canonical, allocation-free parser for the Codex textual wall-time grammar.
///
/// The first colon-form marker has precedence over the first space-form marker,
/// matching the historical `codex_wall_time_ms` lookup. The numeric token is
/// exactly the leading run of ASCII digits and dots after that marker, bounded
/// to 128 bytes before the duration is treated as absent.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct CodexWallTimeParser {
    rolling: [u8; CODEX_WALL_TIME_ROLLING_BYTES],
    rolling_len: usize,
    colon: CodexWallTimeNumber,
    space: CodexWallTimeNumber,
}

impl CodexWallTimeParser {
    pub(crate) fn feed_bytes(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.colon.feed(byte);
            self.space.feed(byte);
            if self.rolling_len < self.rolling.len() {
                self.rolling[self.rolling_len] = byte;
                self.rolling_len += 1;
            } else {
                self.rolling.copy_within(1.., 0);
                self.rolling[self.rolling.len() - 1] = byte;
            }
            let rolling = &self.rolling[..self.rolling_len];
            if !self.colon.found && rolling.ends_with(b"Wall time: ") {
                self.colon.start();
            }
            if !self.space.found && rolling.ends_with(b"Wall time ") {
                self.space.start();
            }
        }
    }

    pub(crate) fn duration_ms(self) -> Option<i64> {
        if self.colon.found {
            self.colon.duration_ms()
        } else {
            self.space.duration_ms()
        }
    }
}

#[cfg(test)]
pub(crate) fn codex_wall_time_ms(text: &str) -> Option<i64> {
    let mut parser = CodexWallTimeParser::default();
    parser.feed_bytes(text.as_bytes());
    parser.duration_ms()
}
pub(crate) fn codex_event_role(role: &str) -> EventRole {
    match role {
        "user" => EventRole::User,
        "assistant" => EventRole::Assistant,
        "tool" => EventRole::Tool,
        "system" | "developer" => EventRole::System,
        _ => EventRole::Unknown,
    }
}
pub(crate) fn codex_content_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => {
            let mut parts = Vec::new();
            for block in blocks {
                if let Some(text) = block
                    .get("text")
                    .or_else(|| block.get("input_text"))
                    .or_else(|| block.get("output_text"))
                    .or_else(|| block.get("summary_text"))
                    .and_then(Value::as_str)
                {
                    parts.push(text.to_owned());
                    continue;
                }
                if let Some(text) = block.get("content").and_then(Value::as_str) {
                    parts.push(text.to_owned());
                    continue;
                }
            }
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n"))
            }
        }
        Value::Object(object) => {
            for key in [
                "text",
                "input_text",
                "output_text",
                "summary_text",
                "content",
            ] {
                if let Some(text) = object.get(key).and_then(Value::as_str) {
                    return Some(text.to_owned());
                }
                if let Some(text) = object.get(key).and_then(codex_content_text) {
                    return Some(text);
                }
            }
            None
        }
        _ => None,
    }
}
