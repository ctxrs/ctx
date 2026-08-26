use super::*;

#[derive(Clone, Copy)]
pub(super) struct CertificationInstallPolicy {
    allow_readonly_seal: bool,
    seal_artifacts: bool,
    require_durable_sidecar: bool,
}

impl CertificationInstallPolicy {
    pub(super) const ACTIVE_CACHE: Self = Self {
        allow_readonly_seal: false,
        seal_artifacts: true,
        require_durable_sidecar: false,
    };
    pub(super) const ACTIVATED_CACHE: Self = Self {
        allow_readonly_seal: true,
        seal_artifacts: true,
        require_durable_sidecar: false,
    };
    pub(super) const CANDIDATE: Self = Self {
        allow_readonly_seal: cfg!(windows),
        seal_artifacts: true,
        require_durable_sidecar: true,
    };
}

pub(super) fn install_certification(
    root: &Path,
    topology_authority: Option<&ActiveGenerationPointer>,
    predecessor_fence: Option<&ActiveGenerationPointerFence>,
    slot: &GenerationSlot,
    index: &tantivy::Index,
    audit: &PhysicalIntegrityAudit,
    policy: CertificationInstallPolicy,
) -> Result<CertifiedPhysicalIntegrity> {
    if audit.digest() != slot.physical_integrity_digest() {
        return Err(IndexError::ChecksumMismatch);
    }
    if let Some(fence) = predecessor_fence {
        fence.validate(root)?;
    } else if let Some(pointer) = topology_authority {
        if load_current_pointer(root)? != *pointer {
            return Err(IndexError::ConcurrentGenerationChange);
        }
    }
    let expected_paths = expected_artifact_paths(index)?;
    if audit.artifact_paths() != expected_paths {
        return Err(IndexError::ChecksumMismatch);
    }

    let generation_path = slot_path(root, slot);
    ensure_real_directory(root)?;
    ensure_real_directory(&root.join(MANIFEST_DIRECTORY))?;
    ensure_real_directory(&root.join(INDEX_GENERATIONS_DIRECTORY))?;
    ensure_real_directory(&generation_path)?;

    let manifest_identity =
        capture_single_link_control(&manifest_path(root, slot.generation_id()))?;
    let mut artifacts = Vec::with_capacity(audit.files().len());
    for prior in audit.files() {
        let mut current = capture_artifact(
            root,
            &generation_path,
            Path::new(&prior.artifact.path),
            topology_authority,
        )?;
        let follows_allowed_seal = policy.allow_readonly_seal && {
            #[cfg(windows)]
            {
                current
                    .identity
                    .follows_readonly_seal(&prior.artifact.identity)
            }
            #[cfg(not(windows))]
            {
                false
            }
        };
        if current.identity != prior.artifact.identity && !follows_allowed_seal {
            return if prior.artifact.same_payload_identity_changed(&current) {
                Err(IndexError::ConcurrentGenerationChange)
            } else {
                Err(IndexError::ChecksumMismatch)
            };
        }
        let sealed = policy.seal_artifacts && artifact_should_be_sealed(&prior.artifact.path);
        if sealed {
            current = seal_artifact(
                root,
                &generation_path,
                Path::new(&prior.artifact.path),
                topology_authority,
                &current,
            )?;
        }
        artifacts.push(CertifiedArtifact {
            artifact: current,
            sha256: prior.sha256,
            sealed,
        });
    }
    let certification = GenerationIntegrityCertification {
        version: CERTIFICATION_VERSION,
        manifest_identity,
        slot: slot.clone(),
        artifacts,
    };
    let sidecar_result = install_certification_sidecar(
        root,
        topology_authority,
        predecessor_fence,
        slot,
        index,
        &certification,
        policy.require_durable_sidecar,
    );
    if policy.require_durable_sidecar {
        sidecar_result?;
    }
    Ok(CertifiedPhysicalIntegrity { certification })
}

fn install_certification_sidecar(
    root: &Path,
    topology_authority: Option<&ActiveGenerationPointer>,
    predecessor_fence: Option<&ActiveGenerationPointerFence>,
    slot: &GenerationSlot,
    index: &tantivy::Index,
    certification: &GenerationIntegrityCertification,
    require_durable_sidecar: bool,
) -> Result<()> {
    if certification.artifacts.len() > MAX_CERTIFIED_ARTIFACTS {
        return if require_durable_sidecar {
            Err(IndexError::ChecksumMismatch)
        } else {
            Ok(())
        };
    }
    let bytes = serde_json::to_vec(certification)?;
    if bytes.len() > MAX_CERTIFICATION_BYTES {
        return if require_durable_sidecar {
            Err(IndexError::ChecksumMismatch)
        } else {
            Ok(())
        };
    }
    let certification_directory = root.join(CERTIFICATION_DIRECTORY);
    if require_durable_sidecar {
        ensure_private_directory(&certification_directory)?;
        ensure_real_directory(&certification_directory)?;
        let directory = DurableMmapDirectory::open(root).map_err(tantivy::TantivyError::from)?;
        let relative_path = Path::new(CERTIFICATION_DIRECTORY).join(certification_file_name(slot));
        directory.atomic_write(&relative_path, &bytes)?;
        return verify_candidate_physical_integrity_read_only(
            root,
            predecessor_fence.ok_or(IndexError::ConcurrentGenerationChange)?,
            slot,
            index,
        );
    }

    // Ordinary reader/open certification remains a cache optimization. A
    // verified full hash is still authoritative when setup or the sidecar
    // write fails. Once replacement succeeds, reread errors remain terminal so
    // an observed identity race cannot be mistaken for a usable cache entry.
    if ensure_private_directory(&certification_directory).is_err()
        || ensure_real_directory(&certification_directory).is_err()
    {
        return Ok(());
    }
    let Ok(directory) = DurableMmapDirectory::open(root) else {
        return Ok(());
    };
    let relative_path = Path::new(CERTIFICATION_DIRECTORY).join(certification_file_name(slot));
    if directory.atomic_write(&relative_path, &bytes).is_err() {
        return Ok(());
    }
    let pointer = topology_authority.ok_or(IndexError::ConcurrentGenerationChange)?;
    if matching_certification(root, pointer, slot, index)?.is_some() {
        Ok(())
    } else {
        Err(IndexError::ConcurrentGenerationChange)
    }
}
