use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

const PLUGIN_MANIFEST_FILE: &str = "ctx-history-plugin.json";
const MAX_PLUGIN_MANIFEST_BYTES: usize = 1024 * 1024;

/// Discovery metadata for a legacy history-source plugin.
///
/// v0.26 does not execute these commands: a plugin must gain a source-backed
/// adapter before it can participate in the new history epoch. Keeping bounded
/// manifest discovery lets `ctx sources` explain that state without retaining a
/// second ingestion mechanism.
#[derive(Debug, Clone)]
pub struct HistorySourcePluginSource {
    pub plugin_name: String,
    pub plugin_display_name: Option<String>,
    pub plugin_version: Option<String>,
    pub manifest_path: PathBuf,
    pub id: String,
    pub display_name: Option<String>,
    pub provider_key: String,
    pub source_id: String,
    pub source_format: String,
    pub enabled: bool,
    pub refresh: HistorySourcePluginRefresh,
}

impl HistorySourcePluginSource {
    pub fn label(&self) -> String {
        format!("{}/{}", self.plugin_name, self.id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum HistorySourcePluginRefresh {
    #[default]
    Manual,
    Auto,
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
    #[serde(default, rename = "working_dir")]
    _working_dir: Option<PathBuf>,
    #[serde(default, rename = "env")]
    _env: BTreeMap<String, String>,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    refresh: HistorySourcePluginRefresh,
    #[serde(default, rename = "timeout_seconds")]
    _timeout_seconds: Option<u64>,
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
    sources.sort_by_key(HistorySourcePluginSource::label);
    Ok(HistorySourcePluginDiscovery { sources, failures })
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
            id: source.id,
            display_name: source.display_name,
            provider_key,
            source_id,
            source_format: source.source_format,
            enabled: source.enabled,
            refresh: source.refresh,
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
        } else if child.is_dir() {
            let manifest = child.join(PLUGIN_MANIFEST_FILE);
            if manifest.is_file() {
                candidates.insert(manifest);
            }
        }
    }
}

fn validate_source_format(value: &str) -> Result<()> {
    if !value.trim().is_empty() && value.len() <= 512 && !value.chars().any(char::is_control) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_keeps_metadata_but_no_executor_state() {
        let temp = tempfile::tempdir().unwrap();
        let manifest = temp.path().join(PLUGIN_MANIFEST_FILE);
        fs::write(
            &manifest,
            r#"{
                "schema_version": 1,
                "name": "example",
                "history_sources": [{
                    "id": "default",
                    "source_format": "ctx_history_jsonl_v1",
                    "command": ["example-export"]
                }]
            }"#,
        )
        .unwrap();
        let sources = read_plugin_manifest(&manifest).unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].label(), "example/default");
    }
}
