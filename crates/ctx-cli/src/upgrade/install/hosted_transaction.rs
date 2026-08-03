use std::{
    env, fs,
    io::Write as _,
    path::{Component, Path, PathBuf},
};

use anyhow::{anyhow, bail, Context, Result};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use super::marker::{install_marker_path, is_valid_install_attempt_id};
use crate::upgrade::{platform_key, sha256_hex};

const SCHEMA_VERSION: u32 = 1;
const JOURNAL_SUFFIX: &str = "hosted-install-transaction.json";
const MAX_MARKER_BYTES: u64 = 64 * 1024;
const MAX_OWNERSHIP_BYTES: u64 = 1024 * 1024;
const MAX_BINARY_BYTES: u64 = 512 * 1024 * 1024;

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
            write_initial_journal(&journal_path, &journal)?;
            journal
        }
    };
    complete_install(&source, &journal_path, &mut journal)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "schema_version": 1,
            "command": "hosted_install_transaction",
            "ok": true,
            "status": "committed",
            "attempt_id": journal.attempt_id,
            "install_path": journal.install_path,
            "binary_sha256": journal.binary_sha256,
            "marker_sha256": journal.marker_sha256,
        }))?
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
            if !is_valid_install_attempt_id(attempt_id) {
                bail!("hosted uninstall has an invalid attempt identity");
            }
            let binary_sha256 = sha256_path(&install_path, MAX_BINARY_BYTES, "managed executable")?;
            let marker_path = install_marker_path(&install_path);
            let marker_body = String::from_utf8(read_bounded(
                &marker_path,
                MAX_MARKER_BYTES,
                "managed install marker",
            )?)?;
            let marker_sha256 = sha256_hex(marker_body.as_bytes());
            validate_marker_body(&marker_body, &install_path, &binary_sha256, None)?;
            let mut journal = Journal {
                schema_version: SCHEMA_VERSION,
                kind: TransactionKind::Uninstall,
                attempt_id: attempt_id.to_owned(),
                marker_path,
                install_path: install_path.clone(),
                binary_sha256,
                marker_sha256,
                marker_body,
                prior_binary_sha256: None,
                prior_marker_sha256: None,
                prior_ownership_sha256: None,
                ownership_path: None,
                ownership_sha256: None,
                ownership_body: None,
                phase: Phase::Prepared,
                binding_sha256: String::new(),
            };
            journal.binding_sha256 = journal_binding(&journal);
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
        write_journal(&journal_path, &journal)?;
        fault("removing_binary")?;
        remove_durable(&journal.install_path)?;
        fault("binary_removed")?;
    }
    journal.phase = Phase::BinaryRemoved;
    write_journal(&journal_path, &journal)?;
    fault("binary_removed_recorded")?;
    if journal.marker_path.try_exists()? {
        verify_file_digest(
            &journal.marker_path,
            &journal.marker_sha256,
            MAX_MARKER_BYTES,
            "managed marker",
        )?;
        journal.phase = Phase::RemovingMarker;
        write_journal(&journal_path, &journal)?;
        fault("removing_marker")?;
        remove_durable(&journal.marker_path)?;
        fault("marker_removed")?;
    }
    journal.phase = Phase::Committed;
    write_journal(&journal_path, &journal)?;
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
        serde_json::to_string_pretty(&json!({
            "schema_version": 1,
            "command": "hosted_uninstall_transaction",
            "ok": true,
            "status": status,
            "attempt_id": journal.attempt_id,
            "install_path": journal.install_path,
            "helper_path": helper,
            "binary_sha256": journal.binary_sha256,
            "marker_sha256": journal.marker_sha256,
        }))?
    );
    Ok(())
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
    let value: Value = serde_json::from_str(&marker)?;
    let prior_ownership = match (
        value.get("integrations_path").and_then(Value::as_str),
        value.get("integrations_sha256").and_then(Value::as_str),
    ) {
        (None, None) => None,
        (Some(path), Some(digest)) => {
            let expected_path = ownership_path(install_path);
            if Some(path) != expected_path.to_str() || !is_normalized_sha256(digest) {
                bail!("prior managed marker has invalid integration ownership");
            }
            verify_file_digest(
                &expected_path,
                digest,
                MAX_OWNERSHIP_BYTES,
                "prior hosted integration ownership",
            )?;
            Some(digest.to_owned())
        }
        _ => bail!("prior managed marker has incomplete integration ownership"),
    };
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

fn validate_install_path(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
        || path.file_name().is_none()
    {
        bail!("hosted transaction install path is not a safe absolute leaf");
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("hosted transaction install path has no parent"))?;
    let canonical_parent = fs::canonicalize(parent)
        .with_context(|| format!("canonicalize hosted install directory {}", parent.display()))?;
    validate_private_directory(&canonical_parent)?;
    let canonical = canonical_parent.join(
        path.file_name()
            .ok_or_else(|| anyhow!("hosted transaction install path has no file name"))?,
    );
    if canonical != path {
        bail!("hosted transaction install path is not canonical");
    }
    Ok(canonical)
}

#[cfg(unix)]
fn validate_private_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        bail!("hosted transaction install directory is not owner-private");
    }
    Ok(())
}

#[cfg(windows)]
fn validate_private_directory(path: &Path) -> Result<()> {
    ctx_history_core::platform_security::verify_private_directory(path)
        .context("verify hosted transaction install directory")
}

#[cfg(not(any(unix, windows)))]
fn validate_private_directory(_path: &Path) -> Result<()> {
    bail!("hosted transactions are unsupported on this platform")
}

fn current_executable() -> Result<PathBuf> {
    fs::canonicalize(env::current_exe().context("resolve hosted transaction executable")?)
        .context("canonicalize hosted transaction executable")
}

fn journal_path(install_path: &Path) -> PathBuf {
    install_path.with_file_name(format!(
        ".{}.{}",
        install_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("ctx"),
        JOURNAL_SUFFIX
    ))
}

fn staged_binary_path(journal: &Journal) -> PathBuf {
    sibling(journal, "binary.new")
}

fn staged_marker_path(journal: &Journal) -> PathBuf {
    sibling(journal, "marker.new")
}

fn staged_ownership_path(journal: &Journal) -> PathBuf {
    sibling(journal, "ownership.new")
}

fn sibling(journal: &Journal, suffix: &str) -> PathBuf {
    journal.install_path.with_file_name(format!(
        ".{}.hosted-{}.{}",
        journal
            .install_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("ctx"),
        journal.attempt_id,
        suffix
    ))
}

fn ownership_path(install_path: &Path) -> PathBuf {
    let mut name = install_path.file_name().unwrap_or_default().to_os_string();
    name.push(".install-integrations");
    install_path.with_file_name(name)
}

pub(in crate::upgrade) fn uninstall_helper_path(install_path: &Path) -> PathBuf {
    let name = install_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("ctx");
    #[cfg(windows)]
    let suffix = "hosted-uninstall-helper.exe";
    #[cfg(not(windows))]
    let suffix = "hosted-uninstall-helper";
    install_path.with_file_name(format!(".{name}.{suffix}"))
}

fn read_journal(path: &Path) -> Result<Option<Journal>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    verify_private_file(path)?;
    let journal = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse hosted transaction {}", path.display()))?;
    Ok(Some(journal))
}

fn write_initial_journal(path: &Path, journal: &Journal) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("hosted journal has no parent"))?;
    let temporary = parent.join(format!(
        ".{JOURNAL_SUFFIX}.{}.initial",
        Uuid::new_v4().simple()
    ));
    let result = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        restrict_private_file(&temporary)?;
        file.write_all(&serde_json::to_vec_pretty(journal)?)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::hard_link(&temporary, path)
            .with_context(|| format!("claim hosted transaction {}", path.display()))?;
        remove_if_present(&temporary)?;
        sync_parent(path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn write_journal(path: &Path, journal: &Journal) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("hosted journal has no parent"))?;
    let temporary = parent.join(format!(".{JOURNAL_SUFFIX}.{}.tmp", Uuid::new_v4().simple()));
    let result = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        restrict_private_file(&temporary)?;
        file.write_all(&serde_json::to_vec_pretty(journal)?)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        atomic_publish(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn remove_journal(path: &Path) -> Result<()> {
    remove_durable(path)
}

fn stage_file(source: &Path, target: &Path, executable: bool) -> Result<()> {
    let bytes = read_bounded(source, MAX_BINARY_BYTES, "hosted transaction source")?;
    stage_bytes(&bytes, target, executable)
}

fn stage_bytes(bytes: &[u8], target: &Path, executable: bool) -> Result<()> {
    remove_if_present(target)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    if executable {
        restrict_private_executable(target)?;
    } else {
        restrict_private_file(target)?;
    }
    sync_parent(target)
}

#[cfg(unix)]
fn atomic_publish(source: &Path, target: &Path) -> Result<()> {
    fs::rename(source, target).with_context(|| {
        format!(
            "atomically publish {} to {}",
            source.display(),
            target.display()
        )
    })?;
    sync_parent(target)
}

#[cfg(windows)]
fn atomic_publish(source: &Path, target: &Path) -> Result<()> {
    super::transaction::durable_replace_file(source, target)
}

#[cfg(not(any(unix, windows)))]
fn atomic_publish(_source: &Path, _target: &Path) -> Result<()> {
    bail!("hosted transactions are unsupported on this platform")
}

fn remove_durable(path: &Path) -> Result<()> {
    remove_if_present(path)?;
    sync_parent(path)
}

fn remove_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("remove {}", path.display())),
    }
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> Result<()> {
    let parent = path.parent().ok_or_else(|| anyhow!("path has no parent"))?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> Result<()> {
    Ok(())
}

fn read_bounded(path: &Path, max: u64, label: &str) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path).with_context(|| format!("inspect {label}"))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() || metadata.len() > max
    {
        bail!("{label} is not a bounded regular file");
    }
    fs::read(path).with_context(|| format!("read {label}"))
}

fn sha256_path(path: &Path, max: u64, label: &str) -> Result<String> {
    Ok(sha256_hex(&read_bounded(path, max, label)?))
}

fn verify_file_digest(path: &Path, expected: &str, max: u64, label: &str) -> Result<()> {
    let actual = sha256_path(path, max, label)?;
    if actual != expected {
        bail!("{label} digest does not match its hosted transaction");
    }
    Ok(())
}

fn file_has_digest(path: &Path, expected: &str, max: u64) -> Result<bool> {
    if !path.try_exists()? {
        return Ok(false);
    }
    Ok(sha256_path(path, max, "hosted transaction path")? == expected)
}

fn normalized_sha256(value: &str) -> Result<String> {
    let normalized = value.to_ascii_lowercase();
    if normalized.len() != 64 || !normalized.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("hosted transaction SHA-256 identity is invalid");
    }
    Ok(normalized)
}

fn is_normalized_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

#[cfg(unix)]
fn restrict_private_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(windows)]
fn restrict_private_file(path: &Path) -> Result<()> {
    ctx_history_core::platform_security::restrict_private_file(path).map_err(Into::into)
}

#[cfg(not(any(unix, windows)))]
fn restrict_private_file(_path: &Path) -> Result<()> {
    bail!("hosted transactions are unsupported on this platform")
}

#[cfg(unix)]
fn restrict_private_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(unix)]
fn set_installed_executable_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    sync_parent(path)
}

#[cfg(not(unix))]
fn set_installed_executable_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(windows)]
fn restrict_private_executable(path: &Path) -> Result<()> {
    ctx_history_core::platform_security::restrict_private_executable(path).map_err(Into::into)
}

#[cfg(not(any(unix, windows)))]
fn restrict_private_executable(_path: &Path) -> Result<()> {
    bail!("hosted transactions are unsupported on this platform")
}

#[cfg(unix)]
fn verify_private_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        bail!("hosted transaction journal is not owner-private");
    }
    Ok(())
}

#[cfg(windows)]
fn verify_private_file(path: &Path) -> Result<()> {
    ctx_history_core::platform_security::verify_private_file(path).map_err(Into::into)
}

#[cfg(not(any(unix, windows)))]
fn verify_private_file(_path: &Path) -> Result<()> {
    bail!("hosted transactions are unsupported on this platform")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn fixture() -> (tempfile::TempDir, PathBuf, String, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let install = temp.path().join("ctx");
        let binary = b"new ctx";
        let digest = sha256_hex(binary);
        let source = temp.path().join("candidate");
        fs::write(&source, binary).unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o700)).unwrap();
        (temp, install, digest, source)
    }

    fn marker(install: &Path, digest: &str) -> String {
        serde_json::to_string_pretty(&json!({
            "schema_version": 1,
            "manager": "ctx-hosted-installer",
            "install_attempt_id": "ia_12345678",
            "install_path": install,
            "platform": platform_key().unwrap(),
            "channel": "stable",
            "version": "1.0.0",
            "sha256": digest,
        }))
        .unwrap()
            + "\n"
    }

    #[test]
    fn journal_binding_rejects_path_and_digest_changes() {
        let (_temp, install, digest, _source) = fixture();
        let body = marker(&install, &digest);
        let mut journal = Journal {
            schema_version: 1,
            kind: TransactionKind::Install,
            attempt_id: "ia_12345678".into(),
            install_path: install.clone(),
            marker_path: install_marker_path(&install),
            binary_sha256: digest,
            marker_sha256: sha256_hex(body.as_bytes()),
            marker_body: body,
            prior_binary_sha256: None,
            prior_marker_sha256: None,
            prior_ownership_sha256: None,
            ownership_path: None,
            ownership_sha256: None,
            ownership_body: None,
            phase: Phase::Prepared,
            binding_sha256: String::new(),
        };
        journal.binding_sha256 = journal_binding(&journal);
        assert!(validate_journal(&journal, &install, TransactionKind::Install).is_ok());
        journal.install_path = install.with_file_name("other-ctx");
        assert!(validate_journal(&journal, &install, TransactionKind::Install).is_err());
        journal.install_path = install.clone();
        journal.binary_sha256 = "0".repeat(64);
        assert!(validate_journal(&journal, &install, TransactionKind::Install).is_err());
    }

    #[test]
    fn markerless_existing_binary_remains_unmanaged() {
        let (_temp, install, _digest, _source) = fixture();
        fs::write(&install, b"new ctx").unwrap();
        assert!(validate_existing_pair_for_install(&install).is_err());
    }

    #[test]
    fn posix_publication_consumes_a_sibling_without_truncating_target() {
        let temp = tempfile::tempdir().unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let target = temp.path().join("ctx");
        let staged = temp.path().join(".ctx.new");
        fs::write(&target, b"old-complete").unwrap();
        fs::write(&staged, b"new-complete").unwrap();
        atomic_publish(&staged, &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new-complete");
        assert!(!staged.exists());
    }

    #[test]
    fn install_retry_converges_from_every_durable_phase() {
        const POINTS: &[&str] = &[
            "journal_prepared",
            "binary_staged",
            "binary_replaced",
            "binary_published",
            "ownership_replaced",
            "ownership_published",
            "marker_replaced",
            "marker_published",
            "committed",
        ];
        for point in POINTS {
            let (_temp, install, digest, source) = fixture();
            let ownership_path = ownership_path(&install);
            let ownership_body = b"CTX_INSTALL_INTEGRATIONS_V1\nrecords_sha256\tfixture\n".to_vec();
            let ownership_digest = sha256_hex(&ownership_body);
            let marker_body = serde_json::to_string_pretty(&json!({
                "schema_version": 1,
                "manager": "ctx-hosted-installer",
                "install_attempt_id": "ia_12345678",
                "install_path": install,
                "platform": platform_key().unwrap(),
                "channel": "stable",
                "version": "1.0.0",
                "sha256": digest,
                "integrations_path": ownership_path,
                "integrations_sha256": ownership_digest,
            }))
            .unwrap()
                + "\n";
            let mut journal = Journal {
                schema_version: 1,
                kind: TransactionKind::Install,
                attempt_id: "ia_12345678".into(),
                install_path: install.clone(),
                marker_path: install_marker_path(&install),
                binary_sha256: digest.clone(),
                marker_sha256: sha256_hex(marker_body.as_bytes()),
                marker_body,
                prior_binary_sha256: None,
                prior_marker_sha256: None,
                prior_ownership_sha256: None,
                ownership_path: Some(ownership_path.clone()),
                ownership_sha256: Some(ownership_digest.clone()),
                ownership_body: Some(ownership_body),
                phase: Phase::Prepared,
                binding_sha256: String::new(),
            };
            journal.binding_sha256 = journal_binding(&journal);
            let journal_path = journal_path(&install);
            write_initial_journal(&journal_path, &journal).unwrap();
            let mut injected = false;
            let error = complete_install_with_fault(
                &source,
                &journal_path,
                &mut journal,
                &mut |observed| {
                    if !injected && observed == *point {
                        injected = true;
                        bail!("injected interruption after {observed}");
                    }
                    Ok(())
                },
            )
            .unwrap_err();
            assert!(
                error.to_string().contains("injected interruption"),
                "{point}"
            );
            let mut recovered = read_journal(&journal_path).unwrap().unwrap();
            validate_journal(&recovered, &install, TransactionKind::Install).unwrap();
            complete_install(&source, &journal_path, &mut recovered).unwrap();
            assert_eq!(fs::read(&install).unwrap(), b"new ctx", "{point}");
            assert_eq!(
                sha256_path(&install_marker_path(&install), MAX_MARKER_BYTES, "marker").unwrap(),
                recovered.marker_sha256,
                "{point}"
            );
            assert_eq!(
                sha256_path(&ownership_path, MAX_OWNERSHIP_BYTES, "ownership").unwrap(),
                ownership_digest,
                "{point}"
            );
            assert!(!journal_path.exists(), "{point}");
        }
    }

    #[test]
    fn uninstall_commit_recovers_every_recorded_removal_phase() {
        const POINTS: &[&str] = &[
            "armed",
            "removing_binary",
            "binary_removed",
            "binary_removed_recorded",
            "removing_marker",
            "marker_removed",
            "committed",
        ];
        for point in POINTS {
            let (_temp, install, digest, source) = fixture();
            fs::copy(&source, &install).unwrap();
            let marker_body = marker(&install, &digest);
            fs::write(install_marker_path(&install), &marker_body).unwrap();
            let helper = uninstall_helper_path(&install);
            fs::copy(&source, &helper).unwrap();
            let mut journal = Journal {
                schema_version: 1,
                kind: TransactionKind::Uninstall,
                attempt_id: "ia_12345678".into(),
                install_path: install.clone(),
                marker_path: install_marker_path(&install),
                binary_sha256: digest,
                marker_sha256: sha256_hex(marker_body.as_bytes()),
                marker_body,
                prior_binary_sha256: None,
                prior_marker_sha256: None,
                prior_ownership_sha256: None,
                ownership_path: None,
                ownership_sha256: None,
                ownership_body: None,
                phase: Phase::Armed,
                binding_sha256: String::new(),
            };
            journal.binding_sha256 = journal_binding(&journal);
            let journal_path = journal_path(&install);
            write_initial_journal(&journal_path, &journal).unwrap();
            let mut injected = false;
            let error =
                complete_uninstall_commit(&helper, &journal_path, &mut journal, &mut |observed| {
                    if !injected && observed == *point {
                        injected = true;
                        bail!("injected interruption after {observed}");
                    }
                    Ok(())
                })
                .unwrap_err();
            assert!(
                error.to_string().contains("injected interruption"),
                "{point}"
            );
            let mut recovered = read_journal(&journal_path).unwrap().unwrap();
            complete_uninstall_commit(&helper, &journal_path, &mut recovered, &mut |_| Ok(()))
                .unwrap();
            remove_journal(&journal_path).unwrap();
            assert!(!install.exists(), "{point}");
            assert!(!install_marker_path(&install).exists(), "{point}");
            assert!(!journal_path.exists(), "{point}");
        }
    }
}
