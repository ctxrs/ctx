use super::*;

#[derive(Debug)]
pub(crate) enum DeepLinkAction {
    Open(DeepLinkOpen),
    Reveal(DeepLinkReveal),
    Workspace(DeepLinkWorkspace),
    Task(DeepLinkTask),
    Focus,
}

#[derive(Debug)]
pub(crate) struct DeepLinkOpen {
    pub(crate) target: DeepLinkTarget,
    pub(crate) line: Option<u32>,
    pub(crate) col: Option<u32>,
    pub(crate) open_with: DeepLinkOpenWith,
    pub(crate) editor_override: Option<DesktopEditorTarget>,
    pub(crate) token: Option<String>,
}

#[derive(Debug)]
pub(crate) struct DeepLinkReveal {
    pub(crate) target: DeepLinkTarget,
    pub(crate) token: Option<String>,
}

#[derive(Debug)]
pub(crate) struct DeepLinkWorkspace {
    pub(crate) workspace_id: Option<String>,
    pub(crate) path: Option<String>,
}

#[derive(Debug)]
pub(crate) struct DeepLinkTask {
    pub(crate) notification_route_id: Option<String>,
    pub(crate) session_id: Option<String>,
    pub(crate) task_id: String,
    pub(crate) workspace_id: String,
}

#[derive(Debug)]
pub(crate) enum DeepLinkTarget {
    WorktreeFile { worktree_id: String, file: String },
    Path { path: String },
}

#[derive(Debug)]
pub(crate) struct WorktreeInfo {
    pub(crate) root: PathBuf,
    pub(crate) workspace_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeepLinkOpenWith {
    Ctx,
    Editor,
    System,
}
