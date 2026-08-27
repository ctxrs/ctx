use std::{path::PathBuf, sync::Arc};

use ctx_history_core::{CertifiedSource, SourceKey};
use sha2::{Digest, Sha256};

use super::super::{
    authenticate_frozen_prefix, authenticate_frozen_prefix_sha256, observe_opened_file,
    observe_opened_file_allow_append, revalidate_frozen_prefix, revalidate_frozen_prefix_sha256,
    JsonlFileObservation,
};
use super::super::{
    JsonlFamilyError, JsonlFamilyRuntime, JsonlResult, JsonlRuntimeError, OpenedProviderSourceFile,
    ProviderSourceRoot,
};
use super::{
    binding_digest, contract_error, FamilyCheckpoint, JsonlFamilyAdapter, JsonlFamilyAppendMode,
    JsonlFamilyAppendTrustContract, JsonlFamilyLeaf, FAMILY_POLICY_REVISION,
};

/// Task-local physical evidence for one optimized or generic JSONL leaf.
///
/// This is deliberately not serialized into a source frontier. It retains the
/// admitted route authority only until the generation's terminal callback and
/// lets the shared family enforce append-safe versus exact replacement rules.
const TERMINAL_CERTIFICATE_BINDING_DOMAIN: &[u8] =
    b"ctx.task-local-jsonl-terminal-certificate-v1\0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonlFamilyTerminalPrefixHash {
    Sha256,
    SharedJsonlDomain,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum JsonlFamilyTerminalPhysicalBinding {
    AppendOnlySameObjectV1 {
        certified_prefix_end: u64,
        admitted_eof_sha256: Option<[u8; 32]>,
    },
    FrozenPrefix {
        prefix_length: u64,
        prefix_sha256: [u8; 32],
        hash_kind: JsonlFamilyTerminalPrefixHash,
        force_authentication: bool,
    },
    ExactFile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[doc(hidden)]
pub struct JsonlFamilyTerminalLeafBinding {
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
    fn new<R: JsonlFamilyRuntime>(
        adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
        leaf: &JsonlFamilyLeaf<JsonlRuntimeError<R>>,
        certificate: &CertifiedSource,
        physical: JsonlFamilyTerminalPhysicalBinding,
    ) -> JsonlResult<Self, JsonlRuntimeError<R>> {
        if matches!(
            &physical,
            JsonlFamilyTerminalPhysicalBinding::AppendOnlySameObjectV1 { .. }
        ) && (adapter.append_trust_contract()
            != JsonlFamilyAppendTrustContract::AppendOnlySameObjectV1
            || !adapter.allows_direct_append_for_leaf(leaf)
            || leaf.whole_record
            || !adapter.append_mode().certified_suffix())
        {
            return Err(JsonlRuntimeError::<R>::invalid_payload(
                "JSONL append-only terminal proof is not authorized for this leaf".to_owned(),
            ));
        }
        certificate
            .validate_contract()
            .map_err(contract_error::<JsonlRuntimeError<R>>)?;
        leaf.source
            .validate_exact_descriptor(certificate.observation().source())
            .map_err(contract_error::<JsonlRuntimeError<R>>)?;
        if certificate.parser_revision() != adapter.parser_revision() {
            return Err(JsonlRuntimeError::<R>::invalid_payload(
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
                Ok::<[u8; 32], JsonlRuntimeError<R>>(Sha256::digest(encoded).into())
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
            certificate_sha256: terminal_certificate_binding::<JsonlRuntimeError<R>>(certificate)?,
            certified_prefix_end: certificate.counts().certified_bytes,
            certified_prefix_digest: *certificate.content_digest(),
            checkpoint_kind,
            checkpoint_sha256,
            physical,
        })
    }

    fn validate_certificate<E: JsonlFamilyError>(
        &self,
        certificate: &CertifiedSource,
    ) -> JsonlResult<(), E> {
        certificate
            .validate_contract()
            .map_err(contract_error::<E>)?;
        self.source
            .validate_exact_descriptor(certificate.observation().source())
            .map_err(contract_error::<E>)?;
        if self.parser_revision != certificate.parser_revision()
            || self.certificate_sha256 != terminal_certificate_binding::<E>(certificate)?
            || self.certified_prefix_end != certificate.counts().certified_bytes
            || self.certified_prefix_digest != *certificate.content_digest()
        {
            return Err(E::invalid_payload(
                "JSONL terminal proof does not match its certified source".to_owned(),
            ));
        }
        Ok(())
    }
}

fn terminal_certificate_binding<E: JsonlFamilyError>(
    certificate: &CertifiedSource,
) -> JsonlResult<[u8; 32], E> {
    let encoded = serde_json::to_vec(certificate)?;
    let mut digest = Sha256::new();
    digest.update(TERMINAL_CERTIFICATE_BINDING_DOMAIN);
    digest.update((encoded.len() as u64).to_be_bytes());
    digest.update(encoded);
    Ok(digest.finalize().into())
}

#[derive(Debug)]
pub enum JsonlFamilyTerminalProof<E: JsonlFamilyError> {
    AppendOnlySameObjectV1 {
        binding: Option<JsonlFamilyTerminalLeafBinding>,
        source_path: PathBuf,
        authority_path: PathBuf,
        authority: Arc<ProviderSourceRoot<E>>,
        admitted: JsonlFileObservation,
        certified_prefix_end: u64,
        admitted_eof_sha256: Option<[u8; 32]>,
    },
    FrozenPrefix {
        binding: Option<JsonlFamilyTerminalLeafBinding>,
        source_path: PathBuf,
        authority_path: PathBuf,
        authority: Arc<ProviderSourceRoot<E>>,
        admitted: JsonlFileObservation,
        prefix_length: u64,
        prefix_sha256: [u8; 32],
        hash_kind: JsonlFamilyTerminalPrefixHash,
        force_authentication: bool,
    },
    ExactFile {
        binding: Option<JsonlFamilyTerminalLeafBinding>,
        source_path: PathBuf,
        authority_path: PathBuf,
        authority: Arc<ProviderSourceRoot<E>>,
        observation: JsonlFileObservation,
    },
}

impl<E: JsonlFamilyError> Clone for JsonlFamilyTerminalProof<E> {
    fn clone(&self) -> Self {
        match self {
            Self::AppendOnlySameObjectV1 {
                binding,
                source_path,
                authority_path,
                authority,
                admitted,
                certified_prefix_end,
                admitted_eof_sha256,
            } => Self::AppendOnlySameObjectV1 {
                binding: binding.clone(),
                source_path: source_path.clone(),
                authority_path: authority_path.clone(),
                authority: Arc::clone(authority),
                admitted: admitted.clone(),
                certified_prefix_end: *certified_prefix_end,
                admitted_eof_sha256: *admitted_eof_sha256,
            },
            Self::FrozenPrefix {
                binding,
                source_path,
                authority_path,
                authority,
                admitted,
                prefix_length,
                prefix_sha256,
                hash_kind,
                force_authentication,
            } => Self::FrozenPrefix {
                binding: binding.clone(),
                source_path: source_path.clone(),
                authority_path: authority_path.clone(),
                authority: Arc::clone(authority),
                admitted: admitted.clone(),
                prefix_length: *prefix_length,
                prefix_sha256: *prefix_sha256,
                hash_kind: *hash_kind,
                force_authentication: *force_authentication,
            },
            Self::ExactFile {
                binding,
                source_path,
                authority_path,
                authority,
                observation,
            } => Self::ExactFile {
                binding: binding.clone(),
                source_path: source_path.clone(),
                authority_path: authority_path.clone(),
                authority: Arc::clone(authority),
                observation: observation.clone(),
            },
        }
    }
}

impl<E: JsonlFamilyError> JsonlFamilyTerminalProof<E> {
    /// Certifies a parsed prefix using the provider's explicit immutable-prefix
    /// contract. Same-object growth is accepted without hashing retained bytes;
    /// bytes beyond `certified_prefix_end` remain dirty for a successor refresh.
    pub(super) fn append_only_same_object_v1<R: JsonlFamilyRuntime<Error = E>>(
        adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
        leaf: &JsonlFamilyLeaf<E>,
        certificate: &CertifiedSource,
        certified_prefix_end: u64,
        admitted_eof_sha256: Option<[u8; 32]>,
    ) -> JsonlResult<Self, E> {
        let proof = Self::bind_admitted_append_only_same_object_v1(
            adapter,
            leaf,
            certificate,
            certified_prefix_end,
            admitted_eof_sha256,
        )?;
        let opened = leaf.authority.open_file(&leaf.authority_path)?;
        let current = observe_opened_file_allow_append(&leaf.source_path, &opened)?;
        if !leaf.observation.admits_frozen_prefix_in(&current) {
            return Err(E::source_changed());
        }
        if leaf.observation.differs_only_by_change_identity(&current) {
            Self::authenticate_append_only_dirty_hint(&proof, &opened)?;
        }
        Ok(proof)
    }

    fn bind_admitted_append_only_same_object_v1<R: JsonlFamilyRuntime<Error = E>>(
        adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
        leaf: &JsonlFamilyLeaf<E>,
        certificate: &CertifiedSource,
        certified_prefix_end: u64,
        admitted_eof_sha256: Option<[u8; 32]>,
    ) -> JsonlResult<Self, E> {
        if certified_prefix_end > leaf.observation.length()
            || certified_prefix_end != certificate.counts().certified_bytes
        {
            return Err(E::source_changed());
        }
        let physical = JsonlFamilyTerminalPhysicalBinding::AppendOnlySameObjectV1 {
            certified_prefix_end,
            admitted_eof_sha256,
        };
        let binding = JsonlFamilyTerminalLeafBinding::new(adapter, leaf, certificate, physical)?;
        Ok(Self::AppendOnlySameObjectV1 {
            binding: Some(binding),
            source_path: leaf.source_path.clone(),
            authority_path: leaf.authority_path.clone(),
            authority: Arc::clone(&leaf.authority),
            admitted: leaf.observation.clone(),
            certified_prefix_end,
            admitted_eof_sha256,
        })
    }

    pub fn frozen_prefix<R: JsonlFamilyRuntime<Error = E>>(
        adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
        leaf: &JsonlFamilyLeaf<E>,
        certificate: &CertifiedSource,
        prefix_length: u64,
        prefix_sha256: [u8; 32],
    ) -> JsonlResult<Self, E> {
        Self::frozen_prefix_with_hash(
            adapter,
            leaf,
            certificate,
            prefix_length,
            prefix_sha256,
            JsonlFamilyTerminalPrefixHash::Sha256,
            false,
        )
    }

    pub(super) fn frozen_shared_prefix<R: JsonlFamilyRuntime<Error = E>>(
        adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
        leaf: &JsonlFamilyLeaf<E>,
        certificate: &CertifiedSource,
        prefix_length: u64,
        prefix_sha256: [u8; 32],
    ) -> JsonlResult<Self, E> {
        Self::frozen_prefix_with_hash(
            adapter,
            leaf,
            certificate,
            prefix_length,
            prefix_sha256,
            JsonlFamilyTerminalPrefixHash::SharedJsonlDomain,
            false,
        )
    }

    pub(super) fn forced_frozen_prefix_with_hash<R: JsonlFamilyRuntime<Error = E>>(
        adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
        leaf: &JsonlFamilyLeaf<E>,
        certificate: &CertifiedSource,
        prefix_length: u64,
        prefix_sha256: [u8; 32],
        hash_kind: JsonlFamilyTerminalPrefixHash,
    ) -> JsonlResult<Self, E> {
        Self::frozen_prefix_with_hash(
            adapter,
            leaf,
            certificate,
            prefix_length,
            prefix_sha256,
            hash_kind,
            true,
        )
    }

    fn frozen_prefix_with_hash<R: JsonlFamilyRuntime<Error = E>>(
        adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
        leaf: &JsonlFamilyLeaf<E>,
        certificate: &CertifiedSource,
        prefix_length: u64,
        prefix_sha256: [u8; 32],
        hash_kind: JsonlFamilyTerminalPrefixHash,
        force_authentication: bool,
    ) -> JsonlResult<Self, E> {
        if prefix_length > leaf.observation.length() {
            return Err(E::source_changed());
        }
        let opened = leaf.authority.open_file(&leaf.authority_path)?;
        let current = observe_opened_file_allow_append(&leaf.source_path, &opened)?;
        if !leaf.observation.admits_frozen_prefix_in(&current) {
            return Err(E::source_changed());
        }
        if current != leaf.observation || force_authentication {
            Self::authenticate_prefix(
                &leaf.source_path,
                &opened,
                &leaf.observation,
                prefix_length,
                prefix_sha256,
                hash_kind,
                force_authentication,
            )?;
        }
        Self::bind_admitted_frozen_prefix(
            adapter,
            leaf,
            certificate,
            prefix_length,
            prefix_sha256,
            hash_kind,
            force_authentication,
        )
    }

    fn bind_admitted_frozen_prefix<R: JsonlFamilyRuntime<Error = E>>(
        adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
        leaf: &JsonlFamilyLeaf<E>,
        certificate: &CertifiedSource,
        prefix_length: u64,
        prefix_sha256: [u8; 32],
        hash_kind: JsonlFamilyTerminalPrefixHash,
        force_authentication: bool,
    ) -> JsonlResult<Self, E> {
        if prefix_length > leaf.observation.length() {
            return Err(E::source_changed());
        }
        let physical = JsonlFamilyTerminalPhysicalBinding::FrozenPrefix {
            prefix_length,
            prefix_sha256,
            hash_kind,
            force_authentication,
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
            force_authentication,
        })
    }

    pub fn exact_file<R: JsonlFamilyRuntime<Error = E>>(
        adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
        leaf: &JsonlFamilyLeaf<E>,
        certificate: &CertifiedSource,
    ) -> JsonlResult<Self, E> {
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
            return Err(E::system_invariant(
                "exact JSONL proof constructor returned the wrong proof kind",
            ));
        };
        if observation != &leaf.observation {
            return Err(E::source_changed());
        }
        *binding = Some(JsonlFamilyTerminalLeafBinding::new(
            adapter,
            leaf,
            certificate,
            JsonlFamilyTerminalPhysicalBinding::ExactFile,
        )?);
        Ok(proof)
    }

    fn bind_admitted_exact_file<R: JsonlFamilyRuntime<Error = E>>(
        adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
        leaf: &JsonlFamilyLeaf<E>,
        certificate: &CertifiedSource,
    ) -> JsonlResult<Self, E> {
        let binding = JsonlFamilyTerminalLeafBinding::new(
            adapter,
            leaf,
            certificate,
            JsonlFamilyTerminalPhysicalBinding::ExactFile,
        )?;
        Ok(Self::ExactFile {
            binding: Some(binding),
            source_path: leaf.source_path.clone(),
            authority_path: leaf.authority_path.clone(),
            authority: Arc::clone(&leaf.authority),
            observation: leaf.observation.clone(),
        })
    }

    /// Builds terminal evidence from a source whose admitted observation and
    /// persisted checkpoint are unchanged. Physical evidence remains bound
    /// and is revalidated by the normal terminal publication path.
    pub(super) fn unchanged<R: JsonlFamilyRuntime<Error = E>>(
        adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
        leaf: &JsonlFamilyLeaf<E>,
        certificate: &CertifiedSource,
        checkpoint: &FamilyCheckpoint,
        authenticated_change_time_hint: bool,
    ) -> JsonlResult<Self, E> {
        let retained = checkpoint.physical.source_observation();
        let change_time_hint = retained.differs_only_by_change_identity(&leaf.observation);
        if authenticated_change_time_hint && !change_time_hint {
            return Err(E::system_invariant(
                "JSONL resident observation does not advance a change-time-only hint",
            ));
        }
        let force_authentication = change_time_hint && !authenticated_change_time_hint;
        if leaf.logical_eof().is_some() {
            let admitted_eof_sha256 = checkpoint
                .exact_admitted_eof_sha256()
                .ok_or_else(E::source_changed)?;
            return Self::bind_admitted_frozen_prefix(
                adapter,
                leaf,
                certificate,
                checkpoint.physical.admitted_length(),
                admitted_eof_sha256,
                JsonlFamilyTerminalPrefixHash::Sha256,
                true,
            );
        }
        if force_authentication {
            if let Some(admitted_eof_sha256) = checkpoint.exact_admitted_eof_sha256() {
                return Self::bind_admitted_frozen_prefix(
                    adapter,
                    leaf,
                    certificate,
                    retained.length(),
                    admitted_eof_sha256,
                    JsonlFamilyTerminalPrefixHash::Sha256,
                    true,
                );
            }
            if checkpoint.physical.complete_prefix_end() == retained.length() {
                return Self::bind_admitted_frozen_prefix(
                    adapter,
                    leaf,
                    certificate,
                    retained.length(),
                    *checkpoint.physical.complete_prefix_sha256(),
                    JsonlFamilyTerminalPrefixHash::SharedJsonlDomain,
                    true,
                );
            }
            return Err(E::source_changed());
        }
        if leaf.whole_record || !adapter.append_mode().certified_suffix() {
            if let Some(admitted_eof_sha256) = checkpoint.exact_admitted_eof_sha256() {
                return Self::bind_admitted_frozen_prefix(
                    adapter,
                    leaf,
                    certificate,
                    retained.length(),
                    admitted_eof_sha256,
                    JsonlFamilyTerminalPrefixHash::Sha256,
                    false,
                );
            }
            if checkpoint.physical.complete_prefix_end() == retained.length() {
                return Self::bind_admitted_frozen_prefix(
                    adapter,
                    leaf,
                    certificate,
                    retained.length(),
                    *checkpoint.physical.complete_prefix_sha256(),
                    JsonlFamilyTerminalPrefixHash::SharedJsonlDomain,
                    false,
                );
            }
            return Self::bind_admitted_exact_file(adapter, leaf, certificate);
        }
        if adapter.append_trust_contract() == JsonlFamilyAppendTrustContract::AppendOnlySameObjectV1
            && adapter.allows_direct_append_for_leaf(leaf)
        {
            return Self::bind_admitted_append_only_same_object_v1(
                adapter,
                leaf,
                certificate,
                checkpoint.physical.complete_prefix_end(),
                checkpoint.exact_admitted_eof_sha256(),
            );
        }
        let (prefix_length, prefix_sha256, hash_kind) =
            if let Some(admitted_eof_sha256) = checkpoint.exact_admitted_eof_sha256() {
                (
                    checkpoint.physical.source_observation().length(),
                    admitted_eof_sha256,
                    JsonlFamilyTerminalPrefixHash::Sha256,
                )
            } else {
                (
                    checkpoint.physical.complete_prefix_end(),
                    *checkpoint.physical.complete_prefix_sha256(),
                    JsonlFamilyTerminalPrefixHash::SharedJsonlDomain,
                )
            };
        Self::bind_admitted_frozen_prefix(
            adapter,
            leaf,
            certificate,
            prefix_length,
            prefix_sha256,
            hash_kind,
            false,
        )
    }

    pub fn exact_path(
        source_path: PathBuf,
        authority: Arc<ProviderSourceRoot<E>>,
        authority_path: PathBuf,
    ) -> JsonlResult<Self, E> {
        let opened = authority.open_file(&authority_path)?;
        Self::exact_opened_path(source_path, authority, authority_path, &opened)
    }

    /// Binds an exact terminal proof only when the reopened member is still
    /// the observation admitted by discovery. Rejected members need this
    /// constructor because they have no scan certificate to carry that fence.
    pub fn exact_admitted_path(
        source_path: PathBuf,
        authority: Arc<ProviderSourceRoot<E>>,
        authority_path: PathBuf,
        admitted: &JsonlFileObservation,
    ) -> JsonlResult<Self, E> {
        let opened = authority.open_file(&authority_path)?;
        let current = observe_opened_file(&source_path, &opened)?;
        if &current != admitted {
            return Err(E::source_changed());
        }
        opened.revalidate_leaf()?;
        Ok(Self::ExactFile {
            binding: None,
            source_path,
            authority_path,
            authority,
            observation: current,
        })
    }

    pub fn exact_opened_path(
        source_path: PathBuf,
        authority: Arc<ProviderSourceRoot<E>>,
        authority_path: PathBuf,
        opened: &OpenedProviderSourceFile<E>,
    ) -> JsonlResult<Self, E> {
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
            Self::AppendOnlySameObjectV1 {
                certified_prefix_end,
                admitted_eof_sha256,
                ..
            } => JsonlFamilyTerminalPhysicalBinding::AppendOnlySameObjectV1 {
                certified_prefix_end: *certified_prefix_end,
                admitted_eof_sha256: *admitted_eof_sha256,
            },
            Self::FrozenPrefix {
                prefix_length,
                prefix_sha256,
                hash_kind,
                force_authentication,
                ..
            } => JsonlFamilyTerminalPhysicalBinding::FrozenPrefix {
                prefix_length: *prefix_length,
                prefix_sha256: *prefix_sha256,
                hash_kind: *hash_kind,
                force_authentication: *force_authentication,
            },
            Self::ExactFile { .. } => JsonlFamilyTerminalPhysicalBinding::ExactFile,
        }
    }

    fn binding(&self) -> Option<&JsonlFamilyTerminalLeafBinding> {
        match self {
            Self::AppendOnlySameObjectV1 { binding, .. }
            | Self::FrozenPrefix { binding, .. }
            | Self::ExactFile { binding, .. } => binding.as_ref(),
        }
    }

    pub(crate) fn validate_for<R: JsonlFamilyRuntime<Error = E>>(
        &self,
        adapter: &dyn JsonlFamilyAdapter<Runtime = R>,
        leaf: &JsonlFamilyLeaf<E>,
        certificate: &CertifiedSource,
    ) -> JsonlResult<(), E> {
        let expected = JsonlFamilyTerminalLeafBinding::new(
            adapter,
            leaf,
            certificate,
            self.physical_binding(),
        )?;
        if self.binding() != Some(&expected) || !self.route_matches_binding(&expected) {
            return Err(E::invalid_payload(
                "JSONL terminal proof is bound to another leaf or certificate".to_owned(),
            ));
        }
        Ok(())
    }

    pub(crate) fn revalidate_for(
        &self,
        certificate: &CertifiedSource,
    ) -> JsonlResult<JsonlFileObservation, E> {
        let binding = self.binding().ok_or_else(|| {
            E::invalid_payload(
                "JSONL leaf terminal proof has no leaf/certificate binding".to_owned(),
            )
        })?;
        binding.validate_certificate::<E>(certificate)?;
        if binding.physical != self.physical_binding() || !self.route_matches_binding(binding) {
            return Err(E::invalid_payload(
                "JSONL terminal proof binding changed before revalidation".to_owned(),
            ));
        }
        self.revalidate_physical()
    }

    pub fn revalidate_dependency(&self) -> JsonlResult<(), E> {
        if self.binding().is_some() {
            return Err(E::invalid_payload(
                "JSONL leaf proof cannot be reused as an exact dependency".to_owned(),
            ));
        }
        self.revalidate_physical().map(drop)
    }

    fn route_matches_binding(&self, binding: &JsonlFamilyTerminalLeafBinding) -> bool {
        match self {
            Self::AppendOnlySameObjectV1 {
                source_path,
                authority_path,
                authority,
                admitted,
                ..
            }
            | Self::FrozenPrefix {
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

    fn revalidate_physical(&self) -> JsonlResult<JsonlFileObservation, E> {
        match self {
            Self::AppendOnlySameObjectV1 {
                binding: _,
                source_path,
                authority_path,
                authority,
                admitted,
                certified_prefix_end,
                admitted_eof_sha256: _,
            } => {
                if *certified_prefix_end > admitted.length() {
                    return Err(E::source_changed());
                }
                let opened = authority.open_file(authority_path)?;
                let current = observe_opened_file_allow_append(source_path, &opened)?;
                if !admitted.admits_frozen_prefix_in(&current) {
                    return Err(E::source_changed());
                }
                if admitted.differs_only_by_change_identity(&current) {
                    Self::authenticate_append_only_dirty_hint(self, &opened)
                } else {
                    Ok(current)
                }
            }
            Self::FrozenPrefix {
                binding: _,
                source_path,
                authority_path,
                authority,
                admitted,
                prefix_length,
                prefix_sha256,
                hash_kind,
                force_authentication,
            } => {
                let opened = authority.open_file(authority_path)?;
                Self::authenticate_prefix(
                    source_path,
                    &opened,
                    admitted,
                    *prefix_length,
                    *prefix_sha256,
                    *hash_kind,
                    *force_authentication,
                )
            }
            Self::ExactFile {
                binding: _,
                source_path,
                authority_path,
                authority,
                observation,
            } => {
                let opened = authority.open_file(authority_path)?;
                let current = observe_opened_file(source_path, &opened)?;
                if current != *observation {
                    return Err(E::source_changed());
                }
                opened.revalidate_leaf()?;
                Ok(current)
            }
        }
    }

    fn authenticate_prefix(
        source_path: &std::path::Path,
        opened: &OpenedProviderSourceFile<E>,
        admitted: &JsonlFileObservation,
        prefix_length: u64,
        prefix_sha256: [u8; 32],
        hash_kind: JsonlFamilyTerminalPrefixHash,
        force_authentication: bool,
    ) -> JsonlResult<JsonlFileObservation, E> {
        match (hash_kind, force_authentication) {
            (JsonlFamilyTerminalPrefixHash::Sha256, true) => authenticate_frozen_prefix_sha256(
                source_path,
                opened,
                admitted,
                prefix_length,
                prefix_sha256,
            ),
            (JsonlFamilyTerminalPrefixHash::Sha256, false) => revalidate_frozen_prefix_sha256(
                source_path,
                opened,
                admitted,
                prefix_length,
                prefix_sha256,
            ),
            (JsonlFamilyTerminalPrefixHash::SharedJsonlDomain, true) => authenticate_frozen_prefix(
                source_path,
                opened,
                admitted,
                prefix_length,
                prefix_sha256,
            ),
            (JsonlFamilyTerminalPrefixHash::SharedJsonlDomain, false) => revalidate_frozen_prefix(
                source_path,
                opened,
                admitted,
                prefix_length,
                prefix_sha256,
            ),
        }
    }

    fn authenticate_append_only_dirty_hint(
        proof: &Self,
        opened: &OpenedProviderSourceFile<E>,
    ) -> JsonlResult<JsonlFileObservation, E> {
        let Self::AppendOnlySameObjectV1 {
            binding,
            source_path,
            admitted,
            certified_prefix_end,
            admitted_eof_sha256,
            ..
        } = proof
        else {
            return Err(E::system_invariant(
                "JSONL append-only dirty-hint authentication received another proof kind",
            ));
        };
        if let Some(admitted_eof_sha256) = admitted_eof_sha256 {
            return authenticate_frozen_prefix_sha256(
                source_path,
                opened,
                admitted,
                admitted.length(),
                *admitted_eof_sha256,
            );
        }
        let binding = binding.as_ref().ok_or_else(|| {
            E::invalid_payload("JSONL append-only terminal proof lost its binding".to_owned())
        })?;
        if *certified_prefix_end != admitted.length() {
            return Err(E::source_changed());
        }
        authenticate_frozen_prefix(
            source_path,
            opened,
            admitted,
            *certified_prefix_end,
            binding.certified_prefix_digest,
        )
    }
}
