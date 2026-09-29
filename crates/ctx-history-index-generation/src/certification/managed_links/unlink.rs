use super::*;
use tantivy::directory::error::DeleteError;

#[cfg(any(test, feature = "test-support"))]
mod test_probe;
#[cfg(any(test, feature = "test-support"))]
pub use test_probe::{ManagedUnlinkStage, ManagedUnlinkTestGuard};

/// Authority attached only to an authenticated writer-owned candidate. Its
/// activation fence outlives the writer and all its background merge workers.
pub(crate) struct CandidateUnlinks {
    root: PathBuf,
    pointer: ActiveGenerationPointer,
    candidate: String,
    #[cfg(any(test, feature = "test-support"))]
    probe: Option<test_probe::Hook>,
}

impl CandidateUnlinks {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn new(root: &Path, pointer: &ActiveGenerationPointer, candidate: &str) -> Self {
        Self {
            root: root.to_owned(),
            pointer: pointer.clone(),
            candidate: candidate.to_owned(),
            #[cfg(any(test, feature = "test-support"))]
            probe: test_probe::capture(),
        }
    }

    pub(crate) fn delete(
        &self,
        path: &Path,
        remove: impl FnOnce() -> std::result::Result<(), DeleteError>,
    ) -> std::result::Result<(), DeleteError> {
        let candidate_path = self
            .root
            .join(INDEX_GENERATIONS_DIRECTORY)
            .join(&self.candidate);
        // New merge output and copy/reflink candidates have no published alias.
        let Ok((file, identity)) = open_regular_file(&candidate_path.join(path)) else {
            return remove();
        };
        if identity.link_count() <= 1 {
            return remove();
        }
        // Never extend this guard across a Tantivy worker join: GC runs inside
        // those workers. Cache preparation/installation takes no nested guard.
        let _update =
            crate::retention::CertificationGuard::update(&self.root).map_err(|error| {
                DeleteError::IoError {
                    io_error: std::sync::Arc::new(std::io::Error::other(error)),
                    filepath: path.to_owned(),
                }
            })?;
        #[cfg(any(test, feature = "test-support"))]
        self.checkpoint(ManagedUnlinkStage::BeforeUnlink, path);
        let captured = self.capture(&candidate_path, path).ok();
        remove()?;
        #[cfg(any(test, feature = "test-support"))]
        self.checkpoint(ManagedUnlinkStage::AfterUnlink, path);
        if let Some((certifications, before)) = captured {
            if let Ok(after) = file_identity(&file) {
                if before.identity.same_payload_identity(&after)
                    && after.link_count().checked_add(1) == Some(before.identity.link_count())
                {
                    certifications.finish_unlink(self, &before, &after);
                }
            }
        }
        Ok(())
    }

    fn capture(
        &self,
        candidate_path: &Path,
        path: &Path,
    ) -> Result<(ManagedLinkCertifications, ArtifactIdentity)> {
        let root = &self.root;
        let fence = ActiveGenerationPointerFence::capture(root, Some(&self.pointer))?;
        let aliases =
            CertificationAliasAuthority::capture_directories(root, &fence, &[&self.candidate])?;
        let before = self.snapshot(candidate_path, path, &aliases)?;
        let mut certifications = Vec::new();
        for directory in aliases
            .directories()
            .iter()
            .filter(|d| **d != self.candidate)
        {
            let Ok(Some(certification)) =
                ManagedLinkCertifications::read_certification(root, directory)
            else {
                continue;
            };
            if certification.artifacts.iter().any(|a| a.artifact == before)
                && self
                    .snapshot(&slot_path(root, &certification.slot), path, &aliases)
                    .ok()
                    .as_ref()
                    == Some(&before)
            {
                certifications.push(certification);
            }
        }
        aliases.validate(root, &fence)?;
        Ok((
            ManagedLinkCertifications {
                fence,
                aliases,
                certifications,
            },
            before,
        ))
    }

    fn snapshot(
        &self,
        generation: &Path,
        path: &Path,
        aliases: &CertificationAliasAuthority,
    ) -> Result<ArtifactIdentity> {
        #[cfg(any(test, feature = "test-support"))]
        self.checkpoint(ManagedUnlinkStage::ArtifactSnapshot, path);
        capture_artifact_with_retained_aliases(&self.root, generation, path, aliases.directories())
    }

    #[cfg(any(test, feature = "test-support"))]
    fn checkpoint(&self, stage: ManagedUnlinkStage, path: &Path) {
        if let Some(probe) = &self.probe {
            probe(stage, path);
        }
    }
}

impl ManagedLinkCertifications {
    fn finish_unlink(
        self,
        owner: &CandidateUnlinks,
        before: &ArtifactIdentity,
        after: &FileIdentity,
    ) {
        let root = &owner.root;
        for mut certification in self.certifications {
            let _ = (|| -> Result<()> {
                self.aliases.validate(root, &self.fence)?;
                let slot = &certification.slot;
                if capture_single_link_control(&manifest_path(root, slot.generation_id()))?
                    != certification.manifest_identity
                {
                    return Err(IndexError::ChecksumMismatch);
                }
                let current = owner.snapshot(
                    &slot_path(root, slot),
                    Path::new(&before.path),
                    &self.aliases,
                )?;
                if current.identity != *after {
                    return Err(IndexError::ChecksumMismatch);
                }
                for expected in &mut certification.artifacts {
                    if expected.artifact == *before {
                        expected.artifact = current.clone();
                    }
                }
                self.aliases.validate(root, &self.fence)?;
                // Only this proven unlink's identity changes. Other artifacts
                // keep their exact old identities, including any stale/tampered
                // ones. Readers still check the entire certificate. Scanning all
                // artifact metadata here would make per-file GC quadratic.
                let bytes = serde_json::to_vec(&certification)?;
                install::write_optional_certification_sidecar(root, slot, &bytes)?;
                Ok(())
            })();
        }
    }
}
