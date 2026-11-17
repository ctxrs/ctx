use std::{
    io::{self, BufRead as _, IsTerminal as _, Read as _, Write as _},
    path::Path,
};

use anyhow::{bail, Result};
use ctx_cli_presentation::upgrade::ExternalReplaceArgs;
use ctx_upgrade_engine::{DaemonUpgradePort, UpgradeEngine};
use serde_json::json;

pub(super) fn run<D: DaemonUpgradePort + ?Sized>(
    engine: &UpgradeEngine<'_, D>,
    data_root: &Path,
    args: &ExternalReplaceArgs,
) -> Result<()> {
    if io::stdin().is_terminal() || io::stdout().is_terminal() {
        bail!("external replacement requires redirected stdin and stdout");
    }
    let mut output = io::stdout().lock();
    let replaced =
        engine.with_external_replacement(data_root, &args.executable, &args.candidate, || {
            writeln!(
                output,
                "{}",
                json!({"schema_version": 1, "status": "ready"})
            )?;
            output.flush()?;
            let mut input = Vec::new();
            io::stdin().lock().take(7).read_until(b'\n', &mut input)?;
            match input.as_slice() {
                b"commit\n" => Ok(true),
                b"abort\n" | b"" => Ok(false),
                _ => bail!("expected commit or abort on the external replacement pipe"),
            }
        })?;
    let status = if replaced { "committed" } else { "aborted" };
    writeln!(output, "{}", json!({"schema_version": 1, "status": status}))?;
    Ok(output.flush()?)
}
