use std::io::{self, Read};

use super::{tests::source, *};

struct CountingReader<R> {
    inner: R,
    bytes_read: usize,
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let bytes = self.inner.read(buffer)?;
        self.bytes_read += bytes;
        Ok(bytes)
    }
}

fn validate(input: impl Read) -> (Result<()>, usize, usize, usize) {
    let path = Path::new("counted-history.jsonl");
    let mut reader = BufReader::new(CountingReader {
        inner: input,
        bytes_read: 0,
    });
    let result = validate_header(&source(path.into()), path, &mut reader);
    let read = reader.get_ref().bytes_read;
    let consumed = read - reader.buffer().len();
    (result, consumed, read, reader.capacity())
}

fn manifest() -> Vec<u8> {
    format!(
        "{}\n",
        serde_json::json!({"record_type":"manifest","schema_version":PUBLIC_HISTORY_SCHEMA_VERSION})
    )
    .into_bytes()
}

fn identity(bytes: usize, newline: bool) -> Vec<u8> {
    let mut line = br#"{"record_type":"source","provider_key":"example","source_id":"default","source_format":"example-v1"}"#.to_vec();
    assert!(line.len() < bytes);
    line.resize(bytes, b' ');
    if newline {
        *line.last_mut().unwrap() = b'\n';
    }
    line
}

fn bounded_error(result: Result<()>) {
    let error = result.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("header exceeds the bounded validation window"),
        "{error:#}"
    );
}

#[test]
fn oversized_line_stops_after_one_sentinel_without_draining() {
    for input_bytes in [2 * MAX_HEADER_BYTES, 8 * MAX_HEADER_BYTES] {
        let input = io::repeat(b' ').take(input_bytes as u64);
        let (result, consumed, read, prefetch) = validate(input);
        bounded_error(result);
        println!(
            "oversized input={input_bytes} consumed={consumed} read={read} prefetch={prefetch}"
        );
        assert_eq!(consumed, MAX_HEADER_LINE_BYTES + 1);
        assert!(read <= MAX_HEADER_LINE_BYTES + 1 + prefetch);
    }
}

#[test]
fn cumulative_budget_limits_the_next_line_before_reading() {
    let mut prefix = Vec::new();
    for bytes in [
        MAX_HEADER_LINE_BYTES,
        MAX_HEADER_LINE_BYTES,
        MAX_HEADER_LINE_BYTES,
        128 * 1024,
    ] {
        prefix.extend(vec![b' '; bytes - 1]);
        prefix.push(b'\n');
    }
    let input = io::Cursor::new(prefix).chain(io::repeat(b' ').take(MAX_HEADER_BYTES as u64));
    let (result, consumed, read, prefetch) = validate(input);
    bounded_error(result);
    println!("cumulative consumed={consumed} read={read} prefetch={prefetch}");
    assert_eq!(consumed, MAX_HEADER_BYTES + 1);
    assert!(read <= MAX_HEADER_BYTES + 1 + prefetch);
}

#[test]
fn line_boundary_accepts_exact_bytes_and_rejects_one_more_with_or_without_newline() {
    for newline in [false, true] {
        for overflow in [false, true] {
            let mut input = manifest();
            input.extend(identity(
                MAX_HEADER_LINE_BYTES + usize::from(overflow),
                newline,
            ));
            let expected = input.len();
            let (result, consumed, read, _) = validate(input.as_slice());
            if overflow {
                bounded_error(result);
            } else {
                result.unwrap();
            }
            assert_eq!(consumed, expected);
            assert_eq!(read, expected);
        }
    }
}

#[test]
fn total_boundary_accepts_exact_bytes_and_rejects_one_more_with_or_without_newline() {
    for newline in [false, true] {
        for overflow in [false, true] {
            let mut input = manifest();
            for _ in 0..3 {
                input.extend(vec![b' '; MAX_HEADER_LINE_BYTES - 1]);
                input.push(b'\n');
            }
            let remaining = MAX_HEADER_BYTES - input.len();
            input.extend(identity(remaining + usize::from(overflow), newline));
            let (result, consumed, read, _) = validate(input.as_slice());
            if overflow {
                bounded_error(result);
            } else {
                result.unwrap();
            }
            assert_eq!(consumed, input.len());
            assert_eq!(read, input.len());
        }
    }
}

#[test]
fn eof_at_total_budget_is_missing_header_but_another_byte_is_over_budget() {
    let mut input = Vec::new();
    for _ in 0..4 {
        input.extend(vec![b' '; MAX_HEADER_LINE_BYTES - 1]);
        input.push(b'\n');
    }
    let (result, consumed, read, _) = validate(input.as_slice());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("must declare its manifest"));
    assert_eq!(consumed, MAX_HEADER_BYTES);
    assert_eq!(read, MAX_HEADER_BYTES);
    input.push(b' ');
    let (result, consumed, read, _) = validate(input.as_slice());
    bounded_error(result);
    assert_eq!(consumed, MAX_HEADER_BYTES + 1);
    assert_eq!(read, MAX_HEADER_BYTES + 1);
}

#[test]
fn empty_eof_reports_missing_manifest() {
    let (result, consumed, read, _) = validate(io::empty());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("must declare its manifest"));
    assert_eq!((consumed, read), (0, 0));
}

#[test]
fn valid_early_header_does_not_scan_large_body() {
    let mut input = manifest();
    input.extend(identity(256, true));
    let header_bytes = input.len();
    input.resize(header_bytes + 8 * MAX_HEADER_BYTES, b'x');
    let (result, consumed, read, prefetch) = validate(input.as_slice());
    result.unwrap();
    println!("valid early header consumed={consumed} read={read} prefetch={prefetch}");
    assert_eq!(consumed, header_bytes);
    assert!(read <= header_bytes + prefetch);
}
