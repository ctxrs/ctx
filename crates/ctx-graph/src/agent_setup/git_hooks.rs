use super::*;

pub(super) fn git(project: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(project)
        .args(args)
        .output()?;
    ensure!(
        output.status.success(),
        "Git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?
        .trim_end_matches(['\r', '\n'])
        .to_owned())
}

pub(super) fn quote(path: &Path) -> Result<String> {
    let value = path
        .to_str()
        .context("hook executable path must be UTF-8")?;
    ensure!(
        !value.contains(['\n', '\r', '\0']),
        "hook executable path contains a line break or NUL"
    );
    Ok(format!("'{}'", value.replace('\'', "'\\''")))
}

pub(super) const HOOKS: &[&str] = &["post-commit", "post-checkout", "post-merge"];

pub fn hook(args: &HookArgs) -> Result<SetupReport> {
    let (project, action) = match &args.command {
        HookCommand::Install { project } => (project, "install"),
        HookCommand::Uninstall { project } => (project, "uninstall"),
        HookCommand::Status { project } => (project, "status"),
    };
    let selected = root(project.as_deref())?;
    let scope = root(Some(Path::new(&git(
        &selected,
        &["rev-parse", "--show-toplevel"],
    )?)))?;
    let raw_hooks = PathBuf::from(git(
        &scope,
        &["rev-parse", "--path-format=absolute", "--git-path", "hooks"],
    )?);
    let mut hooks = PathBuf::new();
    for part in raw_hooks.components() {
        match part {
            std::path::Component::ParentDir => {
                hooks.pop();
            }
            std::path::Component::CurDir => {}
            part => hooks.push(part.as_os_str()),
        }
    }
    let common = PathBuf::from(git(
        &scope,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?)
    .canonicalize()?;
    check_parents(&hooks.join("post-commit"))?;
    // A project selection must not mutate a user-global hooks directory.
    ensure!(
        hooks.starts_with(&scope) || hooks.starts_with(&common),
        "hooks directory is outside this repository; global/shared core.hooksPath is not supported"
    );
    let allowed: Vec<_> = HOOKS
        .iter()
        .flat_map(|name| {
            [
                hooks.join(name),
                hooks.join(format!("{name}.ctx-graph-original")),
            ]
        })
        .collect();
    let path = receipt_path(&scope, "git", "hooks");
    let mut report = SetupReport {
        status: "not-installed".into(),
        platform: "git".into(),
        scope: scope.clone(),
        files: vec![],
        notes: vec![],
    };
    if action != "install" && read(&path)?.is_none() {
        return Ok(report);
    }
    let _lock = if action == "status" {
        None
    } else {
        Some(lock(&scope, action == "install")?)
    };
    let saved = load(&path, &scope, &allowed)?;
    if action == "status" {
        if let Some(receipt) = saved {
            report.files = receipt.changes.iter().map(|c| c.path.clone()).collect();
            report.status = if receipt.changes.iter().all(|c| {
                read(&c.path).ok().flatten().as_deref() == Some(&c.after)
                    && executable_matches(c).unwrap_or(false)
            }) {
                "installed"
            } else {
                "modified"
            }
            .into();
        }
        return Ok(report);
    }
    if action == "uninstall" {
        if let Some(receipt) = saved {
            undo(&path, &receipt)?;
            report.status = "uninstalled".into();
        }
        return Ok(report);
    }
    let receipt = if let Some(receipt) = saved {
        receipt
    } else {
        let exe = quote(&std::env::current_exe()?)?;
        let mut changes = Vec::new();
        for name in HOOKS {
            let path = hooks.join(name);
            let original = read(&path)?;
            ensure!(
                read(&hooks.join(format!("{name}.ctx-graph-original")))?.is_none(),
                "unowned hook backup already exists"
            );
            if let Some(original) = &original {
                let mut backup = planned(
                    hooks.join(format!("{name}.ctx-graph-original")),
                    original.clone(),
                    true,
                )?;
                backup.permission_source = Some(path.clone());
                changes.push(backup);
            }
            let checkout = if *name == "post-checkout" {
                "[ \"${3:-}\" = 0 ] && exit \"$graf_hook_status\"\n"
            } else {
                ""
            };
            let script = format!(
                "#!/bin/sh\n# ctx graph managed refresh hook\ngraf_hook_status=0\nif [ -x \"$0.ctx-graph-original\" ]; then\n  \"$0.ctx-graph-original\" \"$@\" || graf_hook_status=$?\nfi\n{checkout}graf_root=$(git rev-parse --show-toplevel) || exit \"$graf_hook_status\"\nif [ -f \"$graf_root/.graf/index.db\" ]; then\n  {exe} graph --db \"$graf_root/.graf/index.db\" update || printf '%s\\n' 'ctx graph: refresh failed; run ctx graph update manually' >&2\nfi\nexit \"$graf_hook_status\"\n"
            );
            let mut change = edited(path, original, script.into_bytes())?;
            change.executable = true;
            changes.push(change);
        }
        Receipt {
            version: 1,
            scope: scope.clone(),
            changes,
        }
    };
    report.files = receipt.changes.iter().map(|c| c.path.clone()).collect();
    report.status = if apply(&path, &receipt)? {
        "installed"
    } else {
        "unchanged"
    }
    .into();
    report.notes.push("Refresh runs in the foreground only when .graf/index.db exists. Existing executable hooks are chained; their exit status is preserved. Hooks never stage or commit files.".into());
    Ok(report)
}
