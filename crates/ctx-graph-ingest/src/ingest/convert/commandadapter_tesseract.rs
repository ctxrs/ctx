use super::*;

impl CommandAdapter {
    /// `tesseract INPUT OUTPUT_STEM`; requires Tesseract on PATH.
    pub fn tesseract() -> Self {
        Self {
            program: "tesseract".into(),
            args: vec!["{input}".into(), "{output_stem}".into()],
            output_file: true,
        }
    }
    /// OpenAI's open-source Whisper CLI accepts audio/video through FFmpeg.
    /// The user installs Whisper/FFmpeg and its model separately; Graf never installs them.
    pub fn whisper(model: &str) -> Self {
        Self {
            program: "whisper".into(),
            args: vec![
                "{input}".into(),
                "--model".into(),
                model.into(),
                "--output_format".into(),
                "txt".into(),
                "--output_dir".into(),
                "{output_dir}".into(),
            ],
            output_file: true,
        }
    }
    /// Whisper initial context is an explicit argument and participates in cache keys.
    pub fn whisper_with_prompt(model: &str, prompt: &str) -> Self {
        let mut adapter = Self::whisper(model);
        adapter
            .args
            .extend(["--initial_prompt".into(), prompt.into()]);
        adapter
    }
    /// Poppler text extraction; useful as an explicit override for native PDFs.
    pub fn pdftotext() -> Self {
        Self {
            program: "pdftotext".into(),
            args: vec!["-layout".into(), "{input}".into(), "-".into()],
            output_file: false,
        }
    }
    /// Pandoc DOCX/RST/HTML to Markdown. Native DOCX needs no executable.
    pub fn pandoc() -> Self {
        Self {
            program: "pandoc".into(),
            args: vec!["{input}".into(), "--to".into(), "markdown".into()],
            output_file: false,
        }
    }
    /// Explicit Google Workspace CLI export. Docs/slides: plain text;
    /// sheets: XLSX, read by the native workbook parser (all worksheets).
    pub fn google_workspace() -> Self {
        Self {
            program: "gws".into(),
            args: vec![
                "drive".into(),
                "files".into(),
                "export".into(),
                "--params".into(),
                "{google_params}".into(),
                "-o".into(),
                "{output}".into(),
            ],
            output_file: true,
        }
    }

    /// Explicit remote media download, used only by `extract_url` when installed
    /// under converters["url"]. Pair with a local transcription recipe.
    pub fn yt_dlp() -> Self {
        Self {
            program: "yt-dlp".into(),
            args: vec![
                "--no-playlist".into(),
                "--no-progress".into(),
                "--max-filesize".into(),
                "32M".into(),
                "-f".into(),
                "bestaudio".into(),
                "--output".into(),
                "{output}".into(),
                "--".into(),
                "{url}".into(),
            ],
            output_file: true,
        }
    }

    /// AWS CLI v2 Converse; credentials/profile stay in the CLI environment.
    pub fn bedrock() -> Self {
        Self {
            program: "aws".into(),
            args: vec![
                "bedrock-runtime".into(),
                "converse".into(),
                "--cli-input-json".into(),
                "file://{request_file}".into(),
                "--cli-binary-format".into(),
                "base64".into(),
                "--output".into(),
                "json".into(),
                "--no-cli-pager".into(),
            ],
            output_file: false,
        }
    }

    /// Claude Code print-only inference, with tool use and session persistence disabled.
    /// One text generation; native vision explicitly raises the turn limit and
    /// reserves every permitted turn before adding its per-file Read allowlist.
    /// Arbitrary custom executables remain responsible for honoring requested
    /// limits: Graf cannot bound model work hidden inside a user adapter.
    pub fn claude_cli() -> Self {
        Self {
            program: if cfg!(windows) {
                "claude.cmd"
            } else {
                "claude"
            }
            .into(),
            args: vec![
                "--print".into(),
                "--model".into(),
                "{model}".into(),
                "--output-format".into(),
                "json".into(),
                "--tools".into(),
                String::new(),
                "--no-session-persistence".into(),
                "--max-turns".into(),
                "1".into(),
                "--setting-sources".into(),
                String::new(),
                "--strict-mcp-config".into(),
                "--mcp-config".into(),
                "{\"mcpServers\":{}}".into(),
            ],
            output_file: false,
        }
    }
}
