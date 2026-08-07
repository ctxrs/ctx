use std::path::{Path, PathBuf};
#[cfg(not(test))]
use std::sync::atomic::{AtomicBool, Ordering};

use super::marker::{install_marker_path, is_valid_install_attempt_id};
use crate::upgrade::{platform_key, sha256_hex};
use anyhow::{anyhow, bail, Context, Result};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

mod filesystem;

use filesystem::*;

const SCHEMA_VERSION: u32 = 1;
const JOURNAL_SUFFIX: &str = "hosted-install-transaction.json";
const MAX_MARKER_BYTES: u64 = 64 * 1024;
const MAX_OWNERSHIP_BYTES: u64 = 1024 * 1024;
const MAX_BINARY_BYTES: u64 = 512 * 1024 * 1024;
const UNINSTALL_RECEIPT_SCHEMA_VERSION: u32 = 2;
#[cfg(not(test))]
static HOSTED_UNINSTALL_FENCE_OBSERVED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub(crate) enum HostedTransactionAction {
    Install,
    UninstallPrepare,
    UninstallArm,
    UninstallCommit,
}

pub(in crate::upgrade) struct HostedTransactionArgs {
    pub(in crate::upgrade) action: HostedTransactionAction,
    pub(in crate::upgrade) install_path: PathBuf,
    pub(in crate::upgrade) attempt_id: Option<String>,
    pub(in crate::upgrade) marker_source: Option<PathBuf>,
    pub(in crate::upgrade) ownership_source: Option<PathBuf>,
    pub(in crate::upgrade) binary_sha256: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum TransactionKind {
    Install,
    Uninstall,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Prepared,
    BinaryStaged,
    PublishingBinary,
    BinaryPublished,
    PublishingOwnership,
    OwnershipPublished,
    PublishingMarker,
    MarkerPublished,
    HelperStaged,
    Armed,
    RemovingBinary,
    BinaryRemoved,
    RemovingOwnership,
    OwnershipRemoved,
    RemovingMarker,
    Committed,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema_version: u32,
    kind: TransactionKind,
    attempt_id: String,
    install_path: PathBuf,
    marker_path: PathBuf,
    binary_sha256: String,
    marker_sha256: String,
    marker_body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    prior_binary_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prior_marker_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prior_ownership_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ownership_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ownership_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ownership_body: Option<Vec<u8>>,
    phase: Phase,
    binding_sha256: String,
}

pub(in crate::upgrade) fn run(args: HostedTransactionArgs) -> Result<()> {
    reject_unexpected_inputs(&args)?;
    let install_path = validate_install_path(&args.install_path)?;
    match args.action {
        HostedTransactionAction::Install => install(args, install_path),
        HostedTransactionAction::UninstallPrepare => uninstall_prepare(args, install_path),
        HostedTransactionAction::UninstallArm => uninstall_arm(args, install_path),
        HostedTransactionAction::UninstallCommit => uninstall_commit(args, install_path),
    }
}

pub(crate) fn installation_hosted_uninstall_is_active() -> Result<bool> {
    #[cfg(not(test))]
    if HOSTED_UNINSTALL_FENCE_OBSERVED.load(Ordering::Acquire) {
        return Ok(true);
    }
    let executable_path = super::marker::current_install_path()?;
    let active = hosted_uninstall_is_active_for_executable(&executable_path)?;
    // Once this process has observed an identity-validated uninstall, it must
    // never enter daemon ownership. The journal only disappears after this
    // executable has been removed, and caching avoids hashing it at every
    // admission checkpoint.
    #[cfg(not(test))]
    if active {
        HOSTED_UNINSTALL_FENCE_OBSERVED.store(true, Ordering::Release);
    }
    Ok(active)
}

fn hosted_uninstall_is_active_for(install_path: &Path) -> Result<bool> {
    Ok(validated_hosted_uninstall_journal(install_path)?.is_some())
}

pub(crate) fn hosted_uninstall_is_active_for_executable(executable_path: &Path) -> Result<bool> {
    if hosted_uninstall_is_active_for(executable_path)? {
        return Ok(true);
    }
    let Some(install_path) = uninstall_install_path_for_helper(executable_path) else {
        return Ok(false);
    };
    let Some(journal) = validated_hosted_uninstall_journal(&install_path)? else {
        // A dedicated uninstall helper must never own a daemon, including the
        // short interval after commit removes the journal and installed image
        // but before the orchestrator removes the helper itself.
        return Ok(true);
    };
    verify_file_digest(
        executable_path,
        &journal.binary_sha256,
        MAX_BINARY_BYTES,
        "hosted uninstall helper executable",
    )?;
    Ok(true)
}

fn validated_hosted_uninstall_journal(install_path: &Path) -> Result<Option<Journal>> {
    let Some(journal) = read_journal(&journal_path(install_path))? else {
        return Ok(None);
    };
    let kind = journal.kind;
    validate_journal(&journal, install_path, kind)?;
    if kind != TransactionKind::Uninstall {
        return Ok(None);
    }
    verify_file_digest(
        install_path,
        &journal.binary_sha256,
        MAX_BINARY_BYTES,
        "hosted uninstall fenced executable",
    )?;
    verify_file_digest(
        &journal.marker_path,
        &journal.marker_sha256,
        MAX_MARKER_BYTES,
        "hosted uninstall fenced marker",
    )?;
    Ok(Some(journal))
}

fn uninstall_install_path_for_helper(helper_path: &Path) -> Option<PathBuf> {
    let helper_name = helper_path.file_name()?.to_str()?;
    #[cfg(windows)]
    let suffix = ".hosted-uninstall-helper.exe";
    #[cfg(not(windows))]
    let suffix = ".hosted-uninstall-helper";
    let install_name = helper_name.strip_prefix('.')?.strip_suffix(suffix)?;
    if install_name.is_empty() {
        return None;
    }
    let install_path = helper_path.with_file_name(install_name);
    (uninstall_helper_path(&install_path) == helper_path).then_some(install_path)
}

fn reject_unexpected_inputs(args: &HostedTransactionArgs) -> Result<()> {
    match args.action {
        HostedTransactionAction::Install => {
            if args.attempt_id.is_none()
                || args.marker_source.is_none()
                || args.binary_sha256.is_none()
            {
                bail!("hosted install transaction is missing required inputs");
            }
        }
        HostedTransactionAction::UninstallPrepare => {
            if args.attempt_id.is_none()
                || args.marker_source.is_some()
                || args.ownership_source.is_some()
                || args.binary_sha256.is_some()
            {
                bail!("hosted uninstall preparation has invalid inputs");
            }
        }
        HostedTransactionAction::UninstallArm | HostedTransactionAction::UninstallCommit => {
            if args.attempt_id.is_some()
                || args.marker_source.is_some()
                || args.ownership_source.is_some()
                || args.binary_sha256.is_some()
            {
                bail!("hosted uninstall continuation has invalid inputs");
            }
        }
    }
    Ok(())
}

fn install(args: HostedTransactionArgs, install_path: PathBuf) -> Result<()> {
    let supplied_digest = normalized_sha256(
        args.binary_sha256
            .as_deref()
            .ok_or_else(|| anyhow!("hosted install missing binary digest"))?,
    )?;
    let source = current_executable()?;
    verify_file_digest(
        &source,
        &supplied_digest,
        MAX_BINARY_BYTES,
        "hosted installer candidate",
    )?;
    let journal_path = journal_path(&install_path);
    let mut journal = match read_journal(&journal_path)? {
        Some(journal) => {
            validate_journal(&journal, &install_path, TransactionKind::Install)?;
            if journal.binary_sha256 != supplied_digest {
                bail!("an interrupted hosted install records a different signed candidate");
            }
            journal
        }
        None => {
            let attempt_id = args
                .attempt_id
                .as_deref()
                .ok_or_else(|| anyhow!("hosted install missing attempt identity"))?;
            if !is_valid_install_attempt_id(attempt_id) {
                bail!("hosted install has an invalid attempt identity");
            }
            let prior = validate_existing_pair_for_install(&install_path)?;
            let marker_source = args
                .marker_source
                .as_deref()
                .ok_or_else(|| anyhow!("hosted install missing marker source"))?;
            let marker_body =
                read_bounded(marker_source, MAX_MARKER_BYTES, "hosted install marker")?;
            let marker_body =
                String::from_utf8(marker_body).context("hosted install marker is not UTF-8")?;
            let marker_sha256 = sha256_hex(marker_body.as_bytes());
            validate_marker_body(&marker_body, &install_path, &supplied_digest, None)?;
            let (ownership_path, ownership_sha256, ownership_body) =
                if let Some(source) = args.ownership_source.as_deref() {
                    let body =
                        read_bounded(source, MAX_OWNERSHIP_BYTES, "hosted integration ownership")?;
                    let digest = sha256_hex(&body);
                    let path = ownership_path(&install_path);
                    validate_marker_body(
                        &marker_body,
                        &install_path,
                        &supplied_digest,
                        Some((&path, &digest)),
                    )?;
                    (Some(path), Some(digest), Some(body))
                } else {
                    (None, None, None)
                };
            let mut journal = Journal {
                schema_version: SCHEMA_VERSION,
                kind: TransactionKind::Install,
                attempt_id: attempt_id.to_owned(),
                marker_path: install_marker_path(&install_path),
                install_path: install_path.clone(),
                binary_sha256: supplied_digest,
                marker_sha256,
                marker_body,
                prior_binary_sha256: prior.as_ref().map(|pair| pair.0.clone()),
                prior_marker_sha256: prior.as_ref().map(|pair| pair.1.clone()),
                prior_ownership_sha256: prior.and_then(|pair| pair.2),
                ownership_path,
                ownership_sha256,
                ownership_body,
                phase: Phase::Prepared,
                binding_sha256: String::new(),
            };
            journal.binding_sha256 = journal_binding(&journal);
            validate_journal(&journal, &install_path, TransactionKind::Install)?;
            write_initial_journal(&journal_path, &journal)?;
            journal
        }
    };
    complete_install(&source, &journal_path, &mut journal)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&install_receipt(&journal))?
    );
    Ok(())
}

fn complete_install(source: &Path, journal_path: &Path, journal: &mut Journal) -> Result<()> {
    complete_install_with_fault(source, journal_path, journal, &mut |_| Ok(()))
}

fn complete_install_with_fault(
    source: &Path,
    journal_path: &Path,
    journal: &mut Journal,
    fault: &mut dyn FnMut(&'static str) -> Result<()>,
) -> Result<()> {
    fault("journal_prepared")?;
    let binary_staged = staged_binary_path(journal);
    if !file_has_digest(
        &journal.install_path,
        &journal.binary_sha256,
        MAX_BINARY_BYTES,
    )? {
        stage_file(source, &binary_staged, true)?;
        verify_file_digest(
            &binary_staged,
            &journal.binary_sha256,
            MAX_BINARY_BYTES,
            "staged hosted executable",
        )?;
    }
    journal.phase = Phase::BinaryStaged;
    write_journal(journal_path, journal)?;
    fault("binary_staged")?;

    if !file_has_digest(
        &journal.install_path,
        &journal.binary_sha256,
        MAX_BINARY_BYTES,
    )? {
        ensure_target_is_recorded_prior_or_absent(journal, &journal.install_path, "executable")?;
        journal.phase = Phase::PublishingBinary;
        write_journal(journal_path, journal)?;
        atomic_publish(&binary_staged, &journal.install_path)?;
        fault("binary_replaced")?;
    }
    verify_file_digest(
        &journal.install_path,
        &journal.binary_sha256,
        MAX_BINARY_BYTES,
        "published hosted executable",
    )?;
    set_installed_executable_permissions(&journal.install_path)?;
    journal.phase = Phase::BinaryPublished;
    write_journal(journal_path, journal)?;
    fault("binary_published")?;

    if let (Some(path), Some(digest), Some(body)) = (
        journal.ownership_path.as_ref(),
        journal.ownership_sha256.as_ref(),
        journal.ownership_body.as_ref(),
    ) {
        let staged = staged_ownership_path(journal);
        if !file_has_digest(path, digest, MAX_OWNERSHIP_BYTES)? {
            if path.try_exists()? {
                let current = sha256_path(
                    path,
                    MAX_OWNERSHIP_BYTES,
                    "prior hosted integration ownership",
                )?;
                if journal.prior_ownership_sha256.as_deref() != Some(current.as_str()) {
                    bail!("refusing to replace integration ownership outside the recorded transaction");
                }
            }
            stage_bytes(body, &staged, false)?;
            journal.phase = Phase::PublishingOwnership;
            write_journal(journal_path, journal)?;
            atomic_publish(&staged, path)?;
            fault("ownership_replaced")?;
        }
        verify_file_digest(
            path,
            digest,
            MAX_OWNERSHIP_BYTES,
            "hosted integration ownership",
        )?;
        journal.phase = Phase::OwnershipPublished;
        write_journal(journal_path, journal)?;
        fault("ownership_published")?;
    }

    if !file_has_digest(
        &journal.marker_path,
        &journal.marker_sha256,
        MAX_MARKER_BYTES,
    )? {
        ensure_target_is_recorded_prior_or_absent(journal, &journal.marker_path, "marker")?;
        let staged = staged_marker_path(journal);
        stage_bytes(journal.marker_body.as_bytes(), &staged, false)?;
        journal.phase = Phase::PublishingMarker;
        write_journal(journal_path, journal)?;
        atomic_publish(&staged, &journal.marker_path)?;
        fault("marker_replaced")?;
    }
    verify_file_digest(
        &journal.marker_path,
        &journal.marker_sha256,
        MAX_MARKER_BYTES,
        "published hosted marker",
    )?;
    journal.phase = Phase::MarkerPublished;
    write_journal(journal_path, journal)?;
    fault("marker_published")?;
    validate_marker_body(
        &String::from_utf8(read_bounded(
            &journal.marker_path,
            MAX_MARKER_BYTES,
            "published hosted marker",
        )?)?,
        &journal.install_path,
        &journal.binary_sha256,
        journal
            .ownership_path
            .as_ref()
            .zip(journal.ownership_sha256.as_ref())
            .map(|(path, digest)| (path.as_path(), digest.as_str())),
    )?;
    journal.phase = Phase::Committed;
    write_journal(journal_path, journal)?;
    fault("committed")?;
    remove_if_present(&binary_staged)?;
    remove_if_present(&staged_marker_path(journal))?;
    remove_if_present(&staged_ownership_path(journal))?;
    remove_journal(journal_path)
}

fn uninstall_prepare(args: HostedTransactionArgs, install_path: PathBuf) -> Result<()> {
    let journal_path = journal_path(&install_path);
    let mut journal = match read_journal(&journal_path)? {
        Some(journal) => {
            validate_journal(&journal, &install_path, TransactionKind::Uninstall)?;
            if !matches!(journal.phase, Phase::Prepared | Phase::HelperStaged) {
                bail!("hosted uninstall transaction has already advanced past preparation");
            }
            journal
        }
        None => {
            let attempt_id = args
                .attempt_id
                .as_deref()
                .ok_or_else(|| anyhow!("hosted uninstall missing attempt identity"))?;
            let journal = new_uninstall_journal(&install_path, attempt_id)?;
            write_initial_journal(&journal_path, &journal)?;
            journal
        }
    };
    let current = current_executable()?;
    validate_uninstall_caller(&current, &journal)?;
    let helper = uninstall_helper_path(&install_path);
    if !file_has_digest(&helper, &journal.binary_sha256, MAX_BINARY_BYTES)? {
        if helper.try_exists()? {
            bail!("hosted uninstall helper does not match its recorded executable");
        }
        stage_file(&install_path, &helper, true)?;
    }
    verify_file_digest(
        &helper,
        &journal.binary_sha256,
        MAX_BINARY_BYTES,
        "hosted uninstall helper",
    )?;
    journal.phase = Phase::HelperStaged;
    write_journal(&journal_path, &journal)?;
    print_uninstall_receipt(&journal, &helper, "prepared")
}

fn uninstall_arm(_args: HostedTransactionArgs, install_path: PathBuf) -> Result<()> {
    let journal_path = journal_path(&install_path);
    let mut journal = required_uninstall_journal(&journal_path, &install_path)?;
    let current = current_executable()?;
    validate_helper_caller(&current, &journal)?;
    verify_file_digest(
        &install_path,
        &journal.binary_sha256,
        MAX_BINARY_BYTES,
        "managed executable",
    )?;
    verify_file_digest(
        &journal.marker_path,
        &journal.marker_sha256,
        MAX_MARKER_BYTES,
        "managed marker",
    )?;
    verify_recorded_ownership(&journal)?;
    journal.phase = Phase::Armed;
    write_journal(&journal_path, &journal)?;
    print_uninstall_receipt(&journal, &uninstall_helper_path(&install_path), "armed")
}

fn uninstall_commit(_args: HostedTransactionArgs, install_path: PathBuf) -> Result<()> {
    let journal_path = journal_path(&install_path);
    let mut journal = required_uninstall_journal(&journal_path, &install_path)?;
    let current = current_executable()?;
    complete_uninstall_commit(&current, &journal_path, &mut journal, &mut |_| Ok(()))?;
    print_uninstall_receipt(&journal, &uninstall_helper_path(&install_path), "committed")?;
    remove_journal(&journal_path)
}

fn new_uninstall_journal(install_path: &Path, attempt_id: &str) -> Result<Journal> {
    if !is_valid_install_attempt_id(attempt_id) {
        bail!("hosted uninstall has an invalid attempt identity");
    }
    let binary_sha256 = sha256_path(install_path, MAX_BINARY_BYTES, "managed executable")?;
    let marker_path = install_marker_path(install_path);
    let marker_body = String::from_utf8(read_bounded(
        &marker_path,
        MAX_MARKER_BYTES,
        "managed install marker",
    )?)?;
    let marker_sha256 = sha256_hex(marker_body.as_bytes());
    validate_marker_body(&marker_body, install_path, &binary_sha256, None)?;
    let recorded_ownership = read_recorded_ownership(&marker_body, install_path)?;
    let (ownership_path, ownership_sha256, ownership_body) = recorded_ownership
        .map(|(path, digest, body)| (Some(path), Some(digest), Some(body)))
        .unwrap_or((None, None, None));
    let mut journal = Journal {
        schema_version: SCHEMA_VERSION,
        kind: TransactionKind::Uninstall,
        attempt_id: attempt_id.to_owned(),
        marker_path,
        install_path: install_path.to_owned(),
        binary_sha256,
        marker_sha256,
        marker_body,
        prior_binary_sha256: None,
        prior_marker_sha256: None,
        prior_ownership_sha256: None,
        ownership_path,
        ownership_sha256,
        ownership_body,
        phase: Phase::Prepared,
        binding_sha256: String::new(),
    };
    journal.binding_sha256 = journal_binding(&journal);
    validate_journal(&journal, install_path, TransactionKind::Uninstall)?;
    Ok(journal)
}

fn complete_uninstall_commit(
    current: &Path,
    journal_path: &Path,
    journal: &mut Journal,
    fault: &mut dyn FnMut(&'static str) -> Result<()>,
) -> Result<()> {
    validate_helper_caller(current, journal)?;
    if !matches!(
        journal.phase,
        Phase::Armed
            | Phase::RemovingBinary
            | Phase::BinaryRemoved
            | Phase::RemovingOwnership
            | Phase::OwnershipRemoved
            | Phase::RemovingMarker
            | Phase::Committed
    ) {
        bail!("hosted uninstall transaction is not armed for removal");
    }
    fault("armed")?;
    if journal.install_path.try_exists()? {
        verify_file_digest(
            &journal.install_path,
            &journal.binary_sha256,
            MAX_BINARY_BYTES,
            "managed executable",
        )?;
        journal.phase = Phase::RemovingBinary;
        write_journal(journal_path, journal)?;
        fault("removing_binary")?;
        remove_durable(&journal.install_path)?;
        fault("binary_removed")?;
    }
    journal.phase = Phase::BinaryRemoved;
    write_journal(journal_path, journal)?;
    fault("binary_removed_recorded")?;
    if let Some(path) = journal.ownership_path.clone() {
        if journal.ownership_sha256.is_none() {
            bail!("hosted uninstall journal has incomplete integration ownership");
        }
        if path_entry_exists(&path)? {
            verify_recorded_ownership(journal)?;
            journal.phase = Phase::RemovingOwnership;
            write_journal(journal_path, journal)?;
            fault("removing_ownership")?;
            remove_durable(&path)?;
            fault("ownership_removed")?;
        }
        journal.phase = Phase::OwnershipRemoved;
        write_journal(journal_path, journal)?;
        fault("ownership_removed_recorded")?;
    }
    if journal.marker_path.try_exists()? {
        verify_file_digest(
            &journal.marker_path,
            &journal.marker_sha256,
            MAX_MARKER_BYTES,
            "managed marker",
        )?;
        journal.phase = Phase::RemovingMarker;
        write_journal(journal_path, journal)?;
        fault("removing_marker")?;
        remove_durable(&journal.marker_path)?;
        fault("marker_removed")?;
    }
    journal.phase = Phase::Committed;
    write_journal(journal_path, journal)?;
    fault("committed")
}

fn required_uninstall_journal(path: &Path, install_path: &Path) -> Result<Journal> {
    let journal = read_journal(path)?
        .ok_or_else(|| anyhow!("hosted uninstall has no recorded transaction"))?;
    validate_journal(&journal, install_path, TransactionKind::Uninstall)?;
    Ok(journal)
}

fn print_uninstall_receipt(journal: &Journal, helper: &Path, status: &str) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(&uninstall_receipt(journal, helper, status))?
    );
    Ok(())
}

fn install_receipt(journal: &Journal) -> Value {
    json!({
        "schema_version": 1,
        "command": "hosted_install_transaction",
        "ok": true,
        "status": "committed",
        "attempt_id": journal.attempt_id,
        "install_path": journal.install_path,
        "binary_sha256": journal.binary_sha256,
        "marker_sha256": journal.marker_sha256,
    })
}

fn uninstall_receipt(journal: &Journal, helper: &Path, status: &str) -> Value {
    json!({
        "schema_version": UNINSTALL_RECEIPT_SCHEMA_VERSION,
        "command": "hosted_uninstall_transaction",
        "ok": true,
        "status": status,
        "daemon_admission_fenced": true,
        "attempt_id": journal.attempt_id,
        "install_path": journal.install_path,
        "helper_path": helper,
        "binary_sha256": journal.binary_sha256,
        "marker_sha256": journal.marker_sha256,
    })
}

fn validate_uninstall_caller(current: &Path, journal: &Journal) -> Result<()> {
    if current != journal.install_path && current != uninstall_helper_path(&journal.install_path) {
        bail!("hosted uninstall caller is not the recorded executable or helper");
    }
    verify_file_digest(
        current,
        &journal.binary_sha256,
        MAX_BINARY_BYTES,
        "hosted uninstall caller",
    )
}

fn validate_helper_caller(current: &Path, journal: &Journal) -> Result<()> {
    let helper = uninstall_helper_path(&journal.install_path);
    if current != helper {
        bail!("hosted uninstall continuation must run from its recorded helper");
    }
    verify_file_digest(
        current,
        &journal.binary_sha256,
        MAX_BINARY_BYTES,
        "hosted uninstall helper",
    )
}

fn verify_recorded_ownership(journal: &Journal) -> Result<()> {
    if let (Some(path), Some(digest)) = (
        journal.ownership_path.as_ref(),
        journal.ownership_sha256.as_ref(),
    ) {
        verify_file_digest(
            path,
            digest,
            MAX_OWNERSHIP_BYTES,
            "managed integration ownership",
        )
        .with_context(|| {
            format!(
                "integration ownership at {} changed; restore the transaction-owned sidecar or move the changed file aside, then retry hosted uninstall",
                path.display()
            )
        })?;
    }
    Ok(())
}

fn read_recorded_ownership(
    marker: &str,
    install_path: &Path,
) -> Result<Option<(PathBuf, String, Vec<u8>)>> {
    let value: Value = serde_json::from_str(marker)?;
    match (
        value.get("integrations_path"),
        value.get("integrations_sha256"),
    ) {
        (None, None) => Ok(None),
        (Some(Value::String(path)), Some(Value::String(digest))) => {
            let expected_path = ownership_path(install_path);
            if Some(path.as_str()) != expected_path.to_str() || !is_normalized_sha256(digest) {
                bail!("managed marker has invalid integration ownership identity");
            }
            let body = read_bounded(
                &expected_path,
                MAX_OWNERSHIP_BYTES,
                "managed integration ownership",
            )?;
            if sha256_hex(&body) != *digest {
                bail!("managed integration ownership digest does not match its marker");
            }
            Ok(Some((expected_path, digest.clone(), body)))
        }
        _ => bail!("managed marker has incomplete integration ownership identity"),
    }
}

fn validate_existing_pair_for_install(
    install_path: &Path,
) -> Result<Option<(String, String, Option<String>)>> {
    let marker_path = install_marker_path(install_path);
    let binary_exists = install_path.try_exists()?;
    let marker_exists = marker_path.try_exists()?;
    if binary_exists != marker_exists {
        bail!("an existing ctx install requires its regular hosted-install marker");
    }
    if !binary_exists {
        return Ok(None);
    }
    let digest = sha256_path(install_path, MAX_BINARY_BYTES, "prior managed executable")?;
    let marker = String::from_utf8(read_bounded(
        &marker_path,
        MAX_MARKER_BYTES,
        "prior managed marker",
    )?)?;
    validate_marker_body(&marker, install_path, &digest, None)
        .context("existing ctx install is not owned by the hosted installer")?;
    let prior_ownership =
        read_recorded_ownership(&marker, install_path)?.map(|(_path, digest, _body)| digest);
    Ok(Some((
        digest,
        sha256_hex(marker.as_bytes()),
        prior_ownership,
    )))
}

fn ensure_target_is_recorded_prior_or_absent(
    journal: &Journal,
    path: &Path,
    label: &str,
) -> Result<()> {
    if !path.try_exists()? {
        return Ok(());
    }
    if path == journal.install_path {
        let digest = sha256_path(path, MAX_BINARY_BYTES, label)?;
        if journal.prior_binary_sha256.as_deref() == Some(digest.as_str()) {
            return Ok(());
        }
        bail!("refusing to replace an executable outside the recorded transaction");
    }
    if path == journal.marker_path {
        let digest = sha256_path(path, MAX_MARKER_BYTES, label)?;
        if journal.prior_marker_sha256.as_deref() == Some(digest.as_str()) {
            return Ok(());
        }
        bail!("refusing to replace a marker outside the recorded transaction");
    }
    Ok(())
}

fn validate_marker_body(
    body: &str,
    install_path: &Path,
    binary_sha256: &str,
    ownership: Option<(&Path, &str)>,
) -> Result<()> {
    let value: Value = serde_json::from_str(body).context("parse hosted install marker")?;
    if value.get("schema_version").and_then(Value::as_u64) != Some(1)
        || value.get("manager").and_then(Value::as_str) != Some("ctx-hosted-installer")
        || value.get("install_path").and_then(Value::as_str) != install_path.to_str()
        || !value
            .get("sha256")
            .and_then(Value::as_str)
            .is_some_and(|value| value.eq_ignore_ascii_case(binary_sha256))
        || value.get("platform").and_then(Value::as_str) != Some(platform_key()?)
    {
        bail!("hosted install marker does not bind the signed executable and target");
    }
    if let Some((path, digest)) = ownership {
        if value.get("integrations_path").and_then(Value::as_str) != path.to_str()
            || value.get("integrations_sha256").and_then(Value::as_str) != Some(digest)
        {
            bail!("hosted install marker does not bind integration ownership");
        }
    }
    Ok(())
}

fn validate_journal(journal: &Journal, install_path: &Path, kind: TransactionKind) -> Result<()> {
    if journal.schema_version != SCHEMA_VERSION
        || journal.kind != kind
        || journal.install_path != install_path
        || journal.marker_path != install_marker_path(install_path)
        || !is_valid_install_attempt_id(&journal.attempt_id)
        || normalized_sha256(&journal.binary_sha256)? != journal.binary_sha256
        || normalized_sha256(&journal.marker_sha256)? != journal.marker_sha256
        || sha256_hex(journal.marker_body.as_bytes()) != journal.marker_sha256
        || journal.prior_binary_sha256.is_some() != journal.prior_marker_sha256.is_some()
        || (journal.prior_binary_sha256.is_none() && journal.prior_ownership_sha256.is_some())
        || journal
            .prior_binary_sha256
            .as_deref()
            .is_some_and(|value| !is_normalized_sha256(value))
        || journal
            .prior_marker_sha256
            .as_deref()
            .is_some_and(|value| !is_normalized_sha256(value))
        || journal
            .prior_ownership_sha256
            .as_deref()
            .is_some_and(|value| !is_normalized_sha256(value))
        || journal.binding_sha256 != journal_binding(journal)
        || !phase_matches_kind(journal.phase, kind)
    {
        bail!("hosted transaction journal identity is invalid");
    }
    match (
        journal.ownership_path.as_ref(),
        journal.ownership_sha256.as_ref(),
        journal.ownership_body.as_ref(),
    ) {
        (None, None, None) => {}
        (Some(path), Some(digest), Some(body))
            if path == &ownership_path(install_path)
                && normalized_sha256(digest)? == *digest
                && sha256_hex(body) == *digest => {}
        _ => bail!("hosted transaction journal ownership identity is invalid"),
    }
    let marker: Value = serde_json::from_str(&journal.marker_body)?;
    match (
        journal.ownership_path.as_ref(),
        journal.ownership_sha256.as_ref(),
        marker.get("integrations_path"),
        marker.get("integrations_sha256"),
    ) {
        (None, None, None, None) => {}
        (
            Some(path),
            Some(digest),
            Some(Value::String(marker_path)),
            Some(Value::String(marker_digest)),
        ) if Some(marker_path.as_str()) == path.to_str() && marker_digest == digest => {}
        _ => bail!("hosted transaction marker and integration ownership do not match"),
    }
    validate_marker_body(
        &journal.marker_body,
        install_path,
        &journal.binary_sha256,
        journal
            .ownership_path
            .as_ref()
            .zip(journal.ownership_sha256.as_ref())
            .map(|(path, digest)| (path.as_path(), digest.as_str())),
    )
}

fn phase_matches_kind(phase: Phase, kind: TransactionKind) -> bool {
    match kind {
        TransactionKind::Install => matches!(
            phase,
            Phase::Prepared
                | Phase::BinaryStaged
                | Phase::PublishingBinary
                | Phase::BinaryPublished
                | Phase::PublishingOwnership
                | Phase::OwnershipPublished
                | Phase::PublishingMarker
                | Phase::MarkerPublished
                | Phase::Committed
        ),
        TransactionKind::Uninstall => matches!(
            phase,
            Phase::Prepared
                | Phase::HelperStaged
                | Phase::Armed
                | Phase::RemovingBinary
                | Phase::BinaryRemoved
                | Phase::RemovingOwnership
                | Phase::OwnershipRemoved
                | Phase::RemovingMarker
                | Phase::Committed
        ),
    }
}

fn journal_binding(journal: &Journal) -> String {
    sha256_hex(
        format!(
            "{}\0{:?}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}",
            journal.schema_version,
            journal.kind,
            journal.attempt_id,
            journal.install_path.display(),
            journal.marker_path.display(),
            journal.binary_sha256,
            journal.marker_sha256,
            journal.prior_binary_sha256.as_deref().unwrap_or_default(),
            journal.prior_marker_sha256.as_deref().unwrap_or_default(),
            journal
                .prior_ownership_sha256
                .as_deref()
                .unwrap_or_default(),
            journal.ownership_sha256.as_deref().unwrap_or_default()
        )
        .as_bytes(),
    )
}

#[cfg(all(test, unix))]
mod tests;
