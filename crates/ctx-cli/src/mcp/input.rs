use std::io::BufRead;

use anyhow::Result;

use super::MCP_MAX_LINE_BYTES;

pub(super) enum McpInputLine {
    Line(String),
    InvalidUtf8,
    TooLarge,
}

pub(super) fn read_mcp_input_line(reader: &mut impl BufRead) -> Result<Option<McpInputLine>> {
    let mut buffer = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if buffer.is_empty() {
                return Ok(None);
            }
            break;
        }
        if let Some(newline_index) = available.iter().position(|byte| *byte == b'\n') {
            let bytes_to_consume = newline_index + 1;
            if buffer.len().saturating_add(bytes_to_consume) > MCP_MAX_LINE_BYTES {
                reader.consume(bytes_to_consume);
                return Ok(Some(McpInputLine::TooLarge));
            }
            buffer.extend_from_slice(&available[..bytes_to_consume]);
            reader.consume(bytes_to_consume);
            break;
        }

        let bytes_to_consume = available.len();
        if buffer.len().saturating_add(bytes_to_consume) > MCP_MAX_LINE_BYTES {
            reader.consume(bytes_to_consume);
            discard_until_newline(reader)?;
            return Ok(Some(McpInputLine::TooLarge));
        }
        buffer.extend_from_slice(available);
        reader.consume(bytes_to_consume);
    }

    Ok(Some(match String::from_utf8(buffer) {
        Ok(line) => McpInputLine::Line(line),
        Err(_) => McpInputLine::InvalidUtf8,
    }))
}

fn discard_until_newline(reader: &mut impl BufRead) -> Result<()> {
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(());
        }
        let bytes_to_consume = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|index| index + 1)
            .unwrap_or(available.len());
        let found_newline = bytes_to_consume <= available.len()
            && available
                .get(bytes_to_consume.saturating_sub(1))
                .is_some_and(|byte| *byte == b'\n');
        reader.consume(bytes_to_consume);
        if found_newline {
            return Ok(());
        }
    }
}
