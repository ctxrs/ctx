use tantivy::Index;

use crate::Result;

use super::super::super::generation::CandidateGeneration;

pub(in crate::publication::migration) struct MigrationCandidate {
    pub(in crate::publication::migration) directory_name: String,
    pub(in crate::publication::migration) index: Index,
    authentication: CandidateAuthentication,
}

pub(super) enum CandidateAuthentication {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    DescriptorClone(super::unix::CandidateGuard),
    #[cfg(any(test, target_os = "windows", target_os = "freebsd"))]
    Portable(super::portable::CandidateGuard),
}

impl MigrationCandidate {
    pub(super) fn new(
        candidate: CandidateGeneration,
        authentication: CandidateAuthentication,
    ) -> Self {
        Self {
            directory_name: candidate.directory_name,
            index: candidate.index,
            authentication,
        }
    }

    pub(in crate::publication::migration) fn validate_binding(&self) -> Result<()> {
        match &self.authentication {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            CandidateAuthentication::DescriptorClone(guard) => guard.validate_binding(),
            #[cfg(any(test, target_os = "windows", target_os = "freebsd"))]
            CandidateAuthentication::Portable(guard) => guard.validate_binding(),
        }
    }

    pub(in crate::publication::migration) fn discard(self) {
        let Self {
            index,
            authentication,
            ..
        } = self;
        drop(index);
        match authentication {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            CandidateAuthentication::DescriptorClone(guard) => guard.discard(),
            #[cfg(any(test, target_os = "windows", target_os = "freebsd"))]
            CandidateAuthentication::Portable(guard) => guard.discard(),
        }
    }
}
