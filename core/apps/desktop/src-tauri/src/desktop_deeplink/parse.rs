use super::*;

pub(crate) fn parse_deep_link(url: &Url) -> Result<DeepLinkAction> {
    let scheme = url.scheme();
    if scheme != "ctx" {
        anyhow::bail!("unsupported scheme: {scheme}");
    }
    let action = url.host_str().unwrap_or_default();
    let params: HashMap<String, String> = url
        .query_pairs()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let version = params
        .get("v")
        .map(|v| v.parse::<u32>())
        .transpose()
        .context("invalid version")?
        .unwrap_or(1);
    if version != 1 {
        anyhow::bail!("unsupported version: {version}");
    }

    match action {
        "open" => parse_open(&params).map(DeepLinkAction::Open),
        "reveal" => parse_reveal(&params).map(DeepLinkAction::Reveal),
        "workspace" => parse_workspace(&params).map(DeepLinkAction::Workspace),
        "task" => parse_task(&params).map(DeepLinkAction::Task),
        "focus" => Ok(DeepLinkAction::Focus),
        _ => anyhow::bail!("unknown action: {action}"),
    }
}

pub(crate) fn parse_open(params: &HashMap<String, String>) -> Result<DeepLinkOpen> {
    let target = parse_target(params)?;
    let line = parse_optional_positive(params.get("line"));
    let col = parse_optional_positive(params.get("col"));
    let open_with = parse_open_with(params.get("openWith"))?;
    let editor_override = match params.get("editor") {
        Some(value) => Some(parse_editor_target(value)?),
        None => None,
    };
    let token = params.get("token").cloned();
    Ok(DeepLinkOpen {
        target,
        line,
        col,
        open_with,
        editor_override,
        token,
    })
}

pub(crate) fn parse_reveal(params: &HashMap<String, String>) -> Result<DeepLinkReveal> {
    let target = parse_target(params)?;
    let token = params.get("token").cloned();
    Ok(DeepLinkReveal { target, token })
}

pub(crate) fn parse_workspace(params: &HashMap<String, String>) -> Result<DeepLinkWorkspace> {
    let workspace_id = params.get("workspaceId").cloned();
    let path = params.get("path").cloned();
    if workspace_id.is_none() && path.is_none() {
        anyhow::bail!("workspaceId or path is required");
    }
    Ok(DeepLinkWorkspace { workspace_id, path })
}

pub(crate) fn parse_task(params: &HashMap<String, String>) -> Result<DeepLinkTask> {
    let workspace_id = params
        .get("workspaceId")
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("workspaceId is required"))?
        .to_string();
    let task_id = params
        .get("taskId")
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("taskId is required"))?
        .to_string();
    let session_id = params
        .get("sessionId")
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let notification_route_id = params
        .get("notificationRouteId")
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    Ok(DeepLinkTask {
        workspace_id,
        task_id,
        session_id,
        notification_route_id,
    })
}

pub(crate) fn parse_target(params: &HashMap<String, String>) -> Result<DeepLinkTarget> {
    let worktree_id = params.get("worktreeId").cloned();
    let file = params.get("file").cloned();
    let path = params.get("path").cloned();

    if let Some(worktree_id) = worktree_id {
        let file = file.ok_or_else(|| anyhow!("file is required when worktreeId is set"))?;
        let file = validate_relative_file(&file)?;
        return Ok(DeepLinkTarget::WorktreeFile { worktree_id, file });
    }

    let path = path.ok_or_else(|| anyhow!("path is required"))?;
    let path = validate_absolute_path(&path)?;
    Ok(DeepLinkTarget::Path { path })
}

pub(crate) fn parse_open_with(value: Option<&String>) -> Result<DeepLinkOpenWith> {
    match value.map(|v| v.trim().to_lowercase()) {
        None => Ok(DeepLinkOpenWith::Ctx),
        Some(v) if v == "ctx" => Ok(DeepLinkOpenWith::Ctx),
        Some(v) if v == "editor" => Ok(DeepLinkOpenWith::Editor),
        Some(v) if v == "system" => Ok(DeepLinkOpenWith::System),
        Some(v) => anyhow::bail!("unsupported openWith: {v}"),
    }
}

pub(crate) fn parse_editor_target(value: &str) -> Result<DesktopEditorTarget> {
    match value.trim().to_lowercase().as_str() {
        "vscode" => Ok(DesktopEditorTarget::VsCode),
        "vscode_insiders" => Ok(DesktopEditorTarget::VsCodeInsiders),
        "cursor" => Ok(DesktopEditorTarget::Cursor),
        "windsurf" => Ok(DesktopEditorTarget::Windsurf),
        "antigravity" => Ok(DesktopEditorTarget::Antigravity),
        "idea" => Ok(DesktopEditorTarget::Idea),
        "pycharm" => Ok(DesktopEditorTarget::Pycharm),
        "xcode" => Ok(DesktopEditorTarget::Xcode),
        "android_studio" => Ok(DesktopEditorTarget::AndroidStudio),
        "custom" => anyhow::bail!("custom editor commands are disabled"),
        "system" => Ok(DesktopEditorTarget::System),
        other => anyhow::bail!("unknown editor: {other}"),
    }
}

pub(crate) fn parse_optional_positive(value: Option<&String>) -> Option<u32> {
    value.and_then(|v| v.parse::<u32>().ok()).filter(|v| *v > 0)
}

pub(crate) fn validate_relative_file(path: &str) -> Result<String> {
    if path.trim().is_empty() {
        anyhow::bail!("file is empty");
    }
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        anyhow::bail!("file must be relative");
    }
    if path.contains(':') {
        anyhow::bail!("file must be a relative path");
    }
    if candidate
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        anyhow::bail!("file must not contain ..");
    }
    Ok(path.to_string())
}

pub(crate) fn validate_absolute_path(path: &str) -> Result<String> {
    if path.trim().is_empty() {
        anyhow::bail!("path is empty");
    }
    let candidate = Path::new(path);
    if !candidate.is_absolute() {
        anyhow::bail!("path must be absolute");
    }
    Ok(path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_deep_link_focus_action() {
        let url = Url::parse("ctx://focus").expect("valid url");
        let action = parse_deep_link(&url).expect("focus should parse");
        assert!(matches!(action, DeepLinkAction::Focus));
    }

    #[test]
    fn parse_deep_link_open_path_with_line_col() {
        let url = Url::parse("ctx://open?path=%2Ftmp%2Fdemo.txt&line=12&col=3").expect("valid url");
        let action = parse_deep_link(&url).expect("open should parse");
        match action {
            DeepLinkAction::Open(req) => {
                assert!(matches!(req.open_with, DeepLinkOpenWith::Ctx));
                assert_eq!(req.line, Some(12));
                assert_eq!(req.col, Some(3));
                match req.target {
                    DeepLinkTarget::Path { path } => assert_eq!(path, "/tmp/demo.txt"),
                    other => panic!("expected path target, got {other:?}"),
                }
            }
            other => panic!("expected open action, got {other:?}"),
        }
    }

    #[test]
    fn parse_deep_link_open_worktree_target_takes_precedence() {
        let url = Url::parse(
            "ctx://open?worktreeId=wt_123&file=src%2Fmain.rs&path=%2Ftmp%2Fignored.txt&openWith=editor",
        )
        .expect("valid url");
        let action = parse_deep_link(&url).expect("open should parse");
        match action {
            DeepLinkAction::Open(req) => {
                assert!(matches!(req.open_with, DeepLinkOpenWith::Editor));
                match req.target {
                    DeepLinkTarget::WorktreeFile { worktree_id, file } => {
                        assert_eq!(worktree_id, "wt_123");
                        assert_eq!(file, "src/main.rs");
                    }
                    other => panic!("expected worktree target, got {other:?}"),
                }
            }
            other => panic!("expected open action, got {other:?}"),
        }
    }

    #[test]
    fn parse_deep_link_workspace_requires_workspace_id_or_path() {
        let url = Url::parse("ctx://workspace").expect("valid url");
        let err = parse_deep_link(&url).expect_err("workspace should fail without params");
        assert!(
            err.to_string().contains("workspaceId or path is required"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn parse_deep_link_task_with_optional_session() {
        let url = Url::parse(
            "ctx://task?workspaceId=workspace-1&taskId=task-1&sessionId=session-1&notificationRouteId=route-1",
        )
        .expect("valid url");
        let action = parse_deep_link(&url).expect("task should parse");
        match action {
            DeepLinkAction::Task(req) => {
                assert_eq!(req.workspace_id, "workspace-1");
                assert_eq!(req.task_id, "task-1");
                assert_eq!(req.session_id.as_deref(), Some("session-1"));
                assert_eq!(req.notification_route_id.as_deref(), Some("route-1"));
            }
            other => panic!("expected task action, got {other:?}"),
        }
    }

    #[test]
    fn parse_deep_link_task_requires_workspace_and_task() {
        let url = Url::parse("ctx://task?workspaceId=workspace-1").expect("valid url");
        let err = parse_deep_link(&url).expect_err("task should fail without taskId");
        assert!(
            err.to_string().contains("taskId is required"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn parse_deep_link_rejects_invalid_version() {
        let url = Url::parse("ctx://focus?v=2").expect("valid url");
        let err = parse_deep_link(&url).expect_err("version mismatch must fail");
        assert!(
            err.to_string().contains("unsupported version: 2"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn parse_deep_link_rejects_unknown_action() {
        let url = Url::parse("ctx://unknown").expect("valid url");
        let err = parse_deep_link(&url).expect_err("unknown action must fail");
        assert!(
            err.to_string().contains("unknown action"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn parse_deep_link_rejects_non_ctx_scheme() {
        let url = Url::parse("https://example.com").expect("valid url");
        let err = parse_deep_link(&url).expect_err("non-ctx scheme must fail");
        assert!(
            err.to_string().contains("unsupported scheme"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn validate_relative_file_rejects_unsafe_shapes() {
        let err_parent = validate_relative_file("../x").expect_err("parent dir must fail");
        assert!(
            err_parent.to_string().contains("must not contain .."),
            "unexpected error: {err_parent:#}"
        );

        let err_abs = validate_relative_file("/tmp/x").expect_err("absolute path must fail");
        assert!(
            err_abs.to_string().contains("must be relative"),
            "unexpected error: {err_abs:#}"
        );

        let err_drive = validate_relative_file("C:\\demo.txt").expect_err("drive-like path fails");
        assert!(
            err_drive.to_string().contains("must be a relative path"),
            "unexpected error: {err_drive:#}"
        );
    }

    #[test]
    fn validate_absolute_path_requires_non_empty_absolute() {
        let err_empty = validate_absolute_path(" ").expect_err("empty path must fail");
        assert!(
            err_empty.to_string().contains("path is empty"),
            "unexpected error: {err_empty:#}"
        );

        let err_rel = validate_absolute_path("tmp/x").expect_err("relative path must fail");
        assert!(
            err_rel.to_string().contains("path must be absolute"),
            "unexpected error: {err_rel:#}"
        );
    }

    #[test]
    fn parse_open_with_values() {
        assert!(matches!(
            parse_open_with(None).expect("default open_with should parse"),
            DeepLinkOpenWith::Ctx
        ));
        assert!(matches!(
            parse_open_with(Some(&"system".to_string())).expect("system should parse"),
            DeepLinkOpenWith::System
        ));
        assert!(matches!(
            parse_open_with(Some(&"Editor".to_string()))
                .expect("editor should parse case-insensitively"),
            DeepLinkOpenWith::Editor
        ));

        let err =
            parse_open_with(Some(&"invalid".to_string())).expect_err("invalid openWith must fail");
        assert!(
            err.to_string().contains("unsupported openWith"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn parse_editor_target_values() {
        assert!(matches!(
            parse_editor_target("cursor").expect("cursor should parse"),
            DesktopEditorTarget::Cursor
        ));
        assert!(matches!(
            parse_editor_target("SYSTEM").expect("system should parse"),
            DesktopEditorTarget::System
        ));

        let err = parse_editor_target("unknown-editor").expect_err("unknown editor must fail");
        assert!(
            err.to_string().contains("unknown editor"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn parse_optional_positive_filters_invalid_values() {
        assert_eq!(parse_optional_positive(Some(&"15".to_string())), Some(15));
        assert_eq!(parse_optional_positive(Some(&"0".to_string())), None);
        assert_eq!(parse_optional_positive(Some(&"-4".to_string())), None);
        assert_eq!(parse_optional_positive(Some(&"abc".to_string())), None);
        assert_eq!(parse_optional_positive(None), None);
    }
}
