use std::collections::HashMap;

use super::CodexCatalogSource;

#[derive(Clone)]
enum CatalogAncestry {
    Resolved { root: String, depth: usize },
    Detached,
}

pub(super) fn parent_first(sources: &mut [CodexCatalogSource]) {
    // Resolve the functional parent graph once. The former repeated scans and
    // Vec removals were quadratic for large catalogs and dominated startup
    // before any source parsing began.
    let parents = sources
        .iter()
        .filter_map(|source| {
            source
                .catalog_native_session_id
                .as_ref()
                .map(|id| (id.clone(), source.catalog_parent_native_session_id.clone()))
        })
        .collect::<HashMap<_, _>>();
    let mut ancestry = HashMap::<String, CatalogAncestry>::with_capacity(parents.len());
    for id in parents.keys() {
        resolve_ancestry(id, &parents, &mut ancestry);
    }

    for source in sources.iter_mut() {
        let Some(source_id) = source.catalog_native_session_id.as_ref() else {
            source.catalog_parent_native_session_id = None;
            source.catalog_root_native_session_id = None;
            continue;
        };
        match ancestry.get(source_id) {
            Some(CatalogAncestry::Resolved { root, depth }) if *depth > 0 => {
                source.catalog_root_native_session_id = Some(root.clone());
            }
            Some(CatalogAncestry::Resolved { .. }) => {
                source.catalog_root_native_session_id = None;
            }
            Some(CatalogAncestry::Detached) | None => {
                source.catalog_parent_native_session_id = None;
                source.catalog_root_native_session_id = None;
            }
        }
    }

    sources.sort_by(|left, right| {
        ancestry_depth(left, &ancestry)
            .cmp(&ancestry_depth(right, &ancestry))
            .then_with(|| left.source_path.cmp(&right.source_path))
    });
}

fn resolve_ancestry(
    source_id: &str,
    parents: &HashMap<String, Option<String>>,
    memo: &mut HashMap<String, CatalogAncestry>,
) {
    if memo.contains_key(source_id) {
        return;
    }
    let mut path = Vec::<String>::new();
    let mut positions = HashMap::<String, usize>::new();
    let mut current = source_id.to_owned();
    let mut resolved = loop {
        if let Some(known) = memo.get(&current) {
            break known.clone();
        }
        if positions.insert(current.clone(), path.len()).is_some() {
            break CatalogAncestry::Detached;
        }
        path.push(current.clone());
        match parents.get(&current) {
            Some(None) => {
                path.pop();
                let root = CatalogAncestry::Resolved {
                    root: current.clone(),
                    depth: 0,
                };
                memo.insert(current, root.clone());
                break root;
            }
            Some(Some(parent)) if parents.contains_key(parent) => current.clone_from(parent),
            Some(Some(_)) | None => break CatalogAncestry::Detached,
        }
    };

    for id in path.into_iter().rev() {
        resolved = match resolved {
            CatalogAncestry::Resolved { ref root, depth } => CatalogAncestry::Resolved {
                root: root.clone(),
                depth: depth.saturating_add(1),
            },
            CatalogAncestry::Detached => CatalogAncestry::Detached,
        };
        memo.insert(id, resolved.clone());
    }
}

fn ancestry_depth(
    source: &CodexCatalogSource,
    ancestry: &HashMap<String, CatalogAncestry>,
) -> usize {
    source
        .catalog_native_session_id
        .as_ref()
        .and_then(|id| ancestry.get(id))
        .and_then(|resolved| match resolved {
            CatalogAncestry::Resolved { depth, .. } => Some(*depth),
            CatalogAncestry::Detached => None,
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::provider::codex::nativepath::CodexFileObservation;

    fn source(index: usize, parent: Option<String>, path_index: usize) -> CodexCatalogSource {
        CodexCatalogSource {
            source_root: "/codex".to_owned(),
            source_path: PathBuf::from(format!("/codex/{path_index:05}.jsonl")),
            cataloged_at_ms: 0,
            catalog_observation: CodexFileObservation {
                len: 0,
                modified_at_ms: 0,
                change_token: [0; 32],
            },
            catalog_native_session_id: Some(format!("session-{index:05}")),
            catalog_parent_native_session_id: parent,
            catalog_root_native_session_id: None,
            opened: None,
            authority_root: None,
            authority_relative_path: None,
        }
    }

    #[test]
    fn forty_thousand_source_chain_is_resolved_parent_first() {
        const SOURCES: usize = 40_000;
        let mut sources = (0..SOURCES)
            .rev()
            .map(|index| {
                source(
                    index,
                    index
                        .checked_sub(1)
                        .map(|parent| format!("session-{parent:05}")),
                    SOURCES - index,
                )
            })
            .collect::<Vec<_>>();

        parent_first(&mut sources);

        for (index, source) in sources.iter().enumerate() {
            let expected_session_id = format!("session-{index:05}");
            assert_eq!(
                source.catalog_native_session_id.as_deref(),
                Some(expected_session_id.as_str())
            );
            assert_eq!(
                source.catalog_root_native_session_id.as_deref(),
                (index > 0).then_some("session-00000")
            );
        }
    }

    #[test]
    fn missing_and_cyclic_ancestry_are_detached() {
        let mut sources = vec![
            source(0, Some("session-00001".to_owned()), 0),
            source(1, Some("session-00000".to_owned()), 1),
            source(2, Some("missing".to_owned()), 2),
        ];

        parent_first(&mut sources);

        assert!(sources
            .iter()
            .all(|source| source.catalog_parent_native_session_id.is_none()));
        assert!(sources
            .iter()
            .all(|source| source.catalog_root_native_session_id.is_none()));
    }
}
