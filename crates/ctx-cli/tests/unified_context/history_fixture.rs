//! Synthetic native history and the explicit finite worker that publishes it.

use super::{json_output, Sandbox};
use serde_json::{json, Value};
use std::{
    thread,
    time::{Duration, Instant},
};

// The public Codex JSONL import contract is session_meta plus response_item.
pub(super) fn import_synthetic_history(sandbox: &Sandbox) {
    sandbox.write(
        "history/config.toml",
        "[indexing]\nmode = \"manual\"\n[sources]\nautomatic = false\n[search]\nsemantic = false\n",
    );
    let records = [
        json!({
            "timestamp":"2026-09-01T12:00:00Z", "type":"session_meta",
            "payload":{
                "id":"019faaaa-0000-7000-8000-000000000123",
                "timestamp":"2026-09-01T12:00:00Z", "cwd":sandbox.repo(),
                "originator":"codex_cli_rs", "cli_version":"0.1.0",
                "source":"cli", "model_provider":"openai"
            }
        }),
        json!({
            "timestamp":"2026-09-01T12:00:01Z", "type":"response_item",
            "payload":{"type":"message", "role":"user", "content":[{
                "type":"input_text", "text":"kernel acceptance history retains this citation."
            }]}
        }),
    ];
    sandbox.write(
        "providers/codex/sessions/rollout-acceptance.jsonl",
        records
            .iter()
            .map(|record| format!("{record}\n"))
            .collect::<String>(),
    );
    let worker = ImportWorker(sandbox);
    let output = sandbox
        .command()
        // Only this explicit fixture import may start its finite Core worker.
        .env_remove("CTX_DAEMON_AUTOSTART_OFF")
        .args(["import", "--provider", "codex", "--path"])
        .arg(sandbox.root.join("providers/codex/sessions"))
        .args(["--format=json", "--progress", "none"])
        .output()
        .unwrap();
    drop(worker); // Stop before snapshot assertions, including failed imports.
    let imported = json_output(output);
    assert_eq!(imported["outcome"], "success", "{imported}");
    assert!(
        imported["totals"]["current_indexed_documents"]
            .as_u64()
            .unwrap()
            > 0
    );
}

/// Borrowing keeps the isolated root alive through cleanup, including unwinding.
struct ImportWorker<'a>(&'a Sandbox);

impl ImportWorker<'_> {
    fn stop(&self) -> Result<(), String> {
        // Use the same binary and root's normal ownership-aware stop contract;
        // never signal a PID independently or touch the host's daemon root.
        let stopped = self
            .0
            .command()
            .timeout(Duration::from_secs(12))
            .args(["daemon", "disable", "--format=json"])
            .output()
            .map_err(|error| format!("disable fixture worker: {error}"))?;
        if !stopped.status.success() {
            return Err(format!(
                "disable fixture worker ({}): {}",
                stopped.status,
                String::from_utf8_lossy(&stopped.stderr)
            ));
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let status = self
                .0
                .command()
                .timeout(Duration::from_secs(2))
                .args(["daemon", "status", "--format=json"])
                .output()
                .map_err(|error| format!("inspect fixture worker: {error}"))?;
            if status.status.success()
                && serde_json::from_slice::<Value>(&status.stdout)
                    .is_ok_and(|report| report["daemon"]["running"] == false)
            {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "fixture worker did not stop: stdout={} stderr={}",
                    String::from_utf8_lossy(&status.stdout),
                    String::from_utf8_lossy(&status.stderr)
                ));
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for ImportWorker<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            if thread::panicking() {
                eprintln!("fixture cleanup also failed: {error}");
            } else {
                panic!("fixture cleanup failed: {error}");
            }
        }
    }
}
