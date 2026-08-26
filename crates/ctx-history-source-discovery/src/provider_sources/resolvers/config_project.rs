use std::{
    ffi::OsStr,
    path::{Component, Path, PathBuf},
};

use ctx_history_core::CaptureProvider;
use serde_json::Value;

use super::{
    super::{
        context::{DiscoveryContext, DiscoveryPlatform},
        selectors::{
            ordinary_empty_file, SelectorDocument, SelectorFormat, SelectorReadError,
            SelectorReader, MAX_PROJECT_ANCESTORS,
        },
        types::{
            DiscoveryIssueKind, DiscoveryReport, ProviderSourceKind, ProviderSourceRouteProvenance,
            ProviderSourceSpec,
        },
        StaticProviderProbeCatalog,
    },
    dedupe_report, issue, path_presence, push_source_candidate, source_from_parts, PathPresence,
};

mod crush;
mod pi;
mod qwen;
mod roo;
mod rovo;
mod vibe;

#[cfg(test)]
use crush::CrushProjectSelectorKey;
pub use crush::{
    resolve_crush_released_project_inventories, resolve_crush_released_project_inventory,
    CrushDiscoveredProjectInventory, CrushProjectInventorySelector,
    CrushProjectInventorySelectorError, CrushReleasedProjectInventory,
};

const PI_FORMAT: &str = "pi_session_jsonl";
const CRUSH_FORMAT: &str = "crush_sqlite";
const QWEN_FORMAT: &str = "qwen_code_chat_jsonl_tree";
const VIBE_FORMAT: &str = "mistral_vibe_session_jsonl_tree";
const ROVO_FORMAT: &str = "rovodev_session_json_tree";
const ROO_FORMAT: &str = "roo_task_directory_json";

const MANUAL_SELECTOR_REASON: &str =
    "the provider selected a history root that cannot be reconstructed safely; use an exact --path";
const UNSAFE_SELECTOR_REASON: &str =
    "the selected history root crosses a link, network, or consent boundary; use an exact --path";
const INVALID_SELECTOR_REASON: &str =
    "a winning provider selector is malformed or unreadable; the stale default is suppressed";
const PROJECT_TRUST_REASON: &str =
    "project history configuration is not covered by persisted provider trust; use an exact --path";
const SELECTOR_LIMIT_REASON: &str =
    "provider selector discovery exceeded a fixed local bound; use an exact --path";

pub(super) fn resolve(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
) -> DiscoveryReport {
    let report = match spec.provider {
        CaptureProvider::Pi => pi::resolve(probes, context, spec),
        CaptureProvider::Crush => crush::resolve(probes, context, spec),
        CaptureProvider::QwenCode => qwen::resolve(probes, context, spec),
        CaptureProvider::MistralVibe => vibe::resolve(probes, context, spec),
        CaptureProvider::RovoDev => rovo::resolve(probes, context, spec),
        CaptureProvider::RooCode => roo::resolve(probes, context, spec),
        _ => DiscoveryReport::default(),
    };
    dedupe_report(report)
}

fn add_source(
    probes: &StaticProviderProbeCatalog,
    report: &mut DiscoveryReport,
    spec: &ProviderSourceSpec,
    path: PathBuf,
    format: &'static str,
) {
    add_source_with_route_provenance(
        probes,
        report,
        spec,
        path,
        format,
        ProviderSourceRouteProvenance::Unroled,
    );
}

fn add_source_with_route_provenance(
    probes: &StaticProviderProbeCatalog,
    report: &mut DiscoveryReport,
    spec: &ProviderSourceSpec,
    path: PathBuf,
    format: &'static str,
    route_provenance: ProviderSourceRouteProvenance,
) {
    if !path_is_safe_for_automatic_read(&path) {
        report.issues.push(issue(
            spec.provider,
            None,
            DiscoveryIssueKind::SelectorUnreconstructible,
            UNSAFE_SELECTOR_REASON,
        ));
        return;
    }
    let mut source = source_from_parts(
        probes,
        spec,
        path,
        format,
        ProviderSourceKind::NativeHistory,
    );
    source.route_provenance = route_provenance;
    if !push_source_candidate(&mut report.sources, source) {
        report.issues.push(issue(
            spec.provider,
            None,
            DiscoveryIssueKind::SelectorUnreconstructible,
            SELECTOR_LIMIT_REASON,
        ));
    }
}

fn add_manual_issue(report: &mut DiscoveryReport, provider: CaptureProvider, reason: &'static str) {
    report.issues.push(issue(
        provider,
        None,
        DiscoveryIssueKind::SelectorUnreconstructible,
        reason,
    ));
}

fn path_is_safe_for_automatic_read(path: &Path) -> bool {
    matches!(
        path_presence(path),
        PathPresence::Missing | PathPresence::Present
    )
}

fn supported_desktop_platform(context: &DiscoveryContext) -> bool {
    !matches!(context.platform(), DiscoveryPlatform::OtherUnix)
}

#[derive(Debug)]
enum OptionalDocument {
    Missing,
    Empty,
    Present(SelectorDocument),
}

fn read_optional(
    reader: &mut SelectorReader,
    path: &Path,
    format: SelectorFormat,
) -> Result<OptionalDocument, SelectorReadError> {
    match path_presence(path) {
        PathPresence::Missing => Ok(OptionalDocument::Missing),
        PathPresence::Unknown(_) => Err(SelectorReadError::Unavailable),
        PathPresence::Unsupported => Err(SelectorReadError::UnsupportedRoot),
        PathPresence::Present => {
            if !ordinary_empty_file(path)? {
                return reader.read(path, format).map(OptionalDocument::Present);
            }
            Ok(OptionalDocument::Empty)
        }
    }
}

fn structured(document: &SelectorDocument) -> Option<&Value> {
    match document {
        SelectorDocument::Structured(value) => Some(value),
        SelectorDocument::Xml(_) => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum StringSetting {
    Missing,
    Reset,
    Value(String),
    Invalid,
}

fn string_setting(value: &Value, path: &[&str]) -> StringSetting {
    let mut selected = value;
    for component in path {
        let Some(next) = selected.as_object().and_then(|map| map.get(*component)) else {
            return StringSetting::Missing;
        };
        selected = next;
    }
    match selected {
        Value::Null => StringSetting::Reset,
        Value::String(value) if value.is_empty() => StringSetting::Reset,
        Value::String(value) => StringSetting::Value(value.clone()),
        _ => StringSetting::Invalid,
    }
}

fn bool_setting(value: &Value, path: &[&str]) -> Result<Option<bool>, ()> {
    let mut selected = value;
    for component in path {
        let Some(next) = selected.as_object().and_then(|map| map.get(*component)) else {
            return Ok(None);
        };
        selected = next;
    }
    selected.as_bool().map(Some).ok_or(())
}

fn resolve_expand_user(
    raw: &str,
    home: &Path,
    relative_base: Option<&Path>,
    windows_tilde: bool,
) -> Result<PathBuf, ()> {
    let path = if raw == "~" {
        home.to_path_buf()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        home.join(rest)
    } else if windows_tilde {
        if let Some(rest) = raw.strip_prefix("~\\") {
            home.join(rest.replace('\\', "/"))
        } else {
            PathBuf::from(raw)
        }
    } else {
        PathBuf::from(raw)
    };
    if path.is_absolute() {
        Ok(lexical_normalize(&path))
    } else {
        relative_base
            .map(|base| lexical_normalize(&base.join(path)))
            .ok_or(())
    }
}

fn resolve_os_path(raw: &OsStr, relative_base: Option<&Path>) -> Result<PathBuf, ()> {
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        Ok(lexical_normalize(&path))
    } else {
        relative_base
            .map(|base| lexical_normalize(&base.join(path)))
            .ok_or(())
    }
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !output.pop() {
                    output.push(component.as_os_str());
                }
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                output.push(component.as_os_str());
            }
        }
    }
    output
}

fn local_absolute_path(path: &Path) -> bool {
    path.is_absolute() && !is_network_path(path)
}

fn is_network_path(path: &Path) -> bool {
    let text = path.as_os_str().to_string_lossy();
    text.starts_with("//") || text.starts_with("\\\\")
}

fn is_within(path: &Path, root: &Path) -> bool {
    lexical_normalize(path).starts_with(lexical_normalize(root))
}

fn canonical_comparison_path(path: &Path) -> PathBuf {
    lexical_normalize(path)
}

fn git_bounded_ancestors(cwd: &Path) -> Vec<PathBuf> {
    let mut walked = Vec::new();
    for candidate in cwd.ancestors().take(MAX_PROJECT_ANCESTORS) {
        walked.push(candidate.to_path_buf());
        let marker = candidate.join(".git");
        match path_presence(&marker) {
            PathPresence::Missing => {}
            PathPresence::Present => return walked,
            PathPresence::Unsupported | PathPresence::Unknown(_) => return vec![cwd.to_path_buf()],
        }
    }
    vec![cwd.to_path_buf()]
}

#[cfg(test)]
#[path = "config_project_tests.rs"]
mod tests;
