use super::*;

pub(super) async fn inspect_sandbox_machine_memory_mb(
    data_root: &Path,
    machine_name: &str,
) -> Result<Option<u32>> {
    let mut cmd = sandbox_container_command(data_root)?;
    cmd.arg("machine").arg("inspect").arg(machine_name);
    let output = command_output_with_timeout(cmd, SANDBOX_INFO_TIMEOUT).await?;
    if !output.status.success() {
        return Ok(None);
    }
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("parsing sandbox machine inspect output")?;
    let machine = value
        .as_array()
        .and_then(|items| items.first())
        .unwrap_or(&value);
    Ok(machine
        .get("Resources")
        .and_then(|resources| resources.get("Memory"))
        .or_else(|| {
            machine
                .get("resources")
                .and_then(|resources| resources.get("memory"))
        })
        .and_then(|memory| memory.as_u64())
        .and_then(|memory| u32::try_from(memory).ok()))
}

pub(super) async fn inspect_sandbox_machine_state(
    data_root: &Path,
    machine_name: &str,
) -> Result<Option<String>> {
    let mut cmd = sandbox_container_command(data_root)?;
    cmd.arg("machine").arg("inspect").arg(machine_name);
    let output = match command_output_with_timeout(cmd, SANDBOX_INFO_TIMEOUT).await {
        Ok(output) => output,
        Err(err) => {
            tracing::debug!(
                "unable to inspect local sandbox runtime state during workload probe fallback: {err:#}"
            );
            return Ok(None);
        }
    };
    if !output.status.success() {
        return Ok(None);
    }
    let value: serde_json::Value = match serde_json::from_slice(&output.stdout) {
        Ok(value) => value,
        Err(err) => {
            tracing::debug!(
                "unable to parse local sandbox runtime inspect output during workload probe fallback: {err:#}"
            );
            return Ok(None);
        }
    };
    let machine = value
        .as_array()
        .and_then(|items| items.first())
        .unwrap_or(&value);
    Ok(machine
        .get("State")
        .or_else(|| machine.get("state"))
        .and_then(|state| state.as_str())
        .map(|state| state.trim().to_ascii_lowercase()))
}
