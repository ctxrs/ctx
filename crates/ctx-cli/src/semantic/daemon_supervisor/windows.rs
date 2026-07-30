use super::*;
#[cfg(any(test, windows))]
use quick_xml::{
    encoding::Decoder as XmlDecoder,
    escape::unescape as xml_unescape,
    events::{BytesStart as XmlStart, Event as XmlEvent},
    Reader as XmlReader, XmlVersion,
};

#[cfg(windows)]
pub(super) fn install_native_supervisor(
    data_root: &Path,
    executable: &Path,
    environment: &SupervisorEnvironmentSnapshot,
) -> Result<PathBuf> {
    let path = daemon_root_path(data_root).join("windows-task.xml");
    let system_root =
        env::var_os("SystemRoot").ok_or_else(|| anyhow!("Windows SystemRoot is unavailable"))?;
    let sid = current_windows_user_sid()?;
    let task_name = windows_task_name(&sid);
    let xml = windows_task_xml_with_environment(
        executable,
        data_root,
        Path::new(&system_root),
        &sid,
        &task_name,
        environment,
    )?;
    write_atomic_file(&path, &windows_task_xml_bytes(&xml))?;

    let mut create = supervisor_command("schtasks");
    create
        .args(["/Create", "/TN"])
        .arg(&task_name)
        .arg("/XML")
        .arg(&path)
        .arg("/F");
    command_success(&mut create, "schtasks /Create")?;
    migrate_existing_daemon_to_supervisor(data_root)?;
    start_native_supervisor(data_root)?;
    Ok(path)
}

#[cfg(windows)]
pub(super) fn disable_native_supervisor(data_root: &Path) -> Result<Option<PathBuf>> {
    let path = daemon_root_path(data_root).join("windows-task.xml");
    let task_name = windows_task_name(&current_windows_user_sid()?);
    let mut end = supervisor_command("schtasks");
    end.args(["/End", "/TN"]).arg(&task_name);
    let _ = supervisor_output(&mut end);
    let mut delete = supervisor_command("schtasks");
    delete.args(["/Delete", "/TN"]).arg(&task_name).arg("/F");
    let output = supervisor_output(&mut delete).context("run schtasks /Delete")?;
    let query = query_windows_task(&task_name)?;
    if !output.status.success() && query.status.success() {
        return Err(anyhow!(
            "schtasks /Delete failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    if query.status.success() {
        return Err(anyhow!(
            "ctx scheduled task remained registered after deletion"
        ));
    }
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("remove ctx scheduled-task definition"),
    }
    Ok(Some(path))
}

#[cfg(any(test, windows))]
const WINDOWS_TASK_PREFIX: &str = r"\ctx-daemon-";

#[cfg(any(test, windows))]
pub(super) fn windows_task_name(user_sid: &str) -> String {
    format!("{WINDOWS_TASK_PREFIX}{user_sid}")
}

#[cfg(test)]
pub(super) fn windows_task_xml(
    executable: &Path,
    data_root: &Path,
    system_root: &Path,
    user_sid: &str,
    task_name: &str,
) -> Result<String> {
    let environment = supervisor_environment_snapshot()?;
    windows_task_xml_with_environment(
        executable,
        data_root,
        system_root,
        user_sid,
        task_name,
        &environment,
    )
}

#[cfg(any(test, windows))]
fn windows_task_xml_with_environment(
    executable: &Path,
    data_root: &Path,
    system_root: &Path,
    user_sid: &str,
    task_name: &str,
    environment: &SupervisorEnvironmentSnapshot,
) -> Result<String> {
    let script =
        windows_sanitized_daemon_script_with_environment(executable, data_root, environment)?;
    windows_task_xml_with_script(system_root, user_sid, task_name, &script)
}

#[cfg(any(test, windows))]
pub(super) fn windows_task_xml_with_script(
    system_root: &Path,
    user_sid: &str,
    task_name: &str,
    script: &str,
) -> Result<String> {
    let user_sid = validated_supervisor_artifact_text("Windows user SID", user_sid)?;
    let task_name = validated_supervisor_artifact_text("Windows task name", task_name)?;
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
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n<Task version=\"1.4\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\n<RegistrationInfo><URI>{}</URI><Description>ctx persistent history daemon</Description></RegistrationInfo>\n<Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{}</UserId></LogonTrigger></Triggers>\n<Principals><Principal id=\"Author\"><UserId>{}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>\n<Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>false</StopIfGoingOnBatteries><StartWhenAvailable>true</StartWhenAvailable><ExecutionTimeLimit>PT0S</ExecutionTimeLimit><RestartOnFailure><Interval>PT1M</Interval><Count>255</Count></RestartOnFailure></Settings>\n<Actions Context=\"Author\"><Exec><Command>{}</Command><Arguments>-NoLogo -NoProfile -NonInteractive -WindowStyle Hidden -EncodedCommand {}</Arguments></Exec></Actions>\n</Task>\n",
        xml_escape(task_name),
        xml_escape(user_sid),
        xml_escape(user_sid),
        xml_escape(powershell_text),
        encoded,
    ))
}

#[cfg(any(test, windows))]
pub(super) fn windows_task_xml_bytes(xml: &str) -> Vec<u8> {
    // schtasks requires the XML declaration and the file bytes to agree.
    let mut bytes = Vec::with_capacity(2 + xml.len() * 2);
    bytes.extend_from_slice(&[0xff, 0xfe]);
    bytes.extend(xml.encode_utf16().flat_map(u16::to_le_bytes));
    bytes
}

#[cfg(any(test, windows))]
pub(super) fn windows_sanitized_daemon_script(
    executable: &Path,
    data_root: &Path,
) -> Result<String> {
    let environment = supervisor_environment_snapshot()?;
    windows_sanitized_daemon_script_with_environment(executable, data_root, &environment)
}

#[cfg(any(test, windows))]
fn windows_sanitized_daemon_script_with_environment(
    executable: &Path,
    data_root: &Path,
    snapshot: &SupervisorEnvironmentSnapshot,
) -> Result<String> {
    let data_root = validated_supervisor_artifact_path("ctx data root", data_root)?;
    let arguments = vec![
        "--data-root".to_owned(),
        data_root.to_owned(),
        "daemon".to_owned(),
        "run".to_owned(),
        "--format=json".to_owned(),
    ];
    windows_sanitized_process_supervisor_script(executable, &arguments, snapshot)
}

#[cfg(any(test, windows))]
pub(super) fn windows_sanitized_process_supervisor_script(
    executable: &Path,
    arguments: &[String],
    snapshot: &SupervisorEnvironmentSnapshot,
) -> Result<String> {
    let executable = validated_supervisor_artifact_path("Windows child executable", executable)?;
    let environment = snapshot
        .values
        .iter()
        .map(|(name, value)| {
            format!(
                "$p.EnvironmentVariables['{}']='{}';",
                powershell_single_quote(name),
                powershell_single_quote(value)
            )
        })
        .collect::<Vec<_>>()
        .join("");
    let arguments = arguments
        .iter()
        .enumerate()
        .map(|(index, argument)| {
            validated_supervisor_artifact_text(&format!("Windows child argument {index}"), argument)
                .map(windows_command_line_quote)
        })
        .collect::<Result<Vec<_>>>()?
        .join(" ");
    Ok(format!(
        "$ErrorActionPreference='Stop';$p=New-Object System.Diagnostics.ProcessStartInfo;$p.FileName='{}';$p.UseShellExecute=$false;$p.CreateNoWindow=$true;$p.EnvironmentVariables.Clear();{environment}$p.Arguments='{}';[int]$delay=2;while($true){{$c=$null;$code=1;$started=[DateTime]::UtcNow;try{{$c=[Diagnostics.Process]::Start($p);$c.WaitForExit();$code=$c.ExitCode}}catch{{$code=1}}finally{{if($null -ne $c){{$c.Dispose()}}}};if($code -eq 0){{exit 0}};if(([DateTime]::UtcNow-$started).TotalSeconds -ge 60){{$delay=2}};Start-Sleep -Seconds $delay;$delay=[Math]::Min($delay*2,60)}}",
        powershell_single_quote(executable),
        powershell_single_quote(&arguments),
    ))
}

#[cfg(any(test, windows))]
pub(super) fn powershell_single_quote(value: &str) -> String {
    value.replace('\'', "''")
}

#[cfg(any(test, windows))]
pub(super) fn windows_command_line_quote(value: &str) -> String {
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
pub(super) fn current_windows_user_sid() -> Result<String> {
    let mut command = supervisor_command("whoami");
    command.args(["/user", "/fo", "csv", "/nh"]);
    let output = supervisor_output(&mut command).context("query current Windows user SID")?;
    if !output.status.success() {
        return Err(anyhow!(
            "whoami /user failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8_lossy(&output.stdout)
        .split(',')
        .nth(1)
        .map(|value| value.trim().trim_matches('"').to_owned())
        .filter(|value| value.starts_with("S-1-"))
        .ok_or_else(|| anyhow!("whoami returned no current-user SID"))
}

#[cfg(windows)]
fn query_windows_task(task_name: &str) -> Result<std::process::Output> {
    let mut query = supervisor_command("schtasks");
    query.args(["/Query", "/TN"]).arg(task_name).arg("/XML");
    supervisor_output(&mut query).context("run schtasks /Query")
}

#[cfg(windows)]
pub(super) fn verify_native_supervisor_registration(
    data_root: &Path,
    executable: &Path,
) -> Result<()> {
    let system_root =
        env::var_os("SystemRoot").ok_or_else(|| anyhow!("Windows SystemRoot is unavailable"))?;
    let sid = current_windows_user_sid()?;
    let task_name = windows_task_name(&sid);
    let output = query_windows_task(&task_name)?;
    if !output.status.success() {
        return Err(anyhow!(
            "ctx current-user scheduled task is not registered: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let xml = decode_supervisor_text(&output.stdout);
    if !windows_task_registration_matches(
        &xml,
        executable,
        data_root,
        Path::new(&system_root),
        &sid,
        &task_name,
    )? {
        return Err(anyhow!(
            "ctx scheduled task registration does not match the maintained definition"
        ));
    }
    Ok(())
}

#[cfg(windows)]
pub(super) fn verify_native_supervisor(data_root: &Path, executable: &Path) -> Result<u32> {
    verify_native_supervisor_registration(data_root, executable)?;
    let system_root =
        env::var_os("SystemRoot").ok_or_else(|| anyhow!("Windows SystemRoot is unavailable"))?;
    let sid = current_windows_user_sid()?;
    let task_name = windows_task_name(&sid);
    if !windows_task_is_running(&task_name, Path::new(&system_root))? {
        return Err(anyhow!(
            "ctx current-user scheduled task has no live supervisor ownership"
        ));
    }
    verify_daemon_owner_identity(data_root, executable, None)
}

#[cfg(windows)]
pub(super) fn start_native_supervisor(_data_root: &Path) -> Result<()> {
    let task_name = windows_task_name(&current_windows_user_sid()?);
    let mut run = supervisor_command("schtasks");
    run.args(["/Run", "/TN"]).arg(&task_name);
    command_success(&mut run, "schtasks /Run")
}

#[cfg(windows)]
fn windows_task_is_running(task_name: &str, system_root: &Path) -> Result<bool> {
    let powershell = system_root
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    let mut command = supervisor_command(
        powershell
            .to_str()
            .ok_or_else(|| anyhow!("Windows PowerShell path is not Unicode"))?,
    );
    command
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"])
        .arg(windows_task_state_script(task_name));
    let output = supervisor_output(&mut command).context("query scheduled-task running state")?;
    Ok(output.status.success() && parse_windows_task_state(&output.stdout) == Some(4))
}

#[cfg(any(test, windows))]
pub(super) fn windows_task_state_script(task_name: &str) -> String {
    let task = task_name.trim_start_matches('\\');
    format!(
        "$t=Get-ScheduledTask -TaskPath '\\' -TaskName '{}' -ErrorAction Stop;[Console]::Out.Write([int]$t.State)",
        powershell_single_quote(task),
    )
}

#[cfg(any(test, windows))]
pub(super) fn parse_windows_task_state(output: &[u8]) -> Option<u32> {
    decode_supervisor_text(output).trim().parse().ok()
}

#[cfg(any(test, windows))]
pub(super) fn windows_task_registration_matches(
    xml: &str,
    executable: &Path,
    data_root: &Path,
    system_root: &Path,
    user_sid: &str,
    task_name: &str,
) -> Result<bool> {
    let powershell = system_root
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    let script = windows_sanitized_daemon_script(executable, data_root)?;
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
    )?;
    Ok(!registration.invalid
        && registration.root_count == 1
        && registration.namespace.as_deref() == Some(WINDOWS_TASK_XML_NAMESPACE)
        && registration.uri.as_deref() == Some(task_name)
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

#[cfg(any(test, windows))]
pub(super) const WINDOWS_TASK_XML_NAMESPACE: &str =
    "http://schemas.microsoft.com/windows/2004/02/mit/task";

#[cfg(windows)]
fn windows_task_trigger_identity_matches(
    actual: Option<&str>,
    expected_sid: &str,
    system_root: &Path,
) -> Result<bool> {
    let Some(actual) = actual else {
        return Ok(false);
    };
    if windows_task_user_identity_matches(actual, expected_sid, None) {
        return Ok(true);
    }
    let resolved_sid = resolve_windows_account_sid(actual, system_root)?;
    Ok(windows_task_user_identity_matches(
        actual,
        expected_sid,
        Some(&resolved_sid),
    ))
}

#[cfg(all(test, not(windows)))]
fn windows_task_trigger_identity_matches(
    actual: Option<&str>,
    expected_sid: &str,
    _system_root: &Path,
) -> Result<bool> {
    Ok(actual.is_some_and(|actual| windows_task_user_identity_matches(actual, expected_sid, None)))
}

#[cfg(any(test, windows))]
pub(super) fn windows_task_user_identity_matches(
    actual: &str,
    expected_sid: &str,
    resolved_sid: Option<&str>,
) -> bool {
    actual.eq_ignore_ascii_case(expected_sid)
        || resolved_sid.is_some_and(|resolved_sid| resolved_sid.eq_ignore_ascii_case(expected_sid))
}

#[cfg(windows)]
fn resolve_windows_account_sid(account: &str, system_root: &Path) -> Result<String> {
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
    let mut command = supervisor_command(powershell);
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

#[cfg(any(test, windows))]
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

#[cfg(any(test, windows))]
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

#[cfg(any(test, windows))]
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

#[cfg(any(test, windows))]
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

#[cfg(any(test, windows))]
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

#[cfg(any(test, windows))]
fn windows_xml_path_is(path: &[Vec<u8>], expected: &[&str]) -> bool {
    path.len() == expected.len()
        && path
            .iter()
            .zip(expected)
            .all(|(actual, expected)| actual.as_slice() == expected.as_bytes())
}

#[cfg(any(test, windows))]
pub(super) fn decode_supervisor_text(bytes: &[u8]) -> String {
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
