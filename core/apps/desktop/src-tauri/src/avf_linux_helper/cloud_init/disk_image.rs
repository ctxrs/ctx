use super::*;

pub(crate) fn run_command(program: &str, args: &[&str], context_label: &str) -> Result<String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("spawning {program} for {context_label}"))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).to_string());
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let details = [stdout, stderr]
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if details.is_empty() {
        bail!("{context_label} failed with status {}", output.status);
    }
    bail!(
        "{context_label} failed with status {}:\n{details}",
        output.status
    );
}

pub(crate) fn attach_raw_disk_image_nomount(image_path: &Path) -> Result<String> {
    let output = run_command(
        "hdiutil",
        &[
            "attach",
            "-nomount",
            "-imagekey",
            "diskimage-class=CRawDiskImage",
            &image_path.display().to_string(),
        ],
        "attaching raw cloud-init image without mounting",
    )?;
    output
        .lines()
        .find_map(|line| line.split_whitespace().next())
        .filter(|value| value.starts_with("/dev/"))
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow::anyhow!("could not determine raw device from hdiutil output"))
}

pub(crate) fn attach_raw_disk_image_with_mount(image_path: &Path) -> Result<(String, PathBuf)> {
    let output = run_command(
        "hdiutil",
        &[
            "attach",
            "-imagekey",
            "diskimage-class=CRawDiskImage",
            &image_path.display().to_string(),
        ],
        "attaching raw cloud-init image with mount",
    )?;
    for line in output.lines() {
        let parts = line
            .split('\t')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>();
        if parts.len() < 3 {
            continue;
        }
        let device = parts[0];
        let mount = parts[2];
        if device.starts_with("/dev/") && mount.starts_with("/Volumes/") {
            return Ok((device.to_string(), PathBuf::from(mount)));
        }
    }
    bail!("could not determine mounted cloud-init volume from hdiutil output");
}

pub(crate) fn detach_disk_image_device(device: &str) -> Result<()> {
    let _ = run_command(
        "hdiutil",
        &["detach", device],
        "detaching raw cloud-init image",
    )?;
    Ok(())
}
