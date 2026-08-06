use std::{path::PathBuf, sync::Arc};

use ctx_history_core::{CertifiedSource, SourceKey};
use sha2::{Digest, Sha256};

use super::super::{
    observe_opened_file, revalidate_frozen_prefix, revalidate_frozen_prefix_sha256,
    JsonlFileObservation,
};
use super::{
    binding_digest, contract_error, JsonlFamilyAdapter, JsonlFamilyAppendMode, JsonlFamilyLeaf,
    FAMILY_POLICY_REVISION,
};
use crate::{
    common::io::{OpenedProviderSourceFile, ProviderSourceRoot},
    CaptureError, Result,
};

/// Task-local physical evidence for one optimized or generic JSONL leaf.
///
/// This is deliberately not serialized into a source frontier. It retains the
/// admitted route authority only until the generation's terminal callback and
/// lets the shared family enforce append-safe versus exact replacement rules.
const TERMINAL_CERTIFICATE_BINDING_DOMAIN: &[u8] =
    b"ctx.task-local-jsonl-terminal-certificate-v1\0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JsonlFamilyTerminalPrefixHash {
    Sha256,
    SharedJsonlDomain,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum JsonlFamilyTerminalPhysicalBinding {
    FrozenPrefix {
        prefix_length: u64,
        prefix_sha256: [u8; 32],
        hash_kind: JsonlFamilyTerminalPrefixHash,
    },
    ExactFile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JsonlFamilyTerminalLeafBinding {
    source: SourceKey,
    source_path: PathBuf,
    authority_root_path: PathBuf,
    authority_root_fingerprint: [u8; 32],
    authority_path: PathBuf,
    admitted: JsonlFileObservation,
    leaf_binding_sha256: [u8; 32],
    whole_record: bool,
    parser_revision: String,
    event_identity_revision: String,
    append_mode: JsonlFamilyAppendMode,
    family_policy_revision: &'static str,
    certificate_sha256: [u8; 32],
    certified_prefix_end: u64,
    certified_prefix_digest: [u8; 32],
    checkpoint_kind: Option<String>,
    checkpoint_sha256: Option<[u8; 32]>,
    physical: JsonlFamilyTerminalPhysicalBinding,
}

impl JsonlFamilyTerminalLeafBinding {
    fn new(
        adapter: &dyn JsonlFamilyAdapter,
        leaf: &JsonlFamilyLeaf,
        certificate: &CertifiedSource,
        physical: JsonlFamilyTerminalPhysicalBinding,
    ) -> Result<Self> {
        certificate.validate_contract().map_err(contract_error)?;
        leaf.source
            .validate_exact_descriptor(certificate.observation().source())
            .map_err(contract_error)?;
        if certificate.parser_revision() != adapter.parser_revision() {
            return Err(CaptureError::InvalidPayload(
                "JSONL terminal proof parser revision does not match its adapter".to_owned(),
            ));
        }
        let checkpoint_kind = certificate
            .frontier()
            .map(|frontier| frontier.checkpoint_kind().to_owned());
        let checkpoint_sha256 = certificate
            .frontier()
            .map(|frontier| {
                let encoded = serde_json::to_vec(frontier.checkpoint())?;
                Ok::<[u8; 32], CaptureError>(Sha256::digest(encoded).into())
            })
            .transpose()?;
        Ok(Self {
            source: leaf.source.clone(),
            source_path: leaf.source_path.clone(),
            authority_root_path: leaf.authority.named_path().to_path_buf(),
            authority_root_fingerprint: leaf.authority.authority_fingerprint(),
            authority_path: leaf.authority_path.clone(),
            admitted: leaf.observation.clone(),
            leaf_binding_sha256: binding_digest(leaf)?,
            whole_record: leaf.whole_record,
            parser_revision: adapter.parser_revision().to_owned(),
            event_identity_revision: adapter.event_identity_revision().to_owned(),
            append_mode: adapter.append_mode(),
            family_policy_revision: FAMILY_POLICY_REVISION,
            certificate_sha256: terminal_certificate_binding(certificate)?,
            certified_prefix_end: certificate.counts().certified_bytes,
            certified_prefix_digest: *certificate.content_digest(),
            checkpoint_kind,
            checkpoint_sha256,
            physical,
        })
    }

    fn validate_certificate(&self, certificate: &CertifiedSource) -> Result<()> {
        certificate.validate_contract().map_err(contract_error)?;
        self.source
            .validate_exact_descriptor(certificate.observation().source())
            .map_err(contract_error)?;
        if self.parser_revision != certificate.parser_revision()
            || self.certificate_sha256 != terminal_certificate_binding(certificate)?
            || self.certified_prefix_end != certificate.counts().certified_bytes
            || self.certified_prefix_digest != *certificate.content_digest()
        {
            return Err(CaptureError::InvalidPayload(
                "JSONL terminal proof does not match its certified source".to_owned(),
            ));
        }
        Ok(())
    }
}

fn terminal_certificate_binding(certificate: &CertifiedSource) -> Result<[u8; 32]> {
    let encoded = serde_json::to_vec(certificate)?;
    let mut digest = Sha256::new();
    digest.update(TERMINAL_CERTIFICATE_BINDING_DOMAIN);
    digest.update((encoded.len() as u64).to_be_bytes());
    digest.update(encoded);
    Ok(digest.finalize().into())
}

#[derive(Debug, Clone)]
pub(crate) enum JsonlFamilyTerminalProof {
    FrozenPrefix {
        binding: Option<JsonlFamilyTerminalLeafBinding>,
        source_path: PathBuf,
        authority_path: PathBuf,
        authority: Arc<ProviderSourceRoot>,
        admitted: JsonlFileObservation,
        prefix_length: u64,
        prefix_sha256: [u8; 32],
        hash_kind: JsonlFamilyTerminalPrefixHash,
    },
    ExactFile {
        binding: Option<JsonlFamilyTerminalLeafBinding>,
        source_path: PathBuf,
        authority_path: PathBuf,
        authority: Arc<ProviderSourceRoot>,
        observation: JsonlFileObservation,
    },
}

impl JsonlFamilyTerminalProof {
    pub(crate) fn frozen_prefix(
        adapter: &dyn JsonlFamilyAdapter,
        leaf: &JsonlFamilyLeaf,
        certificate: &CertifiedSource,
        prefix_length: u64,
        prefix_sha256: [u8; 32],
    ) -> Result<Self> {
        Self::frozen_prefix_with_hash(
            adapter,
            leaf,
            certificate,
            prefix_length,
            prefix_sha256,
            JsonlFamilyTerminalPrefixHash::Sha256,
        )
    }

    pub(super) fn frozen_shared_prefix(
        adapter: &dyn JsonlFamilyAdapter,
        leaf: &JsonlFamilyLeaf,
        certificate: &CertifiedSource,
        prefix_length: u64,
        prefix_sha256: [u8; 32],
    ) -> Result<Self> {
        Self::frozen_prefix_with_hash(
            adapter,
            leaf,
            certificate,
            prefix_length,
            prefix_sha256,
            JsonlFamilyTerminalPrefixHash::SharedJsonlDomain,
        )
    }

    fn frozen_prefix_with_hash(
        adapter: &dyn JsonlFamilyAdapter,
        leaf: &JsonlFamilyLeaf,
        certificate: &CertifiedSource,
        prefix_length: u64,
        prefix_sha256: [u8; 32],
        hash_kind: JsonlFamilyTerminalPrefixHash,
    ) -> Result<Self> {
        if prefix_length > leaf.observation.length {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let opened = leaf.authority.open_file(&leaf.authority_path)?;
        let current = observe_opened_file(&leaf.source_path, &opened)?;
        if !leaf.observation.admits_frozen_prefix_in(&current) {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        if current != leaf.observation {
            match hash_kind {
                JsonlFamilyTerminalPrefixHash::Sha256 => revalidate_frozen_prefix_sha256(
                    &leaf.source_path,
                    &opened,
                    &leaf.observation,
                    prefix_length,
                    prefix_sha256,
                )?,
                JsonlFamilyTerminalPrefixHash::SharedJsonlDomain => revalidate_frozen_prefix(
                    &leaf.source_path,
                    &opened,
                    &leaf.observation,
                    prefix_length,
                    prefix_sha256,
                )?,
            };
        }
        let physical = JsonlFamilyTerminalPhysicalBinding::FrozenPrefix {
            prefix_length,
            prefix_sha256,
            hash_kind,
        };
        let binding = JsonlFamilyTerminalLeafBinding::new(adapter, leaf, certificate, physical)?;
        Ok(Self::FrozenPrefix {
            binding: Some(binding),
            source_path: leaf.source_path.clone(),
            authority_path: leaf.authority_path.clone(),
            authority: Arc::clone(&leaf.authority),
            admitted: leaf.observation.clone(),
            prefix_length,
            prefix_sha256,
            hash_kind,
        })
    }

    pub(crate) fn exact_file(
        adapter: &dyn JsonlFamilyAdapter,
        leaf: &JsonlFamilyLeaf,
        certificate: &CertifiedSource,
    ) -> Result<Self> {
        let mut proof = Self::exact_path(
            leaf.source_path.clone(),
            Arc::clone(&leaf.authority),
            leaf.authority_path.clone(),
        )?;
        let Self::ExactFile {
            binding,
            observation,
            ..
        } = &mut proof
        else {
            return Err(CaptureError::SystemInvariant(
                "exact JSONL proof constructor returned the wrong proof kind",
            ));
        };
        if observation != &leaf.observation {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        *binding = Some(JsonlFamilyTerminalLeafBinding::new(
            adapter,
            leaf,
            certificate,
            JsonlFamilyTerminalPhysicalBinding::ExactFile,
        )?);
        Ok(proof)
    }

    pub(crate) fn exact_path(
        source_path: PathBuf,
        authority: Arc<ProviderSourceRoot>,
        authority_path: PathBuf,
    ) -> Result<Self> {
        let opened = authority.open_file(&authority_path)?;
        Self::exact_opened_path(source_path, authority, authority_path, &opened)
    }

    pub(crate) fn exact_opened_path(
        source_path: PathBuf,
        authority: Arc<ProviderSourceRoot>,
        authority_path: PathBuf,
        opened: &OpenedProviderSourceFile,
    ) -> Result<Self> {
        let observation = observe_opened_file(&source_path, opened)?;
        opened.revalidate_leaf()?;
        Ok(Self::ExactFile {
            binding: None,
            source_path,
            authority_path,
            authority,
            observation,
        })
    }

    fn physical_binding(&self) -> JsonlFamilyTerminalPhysicalBinding {
        match self {
            Self::FrozenPrefix {
                prefix_length,
                prefix_sha256,
                hash_kind,
                ..
            } => JsonlFamilyTerminalPhysicalBinding::FrozenPrefix {
                prefix_length: *prefix_length,
                prefix_sha256: *prefix_sha256,
                hash_kind: *hash_kind,
            },
            Self::ExactFile { .. } => JsonlFamilyTerminalPhysicalBinding::ExactFile,
        }
    }

    fn binding(&self) -> Option<&JsonlFamilyTerminalLeafBinding> {
        match self {
            Self::FrozenPrefix { binding, .. } | Self::ExactFile { binding, .. } => {
                binding.as_ref()
            }
        }
    }

    pub(crate) fn validate_for(
        &self,
        adapter: &dyn JsonlFamilyAdapter,
        leaf: &JsonlFamilyLeaf,
        certificate: &CertifiedSource,
    ) -> Result<()> {
        let expected = JsonlFamilyTerminalLeafBinding::new(
            adapter,
            leaf,
            certificate,
            self.physical_binding(),
        )?;
        if self.binding() != Some(&expected) || !self.route_matches_binding(&expected) {
            return Err(CaptureError::InvalidPayload(
                "JSONL terminal proof is bound to another leaf or certificate".to_owned(),
            ));
        }
        Ok(())
    }

    pub(crate) fn revalidate_for(&self, certificate: &CertifiedSource) -> Result<()> {
        let binding = self.binding().ok_or_else(|| {
            CaptureError::InvalidPayload(
                "JSONL leaf terminal proof has no leaf/certificate binding".to_owned(),
            )
        })?;
        binding.validate_certificate(certificate)?;
        if binding.physical != self.physical_binding() || !self.route_matches_binding(binding) {
            return Err(CaptureError::InvalidPayload(
                "JSONL terminal proof binding changed before revalidation".to_owned(),
            ));
        }
        self.revalidate_physical()
    }

    pub(crate) fn revalidate_dependency(&self) -> Result<()> {
        if self.binding().is_some() {
            return Err(CaptureError::InvalidPayload(
                "JSONL leaf proof cannot be reused as an exact dependency".to_owned(),
            ));
        }
        self.revalidate_physical()
    }

    fn route_matches_binding(&self, binding: &JsonlFamilyTerminalLeafBinding) -> bool {
        match self {
            Self::FrozenPrefix {
                source_path,
                authority_path,
                authority,
                admitted,
                ..
            } => {
                source_path == &binding.source_path
                    && authority_path == &binding.authority_path
                    && authority.named_path() == binding.authority_root_path
                    && authority.authority_fingerprint() == binding.authority_root_fingerprint
                    && admitted == &binding.admitted
            }
            Self::ExactFile {
                source_path,
                authority_path,
                authority,
                observation,
                ..
            } => {
                source_path == &binding.source_path
                    && authority_path == &binding.authority_path
                    && authority.named_path() == binding.authority_root_path
                    && authority.authority_fingerprint() == binding.authority_root_fingerprint
                    && observation == &binding.admitted
            }
        }
    }

    fn revalidate_physical(&self) -> Result<()> {
        match self {
            Self::FrozenPrefix {
                binding: _,
                source_path,
                authority_path,
                authority,
                admitted,
                prefix_length,
                prefix_sha256,
                hash_kind,
            } => {
                let opened = authority.open_file(authority_path)?;
                match hash_kind {
                    JsonlFamilyTerminalPrefixHash::Sha256 => revalidate_frozen_prefix_sha256(
                        source_path,
                        &opened,
                        admitted,
                        *prefix_length,
                        *prefix_sha256,
                    )?,
                    JsonlFamilyTerminalPrefixHash::SharedJsonlDomain => revalidate_frozen_prefix(
                        source_path,
                        &opened,
                        admitted,
                        *prefix_length,
                        *prefix_sha256,
                    )?,
                };
            }
            Self::ExactFile {
                binding: _,
                source_path,
                authority_path,
                authority,
                observation,
            } => {
                let opened = authority.open_file(authority_path)?;
                if observe_opened_file(source_path, &opened)? != *observation {
                    return Err(CaptureError::SourceChangedDuringCapture);
                }
                opened.revalidate_leaf()?;
            }
        }
        Ok(())
    }
}
