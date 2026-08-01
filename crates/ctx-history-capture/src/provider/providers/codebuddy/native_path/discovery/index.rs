use std::{
    collections::HashMap,
    ffi::OsStr,
    path::{Path, PathBuf},
};

use super::{
    catalog::{CatalogRoute, RouteKind},
    CaptureError, Result,
};

#[derive(Debug)]
pub(super) struct IndexedSession {
    pub(super) canonical_routes: Vec<usize>,
    pub(super) message_files: Vec<usize>,
}

#[derive(Debug)]
pub(super) struct DiscoveryIndex {
    by_path: HashMap<PathBuf, usize>,
    by_parent: HashMap<PathBuf, Vec<usize>>,
    pub(super) jsonl_files: Vec<usize>,
    pub(super) history_directories: Vec<usize>,
    sessions: HashMap<PathBuf, IndexedSession>,
}

impl DiscoveryIndex {
    pub(super) fn new(routes: &[CatalogRoute]) -> Result<Self> {
        let mut index = Self {
            by_path: HashMap::with_capacity(routes.len()),
            by_parent: HashMap::new(),
            jsonl_files: Vec::new(),
            history_directories: Vec::new(),
            sessions: HashMap::new(),
        };
        let mut directories = Vec::new();
        for (route_index, route) in routes.iter().enumerate() {
            if index
                .by_path
                .insert(route.relative_path.clone(), route_index)
                .is_some()
            {
                return Err(CaptureError::SystemInvariant(
                    "CodeBuddy discovery produced a duplicate route",
                ));
            }
            if !route.relative_path.as_os_str().is_empty() {
                let parent = route
                    .relative_path
                    .parent()
                    .unwrap_or_else(|| Path::new(""))
                    .to_path_buf();
                index.by_parent.entry(parent).or_default().push(route_index);
            }
            match route.kind {
                RouteKind::Directory => {
                    directories.push(route_index);
                    if route.relative_path.file_name().and_then(OsStr::to_str) == Some("history")
                        && route.relative_path.components().count() <= 9
                    {
                        index.history_directories.push(route_index);
                    }
                }
                RouteKind::File
                    if route.relative_path.extension().and_then(OsStr::to_str) == Some("jsonl") =>
                {
                    index.jsonl_files.push(route_index);
                }
                RouteKind::File => {}
            }
        }

        let session_paths = directories
            .into_iter()
            .filter_map(|route_index| {
                let directory = index.inspect_route(routes, route_index);
                index
                    .is_session(&directory.relative_path, routes)
                    .then(|| directory.relative_path.clone())
            })
            .collect::<Vec<_>>();
        for session_path in session_paths {
            let session = index.index_session(&session_path, routes)?;
            index.sessions.insert(session_path, session);
        }
        Ok(index)
    }

    pub(super) fn inspect_route<'a>(
        &self,
        routes: &'a [CatalogRoute],
        route_index: usize,
    ) -> &'a CatalogRoute {
        &routes[route_index]
    }

    pub(super) fn route_index(&self, path: &Path) -> Option<usize> {
        self.by_path.get(path).copied()
    }

    pub(super) fn route<'a>(
        &self,
        path: &Path,
        routes: &'a [CatalogRoute],
    ) -> Option<&'a CatalogRoute> {
        self.route_index(path)
            .map(|route_index| self.inspect_route(routes, route_index))
    }

    pub(super) fn children(&self, parent: &Path) -> &[usize] {
        self.by_parent.get(parent).map_or(&[], Vec::as_slice)
    }

    pub(super) fn route_is(&self, path: &Path, kind: RouteKind, routes: &[CatalogRoute]) -> bool {
        self.route(path, routes)
            .is_some_and(|route| route.kind == kind)
    }

    pub(super) fn session(&self, path: &Path) -> Option<&IndexedSession> {
        self.sessions.get(path)
    }

    pub(super) fn is_session(&self, directory: &Path, routes: &[CatalogRoute]) -> bool {
        self.route_is(&directory.join("index.json"), RouteKind::File, routes)
            && self.route_is(&directory.join("messages"), RouteKind::Directory, routes)
    }

    fn index_session(
        &self,
        session_path: &Path,
        routes: &[CatalogRoute],
    ) -> Result<IndexedSession> {
        let session_route = self
            .route_index(session_path)
            .ok_or(CaptureError::SourceChangedDuringCapture)?;
        let session_index = self
            .route_index(&session_path.join("index.json"))
            .ok_or(CaptureError::SourceChangedDuringCapture)?;
        let messages_path = session_path.join("messages");
        let messages_route = self
            .route_index(&messages_path)
            .ok_or(CaptureError::SourceChangedDuringCapture)?;
        let project_index_path = session_path
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join("index.json");
        let project_index = self
            .route_index(&project_index_path)
            .filter(|route_index| self.inspect_route(routes, *route_index).kind == RouteKind::File);

        let mut fixed_routes = project_index
            .into_iter()
            .chain([session_route, session_index])
            .collect::<Vec<_>>();
        fixed_routes.sort_unstable();
        fixed_routes.dedup();
        let mut message_routes = Vec::new();
        self.collect_subtree(messages_route, routes, &mut message_routes);
        let canonical_routes = merge_route_indices(&fixed_routes, &message_routes);
        let message_files = self
            .children(&messages_path)
            .iter()
            .copied()
            .filter(|route_index| self.inspect_route(routes, *route_index).kind == RouteKind::File)
            .collect();
        Ok(IndexedSession {
            canonical_routes,
            message_files,
        })
    }

    fn collect_subtree(
        &self,
        route_index: usize,
        routes: &[CatalogRoute],
        collected: &mut Vec<usize>,
    ) {
        let route = self.inspect_route(routes, route_index);
        collected.push(route_index);
        if route.kind == RouteKind::Directory {
            for child in self.children(&route.relative_path) {
                self.collect_subtree(*child, routes, collected);
            }
        }
    }
}

fn merge_route_indices(left: &[usize], right: &[usize]) -> Vec<usize> {
    let mut merged = Vec::with_capacity(left.len().saturating_add(right.len()));
    let (mut left_index, mut right_index) = (0, 0);
    while left_index < left.len() || right_index < right.len() {
        let candidate = match (left.get(left_index), right.get(right_index)) {
            (Some(left), Some(right)) if left <= right => {
                left_index = left_index.saturating_add(1);
                *left
            }
            (Some(_), Some(right)) => {
                right_index = right_index.saturating_add(1);
                *right
            }
            (Some(left), None) => {
                left_index = left_index.saturating_add(1);
                *left
            }
            (None, Some(right)) => {
                right_index = right_index.saturating_add(1);
                *right
            }
            (None, None) => break,
        };
        if merged.last().copied() != Some(candidate) {
            merged.push(candidate);
        }
    }
    merged
}
