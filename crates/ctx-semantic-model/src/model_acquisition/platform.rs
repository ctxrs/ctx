use anyhow::Result;

use super::{acquisition_error, ModelAcquisitionErrorKind};

#[cfg(target_os = "macos")]
pub(super) fn ensure_macos_version_supported(minimum: &str) -> Result<()> {
    let output = std::process::Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .map_err(|error| {
            acquisition_error(
                ModelAcquisitionErrorKind::Unavailable,
                format!("could not determine macOS version: {error}"),
            )
        })?;
    if !output.status.success() {
        return Err(acquisition_error(
            ModelAcquisitionErrorKind::Unavailable,
            "could not determine macOS version",
        ));
    }
    let actual = String::from_utf8_lossy(&output.stdout);
    if !version_at_least(actual.trim(), minimum)? {
        return Err(acquisition_error(
            ModelAcquisitionErrorKind::Unavailable,
            format!("requires macOS {minimum} or newer"),
        ));
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub(super) fn ensure_macos_version_supported(_minimum: &str) -> Result<()> {
    #[cfg(test)]
    return Ok(());
    #[cfg(not(test))]
    Err(acquisition_error(
        ModelAcquisitionErrorKind::Unavailable,
        "Core ML requires macOS",
    ))
}

pub(super) fn version_at_least(actual: &str, minimum: &str) -> Result<bool> {
    fn parse(value: &str) -> Result<Vec<u64>> {
        let parts = value
            .split('.')
            .map(|part| part.parse::<u64>())
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_| {
                acquisition_error(
                    ModelAcquisitionErrorKind::Unavailable,
                    "macOS version has invalid syntax",
                )
            })?;
        if parts.is_empty() || parts.len() > 3 {
            return Err(acquisition_error(
                ModelAcquisitionErrorKind::Unavailable,
                "macOS version has invalid syntax",
            ));
        }
        Ok(parts)
    }
    let mut actual = parse(actual)?;
    let mut minimum = parse(minimum)?;
    actual.resize(3, 0);
    minimum.resize(3, 0);
    Ok(actual >= minimum)
}
