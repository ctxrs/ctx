use std::fmt;

use ctx_history_core::CaptureProvider;
use thiserror::Error;

use super::{
    context::{DiscoveryContext, DiscoveryPlatform},
    resolvers::resolve_warp_with_authority,
    specs::provider_source_spec,
    types::{ProviderSource, ProviderSourceSpec},
    StaticProviderProbeCatalog,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WarpInstalledPlatform {
    Linux,
    MacOS,
    Windows,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WarpReleaseChannel {
    Stable,
    Preview,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WarpTerminalSurface {
    Gui,
    Tui,
}

/// Stable catalog lineage for one installed Warp channel and terminal surface.
///
/// Keys are constructed only while resolving official Warp installation slots.
/// Physical paths never participate in this identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WarpInstalledSurfaceKey {
    platform: WarpInstalledPlatform,
    channel: WarpReleaseChannel,
    surface: WarpTerminalSurface,
}

impl WarpInstalledSurfaceKey {
    pub(crate) const fn new(
        platform: WarpInstalledPlatform,
        channel: WarpReleaseChannel,
        surface: WarpTerminalSurface,
    ) -> Self {
        Self {
            platform,
            channel,
            surface,
        }
    }

    pub const fn platform(self) -> WarpInstalledPlatform {
        self.platform
    }

    pub const fn channel(self) -> WarpReleaseChannel {
        self.channel
    }

    pub const fn surface(self) -> WarpTerminalSurface {
        self.surface
    }

    pub const fn as_str(self) -> &'static str {
        match (self.platform, self.channel, self.surface) {
            (
                WarpInstalledPlatform::Linux,
                WarpReleaseChannel::Stable,
                WarpTerminalSurface::Gui,
            ) => "linux:stable:gui",
            (
                WarpInstalledPlatform::Linux,
                WarpReleaseChannel::Stable,
                WarpTerminalSurface::Tui,
            ) => "linux:stable:tui",
            (
                WarpInstalledPlatform::Linux,
                WarpReleaseChannel::Preview,
                WarpTerminalSurface::Gui,
            ) => "linux:preview:gui",
            (
                WarpInstalledPlatform::Linux,
                WarpReleaseChannel::Preview,
                WarpTerminalSurface::Tui,
            ) => "linux:preview:tui",
            (
                WarpInstalledPlatform::MacOS,
                WarpReleaseChannel::Stable,
                WarpTerminalSurface::Gui,
            ) => "macos:stable:gui",
            (
                WarpInstalledPlatform::MacOS,
                WarpReleaseChannel::Stable,
                WarpTerminalSurface::Tui,
            ) => "macos:stable:tui",
            (
                WarpInstalledPlatform::MacOS,
                WarpReleaseChannel::Preview,
                WarpTerminalSurface::Gui,
            ) => "macos:preview:gui",
            (
                WarpInstalledPlatform::MacOS,
                WarpReleaseChannel::Preview,
                WarpTerminalSurface::Tui,
            ) => "macos:preview:tui",
            (
                WarpInstalledPlatform::Windows,
                WarpReleaseChannel::Stable,
                WarpTerminalSurface::Gui,
            ) => "windows:stable:gui",
            (
                WarpInstalledPlatform::Windows,
                WarpReleaseChannel::Stable,
                WarpTerminalSurface::Tui,
            ) => "windows:stable:tui",
            (
                WarpInstalledPlatform::Windows,
                WarpReleaseChannel::Preview,
                WarpTerminalSurface::Gui,
            ) => "windows:preview:gui",
            (
                WarpInstalledPlatform::Windows,
                WarpReleaseChannel::Preview,
                WarpTerminalSurface::Tui,
            ) => "windows:preview:tui",
        }
    }
}

impl WarpInstalledPlatform {
    pub(crate) const fn role_component(self) -> &'static [u8] {
        match self {
            Self::Linux => b"linux",
            Self::MacOS => b"macos",
            Self::Windows => b"windows",
        }
    }
}

impl WarpReleaseChannel {
    pub(crate) const fn role_component(self) -> &'static [u8] {
        match self {
            Self::Stable => b"stable",
            Self::Preview => b"preview",
        }
    }
}

impl WarpTerminalSurface {
    pub(crate) const fn role_component(self) -> &'static [u8] {
        match self {
            Self::Gui => b"gui",
            Self::Tui => b"tui",
        }
    }
}

impl fmt::Display for WarpInstalledSurfaceKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One Warp source paired with the installed-surface authority that selected it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredWarpSource {
    source: ProviderSource,
    surface_key: WarpInstalledSurfaceKey,
}

impl DiscoveredWarpSource {
    pub(crate) const fn new(source: ProviderSource, surface_key: WarpInstalledSurfaceKey) -> Self {
        Self {
            source,
            surface_key,
        }
    }

    pub fn source(&self) -> &ProviderSource {
        &self.source
    }

    pub const fn surface_key(&self) -> WarpInstalledSurfaceKey {
        self.surface_key
    }

    pub fn into_parts(self) -> (ProviderSource, WarpInstalledSurfaceKey) {
        (self.source, self.surface_key)
    }
}

#[derive(Debug, Clone, Copy, Error, PartialEq, Eq)]
pub enum WarpDiscoveryUnavailable {
    #[error("Warp installed-surface discovery is unavailable on {platform:?}")]
    UnsupportedPlatform { platform: DiscoveryPlatform },
    #[error("Warp installed-surface discovery has no Windows local-data authority root")]
    WindowsLocalDataRootUnavailable,
    #[error("the Warp provider discovery specification is unavailable")]
    ProviderSpecUnavailable,
    #[error("Warp discovery rejected the {surface_key} source candidate")]
    SourceCandidateRejected {
        surface_key: WarpInstalledSurfaceKey,
    },
    #[error("the source was not selected by authoritative Warp discovery")]
    SourceNotSelected,
}

/// Discovers Warp sources together with their stable installed-surface keys.
///
/// The returned keys come directly from the platform/channel/surface resolver
/// decisions. No key is reconstructed from a selected filesystem path.
pub fn discover_warp_sources_with_authority(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
) -> Result<Vec<DiscoveredWarpSource>, WarpDiscoveryUnavailable> {
    resolve_warp_with_authority(probes, context, warp_spec()?)
}

/// Re-observes Warp discovery and returns authority only for an exact selected source.
///
/// This is the narrow bridge for callers that already hold a `ProviderSource`
/// from the shared discovery report. Unknown or explicit sources fail closed.
pub fn resolve_warp_discovery_authority(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
    selected_source: &ProviderSource,
) -> Result<DiscoveredWarpSource, WarpDiscoveryUnavailable> {
    discover_warp_sources_with_authority(probes, context)?
        .into_iter()
        .find(|candidate| candidate.source() == selected_source)
        .ok_or(WarpDiscoveryUnavailable::SourceNotSelected)
}

/// Reconstructs the installed-surface selector for a previously certified
/// automatic path without requiring that immutable identity path to remain
/// present. The returned path is identity-only; callers must keep filesystem
/// access bound to their current configured root.
pub fn resolve_warp_released_identity_authority(
    probes: &StaticProviderProbeCatalog,
    context: &DiscoveryContext,
    identity_path: &std::path::Path,
) -> Result<DiscoveredWarpSource, WarpDiscoveryUnavailable> {
    discover_warp_sources_with_authority(probes, context)?
        .into_iter()
        .find(|candidate| candidate.source().path == identity_path)
        .ok_or(WarpDiscoveryUnavailable::SourceNotSelected)
}

pub(crate) fn installed_platform(
    platform: DiscoveryPlatform,
) -> Result<WarpInstalledPlatform, WarpDiscoveryUnavailable> {
    match platform {
        DiscoveryPlatform::Linux => Ok(WarpInstalledPlatform::Linux),
        DiscoveryPlatform::MacOS => Ok(WarpInstalledPlatform::MacOS),
        DiscoveryPlatform::Windows => Ok(WarpInstalledPlatform::Windows),
        DiscoveryPlatform::OtherUnix => {
            Err(WarpDiscoveryUnavailable::UnsupportedPlatform { platform })
        }
    }
}

pub(crate) fn warp_spec() -> Result<&'static ProviderSourceSpec, WarpDiscoveryUnavailable> {
    provider_source_spec(CaptureProvider::Warp)
        .ok_or(WarpDiscoveryUnavailable::ProviderSpecUnavailable)
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use super::*;
    use crate::provider_sources::{
        discover_provider_sources_for_provider_with_context, DiscoveryPlatformDirs,
        WarpReleaseChannel, WarpTerminalSurface,
    };

    fn context(root: &Path, platform: DiscoveryPlatform) -> DiscoveryContext {
        DiscoveryContext::new(
            root.join("home"),
            root.join("cwd"),
            platform,
            DiscoveryPlatformDirs {
                data: Some(root.join("platform-data")),
                config: Some(root.join("platform-config")),
                state: Some(root.join("platform-state")),
                local_data: Some(root.join("platform-local-data")),
            },
        )
    }

    fn write_file(path: &Path) {
        fs::create_dir_all(path.parent().expect("fixture file should have a parent")).unwrap();
        fs::write(path, b"sqlite").unwrap();
    }

    #[test]
    fn linux_discovery_retains_channel_and_terminal_surface_authority() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let state = temp.path().join("state");
        let stable_gui = state.join("warp-terminal/warp.sqlite");
        let stable_tui = state.join("warp-terminal/tui/warp.sqlite");
        let preview_gui = state.join("warp-terminal-preview/warp.sqlite");
        write_file(&stable_gui);
        write_file(&stable_tui);
        write_file(&preview_gui);
        let context = context(temp.path(), DiscoveryPlatform::Linux)
            .with_env("XDG_STATE_HOME", state.as_os_str());

        let discovered = discover_warp_sources_with_authority(
            &crate::provider_sources::TEST_PROVIDER_PROBES,
            &context,
        )
        .unwrap();
        let observed = discovered
            .iter()
            .map(|candidate| {
                (
                    candidate.source().path.clone(),
                    candidate.surface_key().as_str(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            observed,
            vec![
                (stable_gui, "linux:stable:gui"),
                (stable_tui, "linux:stable:tui"),
                (preview_gui, "linux:preview:gui"),
            ]
        );
        assert_eq!(
            discovered[2].surface_key().channel(),
            WarpReleaseChannel::Preview
        );
        assert_eq!(
            discovered[1].surface_key().surface(),
            WarpTerminalSurface::Tui
        );
    }

    #[test]
    fn lookup_reemits_authority_and_rejects_nonselected_sources() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let state = temp.path().join("state");
        let stable = state.join("warp-terminal/warp.sqlite");
        write_file(&stable);
        let context = context(temp.path(), DiscoveryPlatform::Linux)
            .with_env("XDG_STATE_HOME", state.as_os_str());
        let source = discover_provider_sources_for_provider_with_context(
            &crate::provider_sources::TEST_PROVIDER_PROBES,
            &context,
            CaptureProvider::Warp,
        )
        .sources
        .remove(0);

        let selected = resolve_warp_discovery_authority(
            &crate::provider_sources::TEST_PROVIDER_PROBES,
            &context,
            &source,
        )
        .unwrap();
        assert_eq!(selected.surface_key().as_str(), "linux:stable:gui");

        let mut unselected = source;
        unselected.path = state.join("manual/warp.sqlite");
        assert_eq!(
            resolve_warp_discovery_authority(
                &crate::provider_sources::TEST_PROVIDER_PROBES,
                &context,
                &unselected,
            ),
            Err(WarpDiscoveryUnavailable::SourceNotSelected)
        );
    }

    #[test]
    fn unavailable_platform_authority_fails_closed() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let other_unix = context(temp.path(), DiscoveryPlatform::OtherUnix);
        assert_eq!(
            discover_warp_sources_with_authority(
                &crate::provider_sources::TEST_PROVIDER_PROBES,
                &other_unix,
            ),
            Err(WarpDiscoveryUnavailable::UnsupportedPlatform {
                platform: DiscoveryPlatform::OtherUnix,
            })
        );

        let windows = DiscoveryContext::new(
            temp.path().join("home"),
            temp.path().join("cwd"),
            DiscoveryPlatform::Windows,
            DiscoveryPlatformDirs::default(),
        );
        assert_eq!(
            discover_warp_sources_with_authority(
                &crate::provider_sources::TEST_PROVIDER_PROBES,
                &windows,
            ),
            Err(WarpDiscoveryUnavailable::WindowsLocalDataRootUnavailable)
        );
    }
}
