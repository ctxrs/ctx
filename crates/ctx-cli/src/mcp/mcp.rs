#[allow(unused_imports)]
use super::*;

pub(crate) const MCP_PROTOCOL_VERSION: &str = "2025-11-25";

pub(crate) const MCP_MAX_LINE_BYTES: usize = 1024 * 1024;

pub(crate) enum McpInputLine {
    Line(String),
    InvalidUtf8,
    TooLarge,
}

#[derive(Debug, Args)]
pub(crate) struct McpArgs {
    #[command(subcommand)]
    pub(crate) command: McpCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum McpCommand {
    #[command(
        about = "Serve a read-only MCP server over stdio",
        long_about = "Serve a read-only MCP server over newline-delimited stdio JSON-RPC.\n\nExample:\n  printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{},\"clientInfo\":{\"name\":\"client\",\"version\":\"0\"}}}' | ctx mcp serve"
    )]
    Serve(McpServeArgs),
}

#[derive(Debug, Args)]
pub(crate) struct McpServeArgs {}

pub(crate) fn run(args: McpArgs, data_root: PathBuf) -> Result<()> {
    match args.command {
        McpCommand::Serve(_) => serve_stdio(data_root),
    }
}

pub(crate) fn serve_stdio(data_root: PathBuf) -> Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut stdin = stdin.lock();
    let mut stdout = stdout.lock();
    let mut initialized = false;

    while let Some(input) = read_mcp_input_line(&mut stdin)? {
        let response = match input {
            McpInputLine::Line(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                handle_line(line, &data_root, &mut initialized)
            }
            McpInputLine::InvalidUtf8 => Some(error_response(
                Value::Null,
                -32700,
                "Parse error",
                Some(json!({ "error": "MCP message is not valid UTF-8" })),
            )),
            McpInputLine::TooLarge => Some(error_response(
                Value::Null,
                -32700,
                "Parse error",
                Some(json!({
                    "error": format!("MCP message exceeds max line bytes ({MCP_MAX_LINE_BYTES})")
                })),
            )),
        };
        if let Some(response) = response {
            writeln!(stdout, "{}", serde_json::to_string(&response)?)?;
            stdout.flush()?;
        }
    }
    Ok(())
}

pub(crate) fn read_mcp_input_line(reader: &mut impl BufRead) -> Result<Option<McpInputLine>> {
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

pub(crate) fn initialize_result() -> Value {
    json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": {
            "tools": {
                "listChanged": false
            }
        },
        "serverInfo": {
            "name": "ctx",
            "version": env!("CARGO_PKG_VERSION")
        },
        "instructions": "Read-only access to the local ctx index. Tool output is private local history and may include absolute paths, source metadata, snippets, transcript text, and raw SQL query results; MCP hosts may log or forward it. This minimal server supports initialize, ping, tools/list, and tools/call over newline-delimited stdio. It does not expose MCP resources or prompts, and tools do not import provider history, write provider files, or write repositories."
    })
}

pub(crate) fn provider_names() -> Vec<&'static str> {
    ProviderArg::mcp_names()
}
