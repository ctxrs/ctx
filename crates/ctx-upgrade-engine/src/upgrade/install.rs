mod archive;
mod durability;
mod hosted_transaction;
mod lock;
mod lock_fs;
#[cfg(test)]
mod lock_tests;
mod marker;
mod runtime;
mod transaction;

use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;

#[cfg(unix)]
use anyhow::Context;
use anyhow::{bail, Result};

pub use hosted_transaction::{
    hosted_uninstall_is_active_for_executable as installation_hosted_uninstall_is_active_for_executable,
    installation_hosted_uninstall_is_active,
};
pub use hosted_transaction::{
    run as run_hosted_transaction, HostedTransactionAction, HostedTransactionArgs,
};
pub(in crate::upgrade) use lock_fs::{read_stable_file, StableFileKind};
#[cfg(any(unix, test))]
pub(in crate::upgrade) use marker::classify_install_marker_at;
pub(in crate::upgrade) use marker::install_marker_path;
pub use marker::is_valid_install_attempt_id;
pub(super) use marker::InstallFingerprint;
pub use marker::{
    current_install_path, invalid_install_marker_recovery_guidance,
    unmanaged_install_conversion_guidance, InstallMarker,
};
pub use marker::{managed_install_marker_for_current_exe, ManagedInstallMarker};
pub(super) use transaction::ApplyResult;
#[cfg(windows)]
pub(in crate::upgrade) use transaction::HelperOutcome;
#[cfg(unix)]
pub(super) use transaction::RECOVERY_REEXEC_ENV;
pub(super) use transaction::{PendingRecovery, TerminalRecovery};

use self::lock::canonical_executable;
pub(in crate::upgrade) use self::lock::InstallationLock;
use super::{ReleaseProcessPort, SemanticLayoutPort, UpgradePlan};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RepairRequirements {
    pub(super) catalog: bool,
    pub(super) legacy_runtime: bool,
}

impl RepairRequirements {
    pub(super) fn any(self) -> bool {
        self.catalog || self.legacy_runtime
    }
}

pub(super) fn classify_repair_requirements(
    layout: &dyn SemanticLayoutPort,
    plan: &UpgradePlan,
    data_root: &Path,
    semantic_enabled: bool,
) -> Result<RepairRequirements> {
    let catalog = runtime::semantic_install_required(layout, plan, data_root)?;
    let legacy_runtime = if !plan.update_available
        && plan.latest_version == plan.current_version
        && semantic_enabled
        && plan.semantic_provisioning.is_none()
        && plan.metadata.onnxruntime.is_some()
    {
        runtime::legacy_runtime_install_required(plan, data_root)?
    } else {
        false
    };
    debug_assert!(!(catalog && legacy_runtime));
    Ok(RepairRequirements {
        catalog,
        legacy_runtime,
    })
}

#[cfg(unix)]
pub(in crate::upgrade) fn discard_legacy_previous_binary(install_path: &Path) -> Result<()> {
    transaction::discard_legacy_previous_binary(install_path)
}

/// The installed executable and marker observed under the executable-scoped
/// lock while a plan is built.  The installed marker version, not this
/// process's compile-time version, is the authority for update decisions.
#[derive(Debug, Clone)]
pub(in crate::upgrade) struct InstallSnapshot {
    pub(in crate::upgrade) marker: InstallMarker,
    pub(in crate::upgrade) fingerprint: InstallFingerprint,
}

pub(in crate::upgrade) fn capture_install_snapshot(
    _installation_lock: &InstallationLock,
    require_managed: bool,
    platform: &str,
    channel: &str,
    fallback_current_version: &str,
    warnings: &mut Vec<String>,
) -> Result<InstallSnapshot> {
    let marker = marker::install_marker_for_plan(
        require_managed,
        platform,
        channel,
        fallback_current_version,
        warnings,
    )?;
    let fingerprint = marker::install_fingerprint(&marker.install_path)?;
    Ok(InstallSnapshot {
        marker,
        fingerprint,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::upgrade) enum InstallRecovery {
    None,
    Recovered {
        committed: bool,
    },
    #[cfg(windows)]
    Scheduled {
        attempt_id: String,
        helper_pid: u32,
    },
    #[cfg(unix)]
    ReexecCurrentFormat(CurrentFormatRecoveryReexec),
}

/// An executable restored from a schema-2 transaction after its journal and
/// filesystem identity were revalidated under the installation lock.
#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::upgrade) struct CurrentFormatRecoveryReexec {
    executable: PathBuf,
    attempt_id: String,
}

#[cfg(unix)]
impl CurrentFormatRecoveryReexec {
    fn validated(
        expected: &PendingRecovery,
        restored: &Path,
        _installation_lock: &InstallationLock,
    ) -> Result<Self> {
        let executable = canonical_executable(restored)
            .context("validate executable restored by schema-2 install recovery")?;
        if restored != expected.install_path || executable != expected.install_path {
            bail!(
                "schema-2 install recovery restored unexpected executable {}; expected {}; refusing re-exec and requiring a fix-forward reinstall or upgrade",
                executable.display(),
                expected.install_path.display()
            );
        }
        Ok(Self {
            executable,
            attempt_id: expected.attempt_id.clone(),
        })
    }
}

#[allow(clippy::too_many_arguments)]
pub(in crate::upgrade) fn apply_artifact(
    process: &dyn ReleaseProcessPort,
    semantic_layout: &dyn SemanticLayoutPort,
    _installation_lock: &InstallationLock,
    plan: &UpgradePlan,
    artifact: Option<&mut super::download::DownloadedArtifact>,
    runtime_artifact: Option<&mut super::download::DownloadedArtifact>,
    semantic_artifacts: &mut [super::download::DownloadedArtifact],
    data_root: &Path,
    attempt_id: &str,
    daemon_restart: Option<(&str, Option<u64>)>,
    before_publish: &mut dyn FnMut() -> Result<()>,
) -> Result<ApplyResult> {
    let running_executable = current_install_path()?;
    let planned_executable = canonical_executable(&plan.install_path)?;
    if planned_executable != running_executable {
        bail!(
            "upgrade target {} is not the running ctx executable {}",
            planned_executable.display(),
            running_executable.display()
        );
    }

    // On Windows the parent retains this lock through the helper's validated
    // readiness receipt. The helper then blocks on the same lock and performs
    // a second fingerprint check immediately before publication.
    revalidate_plan_snapshot_locked(plan, &running_executable)?;
    transaction::apply_artifact_for_attempt(
        process,
        semantic_layout,
        plan,
        artifact,
        runtime_artifact,
        semantic_artifacts,
        data_root,
        attempt_id,
        daemon_restart,
        before_publish,
    )
}

fn revalidate_plan_snapshot_locked(plan: &UpgradePlan, running_executable: &Path) -> Result<()> {
    if running_executable != plan.install_path {
        bail!(
            "managed executable changed after this upgrade plan was created; refusing stale cross-root publication"
        );
    }
    let mut warnings = Vec::new();
    let marker = marker::install_marker_for_plan(
        true,
        &plan.platform,
        &plan.channel,
        &plan.current_version,
        &mut warnings,
    )?;
    let observed = marker::install_fingerprint(running_executable)?;
    if marker.install_path != plan.install_path
        || marker.version != plan.current_version
        || observed != plan.install_fingerprint
    {
        bail!(
            "managed executable or install marker changed after this upgrade plan was created; refusing stale cross-root publication"
        );
    }
    Ok(())
}

pub(in crate::upgrade) fn pending_recovery(
    data_root: &Path,
    semantic_layout: &dyn SemanticLayoutPort,
) -> Result<Option<transaction::PendingRecovery>> {
    transaction::pending_recovery(data_root, semantic_layout)
}

pub(in crate::upgrade) fn remove_terminal_recovery(
    expected: &PendingRecovery,
    installation_lock: &InstallationLock,
    semantic_layout: &dyn SemanticLayoutPort,
) -> Result<()> {
    transaction::remove_terminal_recovery(expected, installation_lock, semantic_layout)
}

pub(in crate::upgrade) fn validate_recovery_observation(
    expected: &PendingRecovery,
    terminal: bool,
    _installation_lock: &InstallationLock,
    semantic_layout: &dyn SemanticLayoutPort,
) -> Result<()> {
    transaction::validate_recovery_observation(expected, terminal, semantic_layout)
}

pub(in crate::upgrade) fn recover_interrupted_install(
    process: &dyn ReleaseProcessPort,
    expected: &PendingRecovery,
    installation_lock: &InstallationLock,
    semantic_layout: &dyn SemanticLayoutPort,
) -> Result<InstallRecovery> {
    #[cfg(unix)]
    {
        let outcome = transaction::recover_interrupted_install_outcome(
            process,
            expected,
            installation_lock,
            semantic_layout,
        )?;
        if let Some(path) = outcome.restored_executable() {
            return Ok(InstallRecovery::ReexecCurrentFormat(
                CurrentFormatRecoveryReexec::validated(expected, path, installation_lock)?,
            ));
        }
        Ok(if outcome.recovered() {
            InstallRecovery::Recovered {
                committed: matches!(
                    outcome,
                    transaction::RecoveryOutcome::Committed
                        | transaction::RecoveryOutcome::CleanupPending { .. }
                ),
            }
        } else {
            InstallRecovery::None
        })
    }

    #[cfg(windows)]
    {
        return match transaction::recover_interrupted_install_outcome(
            process,
            expected,
            installation_lock,
            semantic_layout,
        )? {
            transaction::RecoveryOutcome::None => Ok(InstallRecovery::None),
            transaction::RecoveryOutcome::WindowsHelperScheduled {
                attempt_id,
                helper_pid,
            } => Ok(InstallRecovery::Scheduled {
                attempt_id,
                helper_pid,
            }),
            outcome => Ok(InstallRecovery::Recovered {
                committed: matches!(
                    outcome,
                    transaction::RecoveryOutcome::Committed
                        | transaction::RecoveryOutcome::CleanupPending { .. }
                ),
            }),
        };
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = (expected, installation_lock);
        bail!("executable-scoped installation locking is unsupported on this platform");
    }
}

#[cfg(unix)]
pub(in crate::upgrade) fn reexec_current_format_recovery(
    process: &dyn ReleaseProcessPort,
    recovery: CurrentFormatRecoveryReexec,
) -> Result<()> {
    transaction::reexec_restored_executable(
        process,
        &recovery.executable,
        &recovery.attempt_id,
    )
        .with_context(|| {
            format!(
                "schema-2 recovery safely restored {}, but current-format re-exec failed; rerun `ctx upgrade` or reinstall from https://ctx.rs/install to fix forward",
                recovery.executable.display()
            )
        })
}

#[cfg(windows)]
pub(in crate::upgrade) fn run_replacement_helper<D: super::DaemonUpgradePort + ?Sized>(
    semantic_layout: &dyn SemanticLayoutPort,
    daemon: &D,
    install_path: &Path,
    attempt_id: &str,
    parent_pid: u32,
) -> Result<HelperOutcome> {
    transaction::run_windows_replacement_helper(
        semantic_layout,
        daemon,
        install_path,
        attempt_id,
        parent_pid,
    )
}

// The Windows runtime contract test reads this constant from this exact source path.
#[cfg(windows)]
const EXTRACT_SCRIPT: &str = r#"
param(
  [string]$ArchivePath,
  [string]$Destination,
  [string]$ExpectedVersion,
  [long]$MaxExpandedBytes
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
$expectedFiles = [System.Collections.Generic.HashSet[string]]::new(
  [string[]]@(
    'LICENSE',
    'MICROSOFT_VC_RUNTIME_LICENSE.rtf',
    'ThirdPartyNotices.txt',
    'VERSION_NUMBER',
    'GIT_COMMIT_ID',
    'lib/onnxruntime.dll',
    'lib/msvcp140.dll',
    'lib/msvcp140_1.dll',
    'lib/vcruntime140.dll',
    'lib/vcruntime140_1.dll'
  ),
  [System.StringComparer]::Ordinal
)
$expectedEntries = [System.Collections.Generic.HashSet[string]]::new($expectedFiles, [System.StringComparer]::Ordinal)
[void]$expectedEntries.Add('lib')
$seen = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
$entries = @{}
[long]$totalLength = 0
$archiveStream = [System.IO.FileStream]::new(
  $ArchivePath,
  [System.IO.FileMode]::Open,
  [System.IO.FileAccess]::Read,
  [System.IO.FileShare]::ReadWrite
)
$archive = $null
try {
  $archive = [System.IO.Compression.ZipArchive]::new(
    $archiveStream,
    [System.IO.Compression.ZipArchiveMode]::Read,
    $true
  )
  foreach ($entry in $archive.Entries) {
    $rawName = $entry.FullName
    if (
      [string]::IsNullOrEmpty($rawName) -or
      $rawName.Contains('\') -or
      $rawName.StartsWith('/', [System.StringComparison]::Ordinal) -or
      $rawName -match '^[A-Za-z]:'
    ) {
      throw "unsafe runtime archive entry path: '$rawName'"
    }
    $isDirectory = $rawName.EndsWith('/', [System.StringComparison]::Ordinal)
    $name = if ($isDirectory) { $rawName.Substring(0, $rawName.Length - 1) } else { $rawName }
    $expectedRawName = if ($name -ceq 'lib') { 'lib/' } else { $name }
    if (
      $rawName -cne $expectedRawName -or
      -not $expectedEntries.Contains($name) -or
      -not $seen.Add($name)
    ) {
      throw "unexpected, duplicate, or non-canonical runtime archive entry: '$rawName'"
    }
    $unixMode = ($entry.ExternalAttributes -shr 16) -band 0xFFFF
    $fileType = $unixMode -band 0xF000
    if (($unixMode -band 0x0E00) -ne 0) {
      throw "unsafe permission bits on runtime archive entry: '$rawName'"
    }
    if ($name -ceq 'lib') {
      if (-not $isDirectory -or $fileType -ne 0x4000) {
        throw 'runtime lib entry is not a directory'
      }
    } elseif ($isDirectory -or $fileType -ne 0x8000) {
      throw "runtime archive entry is not a regular file: '$rawName'"
    }
    $totalLength += $entry.Length
    if ($totalLength -gt $MaxExpandedBytes) {
      throw 'runtime archive expands beyond the 1 GiB safety limit'
    }
    $entries[$name] = $entry
  }
  if ($seen.Count -ne $expectedEntries.Count) {
    $missing = @($expectedEntries | Where-Object { -not $seen.Contains($_) })
    throw "runtime archive entries do not exactly match the expected layout; missing: $($missing -join ', ')"
  }
  $versionStream = $entries['VERSION_NUMBER'].Open()
  try {
    $reader = [System.IO.StreamReader]::new($versionStream, [System.Text.UTF8Encoding]::new($false, $true))
    try {
      $versionText = $reader.ReadToEnd()
    } finally {
      $reader.Dispose()
    }
  } finally {
    $versionStream.Dispose()
  }
  if ($versionText -cne ($ExpectedVersion + [char]10)) {
    throw "runtime VERSION_NUMBER is not exactly $ExpectedVersion"
  }
  New-Item -ItemType Directory -Path (Join-Path $Destination 'lib') -Force | Out-Null
  foreach ($name in $expectedFiles) {
    $target = Join-Path $Destination ($name.Replace('/', '\'))
    $sourceStream = $entries[$name].Open()
    try {
      $targetStream = [System.IO.File]::Open($target, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
      try {
        $sourceStream.CopyTo($targetStream)
        $targetStream.Flush($true)
      } finally {
        $targetStream.Dispose()
      }
    } finally {
      $sourceStream.Dispose()
    }
  }
} finally {
  try {
    if ($null -ne $archive) {
      $archive.Dispose()
    }
  } finally {
    $archiveStream.Dispose()
  }
}
"#;

#[cfg(windows)]
fn windows_runtime_extract_script() -> &'static str {
    EXTRACT_SCRIPT
}
