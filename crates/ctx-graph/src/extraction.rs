use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Subcommand};
use ctx_graph_core::{
    index::IndexOptions,
    ingest::{self, CommandAdapter, Provider, SemanticOptions},
};
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Debug, Default, Args)]
pub struct ExtractionArgs {
    /// JSON IndexOptions. Overrides the stored configuration for this update.
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// Re-extract local files even when their fingerprints are unchanged.
    #[arg(long)]
    pub force: bool,
    /// Include measured detection, extraction and commit times in the report.
    #[arg(long)]
    pub timing: bool,
    /// Bypass semantic/transcript cache reads and re-extract local files once.
    #[arg(long)]
    pub refresh_cache: bool,
    /// Accept fewer semantic facts after saving the prior graph in .graf/backups.
    #[arg(long)]
    pub allow_semantic_shrink: bool,
    #[arg(long)]
    pub code_only: bool,
    /// Also enrich code facts with the explicitly selected semantic provider.
    #[arg(long, conflicts_with = "no_semantic")]
    pub deep: bool,
    #[arg(long)]
    pub no_gitignore: bool,
    #[arg(long)]
    pub include_generated: bool,
    /// Repository-relative Python import root; repeat for multiple source trees.
    #[arg(long)]
    pub python_source_root: Vec<String>,
    /// Explicit Swift module and repository-relative source root, NAME=DIR; repeat as needed.
    #[arg(long)]
    pub swift_module: Vec<String>,
    /// Explicit interpreter with Robot Framework installed; native parsing otherwise stays local.
    #[arg(long)]
    pub robot_python: Option<PathBuf>,
    /// Explicitly enable semantic extraction using a built-in or registered provider.
    #[arg(long, conflicts_with = "no_semantic")]
    pub provider: Option<String>,
    #[arg(long)]
    pub model: Option<String>,
    /// Full provider request URL, including its API route.
    #[arg(long)]
    pub endpoint: Option<String>,
    /// Environment variable containing the API key; never a literal secret.
    #[arg(long)]
    pub key_env: Option<String>,
    #[arg(long)]
    pub no_semantic: bool,
    /// Explicitly allow uploading images to the configured semantic provider.
    #[arg(long, conflicts_with = "no_semantic")]
    pub vision: bool,
    #[arg(long)]
    pub max_semantic_files: Option<usize>,
    /// Maximum provider attempts across this invocation; cache hits are free.
    #[arg(long)]
    pub max_semantic_calls: Option<usize>,
    /// Maximum reserved output-token allowance across provider attempts.
    #[arg(long)]
    pub max_semantic_output_tokens: Option<u64>,
    /// Use installed Tesseract for local image OCR.
    #[arg(long)]
    pub ocr: bool,
    /// Use installed Whisper/FFmpeg with the named model for audio/video.
    #[arg(long)]
    pub whisper: Option<String>,
    /// Override the saved graph's topic hints for the selected Whisper adapter.
    #[arg(long, requires = "whisper")]
    pub whisper_prompt: Option<String>,
    /// Use installed gws when explicitly adding Google pointer documents.
    #[arg(long)]
    pub google: bool,
    /// Use installed yt-dlp when explicitly adding remote media.
    #[arg(long)]
    pub download_media: bool,
    /// Allow explicit URL imports from trusted private or localhost addresses.
    #[arg(long)]
    pub allow_private_urls: bool,
}

impl ExtractionArgs {
    pub fn configure(
        &self,
        mut options: IndexOptions,
        root: &Path,
        db: &Path,
    ) -> Result<IndexOptions> {
        if let Some(path) = &self.config {
            options =
                serde_json::from_slice(&bounded(path)?).context("invalid index configuration")?;
        }
        if self.code_only {
            options.code_only = true;
        }
        options.force = self.force || self.refresh_cache;
        options.timing = self.timing;
        options.ingest.force_cache_refresh = self.refresh_cache;
        options.allow_semantic_shrink = self.allow_semantic_shrink || self.no_semantic;
        if let Some(python) = &self.robot_python {
            options.robot_python = Some(python.clone());
        }
        if let Some(python) = &mut options.robot_python
            && python.is_relative()
            && python.components().count() > 1
        {
            *python = std::path::absolute(root)?.join(&*python);
        }
        if self.deep {
            options.semantic_code = true;
        }
        if self.no_gitignore {
            options.no_gitignore = true;
        }
        if self.include_generated {
            options.include_generated = true;
        }
        if !self.python_source_root.is_empty() {
            options.python_source_roots = self
                .python_source_root
                .iter()
                .map(|root| {
                    if root == "." {
                        String::new()
                    } else {
                        root.clone()
                    }
                })
                .collect();
        }
        if let Some(limit) = self.max_semantic_files {
            ensure!(
                limit <= 100_000,
                "max-semantic-files must be at most 100000"
            );
            options.max_semantic_files = limit;
        }
        if let Some(limit) = self.max_semantic_calls {
            options.max_semantic_calls = Some(limit);
        }
        if let Some(limit) = self.max_semantic_output_tokens {
            options.max_semantic_output_tokens = Some(limit);
        }
        if !self.swift_module.is_empty() {
            options.swift_modules.clear();
            for item in &self.swift_module {
                let (name, directory) = item
                    .split_once('=')
                    .context("swift-module requires NAME=DIR")?;
                ensure!(!name.is_empty(), "Swift module name must not be empty");
                let directory = if directory == "." { "" } else { directory };
                ensure!(
                    options
                        .swift_modules
                        .insert(name.to_owned(), directory.to_owned())
                        .is_none(),
                    "duplicate Swift module name"
                );
            }
        }
        if self.no_semantic {
            options.ingest.semantic = None;
            options.semantic_code = false;
        }
        if let Some(name) = &self.provider {
            let settings = if builtin(name).is_some() {
                preset(
                    name,
                    self.model.as_deref(),
                    self.endpoint.as_deref(),
                    self.key_env.as_deref(),
                )?
            } else {
                let mut registry = read_registry(&global_registry()?)?;
                registry.extend(read_registry(&root.join(".graf/providers.json"))?);
                registry
                    .remove(name)
                    .with_context(|| format!("unknown provider {name}"))?
            };
            options.ingest.semantic = Some(settings);
        }
        if self.model.is_some() || self.endpoint.is_some() || self.key_env.is_some() || self.vision
        {
            let settings = options.ingest.semantic.as_mut().context(
                "provider settings require --provider or an existing semantic configuration",
            )?;
            if let Some(model) = &self.model {
                settings.model = model.clone();
            }
            if let Some(endpoint) = &self.endpoint {
                // Changing destinations does not authorize forwarding a saved key.
                if endpoint != &settings.endpoint && self.key_env.is_none() {
                    settings.key_env = None;
                }
                settings.endpoint = endpoint.clone();
            }
            if let Some(key) = &self.key_env {
                settings.key_env = Some(key.clone());
            }
            if self.vision {
                settings.vision = true;
            }
        }
        if self.ocr {
            for ext in ["png", "jpg", "jpeg", "gif", "bmp", "tif", "tiff", "webp"] {
                options
                    .ingest
                    .converters
                    .insert(ext.into(), CommandAdapter::tesseract());
            }
        }
        if let Some(model) = &self.whisper {
            let prompt = if let Some(prompt) = &self.whisper_prompt {
                ensure!(
                    prompt.len() <= 2048 && !prompt.contains('\0'),
                    "Whisper prompt must be at most 2048 bytes without NUL"
                );
                prompt.clone()
            } else if db.try_exists()? {
                ctx_graph_core::store::Store::open_read_only(db)?
                    .transcription_topics()?
                    .join(", ")
            } else {
                String::new()
            };
            let adapter = if prompt.is_empty() {
                CommandAdapter::whisper(model)
            } else {
                CommandAdapter::whisper_with_prompt(model, &prompt)
            };
            for ext in [
                "mp3", "wav", "m4a", "ogg", "flac", "aac", "opus", "mp4", "mkv", "mov", "webm",
                "avi", "m4v",
            ] {
                options
                    .ingest
                    .converters
                    .insert(ext.into(), adapter.clone());
            }
        }
        if self.google {
            for ext in ["gdoc", "gsheet", "gslides"] {
                options
                    .ingest
                    .converters
                    .insert(ext.into(), CommandAdapter::google_workspace());
            }
        }
        if self.download_media {
            options
                .ingest
                .converters
                .insert("url".into(), CommandAdapter::yt_dlp());
        }
        if self.allow_private_urls {
            options.ingest.allow_private_urls = true;
        }
        ingest::config_fingerprint(&options.ingest)?;
        Ok(options)
    }
}

#[derive(Debug, Args)]
pub struct ProviderArgs {
    /// Manage project providers instead of the global registry.
    #[arg(long)]
    pub project: Option<PathBuf>,
    #[command(subcommand)]
    pub command: ProviderCommand,
}
#[derive(Debug, Subcommand)]
pub enum ProviderCommand {
    List,
    /// Inspect environment-name presence and executable paths; never contacts providers or runs commands.
    Detect,
    /// Print a validated SemanticOptions JSON preset without saving or enabling it.
    Template(ProviderPresetArgs),
    /// Save a new named preset; does not enable extraction or replace an existing name.
    Setup {
        name: String,
        #[command(flatten)]
        settings: ProviderPresetArgs,
    },
    Show {
        name: String,
    },
    /// Register a SemanticOptions JSON configuration. Credentials must be environment names.
    Add {
        name: String,
        file: PathBuf,
    },
    Remove {
        name: String,
    },
}

#[derive(Debug, Args)]
pub struct ProviderPresetArgs {
    /// open_ai (or openai), anthropic, gemini, ollama, azure, bedrock, or claude_cli.
    pub preset: String,
    /// Required except for OpenAI, which defaults to gpt-6-astra even with a custom endpoint.
    #[arg(long)]
    pub model: Option<String>,
    /// Full HTTP request URL; required for Azure. Overrides never infer a key environment name.
    #[arg(long)]
    pub endpoint: Option<String>,
    /// API key environment variable name, never its value. Explicitly set for custom endpoints.
    #[arg(long)]
    pub key_env: Option<String>,
}

impl ProviderPresetArgs {
    fn options(&self) -> Result<SemanticOptions> {
        preset(
            &self.preset,
            self.model.as_deref(),
            self.endpoint.as_deref(),
            self.key_env.as_deref(),
        )
    }
}

#[derive(Debug, Args)]
pub struct CacheArgs {
    #[command(subcommand)]
    pub command: CacheCommand,
}

#[derive(Debug, Subcommand)]
pub enum CacheCommand {
    /// Inspect bounded semantic cache records without contacting a provider.
    Inspect {
        directory: PathBuf,
        #[arg(long, default_value_t=1000, value_parser=clap::value_parser!(u32).range(1..=100000))]
        limit: u32,
        #[arg(long, default_value_t=8*1024*1024, value_parser=clap::value_parser!(u32).range(1..=16*1024*1024))]
        max_entry_bytes: u32,
    },
    /// Remove exactly one cache entry by its key so a later extraction can retry it.
    Remove { directory: PathBuf, key: String },
}

pub fn cache(args: &CacheArgs) -> Result<serde_json::Value> {
    match &args.command {
        CacheCommand::Inspect {
            directory,
            limit,
            max_entry_bytes,
        } => Ok(serde_json::json!({
            "directory": directory,
            "entries": ingest::inspect_semantic_cache(directory, *limit as usize, *max_entry_bytes as usize)?,
            "limit": limit,
        })),
        CacheCommand::Remove { directory, key } => Ok(serde_json::json!({
            "directory":directory,"key":key,"removed":ingest::remove_semantic_cache_entry(directory,key)?,
        })),
    }
}

const BUILTINS: [&str; 8] = [
    "open_ai",
    "anthropic",
    "gemini",
    "ollama",
    "azure",
    "cli",
    "bedrock",
    "claude_cli",
];
const PRESETS: [&str; 7] = [
    "open_ai",
    "anthropic",
    "gemini",
    "ollama",
    "azure",
    "bedrock",
    "claude_cli",
];

fn preset(
    name: &str,
    model: Option<&str>,
    endpoint: Option<&str>,
    key_env: Option<&str>,
) -> Result<SemanticOptions> {
    let provider = builtin(name).context("unknown preset; use provider list")?;
    let (route, key, command) = match provider {
        Provider::OpenAi => (
            "https://api.openai.com/v1/chat/completions",
            Some("OPENAI_API_KEY"),
            None,
        ),
        Provider::Anthropic => (
            "https://api.anthropic.com/v1/messages",
            Some("ANTHROPIC_API_KEY"),
            None,
        ),
        Provider::Gemini => (
            "https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent",
            Some("GEMINI_API_KEY"),
            None,
        ),
        Provider::Ollama => ("http://localhost:11434/api/chat", None, None),
        Provider::Azure => ("", None, None),
        Provider::Bedrock => ("", None, Some(CommandAdapter::bedrock())),
        Provider::ClaudeCli => ("", None, Some(CommandAdapter::claude_cli())),
        Provider::Cli => {
            bail!("generic CLI adapters require explicit JSON; use provider add NAME FILE")
        }
    };
    ensure!(
        command.is_none() || (endpoint.is_none() && key_env.is_none()),
        "CLI presets use the command's authentication; --endpoint and --key-env are HTTP-only"
    );
    ensure!(
        provider != Provider::Azure || endpoint.is_some(),
        "Azure requires --endpoint with the full deployment request URL and --key-env for API-key authentication"
    );
    let model = model
        .or_else(|| (provider == Provider::OpenAi).then_some("gpt-6-astra"))
        .context("name a model explicitly for this preset with --model")?;
    let settings = SemanticOptions {
        provider,
        model: model.into(),
        endpoint: endpoint.unwrap_or(route).into(),
        // An endpoint override may be a local server or a different account.
        // Only an explicit key name may accompany that override.
        key_env: key_env
            .or_else(|| endpoint.is_none().then_some(key).flatten())
            .map(str::to_owned),
        command,
        ..Default::default()
    };
    validate_provider(&settings)?;
    Ok(settings)
}

fn validate_provider(settings: &SemanticOptions) -> Result<()> {
    ingest::config_fingerprint(&ingest::IngestOptions {
        semantic: Some(settings.clone()),
        ..Default::default()
    })?;
    Ok(())
}

// Resolve only files on PATH. Never invoke a discovered executable (even --help).
fn installed_command(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        let candidates = if cfg!(windows) && Path::new(program).extension().is_none() {
            vec![
                directory.join(format!("{program}.exe")),
                directory.join(format!("{program}.cmd")),
                directory.join(program),
            ]
        } else {
            vec![directory.join(program)]
        };
        for candidate in candidates {
            let Ok(metadata) = candidate.metadata() else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if metadata.permissions().mode() & 0o111 == 0 {
                    continue;
                }
            }
            // Keep executable shims/symlinks intact; some dispatch by their name.
            if let Ok(path) = std::path::absolute(candidate) {
                return Some(path);
            }
        }
    }
    None
}

fn detect_providers() -> serde_json::Value {
    let providers: Vec<_> = PRESETS
        .iter()
        .map(|name| {
            let (names, command): (&[&str], Option<String>) = match *name {
                "open_ai" => (&["OPENAI_API_KEY"], None),
                "anthropic" => (&["ANTHROPIC_API_KEY"], None),
                "gemini" => (&["GEMINI_API_KEY", "GOOGLE_API_KEY"], None),
                "ollama" => (&["OLLAMA_HOST", "OLLAMA_API_KEY"], Some("ollama".into())),
                "azure" => (&["AZURE_OPENAI_API_KEY", "AZURE_OPENAI_ENDPOINT"], None),
                "bedrock" => (
                    &[
                        "AWS_PROFILE",
                        "AWS_REGION",
                        "AWS_DEFAULT_REGION",
                        "AWS_ACCESS_KEY_ID",
                        "AWS_SECRET_ACCESS_KEY",
                        "AWS_SESSION_TOKEN",
                    ],
                    Some(CommandAdapter::bedrock().program),
                ),
                "claude_cli" => (&[], Some(CommandAdapter::claude_cli().program)),
                _ => unreachable!(),
            };
            let environment: BTreeMap<_, _> = names
                .iter()
                .map(|name| (*name, std::env::var_os(name).is_some()))
                .collect();
            let command = command
                .map(|name| serde_json::json!({"path":installed_command(&name),"name":name}));
            serde_json::json!({"preset":name,"environment":environment,"command":command,
            "model_required":*name != "open_ai","endpoint_required":*name == "azure"})
        })
        .collect();
    serde_json::json!({"providers":providers,
        "note":"Presence only, including empty variables; authentication, CLI versions and service availability are not checked. Nothing is selected or saved. Use provider template PRESET or provider setup NAME PRESET; extraction requires --provider. Alternate keys and endpoints require explicit flags."})
}

fn validate_provider_name(name: &str) -> Result<()> {
    ensure!(
        builtin(name).is_none(),
        "built-in providers cannot be replaced"
    );
    ensure!(
        !name.is_empty()
            && name.len() <= 64
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c)),
        "invalid provider name"
    );
    Ok(())
}

fn builtin(name: &str) -> Option<Provider> {
    match name {
        "open_ai" | "openai" => Some(Provider::OpenAi),
        "anthropic" => Some(Provider::Anthropic),
        "gemini" => Some(Provider::Gemini),
        "ollama" => Some(Provider::Ollama),
        "azure" => Some(Provider::Azure),
        "cli" => Some(Provider::Cli),
        "bedrock" => Some(Provider::Bedrock),
        "claude_cli" => Some(Provider::ClaudeCli),
        _ => None,
    }
}
fn global_registry() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|h| PathBuf::from(h).join(".graf/providers.json"))
        .context("home directory unavailable; use --project")
}
fn bounded(path: &Path) -> Result<Vec<u8>> {
    use std::io::Read;
    let metadata = std::fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() && metadata.len() <= 1024 * 1024,
        "configuration must be a regular file of at most 1 MiB"
    );
    let mut bytes = vec![];
    std::fs::File::open(path)?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 1024 * 1024, "configuration exceeds 1 MiB");
    Ok(bytes)
}
fn read_registry(path: &Path) -> Result<BTreeMap<String, SemanticOptions>> {
    if !path.try_exists()? {
        return Ok(BTreeMap::new());
    }
    serde_json::from_slice(&bounded(path)?).context("invalid provider registry")
}
pub fn provider(args: &ProviderArgs) -> Result<serde_json::Value> {
    // Discovery and templates need neither a registry nor a home directory.
    match &args.command {
        ProviderCommand::Detect => return Ok(detect_providers()),
        ProviderCommand::Template(settings) => {
            return Ok(serde_json::to_value(settings.options()?)?);
        }
        _ => {}
    }
    let path = match &args.project {
        Some(root) => root.join(".graf/providers.json"),
        None => global_registry()?,
    };
    let mut registry = read_registry(&path)?;
    match &args.command {
        ProviderCommand::List => {
            return Ok(
                serde_json::json!({"builtins":BUILTINS,"presets":PRESETS,"custom":registry.keys().collect::<Vec<_>>()}),
            );
        }
        ProviderCommand::Show { name } => {
            if let Some(provider) = builtin(name) {
                return Ok(
                    serde_json::json!({"provider":provider,"configuration":"Set the full request endpoint and model explicitly; credentials use key_env."}),
                );
            }
            return serde_json::to_value(registry.get(name).context("provider not found")?)
                .map_err(Into::into);
        }
        ProviderCommand::Add { name, file } => {
            validate_provider_name(name)?;
            let settings: SemanticOptions = serde_json::from_slice(&bounded(file)?)
                .context("invalid provider configuration")?;
            validate_provider(&settings)?;
            registry.insert(name.clone(), settings);
        }
        ProviderCommand::Setup { name, settings } => {
            validate_provider_name(name)?;
            ensure!(
                !registry.contains_key(name),
                "provider already exists; use a new name or explicitly remove it before setup"
            );
            let mut settings = settings.options()?;
            if let Some(command) = &mut settings.command {
                let path = installed_command(&command.program).context("preset executable not found on PATH; install it first or use provider template for a portable JSON recipe")?;
                command.program = path
                    .to_str()
                    .context("executable path must be UTF-8")?
                    .into();
            }
            registry.insert(name.clone(), settings);
        }
        ProviderCommand::Remove { name } => {
            if builtin(name).is_some() {
                bail!("built-in providers cannot be removed");
            }
            ensure!(registry.remove(name).is_some(), "provider not found");
        }
        ProviderCommand::Detect | ProviderCommand::Template(_) => unreachable!(),
    }
    let parent = path.parent().context("registry has no parent")?;
    std::fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temporary, &registry)?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    temporary.persist(&path).map_err(|e| e.error)?;
    Ok(serde_json::json!({"status":"updated","path":path}))
}
