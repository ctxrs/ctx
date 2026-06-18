use super::*;

pub(super) fn normalize_update_channel_value(raw: &str) -> Result<String, String> {
    let channel = raw.trim();
    if channel.is_empty() {
        return Ok(DEFAULT_DESKTOP_UPDATE_CHANNEL.to_string());
    }
    if channel.len() > 64 {
        return Err("invalid channel (must be 64 characters or fewer)".to_string());
    }
    if matches!(channel, "." | "..") {
        return Err("invalid channel (must not be '.' or '..')".to_string());
    }
    let valid = channel
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.');
    if !valid {
        return Err("invalid channel (expected [A-Za-z0-9._-])".to_string());
    }
    Ok(channel.to_string())
}

pub(super) fn normalize_desktop_update_channel_with_sources(
    requested: Option<&str>,
    env_channel: Option<&str>,
    preference_channel: Option<&str>,
) -> Result<String, String> {
    let selected = requested
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| env_channel.map(str::trim).filter(|value| !value.is_empty()))
        .or_else(|| {
            preference_channel
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
        .unwrap_or(DEFAULT_DESKTOP_UPDATE_CHANNEL);
    normalize_update_channel_value(selected)
}

pub(super) fn resolve_desktop_update_channel(
    app: &tauri::AppHandle,
    requested: Option<&str>,
) -> Result<String, String> {
    let env_channel = std::env::var("CTX_DESKTOP_CHANNEL").ok();
    let requested_present = requested
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some();
    let env_present = env_channel
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some();
    let preference_channel = if requested_present || env_present {
        None
    } else {
        load_desktop_update_channel_preference(app)
            .map_err(|err| format!("failed to load desktop update channel: {err:#}"))?
    };
    normalize_desktop_update_channel_with_sources(
        requested,
        env_channel.as_deref(),
        preference_channel.as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_sources_default_to_stable() {
        assert_eq!(
            normalize_desktop_update_channel_with_sources(None, None, None).expect("channel"),
            "stable"
        );
    }

    #[test]
    fn preference_wins_over_default() {
        assert_eq!(
            normalize_desktop_update_channel_with_sources(None, None, Some("canary"))
                .expect("channel"),
            "canary"
        );
    }

    #[test]
    fn env_wins_over_preference() {
        assert_eq!(
            normalize_desktop_update_channel_with_sources(None, Some("e2e"), Some("canary"))
                .expect("channel"),
            "e2e"
        );
    }

    #[test]
    fn requested_wins_over_env_and_preference() {
        assert_eq!(
            normalize_desktop_update_channel_with_sources(
                Some("preview"),
                Some("e2e"),
                Some("canary")
            )
            .expect("channel"),
            "preview"
        );
    }

    #[test]
    fn legacy_artifact_channel_is_not_a_source() {
        assert_eq!(
            normalize_desktop_update_channel_with_sources(None, None, None).expect("channel"),
            "stable"
        );
    }

    #[test]
    fn invalid_channels_are_rejected() {
        assert!(normalize_update_channel_value("bad channel").is_err());
        assert!(normalize_update_channel_value(".").is_err());
        assert!(normalize_update_channel_value("..").is_err());
        assert!(normalize_update_channel_value(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        )
        .is_err());
    }
}
