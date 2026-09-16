use std::{collections::BTreeMap, ffi::OsString, process::Command, thread};

use super::*;
use serde_json::json;

const CHILD_TEST: &str = "supervisor::windows::provenance_tests::task_wrapper_owner_fixture";
const PREVIOUS_OWNER: &str = "previous-owner";

fn wait_for_file(path: &Path) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !path.exists() {
        if Instant::now() >= deadline {
            return Err(anyhow!("timed out waiting for {}", path.display()));
        }
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

#[test]
fn task_wrapper_owner_fixture() -> Result<()> {
    let Some(environment_path) = std::env::var_os(SUPERVISOR_ENVIRONMENT_FILE_ENV) else {
        return Ok(());
    };
    let environment_path = PathBuf::from(environment_path);
    let root = environment_path
        .parent()
        .context("fixture environment parent")?;
    let read_failure_retried_before_spawn = root.join("read-recovered-before-spawn").exists();
    let lock_path = root.join("daemon.lock");
    if root.join("simulate-pid-reuse").exists() {
        fs::write(
            &lock_path,
            serde_json::to_vec(&json!({
                "lock_protocol": crate::PID_LOCK_PROTOCOL,
                "pid": std::process::id(),
                "owner_id": PREVIOUS_OWNER,
            }))?,
        )?;
        // The wrapper must read the stale record with this child's actual PID
        // before the child publishes its new durable identity.
        wait_for_file(&root.join("stale-owner-read"))?;
    }
    let lock = crate::pid_lock_payload(json!({}));
    let _guard = crate::PidFileLock::acquire(&lock_path, lock.clone())?
        .context("acquire fresh fixture owner")?;
    let owner_path = root.join("owner.json");
    wait_for_file(&owner_path)?;
    let provenance: Value = serde_json::from_slice(&fs::read(owner_path)?)?;
    fs::write(
        root.join("result.json"),
        serde_json::to_vec(&json!({
            "lock": lock,
            "provenance": provenance,
            "read_failure_retried_before_spawn": read_failure_retried_before_spawn,
        }))?,
    )?;
    Ok(())
}

#[test]
#[cfg_attr(not(windows), ignore = "requires Windows PowerShell")]
fn task_wrapper_waits_for_fresh_owner_after_pid_reuse() -> Result<()> {
    let system_root = std::env::var_os("SystemRoot").context("Windows SystemRoot")?;
    let system32 = Path::new(&system_root).join("System32");
    let powershell = system32.join("WindowsPowerShell/v1.0/powershell.exe");
    for scenario in ["absent", "malformed", "pid-reuse", "read-failure"] {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        let lock_path = root.join("daemon.lock");
        let owner_path = root.join("owner.json");
        if scenario == "pid-reuse" {
            fs::write(root.join("simulate-pid-reuse"), b"")?;
        }
        if scenario == "malformed" {
            fs::write(&lock_path, b"not JSON")?;
            fs::OpenOptions::new()
                .write(true)
                .open(&lock_path)?
                .set_modified(std::time::SystemTime::now() - Duration::from_secs(60))?;
        }
        if matches!(scenario, "pid-reuse" | "read-failure") {
            fs::write(
                &lock_path,
                serde_json::to_vec(&json!({
                    "lock_protocol": crate::PID_LOCK_PROTOCOL,
                    "pid": 0,
                    "owner_id": PREVIOUS_OWNER,
                }))?,
            )?;
        }
        let launch = NormalizedLaunch::new(
            std::env::current_exe()?,
            ["--exact", CHILD_TEST, "--nocapture"]
                .map(OsString::from)
                .to_vec(),
            BTreeMap::new(),
        );
        let script = windows_sanitized_process_supervisor_script_with_provenance(
            &launch,
            &root.join("fixture-environment.json"),
            &lock_path,
            &owner_path,
        )?;
        // Observe the actual Get-Content result, returning it unchanged. The
        // handshake forces the PID-reuse window without relying on a sleep.
        let fixture_root = powershell_single_quote(root.to_str().context("fixture path")?);
        let script = format!(
            r#"function Get-Content {{
    [CmdletBinding()] param([string]$LiteralPath, [switch]$Raw)
    $failurePath = Join-Path '{fixture_root}' 'read-failed'
    if (('{scenario}' -eq 'read-failure') -and !(Test-Path -LiteralPath $failurePath)) {{
        [IO.File]::WriteAllText($failurePath, 'failed')
        throw (New-Object IO.IOException 'injected transient read failure')
    }}
    $text = Microsoft.PowerShell.Management\Get-Content @PSBoundParameters
    if (($null -eq $c) -and (Test-Path -LiteralPath $failurePath)) {{
        [IO.File]::WriteAllText((Join-Path '{fixture_root}' 'read-recovered-before-spawn'), 'read')
    }}
    $record = $null
    try {{ $record = $text | ConvertFrom-Json -ErrorAction Stop }} catch {{}}
    if (([uint32]$record.pid -gt 0) -and ($record.owner_id -ceq '{PREVIOUS_OWNER}')) {{
        [IO.File]::WriteAllText((Join-Path '{fixture_root}' 'stale-owner-read'), 'read')
    }}
    return $text
}}
{script}"#
        );
        let encoded = BASE64.encode(
            script
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        );
        let mut wrapper = Command::new(&powershell)
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-EncodedCommand",
            ])
            .arg(encoded)
            .spawn()?;
        let deadline = Instant::now() + Duration::from_secs(30);
        let status: Result<_> = loop {
            match wrapper.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Err(error) => break Err(error.into()),
                Ok(None) if Instant::now() >= deadline => {
                    break Err(anyhow!("task wrapper did not finish; scenario={scenario}"));
                }
                Ok(None) => thread::sleep(Duration::from_millis(25)),
            }
        };
        if status.is_err() {
            let _ = Command::new(system32.join("taskkill.exe"))
                .args(["/PID", &wrapper.id().to_string(), "/T", "/F"])
                .output();
            let _ = wrapper.kill();
            let _ = wrapper.wait();
        }
        assert!(
            status?.success(),
            "task wrapper failed; scenario={scenario}"
        );
        let result: Value = serde_json::from_slice(&fs::read(root.join("result.json"))?)?;
        assert_eq!(
            root.join("stale-owner-read").exists(),
            scenario == "pid-reuse"
        );
        assert_eq!(
            result["read_failure_retried_before_spawn"],
            scenario == "read-failure",
        );
        let fresh_owner = result["lock"]["owner_id"]
            .as_str()
            .context("fresh owner ID")?;
        assert!(!fresh_owner.is_empty());
        assert_ne!(fresh_owner, PREVIOUS_OWNER);
        assert_eq!(result["provenance"]["owner_id"], fresh_owner);
        assert!(
            windows_supervisor_owner_provenance_matches(&result["lock"], &result["provenance"]),
            "wrapper retained a stale owner after PID reuse: {result}",
        );
    }
    Ok(())
}
