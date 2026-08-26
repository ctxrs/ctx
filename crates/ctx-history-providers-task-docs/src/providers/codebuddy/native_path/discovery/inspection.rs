use ctx_history_core::SourceAnchorScope;
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    path::{Path, PathBuf},
};

use super::{
    catalog::{CatalogRoute, CatalogSelection, RouteKind},
    hash_path,
    index::{DiscoveryIndex, IndexedSession},
    invalid, source_backed, CodeBuddyDocumentLeaf, CodeBuddyObservedFile, CodeBuddySourceShape,
    Digest, DocumentLeafFingerprint, DocumentLeafKind, ObservedDocumentLeaf, Result, Sha256,
    SourceKey, LEAF_DOMAIN, TREE_DOMAIN,
};

pub(super) fn logical_leaves(
    selected_path: &Path,
    selected_relative_path: &Path,
    selection: CatalogSelection,
    routes: &[CatalogRoute],
    index: &DiscoveryIndex,
    source_anchor_scope: SourceAnchorScope,
) -> Result<Vec<ObservedDocumentLeaf<CodeBuddyDocumentLeaf>>> {
    let mut extension_dirs = extension_session_dirs(
        selected_path,
        selected_relative_path,
        selection,
        routes,
        index,
    );
    extension_dirs.sort();
    let mut leaves = Vec::new();
    for (ordinal_index, session_relative) in extension_dirs.into_iter().enumerate() {
        leaves.push(extension_leaf(
            session_relative,
            ordinal_index.saturating_add(1),
            routes,
            index,
            source_anchor_scope,
        )?);
    }

    let extension_count = leaves.len();
    let mut physical_cli = BTreeMap::<[u8; 32], Vec<&CatalogRoute>>::new();
    for route in cli_routes(
        selected_path,
        selected_relative_path,
        selection,
        routes,
        index,
    ) {
        physical_cli
            .entry(route.authority_fingerprint)
            .or_default()
            .push(route);
    }
    for (index, mut aliases) in physical_cli.into_values().enumerate() {
        aliases.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
        let selected = aliases[0]
            .observed_file()
            .ok_or(super::CaptureError::SystemInvariant(
                "CodeBuddy CLI route lost its file observation",
            ))?;
        let source = source_backed::codebuddy_source_key_for_path_scoped(
            CodeBuddySourceShape::Cli,
            &selected.display_path,
            source_anchor_scope,
        )?;
        let fingerprint = cli_fingerprint(&source, &selected);
        let aliases = aliases
            .into_iter()
            .map(|route| route.display_path.clone())
            .collect();
        leaves.push(ObservedDocumentLeaf::new(
            fingerprint,
            CodeBuddyDocumentLeaf {
                source,
                session_ordinal: extension_count.saturating_add(index).saturating_add(1),
                kind: DocumentLeafKind::Cli { selected, aliases },
            },
        ));
    }
    leaves.sort_by(|left, right| {
        left.provider_leaf
            .logical_path()
            .cmp(right.provider_leaf.logical_path())
    });
    Ok(leaves)
}

fn extension_leaf(
    session_relative: PathBuf,
    session_ordinal: usize,
    routes: &[CatalogRoute],
    index: &DiscoveryIndex,
    source_anchor_scope: SourceAnchorScope,
) -> Result<ObservedDocumentLeaf<CodeBuddyDocumentLeaf>> {
    let indexed_session = index
        .session(&session_relative)
        .ok_or(super::CaptureError::SourceChangedDuringCapture)?;
    let session_index = required_file(&session_relative.join("index.json"), routes, index)?;
    let project_index_path = session_relative
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .join("index.json");
    let project_index = index
        .route(&project_index_path, routes)
        .filter(|route| route.kind == RouteKind::File)
        .and_then(CatalogRoute::observed_file);
    let mut messages = BTreeMap::new();
    for route_index in &indexed_session.message_files {
        let route = index.inspect_route(routes, *route_index);
        if let Some(id) = route
            .relative_path
            .file_stem()
            .and_then(OsStr::to_str)
            .filter(|id| super::provider_safe_path_segment(id))
        {
            messages.insert(
                id.to_owned(),
                route
                    .observed_file()
                    .ok_or(super::CaptureError::SystemInvariant(
                        "CodeBuddy message route lost its file observation",
                    ))?,
            );
        }
    }
    let session_dir = session_index
        .display_path
        .parent()
        .ok_or_else(|| invalid(&session_index.display_path, "session index has no parent"))?
        .to_path_buf();
    let source = source_backed::codebuddy_source_key_for_path_scoped(
        CodeBuddySourceShape::Extension,
        &session_dir,
        source_anchor_scope,
    )?;
    let fingerprint = extension_fingerprint(&source, indexed_session, routes, index);
    Ok(ObservedDocumentLeaf::new(
        fingerprint,
        CodeBuddyDocumentLeaf {
            source,
            session_ordinal,
            kind: DocumentLeafKind::Extension {
                session_dir,
                session_index,
                project_index,
                messages,
            },
        },
    ))
}

fn extension_session_dirs(
    selected_path: &Path,
    selected_relative_path: &Path,
    selection: CatalogSelection,
    routes: &[CatalogRoute],
    index: &DiscoveryIndex,
) -> Vec<PathBuf> {
    let mut sessions = BTreeSet::new();
    let root = PathBuf::new();
    let exact_index = matches!(
        selection,
        CatalogSelection::ExactFile {
            inventory_parent: true
        }
    ) && selected_relative_path.file_name().and_then(OsStr::to_str)
        == Some("index.json");
    if exact_index {
        if index.is_session(&root, routes) {
            sessions.insert(root);
        } else {
            insert_project_sessions(Path::new(""), routes, index, &mut sessions);
        }
        return sessions.into_iter().collect();
    }
    if selection != CatalogSelection::Directory {
        return Vec::new();
    }
    if index.is_session(&root, routes) {
        sessions.insert(root);
        return sessions.into_iter().collect();
    }
    insert_project_sessions(Path::new(""), routes, index, &mut sessions);
    if selected_path.file_name().and_then(OsStr::to_str) == Some("history") {
        insert_history_sessions(Path::new(""), routes, index, &mut sessions);
    } else {
        for route_index in &index.history_directories {
            let route = index.inspect_route(routes, *route_index);
            insert_history_sessions(&route.relative_path, routes, index, &mut sessions);
        }
    }
    sessions.into_iter().collect()
}

fn insert_project_sessions(
    project: &Path,
    routes: &[CatalogRoute],
    index: &DiscoveryIndex,
    sessions: &mut BTreeSet<PathBuf>,
) {
    for child in direct_child_directories(project, routes, index) {
        if index.is_session(&child, routes) {
            sessions.insert(child);
        }
    }
}

fn insert_history_sessions(
    history: &Path,
    routes: &[CatalogRoute],
    index: &DiscoveryIndex,
    sessions: &mut BTreeSet<PathBuf>,
) {
    for project in direct_child_directories(history, routes, index) {
        insert_project_sessions(&project, routes, index, sessions);
    }
}

fn direct_child_directories(
    parent: &Path,
    routes: &[CatalogRoute],
    index: &DiscoveryIndex,
) -> Vec<PathBuf> {
    index
        .children(parent)
        .iter()
        .map(|route_index| index.inspect_route(routes, *route_index))
        .filter(|route| route.kind == RouteKind::Directory)
        .map(|route| route.relative_path.clone())
        .collect()
}

fn cli_routes<'a>(
    selected_path: &Path,
    selected_relative_path: &Path,
    selection: CatalogSelection,
    routes: &'a [CatalogRoute],
    index: &DiscoveryIndex,
) -> Vec<&'a CatalogRoute> {
    if matches!(selection, CatalogSelection::ExactFile { .. }) {
        return index
            .route(selected_relative_path, routes)
            .filter(|route| {
                route.kind == RouteKind::File
                    && route.relative_path.extension().and_then(OsStr::to_str) == Some("jsonl")
            })
            .into_iter()
            .collect();
    }
    let scan_root = if index.route_is(Path::new("projects"), RouteKind::Directory, routes) {
        Some(PathBuf::from("projects"))
    } else if selected_path.file_name().and_then(OsStr::to_str) == Some("projects")
        || selected_path
            .parent()
            .and_then(Path::file_name)
            .and_then(OsStr::to_str)
            == Some("projects")
    {
        Some(PathBuf::new())
    } else {
        None
    };
    let Some(scan_root) = scan_root else {
        return Vec::new();
    };
    index
        .jsonl_files
        .iter()
        .map(|route_index| index.inspect_route(routes, *route_index))
        .filter(|route| route.relative_path.starts_with(&scan_root))
        .collect()
}

fn required_file(
    path: &Path,
    routes: &[CatalogRoute],
    index: &DiscoveryIndex,
) -> Result<CodeBuddyObservedFile> {
    index
        .route(path, routes)
        .filter(|route| route.kind == RouteKind::File)
        .and_then(CatalogRoute::observed_file)
        .ok_or(super::CaptureError::SourceChangedDuringCapture)
}

fn cli_fingerprint(source: &SourceKey, file: &CodeBuddyObservedFile) -> DocumentLeafFingerprint {
    let mut digest = Sha256::new();
    digest.update(LEAF_DOMAIN);
    digest.update(source.exact_descriptor_digest());
    hash_path(&mut digest, &file.relative_path);
    digest.update(file.authority_fingerprint);
    DocumentLeafFingerprint::new(digest.finalize().into())
}

fn extension_fingerprint(
    source: &SourceKey,
    session: &IndexedSession,
    routes: &[CatalogRoute],
    index: &DiscoveryIndex,
) -> DocumentLeafFingerprint {
    let mut digest = Sha256::new();
    digest.update(LEAF_DOMAIN);
    digest.update(source.exact_descriptor_digest());
    for route_index in &session.canonical_routes {
        let route = index.inspect_route(routes, *route_index);
        digest.update([route.kind.tag()]);
        hash_path(&mut digest, &route.relative_path);
        digest.update(route.authority_fingerprint);
    }
    DocumentLeafFingerprint::new(digest.finalize().into())
}

pub(super) fn tree_fingerprint(
    selection: CatalogSelection,
    selected_relative_path: &Path,
    routes: &[CatalogRoute],
    leaves: &[ObservedDocumentLeaf<CodeBuddyDocumentLeaf>],
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(TREE_DOMAIN);
    digest.update([selection.tag()]);
    hash_path(&mut digest, selected_relative_path);
    for route in routes {
        digest.update([route.kind.tag()]);
        hash_path(&mut digest, &route.relative_path);
        digest.update(route.authority_fingerprint);
    }
    for leaf in leaves {
        digest.update(leaf.fingerprint.as_bytes());
        digest.update(leaf.provider_leaf.source.exact_descriptor_digest());
        for alias in leaf.provider_leaf.aliases() {
            hash_path(&mut digest, alias);
        }
    }
    digest.finalize().into()
}
