use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, MutexGuard},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use ctx_history_core::platform_security::restrict_private_directory;
use ctx_pro_host_protocol::{
    BlameMatch, BlameResult, BlameTarget, CommitBlameMatch, CommitFactType, CommitPredicate,
    FactConfidence, FactState, ResolvedBlameTarget, ResourceKind, ResourceRef,
};
use rusqlite::{Connection, ErrorCode};
use serde_json::json;

use super::store::usage_path;
use super::{
    estimate_usage, read_report, reset, store, CliUsage, CompletedOperation,
    EphemeralContextCorrelation, EstimateCoverage, EstimateFacts, McpInvocation, McpUsageRecorder,
    ProOutcome, ResultObservationAction, Surface, TargetType, ValueClass,
    CONTEXT_CORRELATION_MAX_RECORDS, CTX_VERSION, DEFINITION_VERSION, ESTIMATE_MODEL,
};

mod mcp_tests;
mod migration_tests;
mod persistence_tests;
mod schema_tests;

fn operation(name: &'static str) -> CompletedOperation {
    CompletedOperation::cli(name, true, Duration::from_millis(4))
}

fn private_tempdir() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    restrict_private_directory(root.path()).unwrap();
    root
}

fn auxiliary(path: &Path, suffix: &str) -> PathBuf {
    let mut value = OsString::from(path.as_os_str());
    value.push(suffix);
    PathBuf::from(value)
}

fn directory_bytes(path: &Path) -> Vec<(OsString, Vec<u8>)> {
    let mut entries = fs::read_dir(path)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let name = entry.file_name();
            let bytes = fs::read(entry.path()).unwrap();
            (name, bytes)
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    entries
}

#[derive(Debug, PartialEq, Eq)]
struct FamilyMemberSnapshot {
    name: OsString,
    bytes: Vec<u8>,
    len: u64,
    modified: Option<SystemTime>,
    readonly: bool,
    #[cfg(unix)]
    mode: u32,
    #[cfg(unix)]
    uid: u32,
    #[cfg(unix)]
    gid: u32,
    #[cfg(unix)]
    links: u64,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

fn sqlite_family_snapshot(path: &Path) -> Vec<FamilyMemberSnapshot> {
    ["", "-wal", "-shm"]
        .into_iter()
        .filter_map(|suffix| {
            let member = if suffix.is_empty() {
                path.to_path_buf()
            } else {
                auxiliary(path, suffix)
            };
            let bytes = match fs::read(&member) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
                Err(error) => panic!("read {}: {error}", member.display()),
            };
            let metadata = member.metadata().unwrap();
            #[cfg(unix)]
            use std::os::unix::fs::MetadataExt as _;
            Some(FamilyMemberSnapshot {
                name: member.file_name().unwrap().to_os_string(),
                bytes,
                len: metadata.len(),
                modified: metadata.modified().ok(),
                readonly: metadata.permissions().readonly(),
                #[cfg(unix)]
                mode: metadata.mode(),
                #[cfg(unix)]
                uid: metadata.uid(),
                #[cfg(unix)]
                gid: metadata.gid(),
                #[cfg(unix)]
                links: metadata.nlink(),
                #[cfg(unix)]
                device: metadata.dev(),
                #[cfg(unix)]
                inode: metadata.ino(),
            })
        })
        .collect()
}

struct LocalUsageEnvGuard {
    _lock: MutexGuard<'static, ()>,
    saved: Option<OsString>,
}

impl LocalUsageEnvGuard {
    fn unset() -> Self {
        let lock = crate::config::TEST_LOCAL_USAGE_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let saved = env::var_os("CTX_LOCAL_USAGE_ENABLED");
        env::remove_var("CTX_LOCAL_USAGE_ENABLED");
        Self { _lock: lock, saved }
    }
}

impl Drop for LocalUsageEnvGuard {
    fn drop(&mut self) {
        match &self.saved {
            Some(value) => env::set_var("CTX_LOCAL_USAGE_ENABLED", value),
            None => env::remove_var("CTX_LOCAL_USAGE_ENABLED"),
        }
    }
}

fn mcp_operation(
    name: &'static str,
    success: bool,
    value_class: ValueClass,
    result_count: u64,
) -> CompletedOperation {
    CompletedOperation {
        surface: Surface::Mcp,
        operation: name,
        outcome: if success {
            super::Outcome::Success
        } else {
            super::Outcome::Failure
        },
        value_class,
        duration: super::DurationBucket::Under10Ms,
        target_type: TargetType::NotApplicable,
        pro_outcome: ProOutcome::NotApplicable,
        result_count,
        citation_count: 0,
        result_action: success
            .then(|| super::result_observation_action(name))
            .flatten(),
        latency_ms: 1,
        latency_samples: 1,
        response_bytes: 100,
        response_byte_samples: 1,
        output_bytes: 0,
        output_byte_samples: 0,
        context_bytes: if success && value_class != ValueClass::NotApplicable {
            100
        } else {
            0
        },
        context_byte_samples: u64::from(success && value_class != ValueClass::NotApplicable),
        search_result_bytes: if name == "search" && success && result_count > 0 {
            100
        } else {
            0
        },
        search_result_byte_samples: u64::from(name == "search" && success && result_count > 0),
        context: super::ContextUsage::default(),
    }
}

#[test]
fn cli_result_observation_is_content_free_and_search_scoped() {
    let mut search = CliUsage::excluded();
    search.operation = Some("search");
    search.set_result_observation(ResultObservationAction::Search, 3, 1, 640);
    search.set_semantic_context_bytes(640);
    search.set_measured_output_bytes(701);
    search.add_context_usage(super::ContextUsage {
        context_searches: 1,
        context_found: 3,
        ..super::ContextUsage::default()
    });
    let completed = search.completed(true, Duration::from_millis(17)).unwrap();
    assert_eq!(
        completed.result_metadata_for_test(),
        (ValueClass::ResultBearing, 3, 1)
    );
    assert_eq!(completed.response_bytes, 0);
    assert_eq!(completed.output_bytes, 701);
    assert_eq!(completed.context_bytes, 640);
    assert_eq!(completed.context_byte_samples, 1);
    assert_eq!(completed.search_result_bytes, 640);
    assert_eq!(completed.search_result_byte_samples, 1);
    assert_eq!(completed.context.context_searches, 1);
    assert_eq!(completed.context.context_found, 3);

    let mut show = CliUsage::excluded();
    show.operation = Some("show");
    show.set_result_observation(ResultObservationAction::OpenSession, 1, 0, 320);
    let completed = show.completed(true, Duration::from_millis(5)).unwrap();
    assert_eq!(completed.response_bytes, 0);
    assert_eq!(completed.output_bytes, 0);
    assert_eq!(completed.context_bytes, 320);
    assert_eq!(completed.context_byte_samples, 1);
    assert_eq!(
        completed.search_result_bytes, 0,
        "a mismatched observation cannot apply the search estimate input"
    );

    assert!(CliUsage::excluded()
        .completed(true, Duration::ZERO)
        .is_none());
}

#[test]
fn ephemeral_context_correlation_deduplicates_without_resetting_state() {
    let mut correlation = EphemeralContextCorrelation::default();
    correlation.record_search();
    correlation.record_found("known");
    correlation.record_opened(&"known");
    correlation.record_cited(&"known");
    correlation.record_found("known");
    correlation.record_opened(&"known");
    correlation.record_cited(&"known");
    correlation.record_opened(&"unknown");
    correlation.record_cited(&"unknown");

    let usage = correlation.finish();
    assert_eq!(usage.context_searches, 1);
    assert_eq!(usage.context_found, 1);
    assert_eq!(usage.context_opened, 1);
    assert_eq!(usage.context_cited, 1);
    assert_eq!(usage.validated_discoveries, 1);
}

#[test]
fn cite_only_validation_has_no_open_time_estimate() {
    let mut correlation = EphemeralContextCorrelation::default();
    correlation.record_found("cited-only");
    correlation.record_cited(&"cited-only");
    let usage = correlation.finish();
    assert_eq!(usage.context_opened, 0);
    assert_eq!(usage.context_cited, 1);
    assert_eq!(usage.validated_discoveries, 1);

    let estimates = estimate_usage(EstimateFacts {
        discovered_record_opens: usage.context_opened,
        ..EstimateFacts::default()
    })
    .unwrap();
    assert_eq!(estimates.estimated_time_saved_seconds, 0);
}

#[test]
fn estimate_model_uses_approved_coefficients_and_one_copy_bytes() {
    let estimates = estimate_usage(EstimateFacts {
        result_bearing_searches: 2,
        semantic_context_eligible_samples: 4,
        semantic_context_bytes: 40,
        semantic_context_byte_samples: 4,
        semantic_search_result_bytes: 8,
        semantic_search_result_byte_samples: 2,
        discovered_record_opens: 2,
        produced_blame_requests: 1,
        possible_blame_requests: 1,
    })
    .unwrap();

    assert_eq!(ESTIMATE_MODEL.approximate_bytes_per_token, 4);
    assert_eq!(ESTIMATE_MODEL.avoided_search_token_multiplier, 49);
    assert_eq!(
        estimates.approximate_context_tokens.approximate_tokens,
        Some(10)
    );
    assert_eq!(
        estimates
            .approximate_avoided_context_tokens
            .approximate_tokens,
        Some(98)
    );
    assert_eq!(
        estimates.estimated_time_saved_seconds,
        2 * 60 + 2 * 15 + 300 + 120
    );
}

#[test]
fn legacy_missing_semantic_measurements_are_named_and_not_fabricated_zero() {
    let estimates = estimate_usage(EstimateFacts {
        result_bearing_searches: 3,
        semantic_context_eligible_samples: 4,
        ..EstimateFacts::default()
    })
    .unwrap();

    assert_eq!(
        estimates.approximate_context_tokens.coverage,
        EstimateCoverage::UnavailableLegacy
    );
    assert_eq!(
        estimates.approximate_context_tokens.approximate_tokens,
        None
    );
    assert_eq!(
        estimates.approximate_avoided_context_tokens.coverage,
        EstimateCoverage::UnavailableLegacy
    );
    assert_eq!(
        estimates
            .approximate_avoided_context_tokens
            .approximate_tokens,
        None
    );
    assert_eq!(estimates.estimated_time_saved_seconds, 3 * 60);
    assert_eq!(
        serde_json::to_value(estimates).unwrap()["approximate_context_tokens"]["coverage"],
        "unavailable_legacy"
    );

    let no_eligible_actions = estimate_usage(EstimateFacts::default()).unwrap();
    assert_eq!(
        no_eligible_actions.approximate_context_tokens.coverage,
        EstimateCoverage::Complete
    );
    assert_eq!(
        no_eligible_actions
            .approximate_context_tokens
            .approximate_tokens,
        Some(0)
    );
}

#[test]
fn ephemeral_context_correlation_drops_new_keys_at_its_fixed_cap() {
    let mut correlation = EphemeralContextCorrelation::default();
    for key in 0..CONTEXT_CORRELATION_MAX_RECORDS + 8 {
        correlation.record_found(key);
    }
    correlation.record_opened(&0);
    correlation.record_cited(&(CONTEXT_CORRELATION_MAX_RECORDS + 1));

    let usage = correlation.finish();
    assert_eq!(usage.context_found, CONTEXT_CORRELATION_MAX_RECORDS as u64);
    assert_eq!(usage.context_opened, 1);
    assert_eq!(usage.context_cited, 0);
    assert_eq!(usage.validated_discoveries, 1);
}

#[test]
fn malformed_store_reports_error_instead_of_zero() {
    let root = private_tempdir();
    fs::write(usage_path(root.path()), b"not sqlite").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(usage_path(root.path()), fs::Permissions::from_mode(0o600)).unwrap();
    }
    let report = read_report(root.path(), true, false);
    assert_eq!(report.state, "error");
    assert!(report.summary.is_none());
    assert_eq!(report.error.unwrap().code, "usage_store_unavailable");
}

#[test]
fn oversized_store_image_reports_a_bounded_content_free_error() {
    let root = private_tempdir();
    let path = usage_path(root.path());
    let file = fs::File::create(&path).unwrap();
    file.set_len(6 * 1024 * 1024 + 4096).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let report = read_report(root.path(), true, false);
    assert_eq!(report.state, "error");
    let encoded = serde_json::to_string(&report).unwrap();
    assert!(!encoded.contains(root.path().to_string_lossy().as_ref()));
    assert_eq!(
        report.error.unwrap().message,
        "local usage store exceeds its size limit"
    );
}

#[test]
fn public_usage_errors_never_serialize_raw_paths_or_causes() {
    let marker = "SECRET_PATH_TOKEN_7f98";
    let raw_cause = format!("database at /tmp/{marker} contains bearer-secret");
    let report = super::UsageReport::config_error();
    let encoded = serde_json::to_string(&report).unwrap();
    assert!(!encoded.contains(marker));
    assert!(!encoded.contains("bearer-secret"));
    assert!(raw_cause.contains(marker));
    assert_eq!(
        serde_json::to_value(report).unwrap()["error"]["message"],
        "local usage configuration could not be read"
    );
}

#[test]
fn recording_hot_path_benchmark_smoke() {
    let root = private_tempdir();
    for _ in 0..10 {
        store::record(root.path(), operation("search")).unwrap();
    }
    let mut samples = Vec::with_capacity(1_000);
    for _ in 0..1_000 {
        let started = Instant::now();
        store::record(root.path(), operation("search")).unwrap();
        samples.push(started.elapsed());
    }
    samples.sort_unstable();
    let p50 = samples[499];
    let p90 = samples[899];
    let p95 = samples[949];
    let p99 = samples[989];
    let maximum = samples[999];
    eprintln!(
        "local usage warm upsert over 1,000 samples: \
         p50={p50:?} p90={p90:?} p95={p95:?} p99={p99:?} max={maximum:?}"
    );
    // Fastbuild/debug runs inside the broad unit-test binary and owns only a
    // coarse runaway-I/O smoke ceiling. Exclusive release qualification owns
    // the product contract's <=10 ms p99.
    #[cfg(debug_assertions)]
    assert!(
        p99 <= Duration::from_millis(500),
        "local usage warm upsert exceeded the debug smoke ceiling: \
         p50={p50:?} p90={p90:?} p95={p95:?} p99={p99:?} max={maximum:?}"
    );
    #[cfg(not(debug_assertions))]
    assert!(
        p99 <= Duration::from_millis(10),
        "local usage warm upsert exceeded its release p99 contract: \
         p50={p50:?} p90={p90:?} p95={p95:?} p99={p99:?} max={maximum:?}"
    );
    assert_eq!(DEFINITION_VERSION, 2);
}

#[test]
fn local_control_refresh_p99_is_bounded_across_one_thousand_samples() {
    let _env = LocalUsageEnvGuard::unset();
    let root = private_tempdir();
    fs::write(
        root.path().join("config.toml"),
        "[local_usage]\nenabled = true\n",
    )
    .unwrap();
    let mut samples = Vec::with_capacity(1_000);
    for _ in 0..1_000 {
        let started = Instant::now();
        assert!(
            crate::config::read_local_usage_control(root.path())
                .unwrap()
                .effective_enabled
        );
        samples.push(started.elapsed());
    }
    samples.sort_unstable();
    let p99 = samples[989];
    eprintln!("local usage control refresh p99 over 1,000 samples: {p99:?}");
    assert!(p99 < Duration::from_millis(25));
}

#[test]
fn cli_controls_and_mcp_serve_do_not_create_duplicate_observations() {
    use clap::Parser as _;

    for args in [
        ["ctx", "status", "--usage", "reset"].as_slice(),
        ["ctx", "status", "--usage", "disable"].as_slice(),
        ["ctx", "mcp", "serve"].as_slice(),
        ["ctx", "daemon", "run"].as_slice(),
    ] {
        let cli = crate::Cli::try_parse_from(args).unwrap();
        assert!(CliUsage::from_command(&cli.command)
            .completed(true, Duration::ZERO)
            .is_none());
    }
}

#[test]
fn replacement_helper_is_excluded_while_manual_upgrade_remains_eligible() {
    use clap::Parser as _;

    let helper = crate::Cli::try_parse_from(["ctx", "upgrade", "--replacement-helper"]).unwrap();
    assert!(
        CliUsage::from_command(&helper.command)
            .completed(false, Duration::ZERO)
            .is_none(),
        "the automatic replacement helper must not create a usage descriptor"
    );

    let manual = crate::Cli::try_parse_from(["ctx", "upgrade", "--dry-run"]).unwrap();
    let completed = CliUsage::from_command(&manual.command)
        .completed(true, Duration::ZERO)
        .expect("ordinary manual upgrade must remain usage-eligible");
    assert_eq!(completed.surface, Surface::Cli);
    assert_eq!(completed.operation, "upgrade");
}

#[test]
fn conversion_action_is_limited_to_trial_and_locked_access() {
    let trial = super::pro_conversion_action(Some("trial")).unwrap();
    assert_eq!(trial["kind"], "pro_monthly_conversion");
    assert_eq!(trial["price"], "$20/month");
    assert_eq!(trial["command"], "ctx pro manage");

    let locked = super::pro_conversion_action(Some("locked")).unwrap();
    assert_eq!(locked["kind"], "pro_restore_access");
    assert_eq!(locked["reason"], "access_locked");
    assert_eq!(locked["graph_preserved"], true);
    assert!(locked.get("price").is_none());

    for state in ["active", "canceling_paid", "offline_grace", "grace"] {
        assert!(
            super::pro_conversion_action(Some(state)).is_none(),
            "{state}"
        );
    }
    assert!(super::pro_conversion_action(None).is_none());
}
