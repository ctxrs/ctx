use std::path::{Path, PathBuf};

use ctx_history_capture_model::{
    ProviderRootDefinition, ProviderRootKind, ProviderRouteRole, ProviderSourceRouteProvenance,
};
use ctx_history_core::CaptureProvider;

use super::{
    context::DiscoveryContext,
    probes::{has_openclaw_agent_sqlite_v17, BoundedProbe},
    reasons::path_presence_unknown_reason,
    resolvers::{
        issue, openclaw_agent_ids_for_state_root, path_presence, provider_paths_equivalent,
        push_source_candidate, source_from_parts_with_data_root, unsupported_source,
        OpenClawConfigError,
    },
    selectors::{encoded_path_within_limit, source_path_kind, SourcePathError, SourcePathKind},
    types::{
        DiscoveryIssueKind, DiscoveryReport, ProviderSource, ProviderSourceKind,
        ProviderSourceSpec, ProviderSourceStatus,
    },
    StaticProviderProbeCatalog,
};

mod capabilities;

const CONFIGURED_ROOT_SYMLINK_REASON: &str =
    "the configured provider history root uses a symlink or other unsupported component";
const CONFIGURED_SOURCE_SYMLINK_REASON: &str =
    "a configured provider history child uses a symlink or other unsupported component";
const CONFIGURED_ROOT_DIRECTORY_REASON: &str =
    "the configured provider history root must be an ordinary directory";
const CONFIGURED_ROOT_FILE_REASON: &str =
    "the configured provider history root must be an ordinary file";
const CONFIGURED_SOURCE_DIRECTORY_REASON: &str =
    "the configured provider history child must be an ordinary directory";
const CONFIGURED_SOURCE_FILE_REASON: &str =
    "the configured provider history child must be an ordinary file";
const CONFIGURED_ROOT_PATH_LIMIT_REASON: &str =
    "the configured provider history path exceeds the discovery path limit";
const CONFIGURED_ROOT_CONFLICT_REASON: &str =
    "distinct configured roots resolve to the same physical provider root";
const OPENHANDS_CONFIGURED_ROOT_CONFLICT_REASON: &str =
    "configured OpenHands legacy persistence owns the nested current-conversations history root";
const OPENHANDS_CONFIGURED_ROOT_KIND_REASON: &str =
    "configured OpenHands history root requires a valid kind";
const CONFIGURED_ROOT_ROLE_LIMIT_REASON: &str =
    "the configured provider history child role exceeds the route-role limit";
const OPENCLAW_CONFIG_INVALID_REASON: &str =
    "the configured OpenClaw state root has an invalid or unsafe agent registry";
const OPENCLAW_CONFIG_LIMIT_REASON: &str =
    "the configured OpenClaw state root exceeds a bounded agent-registry limit";
const OPENCLAW_UNSUPPORTED_REASON: &str =
    "OpenClaw openclaw-agent.sqlite does not satisfy the bounded current v17 schema and ownership contract";
const CONFIGURED_ROOT_MISSING_REASON: &str = "the configured provider history root is missing";

/// Filesystem kind required by a configured-root capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfiguredRootPathKind {
    Directory,
    File,
}

/// Frozen expansion strategy for one enabled configured-root capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfiguredRootExpander {
    /// The configured path is the exact landed history tree or database.
    ExactSource {
        source_format: &'static str,
        route_role: &'static str,
    },
    /// Existing v9 Claude home expansion and role bytes.
    ClaudeHomeV1,
    /// Existing v9 Codex home expansion and role bytes.
    CodexHomeV1,
    /// OpenClaw state/profile root with bounded configured-agent expansion.
    OpenClawStateRootV1,
    /// Cline common data/store root with independent task and SDK routes.
    ClineCommonDataRootV1,
    /// OpenHands exact root whose native layout is selected explicitly.
    OpenHandsKindV1,
}

/// Support state and complete expansion metadata for one landed provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfiguredRootCapabilityState {
    Enabled {
        expected_path_kind: ConfiguredRootPathKind,
        expander: ConfiguredRootExpander,
    },
    /// The provider intentionally retains automatic discovery and exact import
    /// without a persistent configured-root contract.
    IntentionalAutomaticExact,
    /// A named-root contract is pending an unresolved public shape decision.
    PendingNamedSupport,
}

impl ConfiguredRootCapabilityState {
    pub const fn is_enabled(self) -> bool {
        matches!(self, Self::Enabled { .. })
    }

    pub const fn expected_path_kind(self) -> Option<ConfiguredRootPathKind> {
        match self {
            Self::Enabled {
                expected_path_kind, ..
            } => Some(expected_path_kind),
            Self::IntentionalAutomaticExact | Self::PendingNamedSupport => None,
        }
    }

    pub const fn expander(self) -> Option<ConfiguredRootExpander> {
        match self {
            Self::Enabled { expander, .. } => Some(expander),
            Self::IntentionalAutomaticExact | Self::PendingNamedSupport => None,
        }
    }
}

/// Provider-neutral configured-root capability row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfiguredRootCapability {
    pub provider: CaptureProvider,
    pub state: ConfiguredRootCapabilityState,
}

pub fn configured_root_capabilities() -> &'static [ConfiguredRootCapability] {
    capabilities::CONFIGURED_ROOT_CAPABILITIES
}

pub fn configured_root_capability(
    provider: CaptureProvider,
) -> Option<&'static ConfiguredRootCapability> {
    capabilities::CONFIGURED_ROOT_CAPABILITIES
        .iter()
        .find(|capability| capability.provider == provider)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfiguredRootAvailability {
    Present,
    Missing,
    Unavailable,
    Unsafe(&'static str),
}

#[derive(Debug, Clone, Copy)]
struct StaticRouteExpansion {
    relative_path: &'static [&'static str],
    expected_path_kind: ConfiguredRootPathKind,
    source_format: &'static str,
    route_role: &'static str,
}

const CLAUDE_ROUTES: &[StaticRouteExpansion] = &[StaticRouteExpansion {
    relative_path: &["projects"],
    expected_path_kind: ConfiguredRootPathKind::Directory,
    source_format: "claude_projects_jsonl_tree",
    route_role: "claude-projects",
}];

const CODEX_ROUTES: &[StaticRouteExpansion] = &[
    StaticRouteExpansion {
        relative_path: &["sessions"],
        expected_path_kind: ConfiguredRootPathKind::Directory,
        source_format: "codex_session_jsonl_tree",
        route_role: "codex-sessions",
    },
    StaticRouteExpansion {
        relative_path: &["archived_sessions"],
        expected_path_kind: ConfiguredRootPathKind::Directory,
        source_format: "codex_session_jsonl_tree",
        route_role: "codex-archived-sessions",
    },
    StaticRouteExpansion {
        relative_path: &["history.jsonl"],
        expected_path_kind: ConfiguredRootPathKind::File,
        source_format: "codex_history_jsonl",
        route_role: "codex-prompt-history",
    },
];

const CLINE_ROUTES: &[StaticRouteExpansion] = &[
    StaticRouteExpansion {
        relative_path: &[],
        expected_path_kind: ConfiguredRootPathKind::Directory,
        source_format: "cline_task_directory_json",
        route_role: "cline-tasks",
    },
    StaticRouteExpansion {
        relative_path: &[],
        expected_path_kind: ConfiguredRootPathKind::Directory,
        source_format: "cline_sdk_session_store",
        route_role: "cline-sdk",
    },
];

pub(super) fn expand_configured_roots_for_provider(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
    spec: &ProviderSourceSpec,
) -> DiscoveryReport {
    let Some(capability) = configured_root_capability(spec.provider) else {
        return DiscoveryReport::default();
    };
    let ConfiguredRootCapabilityState::Enabled {
        expected_path_kind,
        expander,
    } = capability.state
    else {
        return DiscoveryReport::default();
    };

    let mut report = DiscoveryReport::default();
    let roots = context
        .configured_provider_roots()
        .iter()
        .filter(|root| root.provider == spec.provider)
        .collect::<Vec<_>>();
    if let Some(conflicting) = roots.iter().enumerate().find_map(|(index, left)| {
        roots[index + 1..]
            .iter()
            .find(|right| provider_paths_equivalent(&left.path, &right.path))
            .copied()
    }) {
        report.issues.push(issue(
            spec.provider,
            Some(conflicting.path.clone()),
            DiscoveryIssueKind::ConfiguredRootConflict,
            CONFIGURED_ROOT_CONFLICT_REASON,
        ));
        return report;
    }
    if roots.iter().enumerate().any(|(index, left)| {
        roots[index + 1..]
            .iter()
            .any(|right| left.openhands_selected_histories_overlap(right))
    }) {
        report.issues.push(issue(
            spec.provider,
            None,
            DiscoveryIssueKind::ConfiguredRootConflict,
            OPENHANDS_CONFIGURED_ROOT_CONFLICT_REASON,
        ));
        return report;
    }
    for root in roots {
        if !root.has_valid_kind() {
            report.issues.push(issue(
                spec.provider,
                Some(root.path.clone()),
                DiscoveryIssueKind::SelectorUnreconstructible,
                OPENHANDS_CONFIGURED_ROOT_KIND_REASON,
            ));
            continue;
        }
        let Some(availability) =
            inspect_configured_path(&mut report, spec, &root.path, expected_path_kind, true)
        else {
            continue;
        };
        if matches!(availability, ConfiguredRootAvailability::Unsafe(_)) {
            continue;
        }
        match expander {
            ConfiguredRootExpander::ExactSource {
                source_format,
                route_role,
            } => add_static_routes(
                probes,
                context.data_root(),
                &mut report,
                spec,
                root,
                &[StaticRouteExpansion {
                    relative_path: &[],
                    expected_path_kind,
                    source_format,
                    route_role,
                }],
            ),
            ConfiguredRootExpander::ClaudeHomeV1 => add_static_routes(
                probes,
                context.data_root(),
                &mut report,
                spec,
                root,
                CLAUDE_ROUTES,
            ),
            ConfiguredRootExpander::CodexHomeV1 => add_static_routes(
                probes,
                context.data_root(),
                &mut report,
                spec,
                root,
                CODEX_ROUTES,
            ),
            ConfiguredRootExpander::OpenClawStateRootV1 => expand_openclaw_state_root(
                probes,
                context.data_root(),
                &mut report,
                spec,
                root,
                availability,
            ),
            ConfiguredRootExpander::ClineCommonDataRootV1 => add_static_routes(
                probes,
                context.data_root(),
                &mut report,
                spec,
                root,
                CLINE_ROUTES,
            ),
            ConfiguredRootExpander::OpenHandsKindV1 => {
                let Some((source_format, route_role)) = openhands_configured_root_route(root)
                else {
                    push_issue_once(
                        &mut report,
                        spec,
                        Some(root.path.clone()),
                        DiscoveryIssueKind::SelectorUnreconstructible,
                        OPENHANDS_CONFIGURED_ROOT_KIND_REASON,
                    );
                    continue;
                };
                add_static_routes(
                    probes,
                    context.data_root(),
                    &mut report,
                    spec,
                    root,
                    &[StaticRouteExpansion {
                        relative_path: &[],
                        expected_path_kind,
                        source_format,
                        route_role,
                    }],
                );
            }
        }
    }
    report
}

fn openhands_configured_root_route(
    root: &ProviderRootDefinition,
) -> Option<(&'static str, &'static str)> {
    match root.kind {
        Some(ProviderRootKind::OpenHandsCurrentConversations) => Some((
            "openhands_cli_file_events",
            "openhands-current-conversations",
        )),
        Some(ProviderRootKind::OpenHandsLegacyPersistence) => {
            Some(("openhands_file_events", "openhands-legacy-persistence"))
        }
        None => None,
    }
}

fn add_static_routes(
    probes: &StaticProviderProbeCatalog,
    data_root: Option<&Path>,
    report: &mut DiscoveryReport,
    spec: &ProviderSourceSpec,
    root: &ProviderRootDefinition,
    expansions: &[StaticRouteExpansion],
) {
    for expansion in expansions {
        let path = expansion
            .relative_path
            .iter()
            .fold(root.path.clone(), |path, component| path.join(component));
        let role = ProviderRouteRole::from_static(expansion.route_role);
        if let Some(source) = build_configured_source(
            probes,
            data_root,
            report,
            spec,
            root,
            path,
            expansion.expected_path_kind,
            expansion.source_format,
            role,
        ) {
            push_configured_source(report, spec, source);
        }
    }
}

fn expand_openclaw_state_root(
    probes: &StaticProviderProbeCatalog,
    data_root: Option<&Path>,
    report: &mut DiscoveryReport,
    spec: &ProviderSourceSpec,
    root: &ProviderRootDefinition,
    availability: ConfiguredRootAvailability,
) {
    if availability == ConfiguredRootAvailability::Missing {
        // OpenClaw membership is rooted in the state directory. A missing
        // state root cannot safely invent an agent route, but it is still a
        // durable configured root and must remain diagnosable/listable.
        report.issues.push(issue(
            spec.provider,
            Some(root.path.clone()),
            DiscoveryIssueKind::ConfiguredRootMissing,
            CONFIGURED_ROOT_MISSING_REASON,
        ));
        return;
    }
    if availability != ConfiguredRootAvailability::Present {
        // A non-present root cannot safely enumerate agent membership. Leave
        // it route-less so refresh can retain exact prior membership instead
        // of inventing `main`.
        return;
    }
    let (agent_ids, truncated) = match openclaw_agent_ids_for_state_root(&root.path) {
        Ok(inventory) => inventory,
        Err(OpenClawConfigError::Invalid) => {
            push_issue_once(
                report,
                spec,
                Some(root.path.clone()),
                DiscoveryIssueKind::SelectorUnreconstructible,
                OPENCLAW_CONFIG_INVALID_REASON,
            );
            return;
        }
        Err(OpenClawConfigError::Limit) => {
            push_issue_once(
                report,
                spec,
                Some(root.path.clone()),
                DiscoveryIssueKind::SelectorUnreconstructible,
                OPENCLAW_CONFIG_LIMIT_REASON,
            );
            return;
        }
    };
    if truncated {
        push_issue_once(
            report,
            spec,
            Some(root.path.clone()),
            DiscoveryIssueKind::SelectorUnreconstructible,
            OPENCLAW_CONFIG_LIMIT_REASON,
        );
        // The bounded inventory is not an authoritative membership list.
        // Leaving this root route-less lets refresh retain an authenticated
        // prior membership; a first-time root publishes no partial selector.
        return;
    }

    for agent_id in agent_ids {
        let Ok(route_role) =
            ProviderRouteRole::from_dynamic([b"openclaw-agent".as_slice(), agent_id.as_bytes()])
        else {
            push_issue_once(
                report,
                spec,
                None,
                DiscoveryIssueKind::SelectorUnreconstructible,
                CONFIGURED_ROOT_ROLE_LIMIT_REASON,
            );
            continue;
        };
        expand_openclaw_agent(probes, data_root, report, spec, root, &agent_id, route_role);
    }
}

#[allow(clippy::too_many_arguments)]
fn expand_openclaw_agent(
    probes: &StaticProviderProbeCatalog,
    data_root: Option<&Path>,
    report: &mut DiscoveryReport,
    spec: &ProviderSourceSpec,
    root: &ProviderRootDefinition,
    agent_id: &str,
    route_role: ProviderRouteRole,
) {
    let agent_root = root.path.join("agents").join(agent_id);
    let sqlite = agent_root.join("agent/openclaw-agent.sqlite");
    let sessions = agent_root.join("sessions");

    if has_openclaw_agent_sqlite_v17(data_root, &sqlite) == BoundedProbe::Found {
        if let Some(source) = build_configured_source(
            probes,
            data_root,
            report,
            spec,
            root,
            sqlite,
            ConfiguredRootPathKind::File,
            ctx_history_openclaw_schema::OPENCLAW_AGENT_SQLITE_SOURCE_FORMAT,
            route_role,
        ) {
            push_configured_source(report, spec, source);
        }
        return;
    }

    let jsonl = build_configured_source(
        probes,
        data_root,
        report,
        spec,
        root,
        sessions,
        ConfiguredRootPathKind::Directory,
        "openclaw_session_jsonl_tree",
        route_role.clone(),
    );
    let sqlite_suppresses_fallback = path_presence(&sqlite).suppresses_fallback();
    if !sqlite_suppresses_fallback
        || jsonl
            .as_ref()
            .is_some_and(|source| source.status == ProviderSourceStatus::Available)
    {
        if let Some(source) = jsonl {
            push_configured_source(report, spec, source);
        }
        return;
    }

    let Some(sqlite_availability) =
        inspect_configured_path(report, spec, &sqlite, ConfiguredRootPathKind::File, false)
    else {
        return;
    };
    if matches!(sqlite_availability, ConfiguredRootAvailability::Unsafe(_)) {
        return;
    }

    let mut source = unsupported_source(spec, sqlite, OPENCLAW_UNSUPPORTED_REASON);
    apply_configured_provenance(&mut source, root, route_role);
    push_configured_source(report, spec, source);
}

#[allow(clippy::too_many_arguments)]
fn build_configured_source(
    probes: &StaticProviderProbeCatalog,
    data_root: Option<&Path>,
    report: &mut DiscoveryReport,
    spec: &ProviderSourceSpec,
    root: &ProviderRootDefinition,
    path: PathBuf,
    expected_path_kind: ConfiguredRootPathKind,
    source_format: &'static str,
    route_role: ProviderRouteRole,
) -> Option<ProviderSource> {
    let availability =
        inspect_configured_path(report, spec, &path, expected_path_kind, path == root.path)?;
    let mut source = match availability {
        ConfiguredRootAvailability::Unsafe(reason) => ProviderSource {
            provider: spec.provider,
            path,
            exists: true,
            source_format,
            source_kind: if spec.import_support.is_importable() {
                ProviderSourceKind::NativeHistory
            } else {
                ProviderSourceKind::DetectionOnly
            },
            import_support: spec.import_support,
            catalog_support: spec.catalog_support,
            status: ProviderSourceStatus::Unknown,
            unsupported_reason: Some(reason),
            route_provenance: Default::default(),
        },
        ConfiguredRootAvailability::Present
        | ConfiguredRootAvailability::Missing
        | ConfiguredRootAvailability::Unavailable => source_from_parts_with_data_root(
            probes,
            data_root,
            spec,
            path,
            source_format,
            ProviderSourceKind::NativeHistory,
        ),
    };
    apply_configured_provenance(&mut source, root, route_role);
    Some(source)
}

fn inspect_configured_path(
    report: &mut DiscoveryReport,
    spec: &ProviderSourceSpec,
    path: &Path,
    expected_path_kind: ConfiguredRootPathKind,
    is_root: bool,
) -> Option<ConfiguredRootAvailability> {
    if !encoded_path_within_limit(path) {
        push_issue_once(
            report,
            spec,
            None,
            DiscoveryIssueKind::SelectorUnreconstructible,
            CONFIGURED_ROOT_PATH_LIMIT_REASON,
        );
        return None;
    }
    match source_path_kind(path) {
        Ok(actual) if source_path_kind_matches(expected_path_kind, actual) => {
            Some(ConfiguredRootAvailability::Present)
        }
        Ok(_) => {
            let reason = configured_path_kind_reason(expected_path_kind, is_root);
            push_issue_once(
                report,
                spec,
                Some(path.to_path_buf()),
                DiscoveryIssueKind::SelectorUnreconstructible,
                reason,
            );
            Some(ConfiguredRootAvailability::Unsafe(reason))
        }
        Err(SourcePathError::Missing) => Some(ConfiguredRootAvailability::Missing),
        Err(SourcePathError::Unsupported) => {
            let reason = if is_root {
                CONFIGURED_ROOT_SYMLINK_REASON
            } else {
                CONFIGURED_SOURCE_SYMLINK_REASON
            };
            push_issue_once(
                report,
                spec,
                Some(path.to_path_buf()),
                DiscoveryIssueKind::SelectorUnreconstructible,
                reason,
            );
            Some(ConfiguredRootAvailability::Unsafe(reason))
        }
        Err(SourcePathError::Unavailable(kind)) => {
            push_issue_once(
                report,
                spec,
                Some(path.to_path_buf()),
                DiscoveryIssueKind::SelectorUnreconstructible,
                path_presence_unknown_reason(kind),
            );
            Some(ConfiguredRootAvailability::Unavailable)
        }
    }
}

fn source_path_kind_matches(expected: ConfiguredRootPathKind, actual: SourcePathKind) -> bool {
    matches!(
        (expected, actual),
        (ConfiguredRootPathKind::Directory, SourcePathKind::Directory)
            | (ConfiguredRootPathKind::File, SourcePathKind::File)
    )
}

fn configured_path_kind_reason(expected: ConfiguredRootPathKind, is_root: bool) -> &'static str {
    match (expected, is_root) {
        (ConfiguredRootPathKind::Directory, true) => CONFIGURED_ROOT_DIRECTORY_REASON,
        (ConfiguredRootPathKind::File, true) => CONFIGURED_ROOT_FILE_REASON,
        (ConfiguredRootPathKind::Directory, false) => CONFIGURED_SOURCE_DIRECTORY_REASON,
        (ConfiguredRootPathKind::File, false) => CONFIGURED_SOURCE_FILE_REASON,
    }
}

fn apply_configured_provenance(
    source: &mut ProviderSource,
    root: &ProviderRootDefinition,
    route_role: ProviderRouteRole,
) {
    source.route_provenance = ProviderSourceRouteProvenance::ConfiguredRoot {
        root_id: root.id.clone(),
        root_path: root.path.clone(),
        route_role,
        automatic_route_role: None,
    };
}

fn push_configured_source(
    report: &mut DiscoveryReport,
    spec: &ProviderSourceSpec,
    source: ProviderSource,
) {
    if !push_source_candidate(&mut report.sources, source) {
        push_issue_once(
            report,
            spec,
            None,
            DiscoveryIssueKind::SelectorUnreconstructible,
            CONFIGURED_ROOT_PATH_LIMIT_REASON,
        );
    }
}

fn push_issue_once(
    report: &mut DiscoveryReport,
    spec: &ProviderSourceSpec,
    path: Option<PathBuf>,
    kind: DiscoveryIssueKind,
    reason: &'static str,
) {
    if !report
        .issues
        .iter()
        .any(|existing| existing.kind == kind && existing.reason == reason)
    {
        report.issues.push(issue(spec.provider, path, kind, reason));
    }
}
