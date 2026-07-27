//! Rovo Dev provider resolution for the config/project resolver group.

use std::path::PathBuf;

use serde_json::Value;

use crate::provider_sources::{
    context::{DiscoveryContext, DiscoveryPlatform},
    selectors::{SelectorFormat, SelectorReader},
    types::{DiscoveryReport, ProviderSourceSpec},
};

use super::{
    add_manual_issue, add_source, lexical_normalize, local_absolute_path, read_optional,
    string_setting, structured, supported_desktop_platform, OptionalDocument, StringSetting,
    INVALID_SELECTOR_REASON, MANUAL_SELECTOR_REASON, ROVO_FORMAT,
};

// Rovo Dev -----------------------------------------------------------------

pub(super) fn resolve(context: &DiscoveryContext, spec: &ProviderSourceSpec) -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    if !supported_desktop_platform(context) {
        return report;
    }
    let mut reader = SelectorReader::default();
    let config = context.home().join(".rovodev").join("config.yml");
    let root = match read_optional(&mut reader, &config, SelectorFormat::Yaml) {
        Ok(OptionalDocument::Missing) => context.home().join(".rovodev").join("sessions"),
        Ok(OptionalDocument::Empty) | Err(_) => {
            add_manual_issue(&mut report, spec.provider, INVALID_SELECTOR_REASON);
            return report;
        }
        Ok(OptionalDocument::Present(document)) => {
            match string_setting(
                structured(&document).unwrap_or(&Value::Null),
                &["sessions", "persistenceDir"],
            ) {
                StringSetting::Missing => context.home().join(".rovodev").join("sessions"),
                StringSetting::Value(raw) => {
                    let path = if raw == "~" {
                        context.home().to_path_buf()
                    } else if let Some(rest) = raw.strip_prefix("~/") {
                        context.home().join(rest)
                    } else if matches!(context.platform(), DiscoveryPlatform::Windows) {
                        if let Some(rest) = raw.strip_prefix("~\\") {
                            context.home().join(rest.replace('\\', "/"))
                        } else {
                            PathBuf::from(&raw)
                        }
                    } else {
                        PathBuf::from(&raw)
                    };
                    if !local_absolute_path(&path) {
                        add_manual_issue(&mut report, spec.provider, MANUAL_SELECTOR_REASON);
                        return report;
                    }
                    lexical_normalize(&path)
                }
                StringSetting::Reset | StringSetting::Invalid => {
                    add_manual_issue(&mut report, spec.provider, INVALID_SELECTOR_REASON);
                    return report;
                }
            }
        }
    };
    add_source(&mut report, spec, root, ROVO_FORMAT);
    report
}
