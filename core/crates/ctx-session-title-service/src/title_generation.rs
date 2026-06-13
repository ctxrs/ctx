use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use reqwest::StatusCode;
use serde_json::json;
use tokio::process::Command;
use tokio::time::sleep;

use crate::llm::{
    ChatCompletionRequest, ChatMessage, JsonSchemaSpec, OpenAiClient, ResponseFormat,
};
use ctx_core::redaction;
use ctx_managed_installs::title_generation_local;
use ctx_settings_model::{TitleGenerationMode, TitleGenerationSettings};

pub const DEFAULT_SESSION_TITLE: &str = "New Task";
pub const TITLE_MAX_CHARS: usize = 60;

const LOCAL_START_TIMEOUT: Duration = Duration::from_secs(20);
const LOCAL_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const LOCAL_RETRY_COUNT: usize = 2;
const LOCAL_RETRY_BACKOFF_MS: u64 = 500;

pub fn is_configured(cfg: &TitleGenerationSettings) -> bool {
    match cfg.mode {
        TitleGenerationMode::Remote => is_remote_configured(cfg),
        TitleGenerationMode::Local => is_local_configured(cfg),
    }
}

pub fn is_remote_configured(cfg: &TitleGenerationSettings) -> bool {
    let remote = &cfg.remote;
    !remote.base_url.trim().is_empty()
        && !remote.api_key.trim().is_empty()
        && !remote.model.trim().is_empty()
}

pub fn is_local_configured(cfg: &TitleGenerationSettings) -> bool {
    !cfg.local.model_id.trim().is_empty()
}

fn collapse_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_chars(input: &str, max_len: usize) -> String {
    if max_len == 0 {
        return String::new();
    }
    input.chars().take(max_len).collect()
}

fn strip_wrapping_quotes(input: &str) -> &str {
    input
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim_matches('`')
}

fn strip_trailing_punct(input: &str) -> &str {
    const TRAILING_PUNCT: [char; 6] = ['.', ':', ';', '-', '\u{2013}', '\u{2014}'];
    input.trim_end_matches(|c: char| TRAILING_PUNCT.contains(&c))
}

pub fn normalize_title(raw: &str) -> String {
    let collapsed = collapse_whitespace(raw);
    let unquoted = strip_wrapping_quotes(&collapsed);
    let unpunct = strip_trailing_punct(unquoted).trim();
    truncate_chars(unpunct, TITLE_MAX_CHARS)
}

pub fn fallback_title_from_prompt(prompt: &str) -> String {
    let collapsed = collapse_whitespace(prompt);
    normalize_title(&collapsed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleGenerationSource {
    Llm,
    Fallback,
}

impl TitleGenerationSource {
    pub fn as_str(self) -> &'static str {
        match self {
            TitleGenerationSource::Llm => "llm",
            TitleGenerationSource::Fallback => "fallback",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleGenerationOutcome {
    pub title: String,
    pub source: TitleGenerationSource,
}

pub async fn generate_title_for_prompt(
    cfg: Option<&TitleGenerationSettings>,
    prompt: &str,
    data_root: &Path,
) -> Result<TitleGenerationOutcome> {
    let fallback = fallback_title_from_prompt(prompt);
    if fallback.trim().is_empty() {
        return Err(anyhow!("prompt is empty"));
    }

    if let Some(cfg) = cfg.filter(|c| is_configured(c)) {
        match generate_title(cfg, prompt, data_root).await {
            Ok(title) => {
                return Ok(TitleGenerationOutcome {
                    title,
                    source: TitleGenerationSource::Llm,
                });
            }
            Err(err) => {
                tracing::warn!(
                    "title generation failed: {}",
                    redaction::redact_sensitive(&err.to_string())
                );
            }
        }
    }

    Ok(TitleGenerationOutcome {
        title: fallback,
        source: TitleGenerationSource::Fallback,
    })
}

fn title_schema_json() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "title": { "type": "string" }
        },
        "required": ["title"],
        "additionalProperties": false
    })
}

fn structured_response_format() -> ResponseFormat {
    ResponseFormat::JsonSchema {
        json_schema: JsonSchemaSpec {
            name: "session_title".to_string(),
            schema: title_schema_json(),
            strict: true,
        },
    }
}

fn build_prompt_messages(prompt: &str) -> Vec<ChatMessage> {
    vec![
        ChatMessage {
            role: "system".to_string(),
            content: format!(
                "You generate short, information-dense session titles. Requirements: usually <= 3 words, <= {TITLE_MAX_CHARS} characters, no quotes, no trailing punctuation."
            ),
        },
        ChatMessage {
            role: "user".to_string(),
            content: format!(
                "Create a title for this session based on the first user message:\n\n{}",
                prompt.trim()
            ),
        },
    ]
}

fn build_request(model: &str, use_json: bool, prompt: &str) -> ChatCompletionRequest {
    ChatCompletionRequest {
        model: model.trim().to_string(),
        messages: build_prompt_messages(prompt),
        temperature: Some(0.2),
        max_tokens: Some(32),
        response_format: use_json.then(structured_response_format),
    }
}

fn parse_completion(content: &str, use_json: bool) -> Result<String> {
    let raw_title = if use_json {
        let value: serde_json::Value =
            serde_json::from_str(content).context("decoding json response")?;
        value
            .get("title")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing title field in json response"))?
            .to_string()
    } else {
        content.to_string()
    };

    let normalized = normalize_title(&raw_title);
    if normalized.is_empty() {
        return Err(anyhow!("normalized title is empty"));
    }
    Ok(normalized)
}

pub async fn generate_title(
    cfg: &TitleGenerationSettings,
    prompt: &str,
    data_root: &Path,
) -> Result<String> {
    match cfg.mode {
        TitleGenerationMode::Remote => generate_remote_title(cfg, prompt).await,
        TitleGenerationMode::Local => generate_local_title(cfg, prompt, data_root).await,
    }
}

async fn generate_remote_title(cfg: &TitleGenerationSettings, prompt: &str) -> Result<String> {
    let client = OpenAiClient::new(
        cfg.remote.base_url.trim().to_string(),
        cfg.remote.api_key.trim().to_string(),
    )
    .context("creating remote title generation client")?;
    let req = build_request(&cfg.remote.model, cfg.remote.use_json, prompt);

    let resp = client
        .chat_completion(&req)
        .await
        .context("chat completion request failed")?;

    let content = resp
        .choices
        .first()
        .and_then(|c| c.message.content.as_deref())
        .unwrap_or("")
        .trim();

    if content.is_empty() {
        return Err(anyhow!("empty completion response"));
    }

    parse_completion(content, cfg.remote.use_json)
}

async fn generate_local_title(
    cfg: &TitleGenerationSettings,
    prompt: &str,
    data_root: &Path,
) -> Result<String> {
    let model_path = title_generation_local::resolve_model_path(data_root, &cfg.local.model_id)?;
    let runtime = title_generation_local::resolve_runtime_binary(data_root)?;

    let mut last_err = None;
    for attempt in 1..=LOCAL_RETRY_COUNT {
        let attempt_res = generate_local_once(&runtime, &model_path, cfg, prompt, data_root).await;
        match attempt_res {
            Ok(title) => return Ok(title),
            Err(err) => {
                last_err = Some(err);
                if attempt < LOCAL_RETRY_COUNT {
                    sleep(Duration::from_millis(LOCAL_RETRY_BACKOFF_MS)).await;
                }
            }
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow!("local title generation failed")))
}

async fn generate_local_once(
    runtime: &Path,
    model_path: &Path,
    cfg: &TitleGenerationSettings,
    prompt: &str,
    data_root: &Path,
) -> Result<String> {
    let mut server = spawn_llama_server(runtime, model_path, cfg.local.use_json, data_root).await?;
    let base_url = format!("{}/v1", server.base_url);

    let client = OpenAiClient::new_with_timeout(base_url, String::new(), LOCAL_REQUEST_TIMEOUT)
        .context("creating local title generation client")?;
    let req = build_request(&cfg.local.model_id, cfg.local.use_json, prompt);
    let result = async {
        let resp = client
            .chat_completion(&req)
            .await
            .context("chat completion request failed")?;

        let content = resp
            .choices
            .first()
            .and_then(|c| c.message.content.as_deref())
            .unwrap_or("")
            .trim();

        if content.is_empty() {
            return Err(anyhow!("empty completion response"));
        }

        parse_completion(content, cfg.local.use_json)
    }
    .await;

    server.shutdown().await.ok();
    result
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SchemaSupport {
    None,
    JsonSchemaFile,
    JsonSchemaInline,
    GrammarFile,
    GrammarInline,
}

static SCHEMA_SUPPORT: OnceLock<SchemaSupport> = OnceLock::new();

async fn detect_schema_support(runtime: &Path) -> SchemaSupport {
    if let Some(cached) = SCHEMA_SUPPORT.get() {
        return *cached;
    }

    let output = Command::new(runtime).arg("--help").output().await;

    let support = if let Ok(output) = output {
        let mut text = String::new();
        text.push_str(&String::from_utf8_lossy(&output.stdout));
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        let lower = text.to_ascii_lowercase();
        if lower.contains("--json-schema-file") {
            SchemaSupport::JsonSchemaFile
        } else if lower.contains("--json-schema") {
            SchemaSupport::JsonSchemaInline
        } else if lower.contains("--grammar-file") {
            SchemaSupport::GrammarFile
        } else if lower.contains("--grammar") {
            SchemaSupport::GrammarInline
        } else {
            SchemaSupport::None
        }
    } else {
        SchemaSupport::None
    };

    let _ = SCHEMA_SUPPORT.set(support);
    support
}

struct LlamaServerHandle {
    child: tokio::process::Child,
    base_url: String,
}

impl LlamaServerHandle {
    async fn shutdown(&mut self) -> Result<()> {
        if let Ok(Some(_status)) = self.child.try_wait() {
            let _ = self.child.wait().await;
            return Ok(());
        }
        self.child.kill().await.ok();
        let _ = self.child.wait().await;
        Ok(())
    }
}

async fn spawn_llama_server(
    runtime: &Path,
    model_path: &Path,
    use_json: bool,
    data_root: &Path,
) -> Result<LlamaServerHandle> {
    let port = pick_free_port();
    if port == 0 {
        return Err(anyhow!("failed to find free port"));
    }

    let schema_support = if use_json {
        detect_schema_support(runtime).await
    } else {
        SchemaSupport::None
    };

    let mut cmd = Command::new(runtime);
    cmd.arg("--model")
        .arg(model_path)
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());

    match schema_support {
        SchemaSupport::JsonSchemaFile => {
            let path = ensure_schema_file(data_root).await?;
            cmd.arg("--json-schema-file").arg(path);
        }
        SchemaSupport::JsonSchemaInline => {
            let schema = serde_json::to_string(&title_schema_json())?;
            cmd.arg("--json-schema").arg(schema);
        }
        SchemaSupport::GrammarFile => {
            let path = ensure_grammar_file(data_root).await?;
            cmd.arg("--grammar-file").arg(path);
        }
        SchemaSupport::GrammarInline => {
            cmd.arg("--grammar").arg(TITLE_GRAMMAR);
        }
        SchemaSupport::None => {}
    }

    let mut child = cmd.spawn().context("spawning llama-server")?;
    let base_url = format!("http://127.0.0.1:{port}");

    if let Err(err) = wait_for_llama_ready(&base_url, &mut child).await {
        let _ = child.kill().await;
        let _ = child.wait().await;
        return Err(err);
    }

    Ok(LlamaServerHandle { child, base_url })
}

async fn wait_for_llama_ready(base_url: &str, child: &mut tokio::process::Child) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .context("building readiness client")?;
    let health_url = format!("{base_url}/health");
    let models_url = format!("{base_url}/v1/models");

    let start = std::time::Instant::now();
    loop {
        if start.elapsed() > LOCAL_START_TIMEOUT {
            return Err(anyhow!("llama-server did not become ready in time"));
        }

        if let Some(status) = child.try_wait().context("checking llama-server")? {
            let stderr = read_child_stderr(child).await.unwrap_or_default();
            return Err(anyhow!(
                "llama-server exited before ready ({status}): {stderr}"
            ));
        }

        if let Ok(resp) = client.get(&health_url).send().await {
            if resp.status().is_success() || resp.status() == StatusCode::NOT_FOUND {
                return Ok(());
            }
        }
        if let Ok(resp) = client.get(&models_url).send().await {
            if resp.status().is_success() || resp.status() == StatusCode::NOT_FOUND {
                return Ok(());
            }
        }

        sleep(Duration::from_millis(250)).await;
    }
}

async fn read_child_stderr(child: &mut tokio::process::Child) -> Result<String> {
    use tokio::io::AsyncReadExt;

    let mut out = String::new();
    let Some(mut stderr) = child.stderr.take() else {
        return Ok(out);
    };
    let mut buf = Vec::new();
    stderr.read_to_end(&mut buf).await.ok();
    out.push_str(&String::from_utf8_lossy(&buf));
    Ok(out.trim().to_string())
}

fn pick_free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|listener| listener.local_addr().ok().map(|addr| addr.port()))
        .unwrap_or(0)
}

async fn ensure_schema_file(data_root: &Path) -> Result<PathBuf> {
    let path = title_generation_local::model_dir(data_root).join("title_generation.schema.json");
    if path.exists() {
        return Ok(path);
    }
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }
    let bytes = serde_json::to_vec_pretty(&title_schema_json())?;
    tokio::fs::write(&path, bytes)
        .await
        .with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

const TITLE_GRAMMAR: &str = r#"root ::= object
object ::= "{" ws "\"title\"" ws ":" ws string ws "}"
string ::= "\"" chars "\""
chars ::= char chars | ""
char ::= [^"\\] | "\\" escape
escape ::= ["\\/bfnrt] | "u" hex hex hex hex
hex ::= [0-9a-fA-F]
ws ::= [ \t\n\r]*"#;

async fn ensure_grammar_file(data_root: &Path) -> Result<PathBuf> {
    let path = title_generation_local::model_dir(data_root).join("title_generation.grammar.gbnf");
    if path.exists() {
        return Ok(path);
    }
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }
    tokio::fs::write(&path, TITLE_GRAMMAR)
        .await
        .with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_title_collapses_and_trims_prompt() {
        assert_eq!(
            fallback_title_from_prompt("  Build   a better\nsession title.  "),
            "Build a better session title"
        );
    }

    #[test]
    fn normalize_title_strips_quotes_punctuation_and_truncates() {
        let raw = format!("\"{}:\"", "a".repeat(TITLE_MAX_CHARS + 8));
        assert_eq!(normalize_title(&raw), "a".repeat(TITLE_MAX_CHARS));
    }

    #[test]
    fn parse_completion_accepts_json_schema_title() {
        let title = match parse_completion(r#"{"title":"\"Focused refactor:\""}"#, true) {
            Ok(title) => title,
            Err(error) => panic!("json title should parse: {error}"),
        };

        assert_eq!(title, "Focused refactor");
    }

    #[tokio::test]
    async fn generate_title_for_prompt_falls_back_when_local_runtime_missing() {
        let data_dir = tempfile::tempdir().expect("tempdir");
        let model_path = title_generation_local::model_path(data_dir.path());
        if let Some(parent) = model_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .expect("model parent");
        }
        tokio::fs::write(&model_path, b"stub")
            .await
            .expect("model stub");

        let cfg = TitleGenerationSettings {
            mode: TitleGenerationMode::Local,
            local: ctx_settings_model::TitleGenerationLocalSettings {
                model_id: title_generation_local::LOCAL_MODEL_ID.to_string(),
                use_json: true,
            },
            ..Default::default()
        };

        let prompt = "make the title this: hello world";
        let outcome = generate_title_for_prompt(Some(&cfg), prompt, data_dir.path())
            .await
            .expect("fallback outcome");

        assert_eq!(outcome.source, TitleGenerationSource::Fallback);
        assert_eq!(outcome.title, fallback_title_from_prompt(prompt));
    }
}
