//! Legacy Gemini whole-document cursor. Retains one bounded value at a time.
use crate::{source, Error, Result};
use serde_json::{json, Value};
use std::{collections::HashSet, io::BufRead, path::Path};

struct Cursor {
    reader: Box<dyn BufRead>,
    read: u64,
    limit: usize,
}
impl Cursor {
    fn peek(&mut self) -> Result<Option<u8>> {
        Ok(self.reader.fill_buf()?.first().copied())
    }
    fn byte(&mut self) -> Result<u8> {
        let b = self
            .peek()?
            .ok_or_else(|| Error::Invalid("incomplete Gemini JSON snapshot".into()))?;
        self.reader.consume(1);
        self.read += 1;
        Ok(b)
    }
    fn space(&mut self) -> Result<()> {
        while self.peek()?.is_some_and(|b| b.is_ascii_whitespace()) {
            self.byte()?;
        }
        Ok(())
    }
    fn expect(&mut self, b: u8) -> Result<()> {
        self.space()?;
        if self.byte()? != b {
            return Err(Error::Invalid("invalid Gemini JSON framing".into()));
        }
        Ok(())
    }
    fn value(&mut self) -> Result<Option<Value>> {
        self.space()?;
        let mut bytes = Vec::new();
        let mut depth = 0usize;
        let mut string = false;
        let mut escape = false;
        let mut oversized = false;
        loop {
            let Some(b) = self.peek()? else { break };
            if !string
                && depth == 0
                && (b == b':' || b == b',' || b == b'}' || b == b']' || b.is_ascii_whitespace())
            {
                break;
            }
            self.byte()?;
            if bytes.len() < self.limit {
                bytes.push(b);
            } else {
                oversized = true;
            }
            if string {
                if escape {
                    escape = false;
                } else if b == b'\\' {
                    escape = true;
                } else if b == b'"' {
                    string = false;
                }
            } else {
                match b {
                    b'"' => string = true,
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => depth = depth.saturating_sub(1),
                    _ => {}
                }
            }
        }
        if string || depth != 0 {
            return Err(Error::Invalid("incomplete Gemini JSON value".into()));
        }
        if oversized {
            return Ok(None);
        }
        let text = std::str::from_utf8(&bytes).ok();
        text.and_then(ctx_history_asr_core::numbers::decode)
            .map(Some)
            .ok_or_else(|| Error::Invalid("invalid Gemini JSON value".into()))
    }
}

pub(super) fn visit(
    path: &Path,
    limit: usize,
    visitor: &mut dyn FnMut(Value) -> Result<()>,
) -> Result<(Value, u64)> {
    let mut c = Cursor {
        reader: source::reader(path)?,
        read: 0,
        limit,
    };
    c.expect(b'{')?;
    let mut header = json!({});
    let mut keys = HashSet::new();
    let mut header_bytes = 0;
    c.space()?;
    if c.peek()? == Some(b'}') {
        c.byte()?;
        return Ok((header, c.read));
    }
    loop {
        let key = c
            .value()?
            .and_then(|v| v.as_str().map(str::to_owned))
            .ok_or_else(|| Error::Invalid("invalid Gemini metadata key".into()))?;
        if !keys.insert(key.clone()) {
            return Err(Error::Invalid("duplicate Gemini metadata key".into()));
        }
        c.expect(b':')?;
        if key == "messages" {
            c.expect(b'[')?;
            c.space()?;
            if c.peek()? != Some(b']') {
                loop {
                    visitor(
                        c.value()?
                            .unwrap_or_else(|| json!({"type":"_gap","reason":"conversion_loss"})),
                    )?;
                    c.space()?;
                    match c.peek()? {
                        Some(b',') => {
                            c.byte()?;
                        }
                        Some(b']') => break,
                        _ => return Err(Error::Invalid("invalid Gemini messages array".into())),
                    }
                }
            }
            c.expect(b']')?;
        } else {
            let v = c.value()?.ok_or_else(|| {
                Error::Invalid("Gemini snapshot metadata exceeds record bound".into())
            })?;
            header_bytes += key.len() + serde_json::to_vec(&v)?.len();
            if header_bytes > limit {
                return Err(Error::Invalid(
                    "Gemini snapshot metadata exceeds record bound".into(),
                ));
            }
            header[key] = v;
        }
        c.space()?;
        match c.byte()? {
            b',' => {}
            b'}' => break,
            _ => return Err(Error::Invalid("invalid Gemini snapshot object".into())),
        }
    }
    c.space()?;
    if c.peek()?.is_some() {
        return Err(Error::Invalid("trailing Gemini snapshot data".into()));
    }
    Ok((header, c.read))
}
