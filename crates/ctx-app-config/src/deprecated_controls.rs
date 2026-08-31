use std::{env, ffi::OsStr};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeprecatedControlKind {
    Privacy,
    Daemon,
    Upgrade,
}

#[derive(Debug, Clone, Copy)]
struct DeprecatedControl {
    name: &'static str,
    replacement: &'static str,
    kind: DeprecatedControlKind,
}

const REGISTRY: &[DeprecatedControl] = &[
    DeprecatedControl {
        name: "CTX_ANALYTICS_OFF",
        replacement: "CTX_ANALYTICS_ENABLED=false",
        kind: DeprecatedControlKind::Privacy,
    },
    DeprecatedControl {
        name: "CTX_DISABLE_ANALYTICS",
        replacement: "CTX_ANALYTICS_ENABLED=false",
        kind: DeprecatedControlKind::Privacy,
    },
    DeprecatedControl {
        name: "CTX_INSTALL_DIAGNOSTICS_OFF",
        replacement: "CTX_ANALYTICS_ENABLED=false",
        kind: DeprecatedControlKind::Privacy,
    },
    DeprecatedControl {
        name: "CTX_DAEMON_OFF",
        replacement: "CTX_DAEMON_ENABLED=false",
        kind: DeprecatedControlKind::Daemon,
    },
    DeprecatedControl {
        name: "CTX_DISABLE_DAEMON",
        replacement: "CTX_DAEMON_ENABLED=false",
        kind: DeprecatedControlKind::Daemon,
    },
    DeprecatedControl {
        name: "CTX_UPGRADE_OFF",
        replacement: "CTX_UPGRADE_AUTO=off",
        kind: DeprecatedControlKind::Upgrade,
    },
    DeprecatedControl {
        name: "CTX_DISABLE_AUTO_UPGRADE",
        replacement: "CTX_UPGRADE_AUTO=off",
        kind: DeprecatedControlKind::Upgrade,
    },
];

#[derive(Debug, Clone)]
struct DetectedControl {
    control: &'static DeprecatedControl,
    active: bool,
}

#[derive(Debug, Clone, Default)]
pub struct DeprecatedControls {
    detected: Vec<DetectedControl>,
}

impl DeprecatedControls {
    pub fn detect() -> Self {
        let detected = REGISTRY
            .iter()
            .filter_map(|control| {
                env::var_os(control.name).map(|value| DetectedControl {
                    control,
                    active: historical_truthy(&value),
                })
            })
            .collect();
        Self { detected }
    }

    pub fn disables_analytics(&self) -> bool {
        self.active(DeprecatedControlKind::Privacy)
    }

    pub fn disables_daemon(&self) -> bool {
        self.active(DeprecatedControlKind::Daemon)
    }

    pub fn disables_auto_upgrade(&self) -> bool {
        self.active(DeprecatedControlKind::Upgrade)
    }

    pub fn warning(&self) -> Option<String> {
        if self.detected.is_empty() {
            return None;
        }
        let mappings = self
            .detected
            .iter()
            .map(|detected| {
                format!(
                    "{} -> {}",
                    detected.control.name, detected.control.replacement
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        Some(format!(
            "warning: deprecated environment variables detected: {mappings}. Update your environment to use the replacements."
        ))
    }

    pub fn nonprivacy_analytics_ids(&self) -> Option<String> {
        let ids = self
            .detected
            .iter()
            .filter(|detected| detected.control.kind != DeprecatedControlKind::Privacy)
            .map(|detected| detected.control.name)
            .collect::<Vec<_>>();
        (!ids.is_empty()).then(|| ids.join(","))
    }

    fn active(&self, kind: DeprecatedControlKind) -> bool {
        self.detected
            .iter()
            .any(|detected| detected.control.kind == kind && detected.active)
    }
}

fn historical_truthy(value: &OsStr) -> bool {
    let value = value.to_string_lossy();
    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "" | "0" | "false" | "no" | "off"
    )
}

#[cfg(test)]
mod tests {
    use super::historical_truthy;
    use std::ffi::OsStr;

    #[test]
    fn historical_truthiness_matches_the_released_alias_contract() {
        for value in ["", " ", "0", " false ", "NO", "Off"] {
            assert!(!historical_truthy(OsStr::new(value)), "{value:?}");
        }
        for value in ["1", " true ", "YES", "on", "anything"] {
            assert!(historical_truthy(OsStr::new(value)), "{value:?}");
        }
    }
}
