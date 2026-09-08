//! Filesystem mutation and digest-authenticated transfer of validated clone plans.

use std::{
    fs::Permissions,
    io::{Read, Write},
    path::Path,
};

use sha2::{Digest, Sha256};

use super::{
    open_bound_file,
    planning::{PlannedFile, ValidatedClonePlan},
    resource::admit_available_bytes,
    support::{clone_checkpoint, PortableCloneStage},
    BoundDirectory, PermissionIdentity,
};
use crate::{
    certification::{
        capture_artifact_identity, open_authenticated_artifact, recapture_authenticated_artifact,
    },
    clone::{CandidateCloneMetrics, MANAGED_FILE, MAX_REPUBLISH_CLONE_BYTES},
    physical::PhysicalFileDigest,
    ActiveGenerationPointer, CandidatePhysicalProof, CertifiedPhysicalIntegrity,
    GenerationError as IndexError, Result,
};

#[allow(clippy::too_many_arguments)]
pub(super) fn clone_candidate_files(
    root: &Path,
    source_path: &Path,
    predecessor_pointer: &ActiveGenerationPointer,
    certified: &CertifiedPhysicalIntegrity,
    generations: &BoundDirectory,
    source_name: &Path,
    source: &BoundDirectory,
    destination_name: &Path,
    destination: &BoundDirectory,
    plan: &ValidatedClonePlan,
    writer_output_headroom: u64,
    physical_proof: &mut CandidatePhysicalProof,
    metrics: &mut CandidateCloneMetrics,
) -> Result<()> {
    let mut copied_bytes = 0_u64;
    for planned in plan.files() {
        source.validate_child_binding(generations, source_name)?;
        let remaining_copy_bytes = plan
            .logical_bytes()
            .checked_sub(copied_bytes)
            .ok_or(IndexError::CountOverflow)?;
        let required = remaining_copy_bytes
            .checked_add(writer_output_headroom)
            .ok_or(IndexError::CountOverflow)?;
        admit_available_bytes(generations, required, true)?;
        clone_checkpoint(PortableCloneStage::BeforeCopy, planned.path())?;
        if planned.path() == Path::new(MANAGED_FILE) {
            destination.validate_child_binding(generations, destination_name)?;
            let copied =
                write_authenticated_plan_bytes(destination, planned, plan.managed_bytes())?;
            copied_bytes = copied_bytes
                .checked_add(copied)
                .ok_or(IndexError::CountOverflow)?;
            if copied_bytes > MAX_REPUBLISH_CLONE_BYTES || copied_bytes > plan.logical_bytes() {
                return Err(IndexError::CurrentRepublishByteLimit {
                    actual: copied_bytes,
                    maximum: plan.logical_bytes().min(MAX_REPUBLISH_CLONE_BYTES),
                });
            }
            clone_checkpoint(PortableCloneStage::AfterCopy, planned.path())?;
            continue;
        }

        let (expected_artifact, expected_sha256, _sealed) = certified
            .certified_artifact(planned.path())
            .ok_or(IndexError::ChecksumMismatch)?;
        let (mut source_file, source_before) = open_authenticated_artifact(
            root,
            source_path,
            planned.path(),
            Some(predecessor_pointer),
        )?;
        if source_before != expected_artifact {
            return if expected_artifact.same_payload_identity_changed(&source_before) {
                Err(IndexError::ConcurrentGenerationChange)
            } else {
                Err(IndexError::ChecksumMismatch)
            };
        }
        clone_checkpoint(PortableCloneStage::AfterSourceOpen, planned.path())?;
        destination.validate_child_binding(generations, destination_name)?;
        let mut destination_file = super::platform::create_regular_file_at(
            &destination.file,
            &destination.path,
            planned.path(),
        )?;
        let remaining_allowance = plan.logical_bytes().checked_sub(copied_bytes).ok_or(
            IndexError::CurrentRepublishByteLimit {
                actual: copied_bytes,
                maximum: plan.logical_bytes(),
            },
        )?;
        let (copied, source_digest) = copy_with_digest(
            &mut source_file,
            &mut destination_file,
            source_before.identity.length(),
            remaining_allowance,
        )?;
        if source_digest != expected_sha256 {
            return Err(IndexError::ChecksumMismatch);
        }
        destination_file.flush()?;
        destination_file.set_permissions(candidate_permissions(planned.permissions()))?;
        destination_file.sync_all()?;
        if copied != source_before.identity.length() {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "copy byte count does not match authenticated source",
            ));
        }
        copied_bytes = copied_bytes
            .checked_add(copied)
            .ok_or(IndexError::CountOverflow)?;
        if copied_bytes > MAX_REPUBLISH_CLONE_BYTES || copied_bytes > plan.logical_bytes() {
            return Err(IndexError::CurrentRepublishByteLimit {
                actual: copied_bytes,
                maximum: plan.logical_bytes().min(MAX_REPUBLISH_CLONE_BYTES),
            });
        }

        let source_after = recapture_authenticated_artifact(
            root,
            source_path,
            planned.path(),
            &source_file,
            Some(predecessor_pointer),
        )?;
        if source_after != expected_artifact {
            return if expected_artifact.same_payload_identity_changed(&source_after) {
                Err(IndexError::ConcurrentGenerationChange)
            } else {
                Err(IndexError::ChecksumMismatch)
            };
        }
        let destination_opened = open_bound_file(destination, planned.path())?;
        if destination_opened.identity.bytes != planned.identity().bytes
            || destination_opened.identity.permissions
                != candidate_permission_identity(planned.identity().permissions)
        {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "copied file metadata does not match authenticated source",
            ));
        }
        let destination_artifact =
            capture_artifact_identity(root, &destination.path, planned.path(), None)?;
        physical_proof.insert(PhysicalFileDigest {
            artifact: destination_artifact,
            sha256: expected_sha256,
        });
        metrics.retained_copied_files = metrics
            .retained_copied_files
            .checked_add(1)
            .ok_or(IndexError::CountOverflow)?;
        metrics.retained_copied_bytes = metrics
            .retained_copied_bytes
            .checked_add(copied)
            .ok_or(IndexError::CountOverflow)?;
        clone_checkpoint(PortableCloneStage::AfterCopy, planned.path())?;
        drop(destination_file);
    }
    admit_available_bytes(generations, writer_output_headroom, true)?;
    Ok(())
}

fn write_authenticated_plan_bytes(
    destination: &BoundDirectory,
    planned: &PlannedFile,
    bytes: &[u8],
) -> Result<u64> {
    let mut destination_file = super::platform::create_regular_file_at(
        &destination.file,
        &destination.path,
        planned.path(),
    )?;
    destination_file.write_all(bytes)?;
    destination_file.flush()?;
    destination_file.set_permissions(candidate_permissions(planned.permissions()))?;
    destination_file.sync_all()?;
    let copied = u64::try_from(bytes.len()).map_err(|_| IndexError::CountOverflow)?;
    let destination_opened = open_bound_file(destination, planned.path())?;
    if destination_opened.identity.bytes != copied
        || destination_opened.identity.permissions
            != candidate_permission_identity(planned.identity().permissions)
    {
        return Err(IndexError::CurrentRepublishSourceTopology(
            "plan byte count does not match copied control file",
        ));
    }
    Ok(copied)
}

#[cfg(windows)]
fn candidate_permissions(source: &Permissions) -> Permissions {
    let mut candidate = source.clone();
    candidate.set_readonly(false);
    candidate
}

#[cfg(not(windows))]
fn candidate_permissions(source: &Permissions) -> Permissions {
    source.clone()
}

#[cfg(windows)]
fn candidate_permission_identity(_source: PermissionIdentity) -> PermissionIdentity {
    false
}

#[cfg(not(windows))]
fn candidate_permission_identity(source: PermissionIdentity) -> PermissionIdentity {
    source
}

fn copy_with_digest<R: Read, W: Write>(
    source: &mut R,
    destination: &mut W,
    expected_bytes: u64,
    aggregate_allowance: u64,
) -> Result<(u64, [u8; 32])> {
    if expected_bytes > aggregate_allowance {
        return Err(IndexError::CurrentRepublishByteLimit {
            actual: expected_bytes,
            maximum: aggregate_allowance,
        });
    }
    let mut digest = Sha256::new();
    let mut copied = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    while copied < expected_bytes {
        let remaining = expected_bytes - copied;
        let read_limit = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| IndexError::CountOverflow)?;
        let read = source.read(&mut buffer[..read_limit])?;
        if read == 0 {
            return Err(IndexError::CurrentRepublishSourceTopology(
                "source file truncated while cloning",
            ));
        }
        digest.update(&buffer[..read]);
        destination.write_all(&buffer[..read])?;
        copied = copied
            .checked_add(read as u64)
            .ok_or(IndexError::CountOverflow)?;
    }
    let mut growth_probe = [0_u8; 1];
    if source.read(&mut growth_probe)? != 0 {
        return Err(IndexError::CurrentRepublishSourceTopology(
            "source file grew while cloning",
        ));
    }
    Ok((copied, digest.finalize().into()))
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;

    #[test]
    fn authenticated_growth_probe_never_writes_the_extra_byte() {
        let mut source = io::Cursor::new(b"abcde".to_vec());
        let mut destination = Vec::new();
        assert!(matches!(
            copy_with_digest(&mut source, &mut destination, 4, 4),
            Err(IndexError::CurrentRepublishSourceTopology(
                "source file grew while cloning"
            ))
        ));
        assert_eq!(destination, b"abcd");

        let mut source = io::Cursor::new(b"abcde".to_vec());
        let mut destination = Vec::new();
        assert!(matches!(
            copy_with_digest(&mut source, &mut destination, 5, 4),
            Err(IndexError::CurrentRepublishByteLimit {
                actual: 5,
                maximum: 4
            })
        ));
        assert!(destination.is_empty());
    }
}
