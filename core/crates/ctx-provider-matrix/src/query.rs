use super::*;

fn comparable_context_version(raw: &Version) -> Version {
    let mut normalized = raw.clone();
    normalized.pre = semver::Prerelease::EMPTY;
    normalized
}

pub fn get_entry<'a>(
    matrix: &'a ProviderMatrix,
    provider_id: &str,
) -> Option<&'a ProviderMatrixEntry> {
    matrix.providers.iter().find(|p| p.id == provider_id)
}

pub fn is_user_facing_harness_id(matrix: &ProviderMatrix, provider_id: &str) -> bool {
    get_entry(matrix, provider_id)
        .map(|entry| entry.kind == ProviderMatrixEntryKind::Harness)
        .unwrap_or(true)
}

pub fn is_managed_supported_for_context(
    matrix: &ProviderMatrix,
    provider_id: &str,
    context_version: Option<&Version>,
) -> bool {
    let Some(entry) = get_entry(matrix, provider_id) else {
        return false;
    };
    if entry.managed_install.is_none() {
        return false;
    }
    recommended_release(entry, context_version).is_some()
}

pub fn recommended_release<'a>(
    entry: &'a ProviderMatrixEntry,
    context_version: Option<&Version>,
) -> Option<&'a ProviderRelease> {
    let candidates: Vec<&ProviderRelease> = entry
        .releases
        .iter()
        .filter(|r| r.status == ProviderReleaseStatus::Supported)
        .filter(|r| release_matches_context(r, context_version))
        .collect();

    select_latest_release(&candidates)
}

pub fn latest_release(entry: &ProviderMatrixEntry) -> Option<&ProviderRelease> {
    let candidates: Vec<&ProviderRelease> = entry
        .releases
        .iter()
        .filter(|r| r.status == ProviderReleaseStatus::Supported)
        .collect();
    select_latest_release(&candidates)
}

pub fn release_for_version<'a>(
    entry: &'a ProviderMatrixEntry,
    version: &str,
) -> Option<&'a ProviderRelease> {
    entry
        .releases
        .iter()
        .find(|r| version_matches(&r.version, version))
}

pub fn release_matches_context(
    release: &ProviderRelease,
    context_version: Option<&Version>,
) -> bool {
    let Some(ctx) = context_version else {
        return true;
    };
    let comparable_ctx = comparable_context_version(ctx);
    if let Some(min) = release.context_min.as_deref() {
        if let Some(min_v) = parse_version_loose(min) {
            if comparable_ctx < comparable_context_version(&min_v) {
                return false;
            }
        }
    }
    if let Some(max) = release.context_max.as_deref() {
        if let Some(max_v) = parse_version_loose(max) {
            if comparable_ctx > comparable_context_version(&max_v) {
                return false;
            }
        }
    }
    true
}

pub fn select_latest_release<'a>(
    candidates: &[&'a ProviderRelease],
) -> Option<&'a ProviderRelease> {
    let mut best: Option<(&ProviderRelease, Version)> = None;
    for release in candidates {
        if let Some(parsed) = parse_version_loose(&release.version) {
            match &best {
                Some((_, best_v)) if parsed <= *best_v => {}
                _ => best = Some((release, parsed)),
            }
        }
    }
    if let Some((release, _)) = best {
        return Some(release);
    }
    candidates.last().copied()
}

pub fn parse_version_loose(raw: &str) -> Option<Version> {
    let trimmed = raw.trim().trim_start_matches('v');
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(v) = Version::parse(trimmed) {
        return Some(v);
    }
    if trimmed.matches('.').count() == 1 {
        let candidate = format!("{trimmed}.0");
        if let Ok(v) = Version::parse(&candidate) {
            return Some(v);
        }
    }
    None
}

pub fn normalize_version(raw: &str) -> String {
    raw.trim().trim_start_matches('v').to_string()
}

fn strip_cli_suffix(raw: &str) -> &str {
    raw.strip_suffix("-cli")
        .or_else(|| raw.strip_suffix("_cli"))
        .unwrap_or(raw)
}

pub fn version_matches(release: &str, detected: &str) -> bool {
    let a = normalize_version(release);
    let b = normalize_version(detected);
    if a == b {
        return true;
    }
    strip_cli_suffix(&a) == strip_cli_suffix(&b)
}

pub fn extract_version(text: &str) -> Option<String> {
    let mut buf = String::new();
    let mut started = false;
    for ch in text.chars() {
        if !started {
            if ch.is_ascii_digit() {
                started = true;
                buf.push(ch);
            }
            continue;
        }
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' || ch == '_' {
            buf.push(ch);
            continue;
        }
        break;
    }
    if started {
        Some(buf)
    } else {
        None
    }
}
