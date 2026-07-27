pub(crate) fn parse_daemon_interval_seconds(value: &str) -> Result<u64, String> {
    let parsed = value
        .parse::<u64>()
        .map_err(|err| format!("invalid daemon loop interval seconds: {err}"))?;
    if !(1..=3_600).contains(&parsed) {
        return Err("daemon loop interval seconds must be between 1 and 3600".to_owned());
    }
    Ok(parsed)
}

pub(crate) fn parse_event_window_limit(value: &str) -> Result<usize, String> {
    let limit = value
        .parse::<usize>()
        .map_err(|err| format!("invalid event window: {err}"))?;
    if limit > crate::MAX_EVENT_WINDOW {
        return Err(format!(
            "event window must be between 0 and {}",
            crate::MAX_EVENT_WINDOW
        ));
    }
    Ok(limit)
}
