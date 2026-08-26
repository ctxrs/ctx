use super::*;

#[derive(Debug)]
pub struct JsonlFamilyInventory<E: JsonlFamilyError> {
    pub(super) provider: CaptureProvider,
    pub(super) root: PathBuf,
    pub(super) root_missing: bool,
    pub(super) observation: SourceInventoryObservation,
    pub(super) authorities: Vec<Arc<ProviderSourceRoot<E>>>,
    pub(super) members: Vec<JsonlFamilyInventoryMember<E>>,
    pub(super) exact_dependencies: Vec<JsonlFamilyTerminalProof<E>>,
}

impl<E: JsonlFamilyError> Clone for JsonlFamilyInventory<E> {
    fn clone(&self) -> Self {
        Self {
            provider: self.provider,
            root: self.root.clone(),
            root_missing: self.root_missing,
            observation: self.observation.clone(),
            authorities: self.authorities.clone(),
            members: self.members.clone(),
            exact_dependencies: self.exact_dependencies.clone(),
        }
    }
}

impl<E: JsonlFamilyError> JsonlFamilyInventory<E> {
    pub fn present(
        provider: CaptureProvider,
        root: &Path,
        authority: Arc<ProviderSourceRoot<E>>,
        leaves: Vec<JsonlFamilyLeaf<E>>,
    ) -> JsonlResult<Self, E> {
        Self::present_with_rejected(provider, root, authority, leaves, Vec::new())
    }

    pub fn present_with_rejected(
        provider: CaptureProvider,
        root: &Path,
        authority: Arc<ProviderSourceRoot<E>>,
        leaves: Vec<JsonlFamilyLeaf<E>>,
        rejected_leaves: Vec<JsonlFamilyRejectedLeaf>,
    ) -> JsonlResult<Self, E> {
        Self::present_multi_with_rejected(provider, root, vec![authority], leaves, rejected_leaves)
    }

    pub fn present_multi(
        provider: CaptureProvider,
        root: &Path,
        authorities: Vec<Arc<ProviderSourceRoot<E>>>,
        leaves: Vec<JsonlFamilyLeaf<E>>,
    ) -> JsonlResult<Self, E> {
        Self::present_multi_with_rejected(provider, root, authorities, leaves, Vec::new())
    }

    pub fn present_multi_with_rejected(
        provider: CaptureProvider,
        root: &Path,
        authorities: Vec<Arc<ProviderSourceRoot<E>>>,
        leaves: Vec<JsonlFamilyLeaf<E>>,
        rejected_leaves: Vec<JsonlFamilyRejectedLeaf>,
    ) -> JsonlResult<Self, E> {
        Self::present_multi_with_dispositions(
            provider,
            root,
            authorities,
            leaves,
            rejected_leaves,
            Vec::new(),
        )
    }

    pub fn present_multi_with_dispositions(
        provider: CaptureProvider,
        root: &Path,
        mut authorities: Vec<Arc<ProviderSourceRoot<E>>>,
        leaves: Vec<JsonlFamilyLeaf<E>>,
        rejected_leaves: Vec<JsonlFamilyRejectedLeaf>,
        pending_leaves: Vec<JsonlFamilyPendingLeaf>,
    ) -> JsonlResult<Self, E> {
        if authorities.is_empty() {
            return Err(E::invalid_payload(
                "present JSONL inventory has no retained root authority".to_owned(),
            ));
        }
        authorities.sort_by(|left, right| left.named_path().cmp(right.named_path()));
        for pair in authorities.windows(2) {
            if pair[0].named_path() == pair[1].named_path() {
                return Err(E::invalid_payload(format!(
                    "present JSONL inventory has duplicate root authority {}",
                    pair[0].named_path().display()
                )));
            }
        }

        let mut members = Vec::with_capacity(
            leaves
                .len()
                .saturating_add(rejected_leaves.len())
                .saturating_add(pending_leaves.len()),
        );
        for leaf in leaves {
            let retained = authorities.iter().any(|authority| {
                authority.named_path() == leaf.authority.named_path()
                    && authority.authority_fingerprint() == leaf.authority.authority_fingerprint()
            });
            if !retained {
                return Err(E::invalid_payload(format!(
                    "JSONL leaf {} is outside the retained root authorities",
                    leaf.source_path.display()
                )));
            }
            members.push(JsonlFamilyInventoryMember::Accepted {
                identity: JsonlFamilyPhysicalSourceIdentity::derive(provider, &leaf.source_path),
                leaf,
            });
        }
        for leaf in rejected_leaves {
            validate_disposition_authority(&authorities, &leaf.source_path, &leaf.authority_path)?;
            members.push(JsonlFamilyInventoryMember::Quarantined {
                identity: JsonlFamilyPhysicalSourceIdentity::derive(provider, &leaf.source_path),
                leaf,
            });
        }
        for leaf in pending_leaves {
            validate_disposition_authority(&authorities, &leaf.source_path, &leaf.authority_path)?;
            members.push(JsonlFamilyInventoryMember::Pending {
                identity: JsonlFamilyPhysicalSourceIdentity::derive(provider, &leaf.source_path),
                leaf,
            });
        }
        members.sort_by(|left, right| left.source_path().cmp(right.source_path()));
        validate_unique_members(&members)?;
        reconcile_duplicate_accepted_sources(&mut members)?;
        validate_unique_accepted_sources(&members)?;
        let observation = inventory_observation(provider, root, false, &authorities, &members)?;
        Ok(Self {
            provider,
            root: root.to_path_buf(),
            root_missing: false,
            observation,
            authorities,
            members,
            exact_dependencies: Vec::new(),
        })
    }

    pub fn missing(provider: CaptureProvider, root: &Path) -> JsonlResult<Self, E> {
        let members = Vec::new();
        Ok(Self {
            provider,
            root: root.to_path_buf(),
            root_missing: true,
            observation: inventory_observation::<E>(provider, root, true, &[], &members)?,
            authorities: Vec::new(),
            members,
            exact_dependencies: Vec::new(),
        })
    }

    pub fn with_exact_dependencies(
        mut self,
        exact_dependencies: Vec<JsonlFamilyTerminalProof<E>>,
    ) -> Self {
        self.exact_dependencies = exact_dependencies;
        self
    }

    pub(super) fn with_appended_exact_dependencies(
        mut self,
        mut exact_dependencies: Vec<JsonlFamilyTerminalProof<E>>,
    ) -> Self {
        self.exact_dependencies.append(&mut exact_dependencies);
        self
    }

    pub fn root_missing(&self) -> bool {
        self.root_missing
    }

    pub fn members(&self) -> &[JsonlFamilyInventoryMember<E>] {
        &self.members
    }

    pub fn accepted_leaves(&self) -> impl Iterator<Item = &JsonlFamilyLeaf<E>> {
        self.members.iter().filter_map(|member| match member {
            JsonlFamilyInventoryMember::Accepted { leaf, .. } => Some(leaf),
            JsonlFamilyInventoryMember::Quarantined { .. }
            | JsonlFamilyInventoryMember::Pending { .. } => None,
        })
    }

    pub fn quarantined_leaves(&self) -> impl Iterator<Item = &JsonlFamilyRejectedLeaf> {
        self.members.iter().filter_map(|member| match member {
            JsonlFamilyInventoryMember::Quarantined { leaf, .. } => Some(leaf),
            JsonlFamilyInventoryMember::Accepted { .. }
            | JsonlFamilyInventoryMember::Pending { .. } => None,
        })
    }

    pub fn pending_leaves(&self) -> impl Iterator<Item = &JsonlFamilyPendingLeaf> {
        self.members.iter().filter_map(|member| match member {
            JsonlFamilyInventoryMember::Pending { leaf, .. } => Some(leaf),
            JsonlFamilyInventoryMember::Accepted { .. }
            | JsonlFamilyInventoryMember::Quarantined { .. } => None,
        })
    }

    pub fn accepted_len(&self) -> usize {
        self.accepted_leaves().count()
    }

    pub fn quarantined_len(&self) -> usize {
        self.quarantined_leaves().count()
    }

    pub fn pending_len(&self) -> usize {
        self.pending_leaves().count()
    }

    pub(super) fn rebuild_observation(&mut self) -> JsonlResult<(), E> {
        validate_unique_members(&self.members)?;
        self.observation = inventory_observation(
            self.provider,
            &self.root,
            self.root_missing,
            &self.authorities,
            &self.members,
        )?;
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn certify_against(
        &self,
        closing: &Self,
    ) -> JsonlResult<CertifiedSourceInventory, E> {
        self.certify_selected_against(
            closing,
            closing
                .accepted_leaves()
                .map(|leaf| leaf.source.clone())
                .collect(),
        )
    }

    pub(super) fn certify_selected_against(
        &self,
        closing: &Self,
        sources: Vec<SourceKey>,
    ) -> JsonlResult<CertifiedSourceInventory, E> {
        if self.root_missing != closing.root_missing {
            return Err(E::invalid_payload(
                "JSONL root availability changed during capture".to_owned(),
            ));
        }
        CertifiedSourceInventory::certify(
            self.observation.clone(),
            closing.observation.clone(),
            FAMILY_DISCOVERY_REVISION,
            sources,
        )
        .map_err(contract_error)
    }

    pub(super) fn revalidate_root(&self) -> JsonlResult<(), E> {
        if self.root_missing {
            return Ok(());
        }
        if self.authorities.is_empty() {
            return Err(E::invalid_payload(
                "JSONL inventory has no retained root authority".to_owned(),
            ));
        }
        for authority in &self.authorities {
            authority.revalidate()?;
        }
        Ok(())
    }

    pub(super) fn revalidate_root_same_object(&self) -> JsonlResult<(), E> {
        if self.root_missing {
            return Ok(());
        }
        if self.authorities.is_empty() {
            return Err(E::invalid_payload(
                "JSONL inventory has no retained root authority".to_owned(),
            ));
        }
        for authority in &self.authorities {
            authority.revalidate_same_object()?;
        }
        Ok(())
    }

    pub(super) fn revalidate_terminal_root(
        &self,
        root: &Path,
        mode: JsonlFamilyInventoryMode,
    ) -> JsonlResult<(), E> {
        if self.root_missing {
            return match open_provider_source_path::<E>(root) {
                Err(error) if error.is_not_found() => Ok(()),
                Ok(_) => Err(E::source_changed()),
                Err(error) => Err(error),
            };
        }
        match mode {
            JsonlFamilyInventoryMode::Exact => self.revalidate_root(),
            JsonlFamilyInventoryMode::FrozenOpeningAllowAdditions => {
                self.revalidate_root_same_object()
            }
        }
    }
}

fn validate_disposition_authority<E: JsonlFamilyError>(
    authorities: &[Arc<ProviderSourceRoot<E>>],
    source_path: &Path,
    authority_path: &Path,
) -> JsonlResult<(), E> {
    let matches = authorities
        .iter()
        .filter(|authority| authority.named_path().join(authority_path) == source_path)
        .count();
    if matches != 1 {
        return Err(E::invalid_payload(format!(
            "JSONL physical member {} has {matches} retained root authorities",
            source_path.display()
        )));
    }
    Ok(())
}

pub(super) fn exact_member_authority<'a, E: JsonlFamilyError>(
    authorities: &'a [Arc<ProviderSourceRoot<E>>],
    source_path: &Path,
    authority_path: &Path,
) -> JsonlResult<&'a Arc<ProviderSourceRoot<E>>, E> {
    let mut matches = authorities
        .iter()
        .filter(|authority| authority.named_path().join(authority_path) == source_path);
    let authority = matches.next().ok_or_else(|| {
        E::invalid_payload(format!(
            "JSONL physical member {} has no retained root authority",
            source_path.display()
        ))
    })?;
    if matches.next().is_some() {
        return Err(E::invalid_payload(format!(
            "JSONL physical member {} has ambiguous retained root authority",
            source_path.display()
        )));
    }
    Ok(authority)
}

fn validate_unique_members<E: JsonlFamilyError>(
    members: &[JsonlFamilyInventoryMember<E>],
) -> JsonlResult<(), E> {
    for pair in members.windows(2) {
        if pair[0].source_path() == pair[1].source_path() {
            return Err(E::invalid_payload(format!(
                "JSONL physical inventory contains duplicate member {}",
                pair[0].source_path().display()
            )));
        }
    }
    Ok(())
}

fn validate_unique_accepted_sources<E: JsonlFamilyError>(
    members: &[JsonlFamilyInventoryMember<E>],
) -> JsonlResult<(), E> {
    let mut sources = HashMap::<[u8; 32], SourceKey>::new();
    for source in members.iter().filter_map(|member| match member {
        JsonlFamilyInventoryMember::Accepted { leaf, .. } => Some(leaf.source()),
        JsonlFamilyInventoryMember::Quarantined { .. }
        | JsonlFamilyInventoryMember::Pending { .. } => None,
    }) {
        let digest = source.exact_descriptor_digest();
        if let Some(previous) = sources.insert(digest, source.clone()) {
            if previous.exact_descriptor_eq(source) {
                return Err(E::invalid_payload(format!(
                    "JSONL physical inventory contains duplicate logical source identity {}",
                    source.identity()
                )));
            }
            return Err(E::invalid_payload(
                "JSONL physical inventory contains a source descriptor digest collision".to_owned(),
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum DuplicateDisposition {
    IdenticalAlias,
    Unsafe { report_failure: bool },
}

/// Reconciles physical leaves that claim one logical source without guessing
/// which divergent transcript is authoritative.
///
/// Byte-identical leaves are safe aliases: the lowest path remains accepted and
/// the other physical copies are reported as quarantined aliases. If any copy
/// differs, every copy is withheld and one deterministic logical-source
/// failure is reported. Providers that can prove a stronger semantic equality
/// may collapse their aliases before constructing this generic inventory.
fn reconcile_duplicate_accepted_sources<E: JsonlFamilyError>(
    members: &mut Vec<JsonlFamilyInventoryMember<E>>,
) -> JsonlResult<(), E> {
    let mut groups: HashMap<[u8; 32], Vec<usize>> = HashMap::new();
    for (index, member) in members.iter().enumerate() {
        if let JsonlFamilyInventoryMember::Accepted { leaf, .. } = member {
            groups
                .entry(leaf.source().exact_descriptor_digest())
                .or_default()
                .push(index);
        }
    }

    let mut dispositions = HashMap::<usize, DuplicateDisposition>::new();
    for indices in groups.values() {
        if indices.len() < 2 {
            continue;
        }
        let representative = match &members[indices[0]] {
            JsonlFamilyInventoryMember::Accepted { leaf, .. } => leaf.source().clone(),
            _ => unreachable!("duplicate group references an accepted member"),
        };
        let all_equal = indices.iter().all(|&index| match &members[index] {
            JsonlFamilyInventoryMember::Accepted { leaf, .. } => {
                leaf.source().exact_descriptor_eq(&representative)
            }
            _ => false,
        });
        if !all_equal {
            return Err(E::invalid_payload(
                "JSONL physical inventory contains a source descriptor digest collision".to_owned(),
            ));
        }
        let mut content_digest = None;
        let mut identical = true;
        for &index in indices {
            let leaf = match &members[index] {
                JsonlFamilyInventoryMember::Accepted { leaf, .. } => leaf,
                _ => unreachable!("duplicate group references an accepted member"),
            };
            let observed = authenticated_leaf_content_digest(leaf)?;
            match content_digest {
                Some(expected) if expected != observed => identical = false,
                Some(_) => {}
                None => content_digest = Some(observed),
            }
        }
        if identical {
            for &index in &indices[1..] {
                dispositions.insert(index, DuplicateDisposition::IdenticalAlias);
            }
        } else {
            for (position, &index) in indices.iter().enumerate() {
                dispositions.insert(
                    index,
                    DuplicateDisposition::Unsafe {
                        report_failure: position == 0,
                    },
                );
            }
        }
    }

    if dispositions.is_empty() {
        return Ok(());
    }

    let original = std::mem::take(members);
    let mut rebuilt = Vec::with_capacity(original.len());
    for (index, member) in original.into_iter().enumerate() {
        if let Some(disposition) = dispositions.get(&index).copied() {
            let (identity, leaf) = match member {
                JsonlFamilyInventoryMember::Accepted { identity, leaf } => (identity, leaf),
                _ => unreachable!("duplicate index references an accepted member"),
            };
            let source = leaf.source().clone();
            let mut rejected = JsonlFamilyRejectedLeaf::bind_observed(
                leaf.source_path().to_path_buf(),
                leaf.authority_path().to_path_buf(),
                leaf.observation().clone(),
                leaf.binding().clone(),
                0,
            );
            match disposition {
                DuplicateDisposition::IdenticalAlias => {
                    rejected = rejected.with_logical_source_failure(
                        source,
                        "byte-identical physical leaf repeats an already-present logical source identity; quarantined alias",
                    );
                }
                DuplicateDisposition::Unsafe { report_failure } => {
                    rejected = rejected.with_quarantined_source(source.clone());
                    if report_failure {
                        rejected = rejected.with_logical_source_failure(
                            source,
                            "physical leaves claim one logical source identity but their contents differ; all copies quarantined",
                        );
                    }
                }
            }
            rebuilt.push(JsonlFamilyInventoryMember::Quarantined {
                identity,
                leaf: rejected,
            });
        } else {
            rebuilt.push(member);
        }
    }
    *members = rebuilt;
    Ok(())
}

fn authenticated_leaf_content_digest<E: JsonlFamilyError>(
    leaf: &JsonlFamilyLeaf<E>,
) -> JsonlResult<[u8; 32], E> {
    let opened = leaf.authority().open_file(leaf.authority_path())?;
    let before = observe_opened_file(leaf.source_path(), &opened)?;
    if &before != leaf.observation() {
        return Err(E::source_changed());
    }
    let digest = opened_file_prefix_sha256(opened.file(), before.length())?;
    if observe_opened_file(leaf.source_path(), &opened)? != before {
        return Err(E::source_changed());
    }
    Ok(digest)
}
