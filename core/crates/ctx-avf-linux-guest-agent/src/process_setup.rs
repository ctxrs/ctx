#[cfg(target_os = "linux")]
use std::process::Command;

#[cfg(target_os = "linux")]
use anyhow::Context;
use anyhow::{bail, Result};
#[cfg(target_os = "linux")]
use portable_pty::CommandBuilder as PtyCommandBuilder;

use crate::protocol::{AvfLinuxExecRequest, AVF_LINUX_EXEC_PROTOCOL_VERSION};
#[cfg(target_os = "linux")]
use crate::DEFAULT_PATH;

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug)]
pub(crate) struct PreparedExec {
    pub(crate) command: String,
    pub(crate) args: Vec<String>,
    pub(crate) cwd: std::path::PathBuf,
    pub(crate) user: Option<String>,
    pub(crate) env: std::collections::HashMap<String, String>,
    pub(crate) pty: bool,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn prepare_exec_request(request: &AvfLinuxExecRequest) -> Result<PreparedExec> {
    if request.protocol_version != AVF_LINUX_EXEC_PROTOCOL_VERSION {
        bail!(
            "unsupported exec protocol version {}, expected {}",
            request.protocol_version,
            AVF_LINUX_EXEC_PROTOCOL_VERSION
        );
    }
    if request.command.trim().is_empty() {
        bail!("guest exec command must not be empty");
    }
    if request.cwd.trim().is_empty() {
        bail!("guest exec cwd must not be empty");
    }

    let user = request
        .user
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);

    Ok(PreparedExec {
        command: request.command.clone(),
        args: request.args.clone(),
        cwd: request.cwd.clone().into(),
        user,
        env: request.env.clone(),
        pty: request.pty,
    })
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
pub(crate) struct ResolvedUserAccount {
    uid: libc::uid_t,
    gid: libc::gid_t,
    pub(crate) home: String,
    pub(crate) user: String,
}

#[cfg(target_os = "linux")]
pub(crate) fn lookup_user(user: &str) -> Result<ResolvedUserAccount> {
    let c_user =
        std::ffi::CString::new(user).with_context(|| format!("invalid username `{user}`"))?;
    let pwd = unsafe { libc::getpwnam(c_user.as_ptr()) };
    if pwd.is_null() {
        bail!("guest user `{user}` does not exist");
    }
    let pwd = unsafe { &*pwd };
    let home = unsafe { std::ffi::CStr::from_ptr(pwd.pw_dir) }
        .to_string_lossy()
        .to_string();
    Ok(ResolvedUserAccount {
        uid: pwd.pw_uid,
        gid: pwd.pw_gid,
        home,
        user: user.to_string(),
    })
}

#[cfg(target_os = "linux")]
pub(crate) fn build_pty_command(prepared: &PreparedExec) -> Result<PtyCommandBuilder> {
    if let Some(user) = prepared.user.as_deref() {
        let account = lookup_user(user)?;
        let mut env_pairs = prepared.env.clone();
        if !env_pairs.contains_key("PATH") {
            env_pairs.insert("PATH".to_string(), DEFAULT_PATH.to_string());
        }
        if !env_pairs.contains_key("HOME") {
            env_pairs.insert("HOME".to_string(), account.home.clone());
        }
        if !env_pairs.contains_key("USER") {
            env_pairs.insert("USER".to_string(), account.user.clone());
        }
        if !env_pairs.contains_key("LOGNAME") {
            env_pairs.insert("LOGNAME".to_string(), account.user.clone());
        }

        let mut script = format!(
            "cd {} && exec /usr/bin/env -i",
            shell_words::quote(prepared.cwd.to_string_lossy().as_ref())
        );
        let mut env_entries = env_pairs.into_iter().collect::<Vec<_>>();
        env_entries.sort_by(|(left, _), (right, _)| left.cmp(right));
        for (key, value) in env_entries {
            script.push(' ');
            script.push_str(&shell_words::quote(&format!("{key}={value}")));
        }
        script.push(' ');
        script.push_str(&shell_words::quote(&prepared.command));
        for arg in &prepared.args {
            script.push(' ');
            script.push_str(&shell_words::quote(arg));
        }

        let su_path = if std::path::Path::new("/usr/bin/su").exists() {
            "/usr/bin/su"
        } else {
            "/bin/su"
        };
        let mut cmd = PtyCommandBuilder::new(su_path);
        cmd.arg("-s");
        cmd.arg("/bin/sh");
        cmd.arg("-c");
        cmd.arg(script);
        cmd.arg(account.user);
        return Ok(cmd);
    }

    let mut cmd = PtyCommandBuilder::new(prepared.command.clone());
    for arg in &prepared.args {
        cmd.arg(arg);
    }
    cmd.cwd(prepared.cwd.clone());
    for (key, value) in &prepared.env {
        cmd.env(key, value);
    }
    if !prepared.env.contains_key("PATH") {
        cmd.env("PATH", DEFAULT_PATH);
    }
    Ok(cmd)
}

#[cfg(target_os = "linux")]
pub(crate) fn configure_command_process_group(command: &mut Command) -> Result<()> {
    use std::os::unix::process::CommandExt;

    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(crate) fn configure_command_user(
    command: &mut Command,
    account: &ResolvedUserAccount,
) -> Result<()> {
    use std::os::unix::process::CommandExt;

    let uid = account.uid;
    let gid = account.gid;
    unsafe {
        command.pre_exec(move || {
            if libc::setgroups(0, std::ptr::null()) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::setgid(gid) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::setuid(uid) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(crate) fn terminate_exec_process_group(pid: u32) -> Result<()> {
    let pgid = -(pid as i32);
    let signal_result = unsafe { libc::kill(pgid, libc::SIGTERM) };
    if signal_result != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::ESRCH) {
            return Err(err).with_context(|| format!("stopping guest command pid {pid}"));
        }
        return Ok(());
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        let probe = unsafe { libc::kill(pgid, 0) };
        if probe != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let kill_result = unsafe { libc::kill(pgid, libc::SIGKILL) };
    if kill_result != 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::ESRCH) {
            return Err(err).with_context(|| format!("force-stopping guest command pid {pid}"));
        }
    }
    Ok(())
}
