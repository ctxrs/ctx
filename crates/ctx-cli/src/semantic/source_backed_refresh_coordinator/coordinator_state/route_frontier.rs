use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fmt, fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Context, Result};
use ctx_history_capture::{
    SourceBackedWatchCatalog, PROVIDER_JSONL_INVENTORY_MAX_DEPTH,
    PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES, PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES,
};
use ctx_history_index::{SourceRouteIdentity, VerifiedIndex};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::semantic::paths_status::{daemon_root_path, write_private_json_file};

const ROUTE_FRONTIER_FILE: &str = "route-freshness-frontier.json";
const ROUTE_FRONTIER_SCHEMA_VERSION: u32 = 1;
const ROUTE_FRONTIER_MAX_BYTES: u64 = 16 * 1024 * 1024;
const ROUTE_REVISION_MAX_ENTRIES: usize = PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES;
const ROUTE_REVISION_MAX_PATH_BYTES: u64 = (ROUTE_REVISION_MAX_ENTRIES as u64)
    .saturating_mul(PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES as u64);
const ROUTE_REVISION_MAX_DEPTH: usize = PROVIDER_JSONL_INVENTORY_MAX_DEPTH;
const ABSENT_ROUTE_BINDING: &str = "absent";

#[derive(Clone)]
pub(super) struct RouteFreshnessFrontier {
    inner: Arc<Mutex<RouteFreshnessFrontierState>>,
}

struct RouteFreshnessFrontierState {
    data_root: PathBuf,
    path: PathBuf,
    targets: BTreeMap<SourceRouteIdentity, Vec<PathBuf>>,
    observed_target_revisions: BTreeMap<SourceRouteIdentity, TargetRevisionObservation>,
    durable: BTreeMap<SourceRouteIdentity, DurableRouteFrontier>,
}

#[derive(Clone, Debug)]
enum TargetRevisionObservation {
    Revision(String),
    Failed(String),
}

#[derive(Debug)]
pub(super) struct RouteTargetObservationError {
    failures: Vec<(SourceRouteIdentity, String)>,
}

impl fmt::Display for RouteTargetObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "route freshness target observation failed for {} route(s)",
            self.failures.len()
        )?;
        for (route, error) in &self.failures {
            write!(formatter, "; {}: {error}", route.as_str())?;
        }
        Ok(())
    }
}

impl std::error::Error for RouteTargetObservationError {}

struct TargetRevisionObservations {
    revisions: BTreeMap<SourceRouteIdentity, TargetRevisionObservation>,
    failures: Vec<(SourceRouteIdentity, String)>,
}

impl TargetRevisionObservations {
    fn error(&self) -> Option<RouteTargetObservationError> {
        (!self.failures.is_empty()).then(|| RouteTargetObservationError {
            failures: self.failures.clone(),
        })
    }
}

pub(super) struct PreparedRouteFrontierPublication {
    frontier: RouteFreshnessFrontier,
    entries: BTreeMap<SourceRouteIdentity, (String, String)>,
    observation_error: Option<RouteTargetObservationError>,
}

pub(super) struct RouteFrontierReconciliation {
    pub(super) frontier: RouteFreshnessFrontier,
    pub(super) dirty_routes: BTreeSet<SourceRouteIdentity>,
    pub(super) warning: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct DurableRouteFrontier {
    route_identity: String,
    published_route_binding: String,
    target_revision: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DurableRouteFrontierLedger {
    schema_version: u32,
    routes: Vec<DurableRouteFrontier>,
}

impl RouteFreshnessFrontier {
    pub(super) fn reconcile(
        data_root: &Path,
        catalog: &SourceBackedWatchCatalog,
        published: Option<&VerifiedIndex>,
    ) -> RouteFrontierReconciliation {
        let path = daemon_root_path(data_root).join(ROUTE_FRONTIER_FILE);
        let targets = route_targets(catalog);
        let mut warnings = Vec::new();
        let mut durable = match read_durable_frontier(&path) {
            Ok(durable) => durable,
            Err(error) => {
                warnings.push(format!("load durable route freshness frontier: {error:#}"));
                BTreeMap::new()
            }
        };
        durable.retain(|route, _| targets.contains_key(route));

        let observations = observe_target_revisions(&targets);
        if let Some(error) = observations.error() {
            warnings.push(error.to_string());
        }
        let observed_target_revisions = observations.revisions;
        let bindings = match published {
            Some(index) => match published_route_bindings(index, targets.keys()) {
                Ok(bindings) => Some(bindings),
                Err(error) => {
                    warnings.push(format!(
                        "derive published Core route freshness bindings: {error:#}"
                    ));
                    None
                }
            },
            None => None,
        };
        let dirty_routes = routes_outside_frontier(
            targets.keys(),
            &observed_target_revisions,
            bindings.as_ref(),
            &durable,
        );
        let frontier = Self {
            inner: Arc::new(Mutex::new(RouteFreshnessFrontierState {
                data_root: data_root.to_path_buf(),
                path,
                targets,
                observed_target_revisions,
                durable,
            })),
        };
        RouteFrontierReconciliation {
            frontier,
            dirty_routes,
            warning: (!warnings.is_empty()).then(|| warnings.join("; ")),
        }
    }

    /// Refreshes the content-free candidate revision only after the daemon has
    /// admitted the corresponding watcher observation into its dirty ledger.
    /// A later publication may certify this candidate clean; observation
    /// failure deliberately leaves the route uncertain.
    pub(super) fn observe_routes<'a>(
        &self,
        routes: impl IntoIterator<Item = &'a SourceRouteIdentity>,
    ) -> std::result::Result<(), RouteTargetObservationError> {
        let targets = {
            let state = self.lock_state();
            routes
                .into_iter()
                .filter_map(|route| {
                    state
                        .targets
                        .get(route)
                        .cloned()
                        .map(|targets| (route.clone(), targets))
                })
                .collect::<BTreeMap<_, _>>()
        };
        if targets.is_empty() {
            return Ok(());
        }
        let observed = observe_target_revisions(&targets);
        let failure = observed.error();
        let mut state = self.lock_state();
        for (route, revision) in observed.revisions {
            state.observed_target_revisions.insert(route, revision);
        }
        drop(state);
        failure.map_or(Ok(()), Err)
    }

    /// Precomputes the immutable generation bindings and sampled target
    /// revisions that may be persisted after exact dirty-ledger
    /// acknowledgement. This work intentionally runs without the global
    /// coordinator mutex.
    pub(super) fn prepare_acknowledged_routes(
        &self,
        published: &VerifiedIndex,
        routes: &BTreeSet<SourceRouteIdentity>,
    ) -> Result<PreparedRouteFrontierPublication> {
        let (revisions, observation_error) = {
            let state = self.lock_state();
            let mut revisions = BTreeMap::new();
            let mut failures = Vec::new();
            for route in routes {
                match state.observed_target_revisions.get(route) {
                    Some(TargetRevisionObservation::Revision(revision)) => {
                        revisions.insert(route.clone(), revision.clone());
                    }
                    Some(TargetRevisionObservation::Failed(error)) => {
                        failures.push((route.clone(), error.clone()));
                    }
                    None => {
                        failures.push((route.clone(), "observation is missing".to_owned()));
                    }
                }
            }
            let observation_error =
                (!failures.is_empty()).then_some(RouteTargetObservationError { failures });
            (revisions, observation_error)
        };
        let bindings = published_route_bindings(published, routes.iter())?;
        let entries = revisions
            .into_iter()
            .map(|(route, revision)| {
                let binding = bindings.get(&route).cloned().ok_or_else(|| {
                    anyhow!(
                        "published Core route freshness binding for {} is missing",
                        route.as_str()
                    )
                })?;
                Ok((route, (revision, binding)))
            })
            .collect::<Result<_>>()?;
        Ok(PreparedRouteFrontierPublication {
            frontier: self.clone(),
            entries,
            observation_error,
        })
    }

    pub(super) fn data_root(&self) -> PathBuf {
        self.lock_state().data_root.clone()
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, RouteFreshnessFrontierState> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl PreparedRouteFrontierPublication {
    /// Advances only routes whose exact dirty observation was acknowledged
    /// against the pinned Core publication used to build this immutable plan.
    /// Any later watcher observation cannot change the revision persisted by
    /// this acknowledgement and is therefore caught after restart.
    pub(super) fn persist_acknowledged_routes(
        self,
        routes: &BTreeSet<SourceRouteIdentity>,
    ) -> Result<()> {
        let Self {
            frontier,
            entries,
            observation_error,
        } = self;
        if routes.is_empty() {
            return observation_error.map_or(Ok(()), |error| Err(error.into()));
        }
        let (path, ledger) = {
            let mut state = frontier.lock_state();
            for route in routes {
                if let Some((target_revision, binding)) = entries.get(route) {
                    state.durable.insert(
                        route.clone(),
                        DurableRouteFrontier {
                            route_identity: route.as_str().to_owned(),
                            published_route_binding: binding.clone(),
                            target_revision: target_revision.clone(),
                        },
                    );
                } else {
                    // An acknowledged but uncertifiable route cannot retain an
                    // older durable cleanliness claim.
                    state.durable.remove(route);
                }
            }
            let routes = state.durable.values().cloned().collect();
            (
                state.path.clone(),
                DurableRouteFrontierLedger {
                    schema_version: ROUTE_FRONTIER_SCHEMA_VERSION,
                    routes,
                },
            )
        };
        let value = serde_json::to_value(ledger)?;
        write_private_json_file(&path, &value)
            .with_context(|| format!("persist route freshness frontier {}", path.display()))?;
        observation_error.map_or(Ok(()), |error| Err(error.into()))
    }
}

fn route_targets(
    catalog: &SourceBackedWatchCatalog,
) -> BTreeMap<SourceRouteIdentity, Vec<PathBuf>> {
    catalog
        .route_targets()
        .map(|(route, targets)| (route.clone(), targets.iter().cloned().collect()))
        .collect()
}

fn observe_target_revisions(
    targets: &BTreeMap<SourceRouteIdentity, Vec<PathBuf>>,
) -> TargetRevisionObservations {
    let mut revisions = BTreeMap::new();
    let mut failures = Vec::new();
    for (route, targets) in targets {
        let observation = match target_revision(targets) {
            Ok(revision) => TargetRevisionObservation::Revision(revision),
            Err(error) => {
                let error = format!("{error:#}");
                failures.push((route.clone(), error.clone()));
                TargetRevisionObservation::Failed(error)
            }
        };
        revisions.insert(route.clone(), observation);
    }
    TargetRevisionObservations {
        revisions,
        failures,
    }
}

fn routes_outside_frontier<'a>(
    routes: impl IntoIterator<Item = &'a SourceRouteIdentity>,
    observed_target_revisions: &BTreeMap<SourceRouteIdentity, TargetRevisionObservation>,
    bindings: Option<&BTreeMap<SourceRouteIdentity, String>>,
    durable: &BTreeMap<SourceRouteIdentity, DurableRouteFrontier>,
) -> BTreeSet<SourceRouteIdentity> {
    routes
        .into_iter()
        .filter(|route| {
            let Some(TargetRevisionObservation::Revision(target_revision)) =
                observed_target_revisions.get(*route)
            else {
                return true;
            };
            let Some(binding) = bindings.and_then(|bindings| bindings.get(*route)) else {
                return true;
            };
            !durable.get(*route).is_some_and(|entry| {
                entry.published_route_binding == *binding
                    && entry.target_revision == target_revision.as_str()
            })
        })
        .cloned()
        .collect()
}

fn published_route_bindings<'a>(
    index: &VerifiedIndex,
    routes: impl IntoIterator<Item = &'a SourceRouteIdentity>,
) -> Result<BTreeMap<SourceRouteIdentity, String>> {
    let manifest = index.manifest();
    let certified = manifest
        .sources
        .iter()
        .map(|source| (source.observation().source().identity().digest(), source))
        .collect::<BTreeMap<_, _>>();
    routes
        .into_iter()
        .map(|route| {
            let Some(snapshot) = manifest.source_route(route) else {
                return Ok((route.clone(), ABSENT_ROUTE_BINDING.to_owned()));
            };
            let mut digest = Sha256::new();
            digest.update(b"ctx.route-freshness.core-snapshot.v1\0");
            let encoded = serde_json::to_vec(snapshot)?;
            digest.update((encoded.len() as u64).to_be_bytes());
            digest.update(encoded);
            for source in snapshot.sources() {
                let identity = source.identity().digest();
                let certificate = certified.get(&identity).ok_or_else(|| {
                    anyhow!(
                        "published route {} references a source without a certified manifest record",
                        route.as_str()
                    )
                })?;
                let encoded = serde_json::to_vec(certificate)?;
                digest.update((encoded.len() as u64).to_be_bytes());
                digest.update(encoded);
            }
            Ok((route.clone(), hex_digest(digest.finalize().into())))
        })
        .collect()
}

fn read_durable_frontier(
    path: &Path,
) -> Result<BTreeMap<SourceRouteIdentity, DurableRouteFrontier>> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(error.into()),
    };
    if metadata.len() > ROUTE_FRONTIER_MAX_BYTES {
        return Err(anyhow!(
            "route freshness frontier exceeds {} bytes",
            ROUTE_FRONTIER_MAX_BYTES
        ));
    }
    let bytes = fs::read(path)?;
    let ledger: DurableRouteFrontierLedger = serde_json::from_slice(&bytes)?;
    if ledger.schema_version != ROUTE_FRONTIER_SCHEMA_VERSION {
        return Err(anyhow!(
            "unsupported route freshness frontier schema {}",
            ledger.schema_version
        ));
    }
    let mut routes = BTreeMap::new();
    for entry in ledger.routes {
        if !is_sha256(&entry.target_revision)
            || !(entry.published_route_binding == ABSENT_ROUTE_BINDING
                || is_sha256(&entry.published_route_binding))
        {
            return Err(anyhow!("route freshness frontier entry is malformed"));
        }
        let route = SourceRouteIdentity::from_sha256(entry.route_identity.clone())?;
        if routes.insert(route, entry).is_some() {
            return Err(anyhow!(
                "route freshness frontier contains duplicate routes"
            ));
        }
    }
    Ok(routes)
}

#[derive(Default)]
struct RevisionBudget {
    entries: usize,
    path_bytes: u64,
}

impl RevisionBudget {
    fn admit_path(&mut self, path: &Path) -> Result<()> {
        self.admit_path_bytes(os_str_len(path.as_os_str()))
    }

    fn admit_path_bytes(&mut self, path_bytes: usize) -> Result<()> {
        if path_bytes > PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES {
            return Err(anyhow!(
                "route target path exceeds the {}-byte capture inventory bound",
                PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES
            ));
        }
        self.entries = self.entries.saturating_add(1);
        self.path_bytes = self.path_bytes.saturating_add(path_bytes as u64);
        if self.entries > ROUTE_REVISION_MAX_ENTRIES {
            return Err(anyhow!(
                "route target tree exceeds the {ROUTE_REVISION_MAX_ENTRIES}-entry capture inventory bound"
            ));
        }
        if self.path_bytes > ROUTE_REVISION_MAX_PATH_BYTES {
            return Err(anyhow!(
                "route target tree exceeds the {ROUTE_REVISION_MAX_PATH_BYTES}-byte aggregate path bound"
            ));
        }
        Ok(())
    }
}

fn target_revision(targets: &[PathBuf]) -> Result<String> {
    let mut digest = Sha256::new();
    digest.update(b"ctx.route-freshness.target-revision.v1\0");
    digest.update((targets.len() as u64).to_be_bytes());
    let mut budget = RevisionBudget::default();
    for target in targets {
        hash_os_str(&mut digest, target.as_os_str());
        hash_target(&mut digest, target, 0, &mut budget)?;
    }
    Ok(hex_digest(digest.finalize().into()))
}

fn hash_target(
    digest: &mut Sha256,
    path: &Path,
    depth: usize,
    budget: &mut RevisionBudget,
) -> Result<()> {
    if depth > ROUTE_REVISION_MAX_DEPTH {
        return Err(anyhow!("route target tree exceeds the depth bound"));
    }
    budget.admit_path(path)?;

    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            digest.update(b"missing\0");
            return Ok(());
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("inspect route freshness target {}", path.display()))
        }
    };
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        digest.update(b"symlink\0");
    } else if file_type.is_file() {
        digest.update(b"file\0");
    } else if file_type.is_dir() {
        digest.update(b"directory\0");
    } else {
        digest.update(b"special\0");
    }
    hash_metadata(digest, &metadata);
    if !file_type.is_dir() {
        return Ok(());
    }

    let mut children = Vec::new();
    for entry in fs::read_dir(path)
        .with_context(|| format!("read route freshness target directory {}", path.display()))?
    {
        let entry = entry
            .with_context(|| format!("read route freshness target entry in {}", path.display()))?;
        children.push((entry.file_name(), entry.path()));
        if budget.entries.saturating_add(children.len()) > ROUTE_REVISION_MAX_ENTRIES {
            return Err(anyhow!("route target tree exceeds the entry bound"));
        }
    }
    children.sort_by(|left, right| left.0.cmp(&right.0));
    digest.update((children.len() as u64).to_be_bytes());
    for (name, child) in children {
        hash_os_str(digest, &name);
        hash_target(digest, &child, depth.saturating_add(1), budget)?;
    }
    Ok(())
}

fn hash_metadata(digest: &mut Sha256, metadata: &fs::Metadata) {
    digest.update(metadata.len().to_be_bytes());
    digest.update([u8::from(metadata.permissions().readonly())]);
    hash_system_time(digest, metadata.modified().ok());
    hash_system_time(digest, metadata.created().ok());

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        digest.update(metadata.dev().to_be_bytes());
        digest.update(metadata.ino().to_be_bytes());
        digest.update(metadata.mode().to_be_bytes());
        digest.update(metadata.nlink().to_be_bytes());
        digest.update(metadata.mtime().to_be_bytes());
        digest.update(metadata.mtime_nsec().to_be_bytes());
        digest.update(metadata.ctime().to_be_bytes());
        digest.update(metadata.ctime_nsec().to_be_bytes());
    }

    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        digest.update(metadata.file_attributes().to_be_bytes());
        digest.update(metadata.creation_time().to_be_bytes());
        digest.update(metadata.last_write_time().to_be_bytes());
        digest.update(metadata.file_size().to_be_bytes());
    }
}

fn hash_system_time(digest: &mut Sha256, value: Option<SystemTime>) {
    match value.and_then(|value| value.duration_since(UNIX_EPOCH).ok()) {
        Some(value) => {
            digest.update([1]);
            digest.update(value.as_secs().to_be_bytes());
            digest.update(value.subsec_nanos().to_be_bytes());
        }
        None => digest.update([0]),
    }
}

fn hash_os_str(digest: &mut Sha256, value: &OsStr) {
    let bytes = value.as_encoded_bytes();
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
}

fn os_str_len(value: &OsStr) -> usize {
    value.as_encoded_bytes().len()
}

fn hex_digest(digest: [u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(byte: u8) -> SourceRouteIdentity {
        SourceRouteIdentity::from_sha256(format!("{byte:02x}").repeat(32)).unwrap()
    }

    fn durable_entry(
        route: &SourceRouteIdentity,
        target_revision: String,
        binding: String,
    ) -> DurableRouteFrontier {
        DurableRouteFrontier {
            route_identity: route.as_str().to_owned(),
            published_route_binding: binding,
            target_revision,
        }
    }

    #[test]
    fn changed_while_daemon_was_stopped_does_not_match_the_durable_frontier() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("history.jsonl");
        fs::write(&target, b"one\n").unwrap();
        let route = route(1);
        let before = target_revision(std::slice::from_ref(&target)).unwrap();
        let binding = format!("{:02x}", 9).repeat(32);
        let durable = BTreeMap::from([(
            route.clone(),
            durable_entry(&route, before, binding.clone()),
        )]);

        fs::write(&target, b"one\ntwo\n").unwrap();
        let targets = BTreeMap::from([(route.clone(), vec![target])]);
        let observed = observe_target_revisions(&targets);
        let bindings = BTreeMap::from([(route.clone(), binding)]);
        let dirty = routes_outside_frontier(
            targets.keys(),
            &observed.revisions,
            Some(&bindings),
            &durable,
        );

        assert_eq!(dirty, BTreeSet::from([route]));
    }

    #[test]
    fn clean_restart_schedules_zero_healthy_route_scans() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("history.jsonl");
        fs::write(&target, b"stable\n").unwrap();
        let route = route(1);
        let target_revision = target_revision(std::slice::from_ref(&target)).unwrap();
        let binding = format!("{:02x}", 9).repeat(32);
        let durable = BTreeMap::from([(
            route.clone(),
            durable_entry(&route, target_revision, binding.clone()),
        )]);
        let targets = BTreeMap::from([(route.clone(), vec![target])]);
        let observed = observe_target_revisions(&targets);
        let bindings = BTreeMap::from([(route.clone(), binding)]);
        let dirty = routes_outside_frontier(
            targets.keys(),
            &observed.revisions,
            Some(&bindings),
            &durable,
        );

        assert!(dirty.is_empty());
    }

    #[test]
    fn recursive_target_revision_catches_child_edit_addition_and_deletion() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sessions");
        fs::create_dir_all(&root).unwrap();
        let child = root.join("one.jsonl");
        fs::write(&child, b"one\n").unwrap();
        let initial = target_revision(std::slice::from_ref(&root)).unwrap();

        fs::write(&child, b"changed body\n").unwrap();
        let edited = target_revision(std::slice::from_ref(&root)).unwrap();
        assert_ne!(edited, initial);

        let second = root.join("two.jsonl");
        fs::write(&second, b"two\n").unwrap();
        let added = target_revision(std::slice::from_ref(&root)).unwrap();
        assert_ne!(added, edited);

        fs::remove_file(second).unwrap();
        let deleted = target_revision(std::slice::from_ref(&root)).unwrap();
        assert_ne!(deleted, added);
    }

    #[test]
    fn missing_and_reappeared_targets_have_distinct_revisions() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("moved.jsonl");
        let missing = target_revision(std::slice::from_ref(&target)).unwrap();
        fs::write(&target, b"returned\n").unwrap();
        let present = target_revision(std::slice::from_ref(&target)).unwrap();
        assert_ne!(missing, present);
    }

    #[test]
    fn revision_budget_covers_the_full_capture_inventory_contract() {
        assert_eq!(
            ROUTE_REVISION_MAX_ENTRIES,
            PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES
        );
        assert_eq!(
            ROUTE_REVISION_MAX_PATH_BYTES,
            (PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES as u64)
                .saturating_mul(PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES as u64)
        );

        let mut budget = RevisionBudget {
            entries: ROUTE_REVISION_MAX_ENTRIES - 1,
            path_bytes: ROUTE_REVISION_MAX_PATH_BYTES
                - PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES as u64,
        };
        budget
            .admit_path_bytes(PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES)
            .unwrap();
        assert_eq!(budget.entries, ROUTE_REVISION_MAX_ENTRIES);
        assert_eq!(budget.path_bytes, ROUTE_REVISION_MAX_PATH_BYTES);
        assert!(budget.admit_path_bytes(1).is_err());

        let mut overlong_path = RevisionBudget::default();
        assert!(overlong_path
            .admit_path_bytes(PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES + 1)
            .is_err());
    }

    #[test]
    fn observation_failures_are_typed_visible_and_leave_the_route_dirty() {
        let route = route(7);
        let overlong =
            PathBuf::from("x".repeat(PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES.saturating_add(1)));
        let targets = BTreeMap::from([(route.clone(), vec![overlong])]);
        let observed = observe_target_revisions(&targets);
        let error = observed.error().expect("overlong target must be visible");
        assert!(error.to_string().contains(route.as_str()));
        assert!(error.to_string().contains("capture inventory bound"));
        assert!(matches!(
            observed.revisions.get(&route),
            Some(TargetRevisionObservation::Failed(_))
        ));

        let dirty = routes_outside_frontier(
            targets.keys(),
            &observed.revisions,
            Some(&BTreeMap::from([(
                route.clone(),
                format!("{:02x}", 9).repeat(32),
            )])),
            &BTreeMap::new(),
        );
        assert_eq!(dirty, BTreeSet::from([route]));
    }

    #[test]
    fn prepared_publication_keeps_the_pre_acknowledgement_revision() {
        let temp = tempfile::tempdir().unwrap();
        let route = route(3);
        let old_revision = format!("{:02x}", 4).repeat(32);
        let newer_revision = format!("{:02x}", 5).repeat(32);
        let binding = format!("{:02x}", 6).repeat(32);
        let path = temp.path().join(ROUTE_FRONTIER_FILE);
        let frontier = RouteFreshnessFrontier {
            inner: Arc::new(Mutex::new(RouteFreshnessFrontierState {
                data_root: temp.path().to_path_buf(),
                path: path.clone(),
                targets: BTreeMap::new(),
                observed_target_revisions: BTreeMap::from([(
                    route.clone(),
                    TargetRevisionObservation::Revision(newer_revision),
                )]),
                durable: BTreeMap::new(),
            })),
        };
        let prepared = PreparedRouteFrontierPublication {
            frontier,
            entries: BTreeMap::from([(route.clone(), (old_revision.clone(), binding))]),
            observation_error: None,
        };

        prepared
            .persist_acknowledged_routes(&BTreeSet::from([route.clone()]))
            .unwrap();
        let durable = read_durable_frontier(&path).unwrap();
        assert_eq!(durable[&route].target_revision, old_revision);
    }

    #[test]
    fn failed_observation_is_visible_without_blocking_a_certifiable_sibling() {
        let temp = tempfile::tempdir().unwrap();
        let good = route(8);
        let failed = route(9);
        let revision = format!("{:02x}", 10).repeat(32);
        let binding = format!("{:02x}", 11).repeat(32);
        let stale_revision = format!("{:02x}", 12).repeat(32);
        let path = temp.path().join(ROUTE_FRONTIER_FILE);
        let frontier = RouteFreshnessFrontier {
            inner: Arc::new(Mutex::new(RouteFreshnessFrontierState {
                data_root: temp.path().to_path_buf(),
                path: path.clone(),
                targets: BTreeMap::new(),
                observed_target_revisions: BTreeMap::new(),
                durable: BTreeMap::from([(
                    failed.clone(),
                    durable_entry(&failed, stale_revision, binding.clone()),
                )]),
            })),
        };
        let prepared = PreparedRouteFrontierPublication {
            frontier,
            entries: BTreeMap::from([(good.clone(), (revision.clone(), binding))]),
            observation_error: Some(RouteTargetObservationError {
                failures: vec![(failed.clone(), "permission denied".to_owned())],
            }),
        };

        let error = prepared
            .persist_acknowledged_routes(&BTreeSet::from([good.clone(), failed.clone()]))
            .unwrap_err();
        assert!(error
            .downcast_ref::<RouteTargetObservationError>()
            .is_some());
        let durable = read_durable_frontier(&path).unwrap();
        assert_eq!(durable[&good].target_revision, revision);
        assert!(!durable.contains_key(&failed));
    }
}
