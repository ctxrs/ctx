use super::*;
use crate::NormalizedLaunch;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use quick_xml::{
    encoding::Decoder as XmlDecoder,
    escape::unescape as xml_unescape,
    events::{BytesStart as XmlStart, Event as XmlEvent},
    Reader as XmlReader, XmlVersion,
};
use serde_json::Value;

use crate::DAEMON_LOCK_FILE;

#[cfg(windows)]
use crate::{daemon_lock_path, pid_from_lock_json, read_pid_lock_json};

#[cfg(windows)]
use std::fs;

const WINDOWS_SUPERVISOR_OWNER_FILE: &str = "windows-supervisor-owner.json";

pub fn windows_supervisor_owner_provenance_path(identity: &SupervisorIdentity) -> Result<PathBuf> {
    identity
        .artifact_path()
        .parent()
        .map(|parent| parent.join(WINDOWS_SUPERVISOR_OWNER_FILE))
        .ok_or_else(|| anyhow!("Windows supervisor artifact has no parent directory"))
}

fn windows_supervisor_daemon_lock_path(identity: &SupervisorIdentity) -> Result<PathBuf> {
    identity
        .artifact_path()
        .parent()
        .map(|parent| parent.join(DAEMON_LOCK_FILE))
        .ok_or_else(|| anyhow!("Windows supervisor artifact has no parent directory"))
}

#[cfg(windows)]
pub fn probe_windows_task_scheduler(
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<SupervisorManagerOperability> {
    let mut command = windows_task_scheduler_probe_command(manager_environment);
    // A user can own arbitrarily many tasks. Never buffer that enumeration or
    // its diagnostics merely to establish manager operability.
    probe_supervisor_manager_bounded(&mut command, "current-user Task Scheduler")
}

#[cfg(any(windows, test))]
fn windows_task_scheduler_probe_command(
    manager_environment: &SupervisorManagerEnvironment,
) -> std::process::Command {
    let mut command = supervisor_command("schtasks", manager_environment);
    command.args(["/Query", "/FO", "CSV", "/NH"]);
    command
}

#[cfg(windows)]
pub fn install_windows_supervisor(
    data_root: &Path,
    spec: &SupervisorSpec,
    manager_environment: &SupervisorManagerEnvironment,
    migrate_owner: &dyn Fn(&Path) -> Result<()>,
) -> Result<PathBuf> {
    let identity = spec.identity();
    let path = identity.artifact_path();
    let system_root = manager_environment_value(manager_environment, "SystemRoot")
        .ok_or_else(|| anyhow!("Windows SystemRoot is unavailable"))?;
    let sid = crate::current_windows_user_sid()?;
    write_supervisor_environment(spec)?;
    let xml = windows_task_xml(spec, Path::new(system_root), &sid)?;
    write_atomic_supervisor_file(path, &windows_task_xml_bytes(&xml))?;

    let mut create = supervisor_command("schtasks", manager_environment);
    create
        .args(["/Create", "/TN"])
        .arg(identity.name())
        .arg("/XML")
        .arg(path)
        .arg("/F");
    command_success(&mut create, "schtasks /Create")?;
    migrate_owner(data_root)?;
    start_windows_supervisor(identity, manager_environment)?;
    Ok(path.to_path_buf())
}

#[cfg(windows)]
pub fn disable_windows_supervisor(
    identity: &SupervisorIdentity,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<Option<PathBuf>> {
    let path = identity.artifact_path();
    let task_name = identity.name();
    stop_windows_supervisor_action(identity, manager_environment)?;
    let system_root = manager_environment_value(manager_environment, "SystemRoot")
        .ok_or_else(|| anyhow!("Windows SystemRoot is unavailable"))?;
    let mut delete = supervisor_command("schtasks", manager_environment);
    delete.args(["/Delete", "/TN"]).arg(&task_name).arg("/F");
    let output = supervisor_output(&mut delete).context("run schtasks /Delete")?;
    verify_windows_task_deletion(
        output.status.success(),
        &output.stderr,
        windows_task_state(&task_name, Path::new(system_root), manager_environment),
    )?;
    remove_windows_supervisor_owner_provenance(identity)?;
    remove_windows_supervisor_file(path, "remove ctx scheduled-task definition")?;
    Ok(Some(path.to_path_buf()))
}

#[cfg(windows)]
fn remove_windows_supervisor_file(path: &Path, context: &'static str) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context(context),
    }
}

#[cfg(any(windows, test))]
fn verify_windows_task_deletion(
    delete_succeeded: bool,
    delete_stderr: &[u8],
    task_state: Result<Option<u32>>,
) -> Result<()> {
    let task_state = task_state.context("verify ctx scheduled-task deletion")?;
    if task_state.is_none() {
        return Ok(());
    }
    if !delete_succeeded {
        return Err(anyhow!(
            "schtasks /Delete failed: {}",
            String::from_utf8_lossy(delete_stderr).trim()
        ));
    }
    Err(anyhow!(
        "ctx scheduled task remained registered after deletion"
    ))
}

pub fn windows_task_xml(
    spec: &SupervisorSpec,
    system_root: &Path,
    user_sid: &str,
) -> Result<String> {
    let script = windows_sanitized_process_supervisor_script_with_provenance(
        spec.launch(),
        spec.environment_path(),
        &windows_supervisor_daemon_lock_path(spec.identity())?,
        &windows_supervisor_owner_provenance_path(spec.identity())?,
    )?;
    windows_task_xml_with_script(
        system_root,
        user_sid,
        spec.identity().name(),
        spec.description(),
        &script,
    )
}

pub fn windows_task_xml_with_script(
    system_root: &Path,
    user_sid: &str,
    task_name: &str,
    description: &str,
    script: &str,
) -> Result<String> {
    let user_sid = validated_supervisor_artifact_text("Windows user SID", user_sid)?;
    let task_name = validated_supervisor_artifact_text("Windows task name", task_name)?;
    let description = validated_supervisor_artifact_text("Windows task description", description)?;
    let script = validated_supervisor_artifact_text("Windows supervisor script", script)?;
    let powershell = system_root
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    let powershell_text =
        validated_supervisor_artifact_path("Windows PowerShell path", &powershell)?;
    let encoded = BASE64.encode(
        script
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    // Task Scheduler rejects intervals below one minute, and Count is an unsigned byte.
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n<Task version=\"1.4\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\n<RegistrationInfo><URI>{}</URI><Description>{}</Description></RegistrationInfo>\n<Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{}</UserId></LogonTrigger></Triggers>\n<Principals><Principal id=\"Author\"><UserId>{}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>\n<Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>false</StopIfGoingOnBatteries><StartWhenAvailable>true</StartWhenAvailable><ExecutionTimeLimit>PT0S</ExecutionTimeLimit><RestartOnFailure><Interval>PT1M</Interval><Count>255</Count></RestartOnFailure></Settings>\n<Actions Context=\"Author\"><Exec><Command>{}</Command><Arguments>-NoLogo -NoProfile -NonInteractive -WindowStyle Hidden -EncodedCommand {}</Arguments></Exec></Actions>\n</Task>\n",
        xml_escape(task_name),
        xml_escape(description),
        xml_escape(user_sid),
        xml_escape(user_sid),
        xml_escape(powershell_text),
        encoded,
    ))
}

pub fn windows_task_xml_bytes(xml: &str) -> Vec<u8> {
    // schtasks requires the XML declaration and the file bytes to agree.
    let mut bytes = Vec::with_capacity(2 + xml.len() * 2);
    bytes.extend_from_slice(&[0xff, 0xfe]);
    bytes.extend(xml.encode_utf16().flat_map(u16::to_le_bytes));
    bytes
}

pub fn windows_sanitized_process_supervisor_script(
    launch: &NormalizedLaunch,
    environment_path: &Path,
) -> Result<String> {
    let process = windows_sanitized_process_start_info(launch, environment_path)?;
    Ok(format!(
        "$ErrorActionPreference='Stop';{process}[int]$delay=2;while($true){{$c=$null;$code=1;$started=[DateTime]::UtcNow;try{{$c=[Diagnostics.Process]::Start($p);$c.WaitForExit();$code=$c.ExitCode}}catch{{$code=1}}finally{{if($null -ne $c){{$c.Dispose()}}}};if($code -eq 0){{exit 0}};if(([DateTime]::UtcNow-$started).TotalSeconds -ge 60){{$delay=2}};Start-Sleep -Seconds $delay;$delay=[Math]::Min($delay*2,60)}}"
    ))
}

fn windows_sanitized_process_supervisor_script_with_provenance(
    launch: &NormalizedLaunch,
    environment_path: &Path,
    daemon_lock: &Path,
    owner_provenance: &Path,
) -> Result<String> {
    let process = windows_sanitized_process_start_info(launch, environment_path)?;
    let daemon_lock = validated_supervisor_artifact_path("Windows daemon lock", daemon_lock)?;
    let owner_provenance = validated_supervisor_artifact_path(
        "Windows supervisor owner provenance",
        owner_provenance,
    )?;
    let daemon_lock = powershell_single_quote(daemon_lock);
    let owner_provenance = powershell_single_quote(owner_provenance);
    Ok(format!(
        "$ErrorActionPreference='Stop';{process}$lockPath='{daemon_lock}';$ownerPath='{owner_provenance}';$tempPath=$ownerPath+'.'+$PID+'.tmp';Remove-Item -LiteralPath $ownerPath -Force -ErrorAction SilentlyContinue;Remove-Item -LiteralPath $tempPath -Force -ErrorAction SilentlyContinue;[int]$delay=2;while($true){{$c=$null;$code=1;$started=[DateTime]::UtcNow;try{{$c=[Diagnostics.Process]::Start($p);$owner=$null;while(!$c.HasExited -and $null -eq $owner){{try{{$lock=Get-Content -LiteralPath $lockPath -Raw -ErrorAction Stop|ConvertFrom-Json -ErrorAction Stop;if(([uint32]$lock.pid -eq [uint32]$c.Id)-and(-not [string]::IsNullOrWhiteSpace([string]$lock.owner_id))){{$owner=[string]$lock.owner_id}}}}catch{{}};if($null -eq $owner){{Start-Sleep -Milliseconds 25}}}};if($null -ne $owner){{$record=[ordered]@{{schema_version=1;pid=[uint32]$c.Id;owner_id=$owner}}|ConvertTo-Json -Compress;[IO.File]::WriteAllText($tempPath,$record,(New-Object Text.UTF8Encoding($false)));Move-Item -LiteralPath $tempPath -Destination $ownerPath -Force}};$c.WaitForExit();$code=$c.ExitCode}}catch{{$code=1}}finally{{Remove-Item -LiteralPath $ownerPath -Force -ErrorAction SilentlyContinue;Remove-Item -LiteralPath $tempPath -Force -ErrorAction SilentlyContinue;if($null -ne $c){{$c.Dispose()}}}};if($code -eq 0){{exit 0}};if(([DateTime]::UtcNow-$started).TotalSeconds -ge 60){{$delay=2}};Start-Sleep -Seconds $delay;$delay=[Math]::Min($delay*2,60)}}"
    ))
}

fn windows_sanitized_process_start_info(
    launch: &NormalizedLaunch,
    environment_path: &Path,
) -> Result<String> {
    let executable =
        validated_supervisor_artifact_path("Windows child executable", launch.program())?;
    let environment_path =
        validated_supervisor_artifact_path("Windows child environment file", environment_path)?;
    validate_windows_child_environment(launch)?;
    let arguments = launch
        .args()
        .enumerate()
        .map(|(index, argument)| {
            let argument = argument
                .to_str()
                .ok_or_else(|| anyhow!("Windows child argument {index} is not Unicode"))?;
            validated_supervisor_artifact_text(&format!("Windows child argument {index}"), argument)
                .map(windows_command_line_quote)
        })
        .collect::<Result<Vec<_>>>()?
        .join(" ");
    Ok(format!(
        "$p=New-Object System.Diagnostics.ProcessStartInfo;$p.FileName='{}';$p.UseShellExecute=$false;$p.CreateNoWindow=$true;$p.EnvironmentVariables.Clear();$p.EnvironmentVariables['{}']='{}';$p.Arguments='{}';",
        powershell_single_quote(executable),
        SUPERVISOR_ENVIRONMENT_FILE_ENV,
        powershell_single_quote(environment_path),
        powershell_single_quote(&arguments),
    ))
}

fn validate_windows_child_environment(launch: &NormalizedLaunch) -> Result<()> {
    for (name, value) in launch.environment() {
        let name = name
            .to_str()
            .ok_or_else(|| anyhow!("Windows child environment name is not Unicode"))?;
        validated_supervisor_artifact_text("Windows child environment name", name)?;
        let value = value
            .to_str()
            .ok_or_else(|| anyhow!("Windows child environment value {name} is not Unicode"))?;
        validated_supervisor_artifact_text(
            &format!("Windows child environment value {name}"),
            value,
        )?;
    }
    Ok(())
}

pub fn powershell_single_quote(value: &str) -> String {
    value.replace('\'', "''")
}

pub fn windows_command_line_quote(value: &str) -> String {
    if !value.is_empty()
        && !value
            .chars()
            .any(|character| character.is_whitespace() || character == '"')
    {
        return value.to_owned();
    }
    let mut quoted = String::from("\"");
    let mut backslashes = 0;
    for character in value.chars() {
        if character == '\\' {
            backslashes += 1;
        } else if character == '"' {
            quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
            quoted.push('"');
            backslashes = 0;
        } else {
            quoted.push_str(&"\\".repeat(backslashes));
            backslashes = 0;
            quoted.push(character);
        }
    }
    quoted.push_str(&"\\".repeat(backslashes * 2));
    quoted.push('"');
    quoted
}

#[cfg(windows)]
fn query_windows_task(
    task_name: &str,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<std::process::Output> {
    let mut query = supervisor_command("schtasks", manager_environment);
    query.args(["/Query", "/TN"]).arg(task_name).arg("/XML");
    supervisor_output(&mut query).context("run schtasks /Query")
}

#[cfg(windows)]
pub fn verify_windows_supervisor_registration(
    spec: &SupervisorSpec,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<()> {
    verify_supervisor_environment(spec)?;
    let system_root = manager_environment_value(manager_environment, "SystemRoot")
        .ok_or_else(|| anyhow!("Windows SystemRoot is unavailable"))?;
    let sid = crate::current_windows_user_sid()?;
    let task_name = spec.identity().name();
    let output = query_windows_task(&task_name, manager_environment)?;
    if !output.status.success() {
        return Err(anyhow!(
            "ctx current-user scheduled task is not registered: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let xml = decode_supervisor_text(&output.stdout);
    if !windows_task_registration_matches(
        &xml,
        spec,
        Path::new(system_root),
        &sid,
        manager_environment,
    )? {
        return Err(anyhow!(
            "ctx scheduled task registration does not match the maintained definition"
        ));
    }
    Ok(())
}

#[cfg(windows)]
pub fn verify_windows_supervisor(
    data_root: &Path,
    executable: &Path,
    spec: &SupervisorSpec,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<u32> {
    verify_windows_supervisor_registration(spec, manager_environment)?;
    let system_root = manager_environment_value(manager_environment, "SystemRoot")
        .ok_or_else(|| anyhow!("Windows SystemRoot is unavailable"))?;
    let task_name = spec.identity().name();
    if !windows_task_is_running(&task_name, Path::new(system_root), manager_environment)? {
        return Err(anyhow!(
            "ctx current-user scheduled task has no live supervisor ownership"
        ));
    }
    let owner_pid = verify_windows_supervisor_owner_provenance(data_root, spec)?;
    verify_daemon_owner_identity(data_root, executable, Some(owner_pid))
}

#[cfg(windows)]
pub fn start_windows_supervisor(
    identity: &SupervisorIdentity,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<()> {
    stop_windows_supervisor_action(identity, manager_environment)?;
    remove_windows_supervisor_owner_provenance(identity)?;
    let mut run = supervisor_command("schtasks", manager_environment);
    run.args(["/Run", "/TN"]).arg(identity.name());
    command_success(&mut run, "schtasks /Run")
}

#[cfg(windows)]
fn stop_windows_supervisor_action(
    identity: &SupervisorIdentity,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<()> {
    let system_root = manager_environment_value(manager_environment, "SystemRoot")
        .ok_or_else(|| anyhow!("Windows SystemRoot is unavailable"))?;
    let mut end = supervisor_command("schtasks", manager_environment);
    end.args(["/End", "/TN"]).arg(identity.name());
    let end_output = supervisor_output(&mut end).context("run schtasks /End")?;
    let deadline = Instant::now() + SUPERVISOR_HANDOFF_TIMEOUT;
    loop {
        match windows_task_state(identity.name(), Path::new(system_root), manager_environment) {
            Ok(None) => return Ok(()),
            Ok(Some(4)) if Instant::now() < deadline => {
                std::thread::sleep(SUPERVISOR_POLL_INTERVAL);
            }
            Ok(Some(4)) => {
                let detail = String::from_utf8_lossy(&end_output.stderr);
                return Err(anyhow!(
                    "ctx scheduled-task action remained running after schtasks /End{}",
                    (!end_output.status.success())
                        .then(|| format!(": {}", detail.trim()))
                        .unwrap_or_default()
                ));
            }
            Ok(Some(_)) => return Ok(()),
            Err(error) => {
                return Err(error.context("verify ctx scheduled-task action stopped"));
            }
        }
    }
}

#[cfg(windows)]
fn remove_windows_supervisor_owner_provenance(identity: &SupervisorIdentity) -> Result<()> {
    let path = windows_supervisor_owner_provenance_path(identity)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| {
            format!(
                "remove Windows supervisor owner provenance {}",
                path.display()
            )
        }),
    }
}

#[cfg(windows)]
fn verify_windows_supervisor_owner_provenance(
    data_root: &Path,
    spec: &SupervisorSpec,
) -> Result<u32> {
    let lock = read_pid_lock_json(&daemon_lock_path(data_root))
        .ok_or_else(|| anyhow!("Windows supervisor daemon lock has no readable identity"))?;
    let path = windows_supervisor_owner_provenance_path(spec.identity())?;
    let provenance: Value = serde_json::from_slice(&fs::read(&path).with_context(|| {
        format!(
            "read Windows supervisor owner provenance {}",
            path.display()
        )
    })?)
    .with_context(|| {
        format!(
            "parse Windows supervisor owner provenance {}",
            path.display()
        )
    })?;
    if !windows_supervisor_owner_provenance_matches(&lock, &provenance) {
        return Err(anyhow!(
            "Windows scheduled task does not own the live ctx daemon lock"
        ));
    }
    pid_from_lock_json(&lock)
        .ok_or_else(|| anyhow!("Windows supervisor daemon lock has no process identity"))
}

pub fn windows_supervisor_owner_provenance_matches(lock: &Value, provenance: &Value) -> bool {
    let Some(lock_owner_id) = lock
        .get("owner_id")
        .and_then(Value::as_str)
        .filter(|owner_id| !owner_id.is_empty())
    else {
        return false;
    };
    provenance.get("schema_version").and_then(Value::as_u64) == Some(1)
        && provenance.get("pid").and_then(Value::as_u64) == lock.get("pid").and_then(Value::as_u64)
        && provenance.get("owner_id").and_then(Value::as_str) == Some(lock_owner_id)
}

#[cfg(windows)]
fn windows_task_is_running(
    task_name: &str,
    system_root: &Path,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<bool> {
    Ok(windows_task_state(task_name, system_root, manager_environment)? == Some(4))
}

#[cfg(windows)]
fn windows_task_state(
    task_name: &str,
    system_root: &Path,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<Option<u32>> {
    let powershell = system_root
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    let mut command = supervisor_command(
        powershell
            .to_str()
            .ok_or_else(|| anyhow!("Windows PowerShell path is not Unicode"))?,
        manager_environment,
    );
    command
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"])
        .arg(windows_task_state_script(task_name));
    let output = supervisor_output(&mut command).context("query scheduled-task running state")?;
    if !output.status.success() {
        return Err(anyhow!(
            "query scheduled-task running state failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    parse_windows_task_state_query(&output.stdout)
        .ok_or_else(|| anyhow!("scheduled-task running state was neither absent nor numeric"))
}

pub fn windows_task_state_script(task_name: &str) -> String {
    let task = task_name.trim_start_matches('\\');
    format!(
        "$t=Get-ScheduledTask -TaskPath '\\' -ErrorAction Stop | Where-Object {{$_.TaskName -eq '{}'}};if($null -eq $t){{[Console]::Out.Write('absent')}}else{{[Console]::Out.Write([int]$t.State)}}",
        powershell_single_quote(task),
    )
}

pub fn parse_windows_task_state(output: &[u8]) -> Option<u32> {
    decode_supervisor_text(output).trim().parse().ok()
}

#[cfg(any(windows, test))]
fn parse_windows_task_state_query(output: &[u8]) -> Option<Option<u32>> {
    let output = decode_supervisor_text(output);
    let output = output.trim();
    if output == "absent" {
        Some(None)
    } else {
        output.parse().ok().map(Some)
    }
}

pub fn windows_task_registration_matches(
    xml: &str,
    spec: &SupervisorSpec,
    system_root: &Path,
    user_sid: &str,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<bool> {
    let powershell = system_root
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    let script = windows_sanitized_process_supervisor_script_with_provenance(
        spec.launch(),
        spec.environment_path(),
        &windows_supervisor_daemon_lock_path(spec.identity())?,
        &windows_supervisor_owner_provenance_path(spec.identity())?,
    )?;
    let encoded = BASE64.encode(
        script
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    let powershell = powershell.to_string_lossy();
    let arguments =
        format!("-NoLogo -NoProfile -NonInteractive -WindowStyle Hidden -EncodedCommand {encoded}");
    let registration = parse_windows_task_registration(xml)?;
    let trigger_identity_matches = windows_task_trigger_identity_matches(
        registration.logon_trigger_user_id.as_deref(),
        user_sid,
        system_root,
        manager_environment,
    )?;
    Ok(!registration.invalid
        && registration.root_count == 1
        && registration.namespace.as_deref() == Some(WINDOWS_TASK_XML_NAMESPACE)
        && registration.uri.as_deref() == Some(spec.identity().name())
        && registration.trigger_count == 1
        && registration.logon_trigger_count == 1
        && registration
            .logon_trigger_enabled
            .as_deref()
            .unwrap_or("true")
            == "true"
        && trigger_identity_matches
        && registration.principal_user_id.as_deref() == Some(user_sid)
        && registration.logon_type.as_deref() == Some("InteractiveToken")
        && registration
            .run_level
            .as_deref()
            .unwrap_or("LeastPrivilege")
            == "LeastPrivilege"
        && registration.multiple_instances_policy.as_deref() == Some("IgnoreNew")
        && registration.disallow_start_on_batteries.as_deref() == Some("false")
        && registration.stop_on_batteries.as_deref() == Some("false")
        && registration.start_when_available.as_deref() == Some("true")
        && registration.task_enabled.as_deref().unwrap_or("true") == "true"
        && registration.execution_time_limit.as_deref() == Some("PT0S")
        && registration.restart_interval.as_deref() == Some("PT1M")
        && registration.restart_count.as_deref() == Some("255")
        && registration.principal_count == 1
        && registration.action_count == 1
        && registration.exec_action_count == 1
        && registration.command.as_deref() == Some(powershell.as_ref())
        && registration.arguments.as_deref() == Some(arguments.as_str()))
}

pub const WINDOWS_TASK_XML_NAMESPACE: &str =
    "http://schemas.microsoft.com/windows/2004/02/mit/task";

#[cfg(windows)]
fn windows_task_trigger_identity_matches(
    actual: Option<&str>,
    expected_sid: &str,
    system_root: &Path,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<bool> {
    let Some(actual) = actual else {
        return Ok(false);
    };
    if windows_task_user_identity_matches(actual, expected_sid, None) {
        return Ok(true);
    }
    let resolved_sid = resolve_windows_account_sid(actual, system_root, manager_environment)?;
    Ok(windows_task_user_identity_matches(
        actual,
        expected_sid,
        Some(&resolved_sid),
    ))
}

#[cfg(not(windows))]
fn windows_task_trigger_identity_matches(
    actual: Option<&str>,
    expected_sid: &str,
    _system_root: &Path,
    _manager_environment: &SupervisorManagerEnvironment,
) -> Result<bool> {
    Ok(actual.is_some_and(|actual| windows_task_user_identity_matches(actual, expected_sid, None)))
}

pub fn windows_task_user_identity_matches(
    actual: &str,
    expected_sid: &str,
    resolved_sid: Option<&str>,
) -> bool {
    actual.eq_ignore_ascii_case(expected_sid)
        || resolved_sid.is_some_and(|resolved_sid| resolved_sid.eq_ignore_ascii_case(expected_sid))
}

#[cfg(windows)]
fn resolve_windows_account_sid(
    account: &str,
    system_root: &Path,
    manager_environment: &SupervisorManagerEnvironment,
) -> Result<String> {
    let account = validated_supervisor_artifact_text("Windows task account", account)?;
    let powershell = system_root
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    let powershell = validated_supervisor_artifact_path("Windows PowerShell path", &powershell)?;
    let script = format!(
        "$ErrorActionPreference='Stop';$a=[System.Security.Principal.NTAccount]'{}';[Console]::Out.Write($a.Translate([System.Security.Principal.SecurityIdentifier]).Value)",
        powershell_single_quote(account),
    );
    let encoded = BASE64.encode(
        script
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    let mut command = supervisor_command(powershell, manager_environment);
    command.args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-EncodedCommand",
        &encoded,
    ]);
    let output = supervisor_output(&mut command).context("resolve Windows task account SID")?;
    if !output.status.success() {
        return Err(anyhow!(
            "resolve Windows task account SID failed: {}",
            decode_supervisor_text(&output.stderr).trim()
        ));
    }
    let sid = decode_supervisor_text(&output.stdout).trim().to_owned();
    if !sid.starts_with("S-1-") {
        return Err(anyhow!(
            "resolved Windows task account has no canonical SID"
        ));
    }
    Ok(sid)
}

#[derive(Default)]
struct WindowsTaskRegistration {
    root_count: usize,
    namespace: Option<String>,
    invalid: bool,
    uri: Option<String>,
    trigger_count: usize,
    logon_trigger_count: usize,
    logon_trigger_enabled: Option<String>,
    logon_trigger_user_id: Option<String>,
    principal_user_id: Option<String>,
    logon_type: Option<String>,
    run_level: Option<String>,
    multiple_instances_policy: Option<String>,
    disallow_start_on_batteries: Option<String>,
    stop_on_batteries: Option<String>,
    start_when_available: Option<String>,
    task_enabled: Option<String>,
    execution_time_limit: Option<String>,
    restart_interval: Option<String>,
    restart_count: Option<String>,
    principal_count: usize,
    action_count: usize,
    exec_action_count: usize,
    command: Option<String>,
    arguments: Option<String>,
}

fn parse_windows_task_registration(xml: &str) -> Result<WindowsTaskRegistration> {
    let mut reader = XmlReader::from_str(xml);
    reader.config_mut().trim_text(true);
    let decoder = reader.decoder();
    let mut path = Vec::<Vec<u8>>::new();
    let mut registration = WindowsTaskRegistration::default();

    loop {
        match reader
            .read_event()
            .context("parse Windows scheduled-task XML")?
        {
            XmlEvent::Start(element) => {
                let is_root = path.is_empty();
                path.push(element.local_name().as_ref().to_vec());
                let namespace = windows_task_element_namespace(&element, decoder)?;
                if element.name().as_ref().contains(&b':')
                    || namespace
                        .as_deref()
                        .is_some_and(|namespace| namespace != WINDOWS_TASK_XML_NAMESPACE)
                {
                    registration.invalid = true;
                }
                if is_root {
                    registration.root_count += 1;
                    if element.name().as_ref() != b"Task" {
                        registration.invalid = true;
                    }
                    if registration
                        .namespace
                        .replace(namespace.unwrap_or_default())
                        .is_some()
                    {
                        registration.invalid = true;
                    }
                }
                observe_windows_task_element(&mut registration, &path);
            }
            XmlEvent::Text(text) => {
                let decoded = text
                    .decode()
                    .context("decode Windows scheduled-task XML text")?;
                let value = xml_unescape(&decoded)
                    .context("unescape Windows scheduled-task XML text")?
                    .trim()
                    .to_owned();
                if path.is_empty() && !value.is_empty() {
                    registration.invalid = true;
                }
                let duplicate = windows_task_value_slot(&mut registration, &path)
                    .is_some_and(|destination| destination.replace(value).is_some());
                registration.invalid |= duplicate;
            }
            XmlEvent::Empty(element) => {
                let is_root = path.is_empty();
                path.push(element.local_name().as_ref().to_vec());
                let namespace = windows_task_element_namespace(&element, decoder)?;
                if element.name().as_ref().contains(&b':')
                    || namespace
                        .as_deref()
                        .is_some_and(|namespace| namespace != WINDOWS_TASK_XML_NAMESPACE)
                {
                    registration.invalid = true;
                }
                if is_root {
                    registration.root_count += 1;
                    registration.invalid = true;
                }
                observe_windows_task_element(&mut registration, &path);
                if windows_task_value_slot(&mut registration, &path).is_some() {
                    registration.invalid = true;
                }
                path.pop();
            }
            XmlEvent::End(_) => {
                let empty_critical_value = windows_task_value_slot(&mut registration, &path)
                    .is_some_and(|value| value.is_none());
                registration.invalid |= empty_critical_value;
                if path.pop().is_none() {
                    registration.invalid = true;
                }
            }
            XmlEvent::Eof => {
                registration.invalid |= !path.is_empty();
                break;
            }
            _ => {}
        }
    }
    Ok(registration)
}

fn windows_task_element_namespace(
    element: &XmlStart<'_>,
    decoder: XmlDecoder,
) -> Result<Option<String>> {
    element
        .try_get_attribute("xmlns")
        .context("read Windows scheduled-task XML namespace")?
        .map(|attribute| {
            attribute
                .decoded_and_normalized_value(XmlVersion::Explicit1_0, decoder)
                .context("decode Windows scheduled-task XML namespace")
                .map(|value| value.into_owned())
        })
        .transpose()
}

fn observe_windows_task_element(registration: &mut WindowsTaskRegistration, path: &[Vec<u8>]) {
    if path.len() == 3 && windows_xml_path_is(&path[..2], &["Task", "Triggers"]) {
        registration.trigger_count += 1;
    }
    if windows_xml_path_is(path, &["Task", "Triggers", "LogonTrigger"]) {
        registration.logon_trigger_count += 1;
    }
    if windows_xml_path_is(path, &["Task", "Principals", "Principal"]) {
        registration.principal_count += 1;
    }
    if path.len() == 3 && windows_xml_path_is(&path[..2], &["Task", "Actions"]) {
        registration.action_count += 1;
    }
    if windows_xml_path_is(path, &["Task", "Actions", "Exec"]) {
        registration.exec_action_count += 1;
    }
}

fn windows_task_value_slot<'a>(
    registration: &'a mut WindowsTaskRegistration,
    path: &[Vec<u8>],
) -> Option<&'a mut Option<String>> {
    if windows_xml_path_is(path, &["Task", "RegistrationInfo", "URI"]) {
        Some(&mut registration.uri)
    } else if windows_xml_path_is(path, &["Task", "Triggers", "LogonTrigger", "Enabled"]) {
        Some(&mut registration.logon_trigger_enabled)
    } else if windows_xml_path_is(path, &["Task", "Triggers", "LogonTrigger", "UserId"]) {
        Some(&mut registration.logon_trigger_user_id)
    } else if windows_xml_path_is(path, &["Task", "Principals", "Principal", "UserId"]) {
        Some(&mut registration.principal_user_id)
    } else if windows_xml_path_is(path, &["Task", "Principals", "Principal", "LogonType"]) {
        Some(&mut registration.logon_type)
    } else if windows_xml_path_is(path, &["Task", "Principals", "Principal", "RunLevel"]) {
        Some(&mut registration.run_level)
    } else if windows_xml_path_is(path, &["Task", "Settings", "MultipleInstancesPolicy"]) {
        Some(&mut registration.multiple_instances_policy)
    } else if windows_xml_path_is(path, &["Task", "Settings", "DisallowStartIfOnBatteries"]) {
        Some(&mut registration.disallow_start_on_batteries)
    } else if windows_xml_path_is(path, &["Task", "Settings", "StopIfGoingOnBatteries"]) {
        Some(&mut registration.stop_on_batteries)
    } else if windows_xml_path_is(path, &["Task", "Settings", "StartWhenAvailable"]) {
        Some(&mut registration.start_when_available)
    } else if windows_xml_path_is(path, &["Task", "Settings", "Enabled"]) {
        Some(&mut registration.task_enabled)
    } else if windows_xml_path_is(path, &["Task", "Settings", "ExecutionTimeLimit"]) {
        Some(&mut registration.execution_time_limit)
    } else if windows_xml_path_is(path, &["Task", "Settings", "RestartOnFailure", "Interval"]) {
        Some(&mut registration.restart_interval)
    } else if windows_xml_path_is(path, &["Task", "Settings", "RestartOnFailure", "Count"]) {
        Some(&mut registration.restart_count)
    } else if windows_xml_path_is(path, &["Task", "Actions", "Exec", "Command"]) {
        Some(&mut registration.command)
    } else if windows_xml_path_is(path, &["Task", "Actions", "Exec", "Arguments"]) {
        Some(&mut registration.arguments)
    } else {
        None
    }
}

fn windows_xml_path_is(path: &[Vec<u8>], expected: &[&str]) -> bool {
    path.len() == expected.len()
        && path
            .iter()
            .zip(expected)
            .all(|(actual, expected)| actual.as_slice() == expected.as_bytes())
}

pub fn decode_supervisor_text(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xff, 0xfe]) || bytes.iter().skip(1).step_by(2).any(|byte| *byte == 0) {
        let units = bytes
            .strip_prefix(&[0xff, 0xfe])
            .unwrap_or(bytes)
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

#[cfg(test)]
mod manager_probe_tests;
