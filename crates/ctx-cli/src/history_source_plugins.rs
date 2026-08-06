use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use ctx_history_core::CtxHistoryJsonlLineageContract;
use serde::Deserialize;

mod source_backed;

pub(crate) use source_backed::{
    prepare_source_backed_history_source, COMMAND_ONLY_UNSUPPORTED_REASON,
};

const PLUGIN_MANIFEST_FILE: &str = "ctx-history-plugin.json";
const MAX_PLUGIN_MANIFEST_BYTES: usize = 1024 * 1024;

/// Discovery and execution metadata for a manifest-backed history source.
///
/// Durable provider-owned paths use the existing `ctx-history-jsonl-v1`
/// interchange format and are registered with the sole source-backed
/// generation path. Command-only exports remain visible but unsupported.
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
    pub source_path: Option<PathBuf>,
    pub lineage_contract: Option<CtxHistoryJsonlLineageContract>,
    pub enabled: bool,
    pub refresh: HistorySourcePluginRefresh,
}

impl HistorySourcePluginSource {
    pub fn label(&self) -> String {
        format!("{}/{}", self.plugin_name, self.id)
    }

    pub fn history_source(&self) -> String {
        format!("{}/{}", self.provider_key, self.source_id)
    }

    pub fn matches_selector(&self, selector: &str) -> bool {
        selector == self.label() || selector == self.history_source()
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
    #[serde(default, rename = "path")]
    source_path: Option<PathBuf>,
    #[serde(default)]
    lineage_contract: Option<CtxHistoryJsonlLineageContract>,
    #[serde(default)]
    command: Vec<String>,
    #[serde(default)]
    working_dir: Option<PathBuf>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    refresh: HistorySourcePluginRefresh,
    #[serde(default, rename = "timeout_seconds")]
    timeout_seconds: Option<u64>,
}

pub fn discover_history_source_plugins(
    data_root: &Path,
    extra_manifests: &[PathBuf],
) -> Result<Vec<HistorySourcePluginSource>> {
    Ok(discover_history_source_plugins_with_diagnostics(data_root, extra_manifests)?.sources)
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
    sources.sort_by(|left, right| {
        left.label()
            .cmp(&right.label())
            .then_with(|| left.manifest_path.cmp(&right.manifest_path))
    });
    sources.dedup_by(|left, right| {
        left.manifest_path == right.manifest_path
            && left.plugin_name == right.plugin_name
            && left.id == right.id
    });
    Ok(HistorySourcePluginDiscovery { sources, failures })
}

pub(crate) fn select_history_source_plugin(
    data_root: &Path,
    extra_manifests: &[PathBuf],
    selector: Option<&str>,
) -> Result<HistorySourcePluginSource> {
    let sources = discover_history_source_plugins(data_root, extra_manifests)?;
    let matches = if let Some(selector) = selector {
        sources
            .into_iter()
            .filter(|source| source.matches_selector(selector))
            .collect::<Vec<_>>()
    } else {
        sources
            .into_iter()
            .filter(|source| {
                extra_manifests
                    .iter()
                    .any(|path| manifest_arg_matches_source(path, &source.manifest_path))
            })
            .collect::<Vec<_>>()
    };
    if matches.is_empty() {
        let detail = selector.map_or_else(
            || "the supplied manifest path".to_owned(),
            |selector| format!("`{selector}`"),
        );
        return Err(anyhow!(
            "no history source plugin matched {detail}; use `ctx sources` to inspect configured plugins"
        ));
    }
    if matches.len() > 1 {
        let labels = matches
            .iter()
            .map(|source| format!("{} ({})", source.label(), source.manifest_path.display()))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(anyhow!(
            "history source plugin selection matched multiple sources ({labels}); select one plugin/source or provider_key/source_id"
        ));
    }
    matches
        .into_iter()
        .next()
        .context("selected history source plugin disappeared")
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
    if manifest.history_sources.is_empty() {
        return Err(anyhow!(
            "history source plugin manifest {} must declare at least one history source",
            path.display()
        ));
    }
    let mut sources = Vec::new();
    let mut source_ids = BTreeSet::new();
    let mut route_identities = BTreeSet::new();
    for source in manifest.history_sources {
        validate_plugin_id("history source id", &source.id)?;
        if !source_ids.insert(source.id.clone()) {
            return Err(anyhow!(
                "history source plugin manifest {} declares duplicate history source id `{}`",
                path.display(),
                source.id
            ));
        }
        let provider_key = source.provider_key.unwrap_or_else(|| manifest.name.clone());
        validate_plugin_id("provider_key", &provider_key)?;
        let source_id = source.source_id.unwrap_or_else(|| source.id.clone());
        validate_plugin_id("source_id", &source_id)?;
        if !route_identities.insert((provider_key.clone(), source_id.clone())) {
            return Err(anyhow!(
                "history source plugin manifest {} declares duplicate provider/source route `{provider_key}/{source_id}`",
                path.display()
            ));
        }
        validate_source_format(&source.source_format).with_context(|| {
            format!(
                "history source plugin manifest {} source {} has invalid source_format",
                path.display(),
                source.id
            )
        })?;
        if source.command.iter().any(|part| part.trim().is_empty()) {
            return Err(anyhow!(
                "history source plugin manifest {} source {} has an empty command argument",
                path.display(),
                source.id
            ));
        }
        if source.source_path.is_some() && !source.command.is_empty() {
            return Err(anyhow!(
                "history source plugin manifest {} source {} must declare either a durable path or a command, not both",
                path.display(),
                source.id
            ));
        }
        if source.source_path.is_none() && source.command.is_empty() {
            return Err(anyhow!(
                "history source plugin manifest {} source {} must declare a durable path or a command",
                path.display(),
                source.id
            ));
        }
        if source.lineage_contract.is_some() && source.source_path.is_none() {
            return Err(anyhow!(
                "history source plugin manifest {} source {} cannot declare lineage_contract without a durable provider-owned path",
                path.display(),
                source.id
            ));
        }
        if source.source_path.is_some()
            && (source.working_dir.is_some()
                || !source.env.is_empty()
                || source.timeout_seconds.is_some())
        {
            return Err(anyhow!(
                "history source plugin manifest {} source {} durable path cannot declare command runtime options",
                path.display(),
                source.id
            ));
        }
        for (key, value) in &source.env {
            validate_plugin_env(key, value).with_context(|| {
                format!(
                    "history source plugin manifest {} source {} has invalid env entry",
                    path.display(),
                    source.id
                )
            })?;
        }
        let source_path = source.source_path.map(|source_path| {
            if source_path.is_absolute() {
                source_path
            } else {
                manifest_dir.join(source_path)
            }
        });
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
            source_path,
            lineage_contract: source.lineage_contract,
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

fn manifest_arg_matches_source(arg: &Path, manifest_path: &Path) -> bool {
    if arg.is_file() {
        return same_pathish(arg, manifest_path);
    }
    if arg.is_dir() {
        return manifest_path.starts_with(arg);
    }
    same_pathish(arg, manifest_path)
}

fn same_pathish(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    let left = fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    left == right
}

fn validate_plugin_env(key: &str, value: &str) -> Result<()> {
    if key.is_empty()
        || key.contains('=')
        || key.chars().any(|ch| ch == '\0')
        || value.chars().any(|ch| ch == '\0')
    {
        return Err(anyhow!(
            "environment names must be non-empty and exclude '=' or NUL; values must exclude NUL"
        ));
    }
    Ok(())
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
    fn command_only_manifest_remains_discoverable_as_unsupported() {
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
        assert!(sources[0].source_path.is_none());
    }

    #[test]
    fn command_only_manifest_cannot_claim_lineage_authority() {
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
                    "command": ["example-export"],
                    "lineage_contract": "provider_native_v1"
                }]
            }"#,
        )
        .unwrap();
        let error = read_plugin_manifest(&manifest).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cannot declare lineage_contract without a durable provider-owned path"),
            "{error:#}"
        );
    }
}
