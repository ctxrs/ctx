use std::{collections::BTreeMap, ffi::OsString};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum EnvironmentKey {
    Home,
    Path,
    Lang,
    LcAll,
    TimeZone,
    DbusSessionBusAddress,
    XdgRuntimeDir,
    LocalUsageEnabled,
    AnalyticsEnabled,
    HostedInstallerSetup,
    Term,
    ColorTerm,
    NoColor,
    CliColor,
    CliColorForce,
    Ci,
}

impl EnvironmentKey {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Home => "HOME",
            Self::Path => "PATH",
            Self::Lang => "LANG",
            Self::LcAll => "LC_ALL",
            Self::TimeZone => "TZ",
            Self::DbusSessionBusAddress => "DBUS_SESSION_BUS_ADDRESS",
            Self::XdgRuntimeDir => "XDG_RUNTIME_DIR",
            Self::LocalUsageEnabled => "CTX_LOCAL_USAGE_ENABLED",
            Self::AnalyticsEnabled => "CTX_ANALYTICS_ENABLED",
            Self::HostedInstallerSetup => "CTX_HOSTED_INSTALLER_SETUP",
            Self::Term => "TERM",
            Self::ColorTerm => "COLORTERM",
            Self::NoColor => "NO_COLOR",
            Self::CliColor => "CLICOLOR",
            Self::CliColorForce => "CLICOLOR_FORCE",
            Self::Ci => "CI",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct CompanionEnvironment {
    values: BTreeMap<OsString, OsString>,
}

impl CompanionEnvironment {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&mut self, key: EnvironmentKey, value: impl Into<OsString>) -> &mut Self {
        self.set_named(key.as_str(), value)
    }

    /// Adds a name selected by another closed public contract, such as the
    /// daemon supervisor environment allowlist.
    pub fn set_named(
        &mut self,
        name: impl Into<OsString>,
        value: impl Into<OsString>,
    ) -> &mut Self {
        self.values.insert(name.into(), value.into());
        self
    }

    pub fn get(&self, name: &str) -> Option<&std::ffi::OsStr> {
        self.values
            .get(std::ffi::OsStr::new(name))
            .map(OsString::as_os_str)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&std::ffi::OsStr, &std::ffi::OsStr)> {
        self.values
            .iter()
            .map(|(key, value)| (key.as_os_str(), value.as_os_str()))
    }

    pub(crate) fn len(&self) -> usize {
        self.values.len()
    }
}
