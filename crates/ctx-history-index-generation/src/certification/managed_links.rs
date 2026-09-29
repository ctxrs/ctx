use super::*;

mod unlink;
pub(crate) use unlink::CandidateUnlinks;
#[cfg(any(test, feature = "test-support"))]
pub use unlink::{ManagedUnlinkStage, ManagedUnlinkTestGuard};

/// Exact certificates captured before a writer-owned link operation. Keeping
/// the alias guards alive also covers readers that finish during that operation.
pub(crate) struct ManagedLinkCertifications {
    fence: ActiveGenerationPointerFence,
    aliases: CertificationAliasAuthority,
    certifications: Vec<GenerationIntegrityCertification>,
}

impl ManagedLinkCertifications {
    pub(crate) fn capture(
        root: &Path,
        pointer: &ActiveGenerationPointer,
        managed_directories: &[&str],
    ) -> Result<Self> {
        let fence = ActiveGenerationPointerFence::capture(root, Some(pointer))?;
        let aliases =
            CertificationAliasAuthority::capture_directories(root, &fence, managed_directories)?;
        let certifications = aliases
            .directories()
            .iter()
            .filter(|directory| !managed_directories.contains(&directory.as_str()))
            .filter_map(|directory| {
                Self::capture_certification(root, directory, &aliases)
                    .ok()
                    .flatten()
            })
            .collect();
        aliases.validate(root, &fence)?;
        Ok(Self {
            fence,
            aliases,
            certifications,
        })
    }

    fn capture_certification(
        root: &Path,
        directory: &str,
        aliases: &CertificationAliasAuthority,
    ) -> Result<Option<GenerationIntegrityCertification>> {
        let Some(certification) = Self::read_certification(root, directory)? else {
            return Ok(None);
        };
        for expected in &certification.artifacts {
            if capture_artifact_with_retained_aliases(
                root,
                &slot_path(root, &certification.slot),
                Path::new(&expected.artifact.path),
                aliases.directories(),
            )? != expected.artifact
            {
                return Ok(None);
            }
        }
        Ok(Some(certification))
    }

    fn read_certification(
        root: &Path,
        directory: &str,
    ) -> Result<Option<GenerationIntegrityCertification>> {
        let path = root
            .join(CERTIFICATION_DIRECTORY)
            .join(format!("{directory}{CERTIFICATION_SUFFIX}"));
        let Some(bytes) = read_certification(&path) else {
            return Ok(None);
        };
        let certification: GenerationIntegrityCertification = serde_json::from_slice(&bytes)?;
        certification.slot.validate()?;
        if certification.slot.directory() != directory
            || certification.version != CERTIFICATION_VERSION
            || serde_json::to_vec(&certification)? != bytes
            || !certification_digest_matches_slot(&certification)?
            || capture_single_link_control(&manifest_path(
                root,
                certification.slot.generation_id(),
            ))? != certification.manifest_identity
        {
            return Ok(None);
        }
        Ok(Some(certification))
    }

    pub(crate) fn finish_clone(self, root: &Path, proof: &crate::CandidatePhysicalProof) {
        self.finish(root, |expected, current| {
            proof
                .file(&current.path)
                .is_some_and(|file| file.artifact == *current && file.sha256 == expected.sha256)
        });
    }

    pub(super) fn finish_reclaim(self, root: &Path) {
        self.finish(root, |expected, current| {
            current.identity.link_count().checked_add(1)
                == Some(expected.artifact.identity.link_count())
        });
    }

    fn finish(
        self,
        root: &Path,
        changed_identity_is_authenticated: impl Fn(&CertifiedArtifact, &ArtifactIdentity) -> bool,
    ) {
        for mut certification in self.certifications {
            // A cache failure never authorizes stale bytes: ordinary readers
            // must still hash any identity without a completed certificate.
            let _ = (|| -> Result<()> {
                self.aliases.validate(root, &self.fence)?;
                let slot = &certification.slot;
                if capture_single_link_control(&manifest_path(root, slot.generation_id()))?
                    != certification.manifest_identity
                {
                    return Err(IndexError::ChecksumMismatch);
                }
                let mut changed = false;
                for expected in &mut certification.artifacts {
                    let current = capture_artifact_with_retained_aliases(
                        root,
                        &slot_path(root, slot),
                        Path::new(&expected.artifact.path),
                        self.aliases.directories(),
                    )?;
                    if current != expected.artifact {
                        if !current
                            .identity
                            .same_payload_identity(&expected.artifact.identity)
                            || !changed_identity_is_authenticated(expected, &current)
                        {
                            return Err(IndexError::ChecksumMismatch);
                        }
                        changed = true;
                        expected.artifact = current;
                    }
                }
                self.aliases.validate(root, &self.fence)?;
                if changed {
                    let index = crate::open_slot_index(root, slot)?;
                    if expected_artifact_paths(&index)?
                        != certification
                            .artifacts
                            .iter()
                            .map(|a| a.artifact.path.clone())
                            .collect::<Vec<_>>()
                    {
                        return Err(IndexError::ChecksumMismatch);
                    }
                    install_certification_sidecar(
                        root,
                        self.fence.topology_authority(),
                        None,
                        slot,
                        &index,
                        &certification,
                        false,
                    )?;
                }
                Ok(())
            })();
        }
    }
}
