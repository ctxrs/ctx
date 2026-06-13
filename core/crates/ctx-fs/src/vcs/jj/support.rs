use super::*;
use std::io::ErrorKind;
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct JjVersion {
    major: u64,
    minor: u64,
    patch: u64,
}

impl std::fmt::Display for JjVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

const JJ_MIN_VERSION: JjVersion = JjVersion {
    major: 0,
    minor: 25,
    patch: 0,
};

static JJ_VERSION_OK: OnceLock<JjVersion> = OnceLock::new();

fn parse_jj_version(output: &str) -> Option<JjVersion> {
    for token in output.split_whitespace() {
        let token = token.trim_start_matches('v');
        let mut version = String::new();
        let mut saw_digit = false;
        for ch in token.chars() {
            if ch.is_ascii_digit() {
                saw_digit = true;
                version.push(ch);
                continue;
            }
            if ch == '.' && saw_digit {
                version.push(ch);
                continue;
            }
            break;
        }
        if version.is_empty() {
            continue;
        }
        let parts = version.split('.').collect::<Vec<_>>();
        if parts.len() < 2 {
            continue;
        }
        let major = parts[0].parse().ok()?;
        let minor = parts[1].parse().ok()?;
        let patch = parts.get(2).and_then(|part| part.parse().ok()).unwrap_or(0);
        return Some(JjVersion {
            major,
            minor,
            patch,
        });
    }
    None
}

fn jj_version_supported(version: JjVersion) -> bool {
    (version.major, version.minor, version.patch)
        >= (
            JJ_MIN_VERSION.major,
            JJ_MIN_VERSION.minor,
            JJ_MIN_VERSION.patch,
        )
}

async fn probe_jj_version() -> Result<JjVersion> {
    let output = Command::new("jj")
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|err| {
            if err.kind() == ErrorKind::NotFound {
                anyhow::anyhow!("jj is required for Jujutsu repositories but was not found in PATH")
            } else {
                anyhow::anyhow!("running jj --version failed: {err}")
            }
        })?;
    if !output.status.success() {
        bail!(
            "jj --version failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let version = parse_jj_version(&stdout)
        .ok_or_else(|| anyhow::anyhow!("unable to parse jj version from `{}`", stdout.trim()))?;
    Ok(version)
}

pub(super) async fn ensure_jj_usable() -> Result<JjVersion> {
    if let Some(version) = JJ_VERSION_OK.get() {
        return Ok(*version);
    }
    let version = probe_jj_version().await?;
    if !jj_version_supported(version) {
        bail!("jj {version} is too old; ctx requires jj >= {JJ_MIN_VERSION}");
    }
    let _ = JJ_VERSION_OK.set(version);
    Ok(version)
}

pub(super) fn jj_command(root: &Path) -> Command {
    let mut cmd = Command::new("jj");
    cmd.arg("-R")
        .arg(root)
        .arg("--color=never")
        .arg("--no-pager");
    cmd
}

pub async fn jj_command_output(root: &Path, args: &[&str]) -> Result<std::process::Output> {
    ensure_jj_usable().await?;
    let output = jj_command(root)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .with_context(|| format!("running jj {args:?}"))?;
    Ok(output)
}

pub(super) async fn run_jj(root: &Path, args: &[&str]) -> Result<std::process::Output> {
    let output = jj_command_output(root, args).await?;
    if !output.status.success() {
        bail!(
            "jj {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(output)
}
