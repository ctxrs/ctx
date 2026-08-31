const FILE_READ_ATTRIBUTES: u32 = 0x0000_0080;
const FILE_READ_DATA: u32 = 0x0000_0001;
const FILE_TRAVERSE: u32 = 0x0000_0020;
const SYNCHRONIZE: u32 = 0x0010_0000;
const PROVIDER_SOURCE_TARGET_ACCESS: u32 = FILE_READ_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE;
const PROVIDER_SOURCE_ANCESTOR_ACCESS: u32 = FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE;
const FILE_DIRECTORY_FILE: u32 = 0x0000_0001;
const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x0000_0020;
const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

#[derive(Clone, Copy)]
pub(super) enum ProviderSourceOpenKind {
    AncestorDirectory,
    Target,
}

impl ProviderSourceOpenKind {
    pub(super) const fn desired_access(self) -> u32 {
        match self {
            Self::AncestorDirectory => PROVIDER_SOURCE_ANCESTOR_ACCESS,
            Self::Target => PROVIDER_SOURCE_TARGET_ACCESS,
        }
    }

    pub(super) const fn open_options(self) -> u32 {
        let options = FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_REPARSE_POINT;
        match self {
            Self::AncestorDirectory => options | FILE_DIRECTORY_FILE,
            Self::Target => options,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ancestor_walk_requests_traverse_without_directory_listing() {
        let access = ProviderSourceOpenKind::AncestorDirectory.desired_access();

        assert_eq!(access & FILE_READ_DATA, 0);
        assert_ne!(access & FILE_TRAVERSE, 0);
        assert_ne!(access & FILE_READ_ATTRIBUTES, 0);
        assert_ne!(access & SYNCHRONIZE, 0);
        assert_ne!(
            ProviderSourceOpenKind::AncestorDirectory.open_options() & FILE_DIRECTORY_FILE,
            0
        );
    }

    #[test]
    fn target_open_retains_the_data_or_directory_listing_right() {
        let access = ProviderSourceOpenKind::Target.desired_access();

        assert_ne!(access & FILE_READ_DATA, 0);
        assert_ne!(access & FILE_READ_ATTRIBUTES, 0);
        assert_ne!(access & SYNCHRONIZE, 0);
        assert_eq!(
            ProviderSourceOpenKind::Target.open_options() & FILE_DIRECTORY_FILE,
            0
        );
    }
}
