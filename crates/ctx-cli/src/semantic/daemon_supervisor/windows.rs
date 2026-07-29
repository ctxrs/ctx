use super::*;

#[cfg(windows)]
pub(super) fn install_native_supervisor(data_root: &Path, executable: &Path) -> Result<PathBuf> {
    let path = daemon_root_path(data_root).join("windows-task.xml");
    let system_root =
        env::var_os("SystemRoot").ok_or_else(|| anyhow!("Windows SystemRoot is unavailable"))?;
    let sid = current_windows_user_sid()?;
    let task_name = windows_task_name(&sid);
    let xml = windows_task_xml(
        executable,
        data_root,
        Path::new(&system_root),
        &sid,
        &task_name,
    );
    write_atomic_file(&path, xml.as_bytes())?;

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

#[cfg(any(test, windows))]
pub(super) fn windows_task_xml(
    executable: &Path,
    data_root: &Path,
    system_root: &Path,
    user_sid: &str,
    task_name: &str,
) -> String {
    let powershell = system_root
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    let script = windows_sanitized_daemon_script(executable, data_root);
    let encoded = BASE64.encode(
        script
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Task version=\"1.4\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\n<RegistrationInfo><URI>{}</URI><Description>ctx persistent history daemon</Description></RegistrationInfo>\n<Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{}</UserId></LogonTrigger></Triggers>\n<Principals><Principal id=\"Author\"><UserId>{}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>\n<Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>false</StopIfGoingOnBatteries><StartWhenAvailable>true</StartWhenAvailable><ExecutionTimeLimit>PT0S</ExecutionTimeLimit><RestartOnFailure><Interval>PT2S</Interval><Count>999</Count></RestartOnFailure></Settings>\n<Actions Context=\"Author\"><Exec><Command>{}</Command><Arguments>-NoLogo -NoProfile -NonInteractive -WindowStyle Hidden -EncodedCommand {}</Arguments></Exec></Actions>\n</Task>\n",
        xml_escape(task_name),
        xml_escape(user_sid),
        xml_escape(user_sid),
        xml_escape(&powershell.to_string_lossy()),
        encoded,
    )
}

#[cfg(any(test, windows))]
pub(super) fn windows_sanitized_daemon_script(executable: &Path, data_root: &Path) -> String {
    let allowlist = SUPERVISOR_ENV_ALLOWLIST
        .iter()
        .map(|name| format!("'{}'", powershell_single_quote(name)))
        .collect::<Vec<_>>()
        .join(",");
    let arguments = [
        "--data-root".to_owned(),
        data_root.to_string_lossy().into_owned(),
        "daemon".to_owned(),
        "run".to_owned(),
        "--format=json".to_owned(),
    ]
    .iter()
    .map(|argument| windows_command_line_quote(argument))
    .collect::<Vec<_>>()
    .join(" ");
    format!(
        "$ErrorActionPreference='Stop';$p=New-Object System.Diagnostics.ProcessStartInfo;$p.FileName='{}';$p.UseShellExecute=$false;$p.CreateNoWindow=$true;$p.EnvironmentVariables.Clear();foreach($n in @({allowlist})){{$v=[Environment]::GetEnvironmentVariable($n);if($null -ne $v){{$p.EnvironmentVariables[$n]=$v}}}};$p.Arguments='{}';$c=[Diagnostics.Process]::Start($p);$c.WaitForExit();exit $c.ExitCode",
        powershell_single_quote(&executable.to_string_lossy()),
        powershell_single_quote(&arguments),
    )
}

#[cfg(any(test, windows))]
fn powershell_single_quote(value: &str) -> String {
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
fn current_windows_user_sid() -> Result<String> {
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
    ) {
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
) -> bool {
    let powershell = system_root
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    let script = windows_sanitized_daemon_script(executable, data_root);
    let encoded = BASE64.encode(
        script
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    xml.contains(&format!("<URI>{}</URI>", xml_escape(task_name)))
        && xml.contains(&format!("<UserId>{}</UserId>", xml_escape(user_sid)))
        && xml.contains(&format!(
            "<Command>{}</Command>",
            xml_escape(&powershell.to_string_lossy())
        ))
        && xml.contains(&format!("-EncodedCommand {encoded}"))
        && xml.contains("-EncodedCommand")
        && xml.contains("<LogonType>InteractiveToken</LogonType>")
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
