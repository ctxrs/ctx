use std::path::Path;

#[cfg(windows)]
use std::{collections::BTreeMap, ffi::OsString};

use anyhow::Result;
#[cfg(windows)]
use ctx_daemon_runtime::NormalizedLaunch;

#[cfg(windows)]
use super::environment::SUPERVISOR_DESCRIPTION;
use super::environment::{
    supervisor_artifact_spec, windows_supervisor_identity, SupervisorEnvironmentSnapshot,
};
use super::ManagedSupervisorInput;

#[cfg(windows)]
pub(super) use ctx_daemon_runtime::current_windows_user_sid;
pub(super) use ctx_daemon_runtime::{
    decode_supervisor_text, parse_windows_task_state, windows_command_line_quote,
    windows_task_state_script, windows_task_user_identity_matches, windows_task_xml_bytes,
    WINDOWS_TASK_XML_NAMESPACE,
};

pub(super) fn windows_task_name(user_sid: &str) -> String {
    format!(r"\ctx-daemon-{user_sid}")
}

pub(super) fn windows_sanitized_daemon_script_with_environment(
    executable: &Path,
    data_root: &Path,
    snapshot: &SupervisorEnvironmentSnapshot,
) -> Result<String> {
    let identity = windows_supervisor_identity(data_root, "S-1-0-0")?;
    let spec = supervisor_artifact_spec(identity, executable, data_root, snapshot)?;
    ctx_daemon_runtime::windows_sanitized_process_supervisor_script(
        spec.launch(),
        spec.environment_path(),
    )
}

#[cfg(windows)]
pub(super) fn windows_sanitized_process_supervisor_script(
    executable: &Path,
    data_root: &Path,
    arguments: &[String],
    snapshot: &SupervisorEnvironmentSnapshot,
) -> Result<String> {
    let environment = snapshot
        .values
        .iter()
        .map(|(name, value)| (OsString::from(name), OsString::from(value)))
        .collect::<BTreeMap<_, _>>();
    let launch = NormalizedLaunch::new(
        executable.to_path_buf(),
        arguments.iter().map(OsString::from).collect(),
        environment,
    );
    let identity = windows_supervisor_identity(data_root, "S-1-0-0")?;
    let spec = ctx_daemon_runtime::SupervisorSpec::new(
        identity,
        SUPERVISOR_DESCRIPTION,
        ctx_daemon_runtime::supervisor_environment_path(data_root),
        launch,
    )?;
    let environment_path = ctx_daemon_runtime::write_supervisor_environment(&spec)?;
    ctx_daemon_runtime::windows_sanitized_process_supervisor_script(
        spec.launch(),
        &environment_path,
    )
}

pub(super) fn windows_task_xml_with_environment(
    executable: &Path,
    data_root: &Path,
    system_root: &Path,
    user_sid: &str,
    task_name: &str,
    snapshot: &SupervisorEnvironmentSnapshot,
) -> Result<String> {
    let identity = ctx_daemon_runtime::SupervisorIdentity::new(
        task_name,
        ctx_daemon_runtime::daemon_root_path(data_root).join("windows-task.xml"),
    )?;
    let spec = supervisor_artifact_spec(identity, executable, data_root, snapshot)?;
    ctx_daemon_runtime::windows_task_xml(&spec, system_root, user_sid)
}

#[cfg(windows)]
pub(super) fn windows_task_xml_with_script(
    system_root: &Path,
    user_sid: &str,
    task_name: &str,
    script: &str,
) -> Result<String> {
    ctx_daemon_runtime::windows_task_xml_with_script(
        system_root,
        user_sid,
        task_name,
        SUPERVISOR_DESCRIPTION,
        script,
    )
}

pub(super) fn windows_task_registration_matches_with_environment(
    xml: &str,
    system_root: &Path,
    user_sid: &str,
    task_name: &str,
    input: &ManagedSupervisorInput,
) -> Result<bool> {
    let identity = ctx_daemon_runtime::SupervisorIdentity::new(
        task_name,
        ctx_daemon_runtime::daemon_root_path(&input.data_root).join("windows-task.xml"),
    )?;
    let spec = supervisor_artifact_spec(
        identity,
        &input.executable,
        &input.data_root,
        &input.daemon_environment,
    )?;
    ctx_daemon_runtime::windows_task_registration_matches(
        xml,
        &spec,
        system_root,
        user_sid,
        &input.manager_environment,
    )
}
