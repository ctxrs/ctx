use super::*;

pub(super) fn ssh_config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(path) = expand_tilde("~/.ssh/config") {
        paths.push(path);
    }
    let system_config = PathBuf::from("/etc/ssh/ssh_config");
    paths.push(system_config.clone());
    let system_dir = PathBuf::from("/etc/ssh/ssh_config.d");
    if let Ok(entries) = std::fs::read_dir(system_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                paths.push(path);
            }
        }
    }
    paths
}

pub(super) fn parse_ssh_config(text: &str) -> Vec<DesktopSshHost> {
    let mut out = Vec::new();
    let mut current_hosts: Vec<String> = Vec::new();
    let mut current_user: Option<String> = None;
    let mut current_host_name: Option<String> = None;
    let mut current_port: Option<u16> = None;

    let flush = |hosts: &Vec<String>,
                 user: &Option<String>,
                 host_name: &Option<String>,
                 port: &Option<u16>,
                 out: &mut Vec<DesktopSshHost>| {
        if hosts.is_empty() {
            return;
        }
        for host in hosts {
            if is_ssh_pattern(host) {
                continue;
            }
            out.push(DesktopSshHost {
                host: host.to_string(),
                user: user.clone(),
                host_name: host_name.clone(),
                port: *port,
            });
        }
    };

    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let key = parts.next().unwrap_or("");
        let rest: Vec<&str> = parts.collect();
        if key.eq_ignore_ascii_case("host") {
            flush(
                &current_hosts,
                &current_user,
                &current_host_name,
                &current_port,
                &mut out,
            );
            current_hosts = rest.iter().map(|v| v.to_string()).collect();
            current_user = None;
            current_host_name = None;
            current_port = None;
            continue;
        }
        if current_hosts.is_empty() {
            continue;
        }
        if key.eq_ignore_ascii_case("user") {
            current_user = rest.first().map(|v| v.to_string());
        } else if key.eq_ignore_ascii_case("hostname") {
            current_host_name = rest.first().map(|v| v.to_string());
        } else if key.eq_ignore_ascii_case("port") {
            current_port = rest.first().and_then(|v| v.parse::<u16>().ok());
        }
    }

    flush(
        &current_hosts,
        &current_user,
        &current_host_name,
        &current_port,
        &mut out,
    );
    out
}

fn is_ssh_pattern(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.is_empty()
        || trimmed.starts_with('!')
        || trimmed.contains('*')
        || trimmed.contains('?')
        || trimmed.contains('[')
        || trimmed.contains(']')
}

pub(in super::super) fn normalized_ssh_config_override(value: Option<&str>) -> Option<String> {
    normalize_optional_text(value)
}

fn ssh_config_override_path() -> Option<String> {
    std::env::var(SSH_CONFIG_OVERRIDE_ENV)
        .ok()
        .as_deref()
        .and_then(|raw| normalized_ssh_config_override(Some(raw)))
}

pub(in super::super) fn new_ssh_command() -> Command {
    let mut cmd = Command::new("ssh");
    if let Some(path) = ssh_config_override_path() {
        cmd.arg("-F").arg(path);
    }
    cmd
}

pub(super) fn ssh_target(host: &str, user: Option<&str>) -> String {
    match user {
        Some(u) if !u.trim().is_empty() => format!("{}@{}", u.trim(), host),
        _ => host.to_string(),
    }
}

pub(super) fn run_remote_ssh_shell(
    host: &str,
    user: Option<&str>,
    cmd: &str,
) -> Result<std::process::Output> {
    let target = ssh_target(host, user);
    let remote_cmd = format!("sh -lc {}", shell_escape(cmd));
    new_ssh_command()
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ConnectTimeout=15")
        .arg("-o")
        .arg("ServerAliveInterval=15")
        .arg("-o")
        .arg("ServerAliveCountMax=2")
        .arg(target)
        .arg(remote_cmd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("running ssh remote command")
}

pub(crate) fn shell_escape(s: &str) -> String {
    let inner = s.replace('\'', "'\"'\"'");
    format!("'{}'", inner)
}

fn escape_for_double_quotes(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$")
        .replace('`', "\\`")
}

pub(crate) fn remote_path_expr(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed == "~" {
        return "\"$HOME\"".to_string();
    }
    if let Some(rest) = trimmed.strip_prefix("~/") {
        let escaped = escape_for_double_quotes(rest);
        if escaped.is_empty() {
            return "\"$HOME\"".to_string();
        }
        return format!("\"$HOME/{}\"", escaped);
    }
    shell_escape(trimmed)
}

pub(super) fn split_remote_path(raw: &str) -> (String, String) {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return ("~".to_string(), String::new());
    }
    if trimmed == "~" || trimmed.ends_with('/') {
        return (trimmed.to_string(), String::new());
    }
    if let Some((parent, suffix)) = trimmed.rsplit_once('/') {
        if parent.is_empty() {
            return ("/".to_string(), suffix.to_string());
        }
        return (parent.to_string(), suffix.to_string());
    }
    ("~".to_string(), trimmed.to_string())
}

pub(super) fn join_remote_path(parent: &str, name: &str) -> String {
    if parent == "~" || parent == "~/" {
        return format!("~/{name}");
    }
    if parent == "/" {
        return format!("/{name}");
    }
    format!("{}/{}", parent.trim_end_matches('/'), name)
}
