use crate::Result;

pub struct CandidateActivationFence {
    authentication: CandidateAuthentication,
}

pub(super) enum CandidateAuthentication {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    DescriptorClone(super::unix::CandidateGuard),
    #[cfg(any(
        test,
        feature = "test-support",
        target_os = "windows",
        target_os = "freebsd"
    ))]
    Portable(super::portable::CandidateGuard),
}

impl CandidateActivationFence {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) fn descriptor_clone(guard: super::unix::CandidateGuard) -> Self {
        Self {
            authentication: CandidateAuthentication::DescriptorClone(guard),
        }
    }

    #[cfg(any(
        test,
        feature = "test-support",
        target_os = "windows",
        target_os = "freebsd"
    ))]
    pub(super) fn portable(guard: super::portable::CandidateGuard) -> Self {
        Self {
            authentication: CandidateAuthentication::Portable(guard),
        }
    }

    pub fn validate_binding(&self) -> Result<()> {
        match &self.authentication {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            CandidateAuthentication::DescriptorClone(guard) => guard.validate_binding(),
            #[cfg(any(
                test,
                feature = "test-support",
                target_os = "windows",
                target_os = "freebsd"
            ))]
            CandidateAuthentication::Portable(guard) => guard.validate_binding(),
        }
    }

    pub fn discard(self) {
        match self.authentication {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            CandidateAuthentication::DescriptorClone(guard) => guard.discard(),
            #[cfg(any(
                test,
                feature = "test-support",
                target_os = "windows",
                target_os = "freebsd"
            ))]
            CandidateAuthentication::Portable(guard) => guard.discard(),
        }
    }
}
