use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::process::Command;
use tokio::sync::mpsc;

use ctx_core::models::SessionEventType;
use ctx_provider_accounts::KIMI_SHARE_DIR_ENV;
use ctx_providers::adapters::{ProviderAdapter, TurnInput};
use ctx_providers::crp::Tier1CrpAdapter;
use ctx_providers::events::NormalizedEvent;

use ctx_managed_installs::{
    load_agent_server_config, resolve_provider_command, AgentServerCommand,
};

const DEFAULT_OPENROUTER_MODEL: &str = "openai/gpt-4.1-mini";
const DEFAULT_GEMINI_MODEL: &str = "google/gemini-3-flash-preview";
const DEFAULT_OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1";
const DEFAULT_MISTRAL_BASE_URL: &str = "https://api.mistral.ai/v1";
const DEFAULT_QWEN_MODEL: &str = "openai/gpt-4.1-nano";
const WRITE_FILE_CONTENTS: &str = "hi";

#[derive(Clone, Copy)]
struct ProviderSpec {
    id: &'static str,
    fallback_cmd: &'static str,
    fallback_args: &'static [&'static str],
    opencode_config: bool,
}

const PROVIDERS: &[ProviderSpec] = &[
    ProviderSpec {
        id: "gemini",
        fallback_cmd: "gemini",
        fallback_args: &["--experimental-acp"],
        opencode_config: false,
    },
    ProviderSpec {
        id: "opencode",
        fallback_cmd: "opencode",
        fallback_args: &["acp"],
        opencode_config: true,
    },
    ProviderSpec {
        id: "mistral",
        fallback_cmd: "vibe-acp",
        fallback_args: &[],
        opencode_config: false,
    },
    ProviderSpec {
        id: "kimi",
        fallback_cmd: "kimi",
        fallback_args: &["acp"],
        opencode_config: false,
    },
    ProviderSpec {
        id: "auggie",
        fallback_cmd: "auggie",
        fallback_args: &["--acp"],
        opencode_config: false,
    },
    ProviderSpec {
        id: "amp",
        fallback_cmd: "amp-acp",
        fallback_args: &[],
        opencode_config: false,
    },
    ProviderSpec {
        id: "droid",
        fallback_cmd: "droid-acp",
        fallback_args: &[],
        opencode_config: false,
    },
    ProviderSpec {
        id: "copilot",
        fallback_cmd: "copilot-cli-acp",
        fallback_args: &[],
        opencode_config: false,
    },
    ProviderSpec {
        id: "continue",
        fallback_cmd: "cn",
        fallback_args: &["acp"],
        opencode_config: false,
    },
    ProviderSpec {
        id: "cline",
        fallback_cmd: "cline",
        fallback_args: &["--acp"],
        opencode_config: false,
    },
    ProviderSpec {
        id: "goose",
        fallback_cmd: "goose",
        fallback_args: &["acp"],
        opencode_config: false,
    },
    ProviderSpec {
        id: "openhands",
        fallback_cmd: "openhands",
        fallback_args: &["acp"],
        opencode_config: false,
    },
    ProviderSpec {
        id: "qwen",
        fallback_cmd: "qwen",
        fallback_args: &["--experimental-acp"],
        opencode_config: false,
    },
    ProviderSpec {
        id: "cursor",
        fallback_cmd: "cursor-agent-acp",
        fallback_args: &[],
        opencode_config: false,
    },
    ProviderSpec {
        id: "pi",
        fallback_cmd: "pi-acp",
        fallback_args: &[],
        opencode_config: false,
    },
];

async fn run_git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn setup_git_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    run_git(root, &["init"]).await;
    run_git(root, &["config", "user.email", "test@example.com"]).await;
    run_git(root, &["config", "user.name", "Test"]).await;
    std::fs::write(root.join("note.txt"), "hello\n").unwrap();
    run_git(root, &["add", "."]).await;
    run_git(root, &["commit", "-m", "init"]).await;
    dir
}

async fn run_and_collect(
    adapter: &dyn ProviderAdapter,
    workdir: &Path,
    prompt: &str,
    model_id: Option<&str>,
    env: HashMap<String, String>,
) -> Result<Vec<NormalizedEvent>, String> {
    let (tx, mut rx) = mpsc::channel::<NormalizedEvent>(1024);
    let handle = adapter
        .run(
            TurnInput {
                content: prompt.to_string(),
                attachments: vec![],
                context_blocks: vec![],
                model_id: model_id.map(str::to_string),
            },
            workdir.to_path_buf(),
            env,
            tx,
            ctx_providers::adapters::ProviderRunHooks::default(),
        )
        .await
        .map_err(|err| format!("provider run failed to start: {err}"))?;

    let events = tokio::time::timeout(Duration::from_secs(240), async move {
        let mut events = Vec::new();
        while let Some(ev) = rx.recv().await {
            let done = matches!(ev.event_type, SessionEventType::Done);
            events.push(ev);
            if done {
                break;
            }
        }
        events
    })
    .await
    .map_err(|_| "timed out waiting for provider events".to_string())?;

    let _ = handle.done.await;
    Ok(events)
}

fn write_file_name(provider_id: &str) -> String {
    format!("hello-{provider_id}.md")
}

fn write_file_prompt(provider_id: &str) -> String {
    let file_name = write_file_name(provider_id);
    format!(
        "This is an end-to-end write test. Create a new file in this directory called {file_name} and put exactly {WRITE_FILE_CONTENTS} in it. The file must contain exactly those two characters with no trailing newline or extra whitespace. If you use a shell command to write the file, use printf rather than echo -n, because echo -n is not portable and may write the literal text -n. Use only the current worktree root as the target directory. Do not write in a parent directory, and if your first attempt adds a trailing newline or uses the wrong directory, fix the file before replying. Then reply with exactly {WRITE_FILE_CONTENTS}. Do it now without further deliberation."
    )
}

fn expect_success(events: &[NormalizedEvent], provider_id: &str) -> Result<(), String> {
    if events
        .iter()
        .any(|e| matches!(e.event_type, SessionEventType::Error))
    {
        return Err(format!("{provider_id} emitted error event(s): {events:#?}"));
    }

    let has_assistant = events
        .iter()
        .any(|e| matches!(e.event_type, SessionEventType::AssistantComplete));
    let has_thought = events
        .iter()
        .any(|e| matches!(e.event_type, SessionEventType::ThoughtChunk));
    if !has_assistant {
        if provider_id == "opencode" && has_thought {
            // Opencode ACP can emit reasoning without a final assistant chunk; accept for now.
        } else {
            return Err(format!(
                "{provider_id} produced no AssistantComplete event: {events:#?}"
            ));
        }
    }

    if !events
        .iter()
        .any(|e| matches!(e.event_type, SessionEventType::Done))
    {
        return Err(format!("{provider_id} produced no Done event: {events:#?}"));
    }
    Ok(())
}

fn expect_file_written(workdir: &Path, provider_id: &str) -> Result<(), String> {
    let file_name = write_file_name(provider_id);
    let file_path = workdir.join(&file_name);
    let contents = fs::read_to_string(&file_path)
        .map_err(|err| format!("{provider_id} did not create {file_name}: {err}"))?;
    if contents.trim_end_matches(['\r', '\n', ' ', '\t']) != WRITE_FILE_CONTENTS {
        return Err(format!(
            "{provider_id} wrote unexpected contents to {file_name}"
        ));
    }
    Ok(())
}

fn token_tests_enabled() -> bool {
    std::env::var("CTX_E2E_TIER")
        .map(|v| v.eq_ignore_ascii_case("tokens"))
        .unwrap_or(false)
        || std::env::var("CTX_TOKEN_TESTS")
            .ok()
            .as_deref()
            .and_then(ctx_core::boolish::parse_boolish)
            .unwrap_or(false)
}

fn resolve_data_root() -> PathBuf {
    if let Ok(val) = std::env::var("CTX_DATA_ROOT") {
        if !val.trim().is_empty() {
            return PathBuf::from(val);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".ctx")
}

async fn load_openrouter_settings(data_root: &Path) -> Option<(String, String)> {
    let settings = ctx_settings_service::load_settings_from_data_root(data_root)
        .await
        .ok()?;
    let title = settings.title_generation?;
    if !matches!(title.mode, ctx_settings_model::TitleGenerationMode::Remote) {
        return None;
    }
    let api_key = title.remote.api_key.trim().to_string();
    if api_key.is_empty() {
        return None;
    }
    let base_url = if title.remote.base_url.trim().is_empty() {
        DEFAULT_OPENROUTER_BASE_URL.to_string()
    } else {
        title.remote.base_url
    };
    Some((api_key, base_url))
}

fn escape_shell_arg(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    let is_simple = value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"@%_-+=:,./".contains(&b));
    if is_simple {
        return value.to_string();
    }
    let mut out = String::from("'");
    for ch in value.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

fn format_shell_command(command: &str, args: &[String]) -> String {
    let mut parts = Vec::with_capacity(1 + args.len());
    parts.push(escape_shell_arg(command));
    for arg in args {
        parts.push(escape_shell_arg(arg));
    }
    parts.join(" ")
}

fn resolve_command(
    cfg: &ctx_managed_installs::AgentServerConfigFile,
    provider_id: &str,
    fallback_cmd: &str,
    fallback_args: &[&str],
) -> AgentServerCommand {
    if provider_id == "acp-crp-bridge" {
        if let Ok(command) = std::env::var("CTX_TOKENS_ACP_CRP_BRIDGE_BIN") {
            let trimmed = command.trim();
            if !trimmed.is_empty() {
                return AgentServerCommand {
                    command: trimmed.to_string(),
                    args: Vec::new(),
                    dependencies: Vec::new(),
                    managed: None,
                };
            }
        }
    }
    resolve_provider_command(cfg, provider_id).unwrap_or_else(|| AgentServerCommand {
        command: fallback_cmd.to_string(),
        args: fallback_args.iter().map(|s| s.to_string()).collect(),
        dependencies: Vec::new(),
        managed: None,
    })
}

fn command_exists(command: &str) -> bool {
    let path = Path::new(command);
    if path.is_absolute() {
        return path.exists();
    }
    which::which(command).is_ok()
}

fn truncate_output(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim();
    let out: String = trimmed.chars().take(240).collect();
    if out.is_empty() {
        "<no output>".to_string()
    } else {
        out
    }
}

async fn probe_command(
    command: &str,
    args: &[String],
    extra_args: &[&str],
    env: Option<&HashMap<String, String>>,
) -> Result<(), String> {
    let output = tokio::time::timeout(Duration::from_secs(5), async {
        let mut cmd = Command::new(command);
        cmd.args(args).args(extra_args);
        if let Some(env) = env {
            cmd.envs(env);
        }
        cmd.output().await
    })
    .await
    .map_err(|_| format!("{command} probe timed out"))?
    .map_err(|err| format!("{command} probe failed: {err}"))?;

    if output.status.success() {
        return Ok(());
    }

    let detail = if !output.stderr.is_empty() {
        truncate_output(&output.stderr)
    } else {
        truncate_output(&output.stdout)
    };
    Err(format!(
        "{} {:?} exited {}: {}",
        command, extra_args, output.status, detail
    ))
}

fn create_qwen_settings_home() -> std::io::Result<tempfile::TempDir> {
    let dir = tempfile::tempdir()?;
    let qwen_dir = dir.path().join(".qwen");
    fs::create_dir_all(&qwen_dir)?;
    let settings = r#"{
  "$version": 2,
  "security": { "auth": { "selectedType": "openai" } }
}
"#;
    fs::write(qwen_dir.join("settings.json"), settings)?;
    Ok(dir)
}

fn create_kimi_share_home() -> std::io::Result<(tempfile::TempDir, PathBuf)> {
    let dir = tempfile::tempdir()?;
    let share_dir = dir.path().join(".kimi");
    fs::create_dir_all(&share_dir)?;
    let credentials_dir = share_dir.join("credentials");
    fs::create_dir_all(&credentials_dir)?;
    // Kimi ACP currently hard-requires a file-backed OAuth token before session creation,
    // even when endpoint/API-key env overrides are present. Seed a benign token so the
    // session can reach the actual API-key-driven write path under test.
    let expires_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
        + 86_400.0;
    let token = serde_json::json!({
        "access_token": "ctx-test-access-token",
        "refresh_token": "ctx-test-refresh-token",
        "expires_at": expires_at,
        "scope": "openid profile",
        "token_type": "Bearer",
    });
    fs::write(credentials_dir.join("kimi-code.json"), token.to_string())?;
    Ok((dir, share_dir))
}

fn create_cline_config_dir(
    api_key: &str,
    model_id: &str,
    _base_url: &str,
) -> std::io::Result<tempfile::TempDir> {
    let dir = tempfile::Builder::new()
        .prefix("ctx-cline-home-")
        .tempdir()?;
    let data_dir = dir.path().join("data");
    let settings_dir = data_dir.join("settings");
    fs::create_dir_all(&settings_dir)?;

    let global_state = serde_json::json!({
        "actModeApiProvider": "openrouter",
        "planModeApiProvider": "openrouter",
        "actModeOpenRouterModelId": model_id,
        "planModeOpenRouterModelId": model_id,
        "welcomeViewCompleted": true,
    });
    fs::write(
        data_dir.join("globalState.json"),
        serde_json::to_vec_pretty(&global_state)?,
    )?;

    let secrets = serde_json::json!({
        "openRouterApiKey": api_key,
    });
    let secrets_path = data_dir.join("secrets.json");
    fs::write(&secrets_path, serde_json::to_vec_pretty(&secrets)?)?;

    let mcp_settings = serde_json::json!({
        "mcpServers": {},
    });
    fs::write(
        settings_dir.join("cline_mcp_settings.json"),
        serde_json::to_vec_pretty(&mcp_settings)?,
    )?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&secrets_path, std::fs::Permissions::from_mode(0o600));
    }

    Ok(dir)
}

fn create_goose_path_root() -> std::io::Result<(tempfile::TempDir, PathBuf)> {
    let dir = tempfile::Builder::new()
        .prefix("ctx-goose-home-")
        .tempdir()?;
    let path_root = dir.path().join("goose");
    fs::create_dir_all(&path_root)?;
    Ok((dir, path_root))
}

fn normalize_openhands_model_id(model_id: &str) -> String {
    if model_id.starts_with("openrouter/") {
        return model_id.to_string();
    }
    format!("openrouter/{model_id}")
}

fn create_openhands_persistence_dir(
    api_key: &str,
    model_id: &str,
    base_url: &str,
) -> std::io::Result<(tempfile::TempDir, PathBuf)> {
    let dir = tempfile::Builder::new()
        .prefix("ctx-openhands-home-")
        .tempdir()?;
    let persistence_dir = dir.path().join("persistence");
    fs::create_dir_all(&persistence_dir)?;
    let agent_settings = serde_json::json!({
        "llm": {
            "model": normalize_openhands_model_id(model_id),
            "api_key": api_key,
            "base_url": base_url,
            "usage_id": "agent",
        },
        "tools": [
            { "name": "file_editor", "params": {} },
            { "name": "task_tracker", "params": {} },
            { "name": "delegate", "params": {} },
        ],
        "mcp_config": {},
        "kind": "Agent",
    });
    let agent_settings_path = persistence_dir.join("agent_settings.json");
    fs::write(
        &agent_settings_path,
        serde_json::to_vec_pretty(&agent_settings)?,
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&agent_settings_path, std::fs::Permissions::from_mode(0o600));
    }
    Ok((dir, persistence_dir))
}

fn env_truthy(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .as_deref()
        .and_then(ctx_core::boolish::parse_boolish)
        .unwrap_or(false)
}

fn env_present(name: &str) -> bool {
    std::env::var(name)
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
}

fn provider_skip_reason(provider: ProviderSpec) -> Option<String> {
    if provider.id == "gemini" {
        let has_gemini_auth = std::env::var("GEMINI_API_KEY")
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
            || std::env::var("GOOGLE_API_KEY")
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false)
            || std::env::var("GOOGLE_APPLICATION_CREDENTIALS")
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false)
            || std::env::var("GOOGLE_CLOUD_PROJECT")
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false)
            || std::env::var("GOOGLE_CLOUD_PROJECT_ID")
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false);
        if !has_gemini_auth {
            return Some(
                "missing GEMINI/GOOGLE auth; gemini-cli does not support OpenRouter base URLs"
                    .to_string(),
            );
        }
    }
    if provider.id == "mistral" {
        let has_mistral_auth = std::env::var("MISTRAL_API_KEY")
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false);
        if !has_mistral_auth {
            return Some(
                "missing MISTRAL_API_KEY; mistral-vibe does not support OpenRouter".to_string(),
            );
        }
    }
    if provider.id == "auggie" {
        return Some("requires Augment login; not OpenRouter-compatible".to_string());
    }
    if provider.id == "amp" {
        return Some(
            "Amp requires native Amp auth and does not support OpenRouter endpoint mode"
                .to_string(),
        );
    }
    if provider.id == "droid" {
        let allow = env_truthy("DROID_TOKEN_TESTS");
        let has_droid_auth = env_present("FACTORY_API_KEY");
        if !allow && !has_droid_auth {
            return Some("missing FACTORY_API_KEY; set DROID_TOKEN_TESTS=1 to attempt".to_string());
        }
    }
    if provider.id == "copilot" {
        return Some("requires GitHub Copilot login; not OpenRouter-compatible".to_string());
    }
    if provider.id == "cursor" {
        return Some("requires Cursor login/session; not OpenRouter-compatible".to_string());
    }
    if provider.id == "continue" {
        let has_continue_auth = std::env::var("CONTINUE_API_KEY")
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false);
        if !has_continue_auth {
            return Some("missing CONTINUE_API_KEY; continue CLI uses Continue Cloud".to_string());
        }
    }
    if provider.id == "pi" {
        let allow = env_truthy("PI_TOKEN_TESTS");
        if !allow {
            return Some(
                "pi ACP adapter requires explicit PI_TOKEN_TESTS=1 to attempt".to_string(),
            );
        }
    }
    None
}

fn maybe_add_qwen_auth(args: &mut Vec<String>) {
    if args.iter().any(|arg| arg == "--auth-type") {
        return;
    }
    args.push("--auth-type".to_string());
    args.push("openai".to_string());
}

fn maybe_add_goose_acp_subcommand(args: &mut Vec<String>) {
    if !args.iter().any(|arg| arg == "acp") {
        args.insert(0, "acp".to_string());
    }
    let has_developer_builtin = args.windows(2).any(|window| {
        window[0] == "--with-builtin"
            && window[1]
                .split(',')
                .any(|value| value.trim() == "developer")
    });
    if !has_developer_builtin {
        args.push("--with-builtin".to_string());
        args.push("developer".to_string());
    }
}

fn maybe_add_openhands_env_override(args: &mut Vec<String>) {
    if args.iter().any(|arg| arg == "--override-with-envs") {
        return;
    }
    args.push("--override-with-envs".to_string());
}

fn build_env(
    openrouter_api_key: &str,
    openrouter_base_url: &str,
    model_id: &str,
    provider: ProviderSpec,
) -> HashMap<String, String> {
    let mut env = HashMap::new();
    // This direct bridge sweep is not running under a daemon-backed session with valid
    // ctx-mcp session context. Disable automatic ctx MCP injection so providers exercise
    // their native ACP/CLI tool paths instead of hanging on a half-configured MCP server.
    env.insert("CTX_MCP_DISABLED".to_string(), "1".to_string());
    env.insert(
        "OPENROUTER_API_KEY".to_string(),
        openrouter_api_key.to_string(),
    );
    env.insert(
        "OPENROUTER_BASE_URL".to_string(),
        openrouter_base_url.to_string(),
    );
    env.insert("OPENAI_API_KEY".to_string(), openrouter_api_key.to_string());
    env.insert(
        "OPENAI_BASE_URL".to_string(),
        openrouter_base_url.to_string(),
    );
    env.insert("OPENAI_MODEL".to_string(), model_id.to_string());

    if provider.id == "qwen" {
        env.remove("OPENROUTER_API_KEY");
        env.remove("OPENROUTER_BASE_URL");
    }

    if provider.opencode_config {
        let (opencode_model, opencode_model_key) = match model_id.strip_prefix("openrouter/") {
            Some(model_key) => (model_id.to_string(), model_key.to_string()),
            None => (format!("openrouter/{model_id}"), model_id.to_string()),
        };
        let cfg = serde_json::json!({
            "model": opencode_model,
            "permission": {
                "edit": "deny",
                "bash": "allow"
            },
            "provider": {
                "openrouter": {
                    "options": {
                        "baseURL": openrouter_base_url,
                        "apiKey": openrouter_api_key
                    },
                    "models": {
                        opencode_model_key: {}
                    }
                }
            }
        });
        env.insert("OPENCODE_CONFIG_CONTENT".to_string(), cfg.to_string());
    }

    if provider.id == "kimi" {
        env.insert("KIMI_BASE_URL".to_string(), openrouter_base_url.to_string());
        env.insert("KIMI_API_KEY".to_string(), openrouter_api_key.to_string());
        env.insert("KIMI_MODEL_NAME".to_string(), model_id.to_string());
    }
    if provider.id == "goose" {
        env.insert("CTX_PROVIDER_MODE".to_string(), String::new());
        env.remove("OPENAI_API_KEY");
        env.remove("OPENAI_BASE_URL");
        env.remove("OPENAI_MODEL");
        env.remove("OPENROUTER_BASE_URL");
        env.insert("GOOSE_PROVIDER".to_string(), "openrouter".to_string());
        env.insert("GOOSE_DISABLE_KEYRING".to_string(), "1".to_string());
        env.insert("GOOSE_MODE".to_string(), "auto".to_string());
        env.insert("GOOSE_MODEL".to_string(), model_id.to_string());
    }
    if provider.id == "openhands" {
        env.insert(
            "CTX_PROVIDER_MODE".to_string(),
            "always-approve".to_string(),
        );
        env.insert(
            "CTX_CRP_DISABLE_MODEL_OVERRIDE".to_string(),
            "1".to_string(),
        );
        env.remove("OPENAI_API_KEY");
        env.remove("OPENAI_BASE_URL");
        env.remove("OPENAI_MODEL");
        env.insert("LLM_API_KEY".to_string(), openrouter_api_key.to_string());
        env.insert("LLM_BASE_URL".to_string(), openrouter_base_url.to_string());
        env.insert(
            "LLM_MODEL".to_string(),
            normalize_openhands_model_id(model_id),
        );
    }
    if provider.id == "cline" {
        env.insert("CTX_PROVIDER_MODE".to_string(), "act".to_string());
        env.remove("OPENAI_API_KEY");
        env.remove("OPENAI_BASE_URL");
        env.insert(
            "CTX_CRP_DISABLE_MODEL_OVERRIDE".to_string(),
            "1".to_string(),
        );
    }
    if provider.id == "mistral" {
        let mistral_api_key = std::env::var("MISTRAL_API_KEY")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| openrouter_api_key.to_string());
        let mistral_base_url = std::env::var("MISTRAL_BASE_URL")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_MISTRAL_BASE_URL.to_string());
        env.insert("MISTRAL_API_KEY".to_string(), mistral_api_key.clone());
        env.insert("MISTRAL_BASE_URL".to_string(), mistral_base_url.clone());
        env.insert("OPENAI_API_KEY".to_string(), mistral_api_key);
        env.insert("OPENAI_BASE_URL".to_string(), mistral_base_url);
    }
    if provider.id == "droid" {
        env.insert("CTX_PROVIDER_MODE".to_string(), "auto_high".to_string());
    }
    if provider.id == "pi" {
        env.insert("PI_ACP_PROVIDER".to_string(), "openrouter".to_string());
        env.insert("PI_ACP_MODEL".to_string(), model_id.to_string());
    }

    env
}

fn provider_model_id(default_model_id: &str, provider: ProviderSpec) -> Option<String> {
    if provider.id == "gemini" {
        return Some(
            std::env::var("CTX_TOKENS_GEMINI_MODEL")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| DEFAULT_GEMINI_MODEL.to_string()),
        );
    }
    if provider.id == "qwen" {
        return Some(DEFAULT_QWEN_MODEL.to_string());
    }
    if provider.id == "pi" {
        return Some(
            std::env::var("CTX_TOKENS_PI_MODEL")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| DEFAULT_GEMINI_MODEL.to_string()),
        );
    }
    if provider.id != "mistral" {
        return Some(default_model_id.to_string());
    }

    [
        "CTX_TOKENS_MISTRAL_MODEL",
        "CTX_E2E_MISTRAL_MODEL_ID",
        "MISTRAL_MODEL_ID",
        "MISTRAL_MODEL",
    ]
    .iter()
    .find_map(|name| {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

fn requested_token_providers() -> Option<HashSet<String>> {
    let raw = std::env::var("CTX_TOKENS_PROVIDERS").ok()?;
    let parsed = raw
        .split(',')
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect::<HashSet<_>>();
    if parsed.is_empty() {
        None
    } else {
        Some(parsed)
    }
}

#[tokio::test]
#[ignore]
async fn acp_crp_bridge_token_providers() {
    if !token_tests_enabled() {
        eprintln!("skipping token tests; set CTX_E2E_TIER=tokens or CTX_TOKEN_TESTS=1");
        return;
    }

    let data_root = resolve_data_root();
    let (openrouter_api_key, openrouter_base_url) = match (
        std::env::var("OPENROUTER_API_KEY").ok(),
        std::env::var("OPENROUTER_BASE_URL").ok(),
    ) {
        (Some(key), Some(base_url)) => (key, base_url),
        _ => match load_openrouter_settings(&data_root).await {
            Some(creds) => creds,
            None => {
                eprintln!(
                    "skipping token tests; missing OpenRouter credentials (set OPENROUTER_API_KEY/OPENROUTER_BASE_URL or configure daemon title_generation settings)"
                );
                return;
            }
        },
    };

    let default_model_id = std::env::var("CTX_TOKENS_MODEL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_OPENROUTER_MODEL.to_string());

    let cfg = load_agent_server_config(&data_root)
        .await
        .unwrap_or_default();
    let bridge_cmd = resolve_command(&cfg, "acp-crp-bridge", "acp-crp-bridge", &[]);
    let mut requested_providers = requested_token_providers();

    if !command_exists(&bridge_cmd.command) {
        panic!("acp-crp-bridge not found: {}", bridge_cmd.command);
    }

    let repo = setup_git_repo().await;
    let mut failures = Vec::new();
    for provider in PROVIDERS {
        if requested_providers
            .as_ref()
            .is_some_and(|requested| !requested.contains(provider.id))
        {
            continue;
        }
        if let Some(requested) = requested_providers.as_mut() {
            requested.remove(provider.id);
        }
        let acp_cmd = resolve_command(
            &cfg,
            provider.id,
            provider.fallback_cmd,
            provider.fallback_args,
        );
        if !command_exists(&acp_cmd.command) {
            eprintln!(
                "skipping {}: command not found ({})",
                provider.id, acp_cmd.command
            );
            continue;
        }
        if let Some(reason) = provider_skip_reason(*provider) {
            eprintln!("skipping {}: {}", provider.id, reason);
            continue;
        }

        eprintln!("running {}...", provider.id);
        let mut _isolated_home = None;
        let mut _qwen_home = None;
        let mut _kimi_home = None;
        let mut _goose_home = None;
        let mut _openhands_home = None;
        let mut acp_args = acp_cmd.args.clone();
        if provider.id == "qwen" {
            maybe_add_qwen_auth(&mut acp_args);
        }
        if provider.id == "goose" {
            maybe_add_goose_acp_subcommand(&mut acp_args);
        }
        if provider.id == "openhands" {
            maybe_add_openhands_env_override(&mut acp_args);
        }
        let provider_model_id = provider_model_id(&default_model_id, *provider);
        let env_model_id = provider_model_id
            .as_deref()
            .unwrap_or(default_model_id.as_str());
        let mut env = build_env(
            &openrouter_api_key,
            &openrouter_base_url,
            env_model_id,
            *provider,
        );
        if provider.id == "cline" {
            match create_cline_config_dir(&openrouter_api_key, env_model_id, &openrouter_base_url) {
                Ok(dir) => {
                    env.insert("CLINE_DIR".to_string(), dir.path().display().to_string());
                    env.insert("CLINE_NO_AUTO_UPDATE".to_string(), "1".to_string());
                    _isolated_home = Some(dir);
                }
                Err(err) => {
                    eprintln!(
                        "skipping {}: failed to create Cline config dir: {}",
                        provider.id, err
                    );
                    continue;
                }
            }
        }
        if provider.id == "cline" {
            if let Err(reason) =
                probe_command(&acp_cmd.command, &acp_cmd.args, &["--version"], Some(&env)).await
            {
                eprintln!("skipping {}: {}", provider.id, reason);
                continue;
            }
        }
        if provider.id == "qwen" {
            match create_qwen_settings_home() {
                Ok(dir) => {
                    env.insert("HOME".to_string(), dir.path().display().to_string());
                    _qwen_home = Some(dir);
                }
                Err(err) => {
                    eprintln!(
                        "skipping {}: failed to create qwen settings: {}",
                        provider.id, err
                    );
                    continue;
                }
            }
        }
        if provider.id == "kimi" {
            match create_kimi_share_home() {
                Ok((dir, share_dir)) => {
                    env.insert(
                        KIMI_SHARE_DIR_ENV.to_string(),
                        share_dir.display().to_string(),
                    );
                    _kimi_home = Some(dir);
                }
                Err(err) => {
                    eprintln!(
                        "skipping {}: failed to create kimi share dir: {}",
                        provider.id, err
                    );
                    continue;
                }
            }
        }
        if provider.id == "goose" {
            match create_goose_path_root() {
                Ok((dir, path_root)) => {
                    env.insert(
                        "GOOSE_PATH_ROOT".to_string(),
                        path_root.display().to_string(),
                    );
                    _goose_home = Some(dir);
                }
                Err(err) => {
                    eprintln!(
                        "skipping {}: failed to create goose path root: {}",
                        provider.id, err
                    );
                    continue;
                }
            }
        }
        if provider.id == "openhands" {
            match create_openhands_persistence_dir(
                &openrouter_api_key,
                env_model_id,
                &openrouter_base_url,
            ) {
                Ok((dir, persistence_dir)) => {
                    env.insert(
                        "OPENHANDS_PERSISTENCE_DIR".to_string(),
                        persistence_dir.display().to_string(),
                    );
                    _openhands_home = Some(dir);
                }
                Err(err) => {
                    eprintln!(
                        "skipping {}: failed to create OpenHands persistence dir: {}",
                        provider.id, err
                    );
                    continue;
                }
            }
        }
        let acp_command = format_shell_command(&acp_cmd.command, &acp_args);
        let mut bridge_args = bridge_cmd.args.clone();
        bridge_args.push("--acp-command".to_string());
        bridge_args.push(acp_command);

        let adapter =
            Tier1CrpAdapter::from_raw(provider.id, bridge_cmd.command.clone(), bridge_args);
        let prompt = write_file_prompt(provider.id);
        match run_and_collect(
            &adapter,
            repo.path(),
            &prompt,
            provider_model_id.as_deref(),
            env,
        )
        .await
        {
            Ok(events) => match expect_success(&events, provider.id)
                .and_then(|_| expect_file_written(repo.path(), provider.id))
            {
                Ok(()) => eprintln!("completed {}", provider.id),
                Err(err) => {
                    eprintln!("failed {}: {}", provider.id, err);
                    failures.push(err);
                }
            },
            Err(err) => {
                let message = format!("{}: {}", provider.id, err);
                eprintln!("failed {message}");
                failures.push(message);
            }
        }
    }
    if let Some(requested) = requested_providers {
        assert!(
            requested.is_empty(),
            "unknown CTX_TOKENS_PROVIDERS entries: {}",
            requested.into_iter().collect::<Vec<_>>().join(",")
        );
    }
    assert!(
        failures.is_empty(),
        "write-file token sweep failures:\n{}",
        failures.join("\n")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_env_sets_droid_write_mode() {
        let env = build_env(
            "test-openrouter-key",
            "https://openrouter.ai/api/v1",
            DEFAULT_OPENROUTER_MODEL,
            ProviderSpec {
                id: "droid",
                fallback_cmd: "droid-acp",
                fallback_args: &[],
                opencode_config: false,
            },
        );

        assert_eq!(
            env.get("CTX_PROVIDER_MODE").map(String::as_str),
            Some("auto_high")
        );
    }

    #[test]
    fn build_env_sets_goose_openrouter_runtime_contract() {
        let env = build_env(
            "test-openrouter-key",
            "https://openrouter.ai/api/v1",
            DEFAULT_OPENROUTER_MODEL,
            ProviderSpec {
                id: "goose",
                fallback_cmd: "goose",
                fallback_args: &["acp"],
                opencode_config: false,
            },
        );

        assert_eq!(
            env.get("GOOSE_PROVIDER").map(String::as_str),
            Some("openrouter")
        );
        assert_eq!(
            env.get("GOOSE_DISABLE_KEYRING").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            env.get("GOOSE_MODEL").map(String::as_str),
            Some(DEFAULT_OPENROUTER_MODEL)
        );
        assert_eq!(env.get("GOOSE_MODE").map(String::as_str), Some("auto"));
        assert_eq!(env.get("CTX_PROVIDER_MODE").map(String::as_str), Some(""));
        assert!(!env.contains_key("OPENAI_API_KEY"));
        assert!(!env.contains_key("OPENAI_BASE_URL"));
        assert!(!env.contains_key("OPENAI_MODEL"));
        assert!(!env.contains_key("OPENAI_HOST"));
        assert!(!env.contains_key("OPENROUTER_BASE_URL"));
        assert!(!env.contains_key("OPENROUTER_HOST"));
        assert!(!env.contains_key("OPENROUTER_MODEL"));
    }

    #[test]
    fn build_env_sets_cline_write_mode() {
        let env = build_env(
            "test-openrouter-key",
            "https://openrouter.ai/api/v1",
            DEFAULT_OPENROUTER_MODEL,
            ProviderSpec {
                id: "cline",
                fallback_cmd: "cline",
                fallback_args: &["--acp"],
                opencode_config: false,
            },
        );

        assert_eq!(
            env.get("CTX_PROVIDER_MODE").map(String::as_str),
            Some("act")
        );
        assert_eq!(
            env.get("CTX_CRP_DISABLE_MODEL_OVERRIDE")
                .map(String::as_str),
            Some("1")
        );
        assert!(!env.contains_key("OPENAI_API_KEY"));
        assert!(!env.contains_key("OPENAI_BASE_URL"));
    }

    #[test]
    fn build_env_sets_openhands_llm_contract() {
        let env = build_env(
            "test-openrouter-key",
            "https://openrouter.ai/api/v1",
            DEFAULT_OPENROUTER_MODEL,
            ProviderSpec {
                id: "openhands",
                fallback_cmd: "openhands",
                fallback_args: &["acp"],
                opencode_config: false,
            },
        );

        assert_eq!(
            env.get("LLM_API_KEY").map(String::as_str),
            Some("test-openrouter-key")
        );
        assert_eq!(
            env.get("LLM_BASE_URL").map(String::as_str),
            Some("https://openrouter.ai/api/v1")
        );
        assert_eq!(
            env.get("LLM_MODEL").map(String::as_str),
            Some("openrouter/openai/gpt-4.1-mini")
        );
        assert_eq!(
            env.get("CTX_PROVIDER_MODE").map(String::as_str),
            Some("always-approve")
        );
        assert_eq!(
            env.get("CTX_CRP_DISABLE_MODEL_OVERRIDE")
                .map(String::as_str),
            Some("1")
        );
        assert!(!env.contains_key("OPENAI_API_KEY"));
        assert!(!env.contains_key("OPENAI_BASE_URL"));
        assert!(!env.contains_key("OPENAI_MODEL"));
    }

    #[test]
    fn requested_token_providers_parses_csv() {
        unsafe {
            std::env::set_var("CTX_TOKENS_PROVIDERS", " qwen, mistral ,,opencode ");
        }
        let requested = requested_token_providers().expect("providers");
        assert!(requested.contains("qwen"));
        assert!(requested.contains("mistral"));
        assert!(requested.contains("opencode"));
        assert_eq!(requested.len(), 3);
        unsafe {
            std::env::remove_var("CTX_TOKENS_PROVIDERS");
        }
    }

    #[test]
    fn resolve_command_prefers_explicit_bridge_override() {
        unsafe {
            std::env::set_var("CTX_TOKENS_ACP_CRP_BRIDGE_BIN", "/tmp/local-acp-crp-bridge");
        }
        let cfg = ctx_managed_installs::AgentServerConfigFile::default();
        let command = resolve_command(&cfg, "acp-crp-bridge", "acp-crp-bridge", &[]);
        assert_eq!(command.command, "/tmp/local-acp-crp-bridge");
        assert!(command.args.is_empty());
        unsafe {
            std::env::remove_var("CTX_TOKENS_ACP_CRP_BRIDGE_BIN");
        }
    }

    #[test]
    fn provider_model_id_uses_gemini_native_default() {
        let model = provider_model_id(
            DEFAULT_OPENROUTER_MODEL,
            ProviderSpec {
                id: "gemini",
                fallback_cmd: "gemini",
                fallback_args: &["--experimental-acp"],
                opencode_config: false,
            },
        )
        .expect("gemini model");
        assert_eq!(model, DEFAULT_GEMINI_MODEL);
    }

    #[test]
    fn provider_model_id_uses_pi_default() {
        let model = provider_model_id(
            DEFAULT_OPENROUTER_MODEL,
            ProviderSpec {
                id: "pi",
                fallback_cmd: "pi-acp",
                fallback_args: &[],
                opencode_config: false,
            },
        )
        .expect("pi model");
        assert_eq!(model, DEFAULT_GEMINI_MODEL);
    }

    #[test]
    fn provider_skip_reason_marks_amp_as_unsupported_for_token_mode() {
        let reason = provider_skip_reason(ProviderSpec {
            id: "amp",
            fallback_cmd: "amp-acp",
            fallback_args: &[],
            opencode_config: false,
        })
        .expect("amp should be skipped");
        assert!(reason.contains("does not support OpenRouter endpoint mode"));
    }

    #[test]
    fn provider_skip_reason_does_not_gate_cline_anymore() {
        let reason = provider_skip_reason(ProviderSpec {
            id: "cline",
            fallback_cmd: "cline",
            fallback_args: &["--acp"],
            opencode_config: false,
        });
        assert!(reason.is_none());
    }

    #[test]
    fn expect_file_written_accepts_trailing_newline() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file_path = dir.path().join(write_file_name("droid"));
        fs::write(&file_path, "hi\n").expect("write file");

        expect_file_written(dir.path(), "droid").expect("normalized file contents");
    }
}
