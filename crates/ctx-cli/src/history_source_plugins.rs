use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use uuid::Uuid;

mod child_output;

use child_output::collect_child_output_with_timeout;

const PLUGIN_MANIFEST_FILE: &str = "ctx-history-plugin.json";
const MAX_PLUGIN_MANIFEST_BYTES: usize = 1024 * 1024;
const DEFAULT_PLUGIN_TIMEOUT_SECONDS: u64 = 300;
const MAX_PLUGIN_STDERR_SNIPPET_BYTES: usize = 4096;
const MAX_INLINE_CURSOR_ENV_BYTES: usize = 8192;
const SAFE_PLUGIN_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TMPDIR",
    "TEMP",
    "TMP",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
];

#[derive(Debug, Clone)]
pub struct HistorySourcePluginSource {
    pub plugin_name: String,
    pub plugin_display_name: Option<String>,
    pub plugin_version: Option<String>,
    pub manifest_path: PathBuf,
    pub manifest_dir: PathBuf,
    pub id: String,
    pub display_name: Option<String>,
    pub provider_key: String,
    pub source_id: String,
    pub source_format: String,
    pub command: Vec<String>,
    pub working_dir: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub enabled: bool,
    pub refresh: HistorySourcePluginRefresh,
    pub timeout: Duration,
}

impl HistorySourcePluginSource {
    pub fn label(&self) -> String {
        format!("{}/{}", self.plugin_name, self.id)
    }

    pub fn cursor_stream(&self) -> String {
        ctx_history_capture::custom_history_jsonl_v1_cursor_stream(
            &self.provider_key,
            &self.source_id,
            &self.source_format,
        )
    }

    pub fn matches_selector(&self, selector: &str) -> bool {
        selector == self.label() || selector == format!("{}/{}", self.provider_key, self.source_id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum HistorySourcePluginRefresh {
    #[default]
    Manual,
    Auto,
}

#[derive(Debug, Clone)]
pub struct HistorySourcePluginRun {
    pub stdout: Vec<u8>,
    pub stderr: String,
}

#[derive(Debug, Clone)]
pub struct HistorySourcePluginRunOptions<'a> {
    pub data_root: &'a Path,
    pub machine_id: &'a str,
    pub cursor: Option<&'a str>,
    pub cursor_stream: &'a str,
    pub full_rescan: bool,
}

#[derive(Debug, Clone, Default)]
pub struct HistorySourcePluginDiscovery {
    pub sources: Vec<HistorySourcePluginSource>,
    pub failures: Vec<HistorySourcePluginManifestFailure>,
}

#[derive(Debug, Clone)]
pub struct HistorySourcePluginManifestFailure {
    pub manifest_path: PathBuf,
    pub error: String,
}

#[derive(Debug, Deserialize)]
struct HistorySourcePluginManifest {
    schema_version: u32,
    name: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    history_sources: Vec<HistorySourcePluginSourceManifest>,
}

#[derive(Debug, Deserialize)]
struct HistorySourcePluginSourceManifest {
    id: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    provider_key: Option<String>,
    #[serde(default)]
    source_id: Option<String>,
    source_format: String,
    command: Vec<String>,
    #[serde(default)]
    working_dir: Option<PathBuf>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    refresh: HistorySourcePluginRefresh,
    #[serde(default)]
    timeout_seconds: Option<u64>,
}

pub fn discover_history_source_plugins(
    data_root: &Path,
    extra_manifests: &[PathBuf],
) -> Result<Vec<HistorySourcePluginSource>> {
    let discovery = discover_history_source_plugins_with_diagnostics(data_root, extra_manifests)?;
    Ok(discovery.sources)
}

pub fn discover_history_source_plugins_with_diagnostics(
    data_root: &Path,
    extra_manifests: &[PathBuf],
) -> Result<HistorySourcePluginDiscovery> {
    let mut sources = Vec::new();
    let mut failures = Vec::new();
    for manifest_path in plugin_manifest_paths(data_root) {
        match read_plugin_manifest(&manifest_path) {
            Ok(mut manifest_sources) => sources.append(&mut manifest_sources),
            Err(error) => failures.push(HistorySourcePluginManifestFailure {
                manifest_path,
                error: error.to_string(),
            }),
        }
    }
    for manifest_path in explicit_plugin_manifest_paths(extra_manifests)? {
        let mut manifest_sources = read_plugin_manifest(&manifest_path)?;
        sources.append(&mut manifest_sources);
    }
    sources.sort_by_key(|source| source.label());
    Ok(HistorySourcePluginDiscovery { sources, failures })
}

pub fn run_history_source_plugin(
    source: &HistorySourcePluginSource,
    options: HistorySourcePluginRunOptions<'_>,
) -> Result<HistorySourcePluginRun> {
    let (program, args) = source.command.split_first().ok_or_else(|| {
        anyhow!(
            "history source plugin {} has an empty command",
            source.label()
        )
    })?;
    let mut command = Command::new(program);
    command.env_clear();
    inherit_safe_plugin_env(&mut command);
    command.args(args);
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    if let Some(working_dir) = &source.working_dir {
        command.current_dir(resolve_manifest_path(&source.manifest_dir, working_dir));
    }
    for (key, value) in &source.env {
        command.env(key, value);
    }
    command.env("CTX_DATA_ROOT", options.data_root);
    command.env("CTX_HISTORY_PLUGIN", "1");
    command.env("CTX_HISTORY_PLUGIN_NAME", &source.plugin_name);
    command.env("CTX_HISTORY_PLUGIN_MANIFEST", &source.manifest_path);
    command.env("CTX_HISTORY_SOURCE", source.label());
    command.env("CTX_HISTORY_SOURCE_ID", &source.source_id);
    command.env("CTX_HISTORY_PROVIDER_KEY", &source.provider_key);
    command.env("CTX_HISTORY_SOURCE_FORMAT", &source.source_format);
    command.env("CTX_HISTORY_CURSOR_STREAM", options.cursor_stream);
    command.env("CTX_HISTORY_MACHINE_ID", options.machine_id);
    command.env(
        "CTX_HISTORY_FULL_RESCAN",
        if options.full_rescan { "1" } else { "0" },
    );
    let cursor_file = if let Some(cursor) = options.cursor {
        let path = write_private_temp_file("ctx-history-cursor", cursor).with_context(|| {
            format!("write history source plugin {} cursor file", source.label())
        })?;
        if cursor.len() <= MAX_INLINE_CURSOR_ENV_BYTES {
            command.env("CTX_HISTORY_CURSOR", cursor);
        } else {
            command.env_remove("CTX_HISTORY_CURSOR");
        }
        command.env("CTX_HISTORY_CURSOR_FILE", &path);
        Some(path)
    } else {
        command.env_remove("CTX_HISTORY_CURSOR");
        command.env_remove("CTX_HISTORY_CURSOR_FILE");
        None
    };
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            cleanup_cursor_file(cursor_file.as_ref());
            return Err(err).with_context(|| {
                format!(
                    "spawn history source plugin {} command {}",
                    source.label(),
                    shell_like_command(&source.command)
                )
            });
        }
    };
    let stdout = child
        .stdout
        .take()
        .context("history source plugin stdout was not piped")?;
    let stderr = child
        .stderr
        .take()
        .context("history source plugin stderr was not piped")?;
    let run_result = collect_child_output_with_timeout(
        &mut child,
        stdout,
        stderr,
        source.timeout,
        &source.label(),
    );
    cleanup_cursor_file(cursor_file.as_ref());
    let (status, stdout, stderr) = run_result?;
    let stderr = String::from_utf8_lossy(&stderr).trim().to_owned();
    if !status.success() {
        let detail = if stderr.is_empty() {
            format!("exit status {status}")
        } else {
            format!("exit status {status}: {}", stderr_snippet(&stderr))
        };
        return Err(anyhow!(
            "history source plugin {} failed: {detail}",
            source.label()
        ));
    }
    Ok(HistorySourcePluginRun { stdout, stderr })
}

fn inherit_safe_plugin_env(command: &mut Command) {
    for key in SAFE_PLUGIN_ENV {
        if let Some(value) = env::var_os(key) {
            command.env(key, value);
        }
    }
}

fn write_private_temp_file(prefix: &str, contents: &str) -> Result<PathBuf> {
    for _ in 0..16 {
        let path = env::temp_dir().join(format!("{prefix}-{}.cursor", Uuid::new_v4()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        match options.open(&path) {
            Ok(mut file) => {
                file.write_all(contents.as_bytes())?;
                return Ok(path);
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("create private temp file {}", path.display()));
            }
        }
    }
    Err(anyhow!("failed to allocate unique private temp file"))
}

fn cleanup_cursor_file(path: Option<&PathBuf>) {
    if let Some(path) = path {
        let _ = fs::remove_file(path);
    }
}

fn read_plugin_manifest(path: &Path) -> Result<Vec<HistorySourcePluginSource>> {
    let raw = read_plugin_manifest_text(path)?;
    let manifest: HistorySourcePluginManifest = serde_json::from_str(&raw)
        .with_context(|| format!("parse history source plugin manifest {}", path.display()))?;
    validate_plugin_id("plugin name", &manifest.name)?;
    if manifest.schema_version != 1 {
        return Err(anyhow!(
            "history source plugin manifest {} has unsupported schema_version {}; expected 1",
            path.display(),
            manifest.schema_version
        ));
    }
    let manifest_dir = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let mut sources = Vec::new();
    for source in manifest.history_sources {
        validate_plugin_id("history source id", &source.id)?;
        let provider_key = source.provider_key.unwrap_or_else(|| manifest.name.clone());
        validate_plugin_id("provider_key", &provider_key)?;
        let source_id = source.source_id.unwrap_or_else(|| source.id.clone());
        validate_plugin_id("source_id", &source_id)?;
        validate_source_format(&source.source_format).with_context(|| {
            format!(
                "history source plugin manifest {} source {} has invalid source_format",
                path.display(),
                source.id
            )
        })?;
        if source.command.is_empty() || source.command.iter().any(|part| part.trim().is_empty()) {
            return Err(anyhow!(
                "history source plugin manifest {} source {} has empty command",
                path.display(),
                source.id
            ));
        }
        sources.push(HistorySourcePluginSource {
            plugin_name: manifest.name.clone(),
            plugin_display_name: manifest.display_name.clone(),
            plugin_version: manifest.version.clone(),
            manifest_path: path.to_path_buf(),
            manifest_dir: manifest_dir.clone(),
            id: source.id,
            display_name: source.display_name,
            provider_key,
            source_id,
            source_format: source.source_format,
            command: source.command,
            working_dir: source.working_dir,
            env: source.env,
            enabled: source.enabled,
            refresh: source.refresh,
            timeout: Duration::from_secs(
                source
                    .timeout_seconds
                    .unwrap_or(DEFAULT_PLUGIN_TIMEOUT_SECONDS)
                    .max(1),
            ),
        });
    }
    Ok(sources)
}

fn read_plugin_manifest_text(path: &Path) -> Result<String> {
    let file = fs::File::open(path)
        .with_context(|| format!("read history source plugin manifest {}", path.display()))?;
    let mut bytes = Vec::new();
    file.take((MAX_PLUGIN_MANIFEST_BYTES as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .with_context(|| format!("read history source plugin manifest {}", path.display()))?;
    if bytes.len() > MAX_PLUGIN_MANIFEST_BYTES {
        return Err(anyhow!(
            "history source plugin manifest {} exceeds max bytes ({MAX_PLUGIN_MANIFEST_BYTES})",
            path.display()
        ));
    }
    String::from_utf8(bytes).with_context(|| {
        format!(
            "history source plugin manifest {} is not UTF-8",
            path.display()
        )
    })
}

fn plugin_manifest_paths(data_root: &Path) -> Vec<PathBuf> {
    let mut candidates = BTreeSet::new();
    collect_manifest_path_candidates(&data_root.join("plugins"), &mut candidates);
    if let Some(paths) = env::var_os("CTX_HISTORY_PLUGIN_PATH") {
        for path in env::split_paths(&paths) {
            collect_manifest_path_candidates(&path, &mut candidates);
        }
    }
    candidates.into_iter().collect()
}

fn explicit_plugin_manifest_paths(extra_manifests: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut candidates = BTreeSet::new();
    for path in extra_manifests {
        if !path
            .try_exists()
            .with_context(|| format!("check import path {}", path.display()))?
        {
            return Err(anyhow!("import path does not exist: {}", path.display()));
        }
        let before = candidates.len();
        collect_manifest_path_candidates(path, &mut candidates);
        if candidates.len() == before {
            return Err(anyhow!(
                "history source plugin manifest path {} did not contain {}",
                path.display(),
                PLUGIN_MANIFEST_FILE
            ));
        }
    }
    Ok(candidates.into_iter().collect())
}

fn collect_manifest_path_candidates(path: &Path, candidates: &mut BTreeSet<PathBuf>) {
    if path.is_file() {
        candidates.insert(path.to_path_buf());
        return;
    }
    if !path.is_dir() {
        return;
    }
    let direct = path.join(PLUGIN_MANIFEST_FILE);
    if direct.is_file() {
        candidates.insert(direct);
    }
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let child = entry.path();
        if child.is_file()
            && child
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name == PLUGIN_MANIFEST_FILE)
        {
            candidates.insert(child);
            continue;
        }
        if child.is_dir() {
            let manifest = child.join(PLUGIN_MANIFEST_FILE);
            if manifest.is_file() {
                candidates.insert(manifest);
            }
        }
    }
}

fn validate_source_format(value: &str) -> Result<()> {
    let valid =
        !value.trim().is_empty() && value.len() <= 512 && !value.chars().any(char::is_control);
    if valid {
        Ok(())
    } else {
        Err(anyhow!(
            "source_format must be non-empty, at most 512 bytes, and contain no control characters"
        ))
    }
}

fn validate_plugin_id(label: &str, value: &str) -> Result<()> {
    let valid = !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    if valid {
        Ok(())
    } else {
        Err(anyhow!(
            "{label} must be 1 to 128 bytes, start with a lowercase ASCII letter or digit, and use only lowercase ASCII letters, digits, '.', '_', or '-'"
        ))
    }
}

fn resolve_manifest_path(manifest_dir: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        manifest_dir.join(path)
    }
}

fn shell_like_command(command: &[String]) -> String {
    command.join(" ")
}

fn stderr_snippet(value: &str) -> String {
    let mut snippet = value
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(12)
        .collect::<Vec<_>>()
        .join(" | ");
    if snippet.len() > MAX_PLUGIN_STDERR_SNIPPET_BYTES {
        snippet.truncate(MAX_PLUGIN_STDERR_SNIPPET_BYTES);
        snippet.push_str("...");
    }
    snippet
}
