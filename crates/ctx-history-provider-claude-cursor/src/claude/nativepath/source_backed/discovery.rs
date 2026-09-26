use super::*;
use ctx_history_provider_runtime::{
    observe_opened_file_allow_append, ProviderJsonlMembershipObservation,
};
use ctx_history_source_io::visit_bounded_tree_files_frozen;

pub(super) fn membership<B: ProviderRuntimeBinding>(
    adapter: &ClaudeJsonlAdapter<B>,
    root: &Path,
    opening: &ProviderJsonlInventory,
) -> Result<ProviderJsonlMembershipObservation> {
    if opening.root_missing() {
        return ProviderJsonlMembershipObservation::observe(root, opening);
    }
    let mut membership = ProviderJsonlMembershipObservation::observe_frozen_authorities(opening)?;
    let unbound = membership
        .unbound_routes()
        .map(|(path, authority, _)| (path.to_path_buf(), authority))
        .collect::<Vec<_>>();
    let claimed = opening
        .members()
        .iter()
        .filter_map(|member| member.source())
        .map(SourceKey::exact_descriptor_digest)
        .collect::<HashSet<_>>();
    for (path, authority) in unbound {
        let Some((_, _, key)) =
            classify_claude_path(&claude_projects_root(authority.named_path()), &path)?
        else {
            continue;
        };
        let source = source_key(adapter.source_root_lineage, &key)?;
        if claimed.contains(&source.exact_descriptor_digest()) {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        membership.bind_source_hint(path, source);
    }
    Ok(membership)
}

pub(super) fn discover<B: ProviderRuntimeBinding>(
    adapter: &ClaudeJsonlAdapter<B>,
    root: &Path,
) -> Result<ProviderJsonlInventory> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: root.to_path_buf(),
                reason: "Claude source-backed discovery requires a projects directory",
            });
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return ProviderJsonlInventory::missing(adapter.provider(), root);
        }
        Err(error) => return Err(error.into()),
    }
    let canonical_root = fs::canonicalize(root)?;
    let projects_root = claude_projects_root(&canonical_root);
    let source_root_lineage = adapter.source_root_lineage;
    let authority = Arc::new(ProviderSourceRoot::open(&canonical_root)?);
    let mut observed = Vec::new();
    let mut unreadable = Vec::new();
    visit_bounded_tree_files_frozen::<CaptureError, _>(
        &canonical_root,
        &mut |candidate| {
            candidate
                .path()
                .extension()
                .and_then(|extension| extension.to_str())
                == Some("jsonl")
        },
        &mut |source_file| {
            let path = source_file.path().to_path_buf();
            let relative_path = relative_to_authority(&authority, &path)?;
            let opened = authority.open_file(&relative_path)?;
            observed.push((
                path.clone(),
                observe_opened_file_allow_append(&path, &opened)?,
            ));
            Ok(())
        },
        &mut |path, error| {
            if !is_quarantinable_claude_leaf_error(&error) {
                return Err(error);
            }
            unreadable.push((path.to_path_buf(), error.to_string()));
            Ok(())
        },
    )?;

    let mut claimed_sources = HashMap::<[u8; 32], SourceKey>::new();
    let mut duplicate_sources = HashSet::new();
    let mut source_owner_paths = HashMap::<[u8; 32], PathBuf>::new();
    let mut prepared_observed = Vec::new();
    observed.sort_by(|left, right| left.0.cmp(&right.0));
    for (path, observation) in observed {
        let Some((project_dir, layout, key)) = classify_claude_path(&projects_root, &path)? else {
            continue;
        };
        let binding = Binding {
            project_dir,
            source_root_lineage,
            key,
            layout,
        };
        let source = source_key(binding.source_root_lineage, &binding.key)?;
        let relative_path = relative_to_authority(&authority, &path)?;
        let proof = TypedKey::bytes(serde_json::to_vec(&binding)?).map_err(contract)?;
        let digest = source.exact_descriptor_digest();
        if claim_claude_source(&mut claimed_sources, &source)? == ClaudeSourceClaim::Duplicate {
            duplicate_sources.insert(digest);
        }
        source_owner_paths
            .entry(digest)
            .and_modify(|owner| {
                if path.as_path() < owner.as_path() {
                    *owner = path.clone();
                }
            })
            .or_insert_with(|| path.clone());
        prepared_observed.push((source, path, relative_path, proof, observation));
    }
    let mut prepared_unreadable = Vec::new();
    unreadable.sort_by(|left, right| left.0.cmp(&right.0));
    for (path, detail) in unreadable {
        let Some((project_dir, layout, key)) = classify_claude_path(&projects_root, &path)? else {
            continue;
        };
        let binding = Binding {
            project_dir,
            source_root_lineage,
            key,
            layout,
        };
        let source = source_key(binding.source_root_lineage, &binding.key)?;
        let relative_path = relative_to_authority(&authority, &path)?;
        let digest = source.exact_descriptor_digest();
        if claim_claude_source(&mut claimed_sources, &source)? == ClaudeSourceClaim::Duplicate {
            duplicate_sources.insert(digest);
        }
        source_owner_paths
            .entry(digest)
            .and_modify(|owner| {
                if path.as_path() < owner.as_path() {
                    *owner = path.clone();
                }
            })
            .or_insert_with(|| path.clone());
        prepared_unreadable.push((
            source,
            path,
            relative_path,
            TypedKey::bytes(serde_json::to_vec(&binding)?).map_err(contract)?,
            detail,
        ));
    }

    let mut leaves = Vec::new();
    let mut rejected_leaves = Vec::new();
    for (source, path, relative_path, proof, observation) in prepared_observed {
        let digest = source.exact_descriptor_digest();
        if duplicate_sources.contains(&digest) {
            let mut rejected = JsonlFamilyRejectedLeaf::bind_observed(
                path.clone(),
                relative_path,
                observation,
                proof,
                0,
            )
            .with_quarantined_source(source.clone());
            if source_owner_paths
                .get(&digest)
                .is_some_and(|owner| owner == &path)
            {
                rejected = rejected.with_logical_source_failure(
                        source.clone(),
                        format!(
                            "Claude transcript {} repeats a native session identity claimed by another transcript",
                            path.display()
                        ),
                    );
            }
            rejected_leaves.push(rejected);
        } else {
            leaves.push(ProviderJsonlLeaf::bind_frozen_observed(
                source,
                path,
                Arc::clone(&authority),
                relative_path,
                proof,
                observation,
            ));
        }
    }
    for (source, path, relative_path, proof, detail) in prepared_unreadable {
        let digest = source.exact_descriptor_digest();
        let duplicate = duplicate_sources.contains(&digest);
        let failure_detail = if duplicate {
            format!(
                    "Claude transcript {} repeats a native session identity claimed by another transcript and is unreadable: {detail}",
                    path.display()
                )
        } else {
            format!(
                "Claude transcript {} is unreadable: {detail}",
                path.display()
            )
        };
        let mut rejected =
            JsonlFamilyRejectedLeaf::bind_unobserved(path.clone(), relative_path, proof, 0)
                .with_quarantined_source(source.clone());
        if !duplicate
            || source_owner_paths
                .get(&digest)
                .is_some_and(|owner| owner == &path)
        {
            rejected = rejected.with_logical_source_failure(source, failure_detail);
        }
        rejected_leaves.push(rejected);
    }
    ProviderJsonlInventory::present_with_rejected(
        adapter.provider(),
        root,
        authority,
        leaves,
        rejected_leaves,
    )
}
