//! Bounded, item-at-a-time reader for rewritten JSON documents.
use crate::{Error, Result};
use serde_json::{Map, Value};
use std::{
    collections::BTreeSet,
    fs::File,
    io::{BufRead, BufReader},
    path::Path,
};

type ItemVisitor<'a> = dyn FnMut(&str, u64, &[u8]) -> Result<()> + 'a;

struct Cursor {
    input: BufReader<File>,
    limit: usize,
}
impl Cursor {
    fn peek(&mut self) -> Result<Option<u8>> {
        Ok(self.input.fill_buf()?.first().copied())
    }
    fn take(&mut self) -> Result<Option<u8>> {
        let b = self.peek()?;
        if b.is_some() {
            self.input.consume(1);
        }
        Ok(b)
    }
    fn ws(&mut self) -> Result<()> {
        while self.peek()?.is_some_and(|b| b.is_ascii_whitespace()) {
            self.take()?;
        }
        Ok(())
    }
    fn expect(&mut self, b: u8) -> Result<()> {
        self.ws()?;
        if self.take()? != Some(b) {
            return Err(Error::Invalid(
                "invalid native JSON document framing".into(),
            ));
        }
        Ok(())
    }
    fn value(&mut self) -> Result<Vec<u8>> {
        self.ws()?;
        let first = self
            .peek()?
            .ok_or_else(|| Error::Invalid("incomplete native JSON document".into()))?;
        let mut bytes = Vec::new();
        let mut depth = 0usize;
        let mut string = false;
        let mut escaped = false;
        loop {
            let Some(b) = self.peek()? else {
                if depth > 0 || string {
                    return Err(Error::Invalid("incomplete native JSON value".into()));
                }
                break;
            };
            if !string
                && depth == 0
                && !bytes.is_empty()
                && (b.is_ascii_whitespace() || matches!(b, b',' | b']' | b'}'))
            {
                break;
            }
            self.take()?;
            if bytes.len() < self.limit {
                bytes.push(b);
            } else {
                return Err(Error::Invalid(
                    "native JSON item exceeds --max-record-bytes".into(),
                ));
            }
            if string {
                if escaped {
                    escaped = false;
                } else if b == b'\\' {
                    escaped = true;
                } else if b == b'"' {
                    string = false;
                    if depth == 0 {
                        break;
                    }
                }
            } else {
                match b {
                    b'"' => string = true,
                    b'{' | b'[' => {
                        depth += 1;
                        if depth > 128 {
                            return Err(Error::Invalid("native JSON nesting exceeds 128".into()));
                        }
                    }
                    b'}' | b']' => {
                        depth = depth
                            .checked_sub(1)
                            .ok_or_else(|| Error::Invalid("unbalanced native JSON".into()))?;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }
        if first == b'"' && string {
            return Err(Error::Invalid("unterminated native JSON string".into()));
        }
        Ok(bytes)
    }
}

pub(super) fn read(
    path: &Path,
    arrays: &[&str],
    limit: usize,
    item: &mut ItemVisitor<'_>,
) -> Result<Value> {
    let mut c = Cursor {
        input: BufReader::new(File::open(path)?),
        limit,
    };
    let fields = object(&mut c, arrays, "", item)?;
    c.ws()?;
    if c.peek()?.is_some() {
        return Err(Error::Invalid("trailing native document data".into()));
    }
    Ok(fields)
}
fn object(
    c: &mut Cursor,
    arrays: &[&str],
    prefix: &str,
    item: &mut ItemVisitor<'_>,
) -> Result<Value> {
    c.expect(b'{')?;
    let mut fields = Map::new();
    let mut seen = BTreeSet::new();
    let mut metadata_bytes = 0;
    c.ws()?;
    if c.peek()? == Some(b'}') {
        c.take()?;
        return Ok(Value::Object(fields));
    }
    loop {
        let key: String = serde_json::from_slice(&c.value()?)?;
        if !seen.insert(key.clone()) {
            return Err(Error::Invalid("duplicate native document field".into()));
        }
        c.expect(b':')?;
        let full = format!("{prefix}{key}");
        if arrays.contains(&full.as_str()) {
            c.expect(b'[')?;
            c.ws()?;
            let mut index = 0;
            if c.peek()? != Some(b']') {
                loop {
                    let raw = c.value()?;
                    item(&key, index, &raw)?;
                    index += 1;
                    c.ws()?;
                    if c.peek()? == Some(b']') {
                        break;
                    }
                    c.expect(b',')?;
                }
            }
            c.expect(b']')?;
            fields.insert(format!("_ctx_count_{key}"), Value::from(index));
        } else if arrays
            .iter()
            .any(|path| path.starts_with(&format!("{full}/")))
        {
            let nested = object(c, arrays, &format!("{full}/"), item)?;
            fields.insert(key, nested);
        } else {
            let raw = c.value()?;
            metadata_bytes += raw.len();
            if metadata_bytes > c.limit {
                return Err(Error::Invalid(
                    "native document metadata exceeds --max-record-bytes".into(),
                ));
            }
            let v = decode(&raw)?;
            fields.insert(key, v);
        }
        c.ws()?;
        if c.peek()? == Some(b'}') {
            c.take()?;
            break;
        }
        c.expect(b',')?;
    }
    Ok(Value::Object(fields))
}

pub(super) fn decode(raw: &[u8]) -> Result<Value> {
    let text =
        std::str::from_utf8(raw).map_err(|_| Error::Invalid("native JSON is not UTF-8".into()))?;
    ctx_history_asr_core::numbers::decode(text)
        .ok_or_else(|| Error::Invalid("invalid native JSON".into()))
}
pub(super) fn whole(path: &Path, limit: usize) -> Result<Value> {
    use std::io::Read;
    let mut bytes = Vec::new();
    File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(Error::Invalid(
            "native JSON item exceeds --max-record-bytes".into(),
        ));
    }
    decode(&bytes)
}
