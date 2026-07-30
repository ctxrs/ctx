use super::*;

pub(super) struct InventoryObservation {
    pub(super) digest: [u8; 32],
}

pub(super) struct DirectoryChild {
    order_key: Vec<u8>,
    name: OsString,
}

pub(super) fn observe_inventory(
    authority: &ProviderSourceRoot,
    selected_relative: &Path,
    selected_file: bool,
    mut leaves: Option<&mut Vec<ContinueDocumentLeaf>>,
    mutation_watch: Option<&RootMutationWatch>,
) -> Result<InventoryObservation, ContinueNativePathError> {
    let mut hasher = Sha256::new();
    hasher.update(INVENTORY_DIGEST_DOMAIN);
    let mut entries = 0_usize;
    visit_inventory(
        authority,
        selected_relative,
        selected_file,
        0,
        &mut entries,
        &mut hasher,
        &mut leaves,
        mutation_watch,
    )?;
    authority.revalidate().map_err(|error| {
        capture_source_error(authority.named_path(), "revalidate Continue root", error)
    })?;
    Ok(InventoryObservation {
        digest: hasher.finalize().into(),
    })
}

#[allow(clippy::too_many_arguments)]
fn visit_inventory(
    authority: &ProviderSourceRoot,
    relative: &Path,
    selected_file: bool,
    depth: usize,
    entries: &mut usize,
    hasher: &mut Sha256,
    leaves: &mut Option<&mut Vec<ContinueDocumentLeaf>>,
    mutation_watch: Option<&RootMutationWatch>,
) -> Result<(), ContinueNativePathError> {
    let path = authority.named_path().join(relative);
    if depth > MAX_CONTINUE_DIRECTORY_DEPTH {
        return Err(ContinueNativePathError::SourceAccess {
            path,
            message: "Continue session tree exceeds the supported depth".to_owned(),
        });
    }
    if *entries >= MAX_CONTINUE_INVENTORY_ENTRIES {
        return Err(ContinueNativePathError::SourceAccess {
            path,
            message: "Continue session tree exceeds the supported inventory limit".to_owned(),
        });
    }
    *entries = entries.saturating_add(1);
    let opened = authority
        .open_path(relative)
        .map_err(|error| capture_source_error(&path, "open Continue inventory entry", error))?;
    match opened {
        OpenedProviderSourcePath::File(file) => {
            let file_token = file.ordinary_file_token();
            hash_inventory_entry(hasher, relative, b'f', file_token, &path)?;
            if super::super::super::continue_session_json_path(&path) {
                if let Some(leaves) = leaves.as_deref_mut() {
                    leaves.push(ContinueDocumentLeaf::new(
                        relative.to_path_buf(),
                        path.clone(),
                        file_token,
                    ));
                }
            }
            file.revalidate_leaf().map_err(|error| {
                capture_source_error(&path, "revalidate Continue inventory file", error)
            })?;
        }
        OpenedProviderSourcePath::Directory(directory) => {
            if selected_file {
                return Err(ContinueNativePathError::SourceChanged { path });
            }
            hash_inventory_entry(
                hasher,
                relative,
                b'd',
                directory.authority_fingerprint(),
                &path,
            )?;
            if let Some(watch) = mutation_watch {
                watch.add(&path)?;
            }
            let remaining = MAX_CONTINUE_INVENTORY_ENTRIES.saturating_sub(*entries);
            let names = directory
                .entries(remaining.saturating_add(1))
                .map_err(|error| {
                    capture_source_error(&path, "enumerate Continue inventory directory", error)
                })?;
            if names.len() > remaining {
                return Err(ContinueNativePathError::SourceAccess {
                    path,
                    message: "Continue session tree exceeds the supported inventory limit"
                        .to_owned(),
                });
            }
            let mut children = names
                .into_iter()
                .map(|name| DirectoryChild {
                    order_key: os_order_key(&name),
                    name,
                })
                .collect::<Vec<_>>();
            children.sort_by(|left, right| left.order_key.cmp(&right.order_key));
            for child in children {
                visit_inventory(
                    authority,
                    &relative.join(child.name),
                    false,
                    depth.saturating_add(1),
                    entries,
                    hasher,
                    leaves,
                    mutation_watch,
                )?;
            }
            directory.revalidate().map_err(|error| {
                capture_source_error(&path, "revalidate Continue inventory directory", error)
            })?;
        }
    }
    Ok(())
}

fn hash_inventory_entry(
    hasher: &mut Sha256,
    relative: &Path,
    kind: u8,
    object_fingerprint: [u8; 32],
    path: &Path,
) -> Result<(), ContinueNativePathError> {
    let encoded = encode_path(relative).ok_or_else(|| ContinueNativePathError::SourceAccess {
        path: path.to_path_buf(),
        message: "Continue inventory path cannot be encoded".to_owned(),
    })?;
    hasher.update((encoded.len() as u64).to_be_bytes());
    hasher.update(encoded);
    hasher.update([kind]);
    hasher.update(object_fingerprint);
    Ok(())
}
