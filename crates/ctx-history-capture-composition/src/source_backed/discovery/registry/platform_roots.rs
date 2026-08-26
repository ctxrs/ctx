use super::*;

pub(in crate::source_backed::discovery) fn goose_platform_root(
    discovery: &DiscoveryContext,
    database: &Path,
) -> Option<PathBuf> {
    if let Some(root) = discovery
        .env("GOOSE_PATH_ROOT")
        .filter(|value| !value.is_empty())
    {
        let root = PathBuf::from(root);
        if root.is_absolute() && database == root.join("data/sessions/sessions.db") {
            return Some(root);
        }
    }
    let root = match discovery.platform() {
        DiscoveryPlatform::Linux | DiscoveryPlatform::MacOS => {
            match discovery.env("XDG_DATA_HOME") {
                Some(value) if !value.is_empty() && Path::new(value).is_absolute() => {
                    PathBuf::from(value).join("goose")
                }
                _ => discovery.home().join(".local/share/goose"),
            }
        }
        DiscoveryPlatform::Windows => discovery
            .platform_dirs()
            .data
            .as_ref()?
            .join("Block/goose/data"),
        DiscoveryPlatform::OtherUnix => {
            let value = discovery
                .env("XDG_DATA_HOME")
                .filter(|value| !value.is_empty() && Path::new(value).is_absolute())?;
            PathBuf::from(value).join("goose")
        }
    };
    (database == root.join("sessions/sessions.db")).then_some(root)
}
