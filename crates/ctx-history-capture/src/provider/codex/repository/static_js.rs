use std::collections::HashMap;

use serde_json::{Map, Value};

const MAX_STATIC_NESTED_TOOL_CALLS: usize = 24;
const MAX_STATIC_BINDINGS: usize = 24;
const MAX_STATIC_LITERAL_DEPTH: usize = 32;
const MAX_STATIC_LITERAL_ITEMS: usize = 256;

pub(super) enum StaticNestedToolCall {
    ExecCommand(Map<String, Value>),
    ApplyPatch(String),
}

pub(super) struct StaticJsParser<'a> {
    source: &'a [u8],
    cursor: usize,
    static_strings: HashMap<String, String>,
}

impl<'a> StaticJsParser<'a> {
    pub(super) fn new(source: &'a str) -> Self {
        Self {
            source: source.as_bytes(),
            cursor: 0,
            static_strings: HashMap::new(),
        }
    }

    pub(super) fn parse_program(mut self) -> Option<Vec<StaticNestedToolCall>> {
        let mut calls = Vec::new();
        let mut terminal_output_statements = 0_usize;
        loop {
            self.skip_program_trivia()?;
            if self.cursor == self.source.len() {
                break;
            }
            if terminal_output_statements == 0 {
                if let Some((name, value)) = self.parse_static_string_declaration() {
                    if self.static_strings.len() >= MAX_STATIC_BINDINGS
                        || self.static_strings.insert(name, value).is_some()
                    {
                        return None;
                    }
                    continue;
                }
                if let Some(call) = self.parse_tool_statement() {
                    calls.push(call);
                    if calls.len() > MAX_STATIC_NESTED_TOOL_CALLS {
                        return None;
                    }
                    continue;
                }
                if let Some(call) = self.parse_wrapped_tool_statement() {
                    calls.push(call);
                    if calls.len() > MAX_STATIC_NESTED_TOOL_CALLS {
                        return None;
                    }
                    continue;
                }
            }
            if self.parse_terminal_output_statement() {
                terminal_output_statements += 1;
                if terminal_output_statements > MAX_STATIC_NESTED_TOOL_CALLS {
                    return None;
                }
                continue;
            }
            return None;
        }
        Some(calls)
    }

    fn parse_static_string_declaration(&mut self) -> Option<(String, String)> {
        let checkpoint = self.cursor;
        let parsed = self.parse_static_string_declaration_inner();
        if parsed.is_none() {
            self.cursor = checkpoint;
        }
        parsed
    }

    fn parse_static_string_declaration_inner(&mut self) -> Option<(String, String)> {
        self.consume_keyword("const").then_some(())?;
        self.skip_whitespace();
        let name = self.parse_identifier()?;
        self.skip_whitespace();
        self.consume_byte(b'=').then_some(())?;
        self.skip_whitespace();
        let value = self.parse_json_string()?;
        self.consume_statement_terminator().then_some(())?;
        Some((name, value))
    }

    fn parse_terminal_output_statement(&mut self) -> bool {
        let checkpoint = self.cursor;
        if !self.consume_keyword("text") {
            return false;
        }
        self.skip_whitespace();
        if !self.consume_byte(b'(') {
            self.cursor = checkpoint;
            return false;
        }
        self.skip_whitespace();
        let argument_ok = if self.source.get(self.cursor) == Some(&b'`') {
            self.parse_output_template()
        } else if self.source.get(self.cursor) == Some(&b'"') {
            self.parse_json_string().is_some()
        } else {
            self.parse_member_reference()
        };
        if !argument_ok {
            self.cursor = checkpoint;
            return false;
        }
        self.skip_whitespace();
        if !self.consume_byte(b')') {
            self.cursor = checkpoint;
            return false;
        }
        if !self.consume_statement_terminator() {
            self.cursor = checkpoint;
            return false;
        }
        true
    }

    fn parse_output_template(&mut self) -> bool {
        if !self.consume_byte(b'`') {
            return false;
        }
        while let Some(byte) = self.source.get(self.cursor).copied() {
            match byte {
                b'`' => {
                    self.cursor += 1;
                    return true;
                }
                b'\\' => {
                    self.cursor += 1;
                    if self.source.get(self.cursor).is_none() {
                        return false;
                    }
                    self.cursor += 1;
                }
                b'$' if self
                    .source
                    .get(self.cursor.saturating_add(1))
                    .is_some_and(|next| *next == b'{') =>
                {
                    self.cursor += 2;
                    self.skip_whitespace();
                    if !self.parse_member_reference() {
                        return false;
                    }
                    self.skip_whitespace();
                    if !self.consume_byte(b'}') {
                        return false;
                    }
                }
                _ => self.cursor += 1,
            }
        }
        false
    }

    fn parse_member_reference(&mut self) -> bool {
        if self.parse_identifier().is_none() {
            return false;
        }
        loop {
            if !self.consume_byte(b'.') {
                return true;
            }
            if self.parse_identifier().is_none() {
                return false;
            }
        }
    }

    fn consume_statement_terminator(&mut self) -> bool {
        self.skip_whitespace();
        if self.consume_byte(b';') {
            return true;
        }
        let saved = self.cursor;
        if self.skip_program_trivia().is_some() && self.cursor == self.source.len() {
            self.cursor = saved;
            true
        } else {
            self.cursor = saved;
            false
        }
    }

    fn parse_tool_statement(&mut self) -> Option<StaticNestedToolCall> {
        let checkpoint = self.cursor;
        let parsed = self.parse_tool_statement_inner();
        if parsed.is_none() {
            self.cursor = checkpoint;
        }
        parsed
    }

    fn parse_tool_statement_inner(&mut self) -> Option<StaticNestedToolCall> {
        if self.consume_keyword("const") {
            self.skip_whitespace();
            self.parse_identifier()?;
            self.skip_whitespace();
            self.consume_byte(b'=').then_some(())?;
            self.skip_whitespace();
        }
        let call = self.parse_tool_invocation()?;
        self.consume_statement_terminator().then_some(())?;
        Some(call)
    }

    fn parse_wrapped_tool_statement(&mut self) -> Option<StaticNestedToolCall> {
        let checkpoint = self.cursor;
        let parsed = self.parse_wrapped_tool_statement_inner();
        if parsed.is_none() {
            self.cursor = checkpoint;
        }
        parsed
    }

    fn parse_wrapped_tool_statement_inner(&mut self) -> Option<StaticNestedToolCall> {
        self.consume_keyword("text").then_some(())?;
        self.skip_whitespace();
        self.consume_byte(b'(').then_some(())?;
        self.skip_whitespace();
        let call = self.parse_tool_invocation()?;
        self.skip_whitespace();
        self.consume_byte(b')').then_some(())?;
        self.consume_statement_terminator().then_some(())?;
        Some(call)
    }

    fn parse_tool_invocation(&mut self) -> Option<StaticNestedToolCall> {
        self.consume_keyword("await").then_some(())?;
        self.skip_whitespace();
        self.consume_bytes(b"tools.").then_some(())?;
        let method = self.parse_identifier()?;
        self.skip_whitespace();
        self.consume_byte(b'(').then_some(())?;
        self.skip_whitespace();
        let value = if method == "apply_patch"
            && self
                .source
                .get(self.cursor)
                .is_some_and(|byte| byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'$'))
        {
            let binding = self.parse_identifier()?;
            Value::String(self.static_strings.get(&binding)?.clone())
        } else {
            self.parse_static_value(0)?
        };
        self.skip_whitespace();
        self.consume_byte(b')').then_some(())?;
        match (method.as_str(), value) {
            ("exec_command", Value::Object(arguments)) => {
                Some(StaticNestedToolCall::ExecCommand(arguments))
            }
            ("apply_patch", Value::String(patch)) => Some(StaticNestedToolCall::ApplyPatch(patch)),
            _ => None,
        }
    }

    fn parse_static_value(&mut self, depth: usize) -> Option<Value> {
        if depth > MAX_STATIC_LITERAL_DEPTH {
            return None;
        }
        self.skip_whitespace();
        match self.source.get(self.cursor).copied()? {
            b'"' => self.parse_json_string().map(Value::String),
            b'{' => self.parse_static_object(depth + 1).map(Value::Object),
            b'[' => self.parse_static_array(depth + 1).map(Value::Array),
            b't' if self.consume_keyword("true") => Some(Value::Bool(true)),
            b'f' if self.consume_keyword("false") => Some(Value::Bool(false)),
            b'n' if self.consume_keyword("null") => Some(Value::Null),
            b'-' | b'0'..=b'9' => self.parse_number(),
            _ => None,
        }
    }

    fn parse_static_object(&mut self, depth: usize) -> Option<Map<String, Value>> {
        self.consume_byte(b'{').then_some(())?;
        let mut object = Map::new();
        loop {
            self.skip_whitespace();
            if self.consume_byte(b'}') {
                return Some(object);
            }
            if object.len() >= MAX_STATIC_LITERAL_ITEMS {
                return None;
            }
            let key = if self.source.get(self.cursor) == Some(&b'"') {
                self.parse_json_string()?
            } else {
                self.parse_identifier()?
            };
            self.skip_whitespace();
            self.consume_byte(b':').then_some(())?;
            let value = self.parse_static_value(depth)?;
            if object.insert(key, value).is_some() {
                return None;
            }
            self.skip_whitespace();
            if self.consume_byte(b'}') {
                return Some(object);
            }
            self.consume_byte(b',').then_some(())?;
        }
    }

    fn parse_static_array(&mut self, depth: usize) -> Option<Vec<Value>> {
        self.consume_byte(b'[').then_some(())?;
        let mut values = Vec::new();
        loop {
            self.skip_whitespace();
            if self.consume_byte(b']') {
                return Some(values);
            }
            if values.len() >= MAX_STATIC_LITERAL_ITEMS {
                return None;
            }
            values.push(self.parse_static_value(depth)?);
            self.skip_whitespace();
            if self.consume_byte(b']') {
                return Some(values);
            }
            self.consume_byte(b',').then_some(())?;
        }
    }

    fn parse_json_string(&mut self) -> Option<String> {
        let start = self.cursor;
        self.consume_byte(b'"').then_some(())?;
        let mut escaped = false;
        while let Some(byte) = self.source.get(self.cursor).copied() {
            self.cursor += 1;
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                return serde_json::from_slice(self.source.get(start..self.cursor)?).ok();
            } else if byte.is_ascii_control() {
                return None;
            }
        }
        None
    }

    fn parse_number(&mut self) -> Option<Value> {
        let start = self.cursor;
        while self.source.get(self.cursor).is_some_and(|byte| {
            byte.is_ascii_digit() || matches!(byte, b'-' | b'+' | b'.' | b'e' | b'E')
        }) {
            self.cursor += 1;
        }
        serde_json::from_slice(self.source.get(start..self.cursor)?).ok()
    }

    fn parse_identifier(&mut self) -> Option<String> {
        let start = self.cursor;
        let first = self.source.get(self.cursor).copied()?;
        if !first.is_ascii_alphabetic() && !matches!(first, b'_' | b'$') {
            return None;
        }
        self.cursor += 1;
        while self
            .source
            .get(self.cursor)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$'))
        {
            self.cursor += 1;
        }
        std::str::from_utf8(self.source.get(start..self.cursor)?)
            .ok()
            .map(str::to_owned)
    }

    fn consume_keyword(&mut self, keyword: &str) -> bool {
        let start = self.cursor;
        if !self.consume_bytes(keyword.as_bytes()) {
            return false;
        }
        if self
            .source
            .get(self.cursor)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$'))
        {
            self.cursor = start;
            return false;
        }
        true
    }

    fn consume_bytes(&mut self, expected: &[u8]) -> bool {
        if self
            .source
            .get(self.cursor..self.cursor.saturating_add(expected.len()))
            == Some(expected)
        {
            self.cursor += expected.len();
            true
        } else {
            false
        }
    }

    fn consume_byte(&mut self, expected: u8) -> bool {
        if self.source.get(self.cursor) == Some(&expected) {
            self.cursor += 1;
            true
        } else {
            false
        }
    }

    fn skip_whitespace(&mut self) {
        while self
            .source
            .get(self.cursor)
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            self.cursor += 1;
        }
    }

    fn skip_program_trivia(&mut self) -> Option<()> {
        loop {
            self.skip_whitespace();
            if self.source.get(self.cursor..self.cursor.saturating_add(2)) == Some(b"//") {
                self.cursor += 2;
                while self
                    .source
                    .get(self.cursor)
                    .is_some_and(|byte| *byte != b'\n')
                {
                    self.cursor += 1;
                }
                continue;
            }
            if self.source.get(self.cursor..self.cursor.saturating_add(2)) == Some(b"/*") {
                self.cursor += 2;
                while self.source.get(self.cursor..self.cursor.saturating_add(2)) != Some(b"*/") {
                    self.cursor += 1;
                    if self.cursor >= self.source.len() {
                        return None;
                    }
                }
                self.cursor += 2;
                continue;
            }
            return Some(());
        }
    }
}
