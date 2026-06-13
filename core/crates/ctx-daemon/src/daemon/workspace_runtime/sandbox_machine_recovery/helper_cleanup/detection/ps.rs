#[cfg(unix)]
use std::process::Command as StdCommand;

use super::PsProcessRow;

fn parse_ps_pid_and_command(line: &str) -> Option<(u32, &str)> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let split_idx = trimmed.find(char::is_whitespace)?;
    let pid = trimmed[..split_idx].trim().parse::<u32>().ok()?;
    let command = trimmed[split_idx..].trim_start();
    if command.is_empty() {
        return None;
    }
    Some((pid, command))
}

pub(super) fn collect_ps_process_rows(output: &str) -> Vec<PsProcessRow> {
    let mut rows = output
        .lines()
        .filter_map(parse_ps_pid_and_command)
        .map(|(pid, command)| PsProcessRow {
            pid,
            command: command.to_string(),
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        left.pid
            .cmp(&right.pid)
            .then_with(|| left.command.cmp(&right.command))
    });
    rows.dedup_by(|left, right| left.pid == right.pid && left.command == right.command);
    rows
}

#[cfg(unix)]
pub(super) fn snapshot_ps_process_rows() -> Option<Vec<PsProcessRow>> {
    let output = StdCommand::new("ps")
        .arg("-axo")
        .arg("pid=,command=")
        .output()
        .ok()?;
    Some(collect_ps_process_rows(&String::from_utf8_lossy(
        &output.stdout,
    )))
}
