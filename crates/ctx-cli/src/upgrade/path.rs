use std::{
    env, fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const VERSION_PROBE_OUTPUT_LIMIT: usize = 4096;

#[derive(Debug, Clone)]
pub(super) struct PathDiagnostics {
    pub(super) current_exe: PathBuf,
    pub(super) entries: Vec<PathDiagnosticEntry>,
    pub(super) warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub(super) struct PathDiagnosticEntry {
    pub(super) path: PathBuf,
    pub(super) version: Option<String>,
    pub(super) current: bool,
}

impl PathDiagnostics {
    pub(super) fn json(&self) -> Value {
        json!({
            "current_exe": self.current_exe.display().to_string(),
            "first_ctx": self.entries.first().map(|entry| entry.path.display().to_string()),
            "entries": self.entries.iter().map(|entry| {
                json!({
                    "path": entry.path.display().to_string(),
                    "version": entry.version.as_deref(),
                    "current": entry.current,
                })
            }).collect::<Vec<_>>(),
            "warnings": self.warnings,
        })
    }
}

pub(super) fn path_diagnostics(current_exe: &Path, current_version: &str) -> PathDiagnostics {
    let current_identity = path_identity(current_exe);
    let current_display = current_exe.display().to_string();
    let binary_name = if cfg!(windows) { "ctx.exe" } else { "ctx" };
    let mut entries = Vec::new();
    for dir in env::var_os("PATH")
        .map(|path| env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default()
    {
        let candidate = dir.join(binary_name);
        if !candidate.is_file() {
            continue;
        }
        if entries
            .iter()
            .any(|entry: &PathDiagnosticEntry| same_path(&entry.path, &candidate))
        {
            continue;
        }
        let current = path_identity(&candidate) == current_identity;
        entries.push(PathDiagnosticEntry {
            version: current.then(|| format!("ctx {current_version}")),
            path: candidate,
            current,
        });
    }

    let mut warnings = Vec::new();
    match entries.first() {
        Some(first) if !first.current => warnings.push(format!(
            "PATH resolves ctx to {} before the current executable {}; your shell may keep using the earlier binary after upgrade",
            first.path.display(),
            current_display
        )),
        None => warnings.push(format!(
            "current ctx executable {current_display} is not discoverable on PATH"
        )),
        _ => {}
    }
    if entries.len() > 1 {
        warnings.push(format!(
            "multiple ctx binaries are on PATH; first is {}",
            entries[0].path.display()
        ));
    }
    let expected = format!("ctx {current_version}");
    for entry in &entries {
        if let Some(version) = &entry.version {
            if version != &expected {
                warnings.push(format!(
                    "ctx on PATH at {} reports {version}; current binary reports {expected}",
                    entry.path.display()
                ));
            }
        }
    }

    PathDiagnostics {
        current_exe: current_exe.to_path_buf(),
        entries,
        warnings,
    }
}

fn path_identity(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn same_path(left: &Path, right: &Path) -> bool {
    path_identity(left) == path_identity(right)
}

pub(super) fn ctx_binary_version(path: &Path) -> Result<String> {
    let output = run_ctx_version_command(path)?;
    if !output.status.success() {
        return Err(anyhow!("{} --version failed", path.display()));
    }
    if output.truncated {
        return Err(anyhow!(
            "{} --version output exceeded {} bytes",
            path.display(),
            VERSION_PROBE_OUTPUT_LIMIT
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.lines().next().unwrap_or_default().trim().to_owned())
}

struct VersionCommandOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    truncated: bool,
}

fn run_ctx_version_command(path: &Path) -> Result<VersionCommandOutput> {
    let mut child = Command::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("run {} --version", path.display()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("capture {} --version output", path.display()))?;
    let (output_tx, output_rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = output_tx.send(read_capped_output(stdout, VERSION_PROBE_OUTPUT_LIMIT));
    });
    let started = Instant::now();
    let mut status = None;
    let mut output = None;
    loop {
        if status.is_none() {
            status = child
                .try_wait()
                .with_context(|| format!("wait for {} --version", path.display()))?;
        }
        if output.is_none() {
            match output_rx.try_recv() {
                Ok(result) => {
                    output =
                        Some(result.with_context(|| {
                            format!("read {} --version output", path.display())
                        })?);
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(anyhow!(
                        "reader thread stopped for {} --version",
                        path.display()
                    ));
                }
            }
        }
        match (status.take(), output.take()) {
            (Some(status), Some((stdout, truncated))) => {
                return Ok(VersionCommandOutput {
                    status,
                    stdout,
                    truncated,
                });
            }
            (next_status, next_output) => {
                status = next_status;
                output = next_output;
            }
        }
        if started.elapsed() >= VERSION_PROBE_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Err(anyhow!(
                "{} --version timed out after {}ms",
                path.display(),
                VERSION_PROBE_TIMEOUT.as_millis()
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn read_capped_output(mut reader: impl Read, limit: usize) -> std::io::Result<(Vec<u8>, bool)> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 1024];
    while output.len() < limit {
        let remaining = limit - output.len();
        let max_read = remaining.min(buffer.len());
        let read = reader.read(&mut buffer[..max_read])?;
        if read == 0 {
            return Ok((output, false));
        }
        output.extend_from_slice(&buffer[..read]);
    }
    Ok((output, true))
}
