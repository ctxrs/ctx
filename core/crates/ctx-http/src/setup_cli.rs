use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use ctx_core::models::{VcsKind, Workspace};
use ctx_store::StoreManager;
use directories::BaseDirs;
use uuid::Uuid;

const SHIM_HEADER: &str = "# ctx-managed work capture shim v1";
const SHIM_DATA_ROOT_PREFIX: &str = "# ctx-data-root: ";
const SHIM_TOOLS: &[&str] = &["git", "gh"];

#[derive(Debug, Args)]
pub(crate) struct SetupCommand {
    #[command(subcommand)]
    pub(crate) command: SetupSubcommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum SetupSubcommand {
    /// Register a local workspace and install opt-in Work capture shims.
    Workspace(SetupWorkspaceArgs),
    /// Create an empty user-local scratch git workspace.
    Scratch(SetupScratchArgs),
    /// Show local setup state.
    Status(SetupStatusArgs),
    /// Remove ctx-owned setup shims.
    Uninstall(SetupUninstallArgs),
}

#[derive(Debug, Args, Clone)]
pub(crate) struct SetupWorkspaceArgs {
    /// ctx data root. Defaults to CTX_DATA_ROOT, then ~/.ctx.
    #[arg(long)]
    pub(crate) data_dir: Option<PathBuf>,
    /// Workspace root or a path inside a git repo.
    #[arg(default_value = ".")]
    pub(crate) path: PathBuf,
    /// Workspace display name. Defaults to the root directory name.
    #[arg(long)]
    pub(crate) name: Option<String>,
    /// Do not install git/gh capture shims.
    #[arg(long)]
    pub(crate) no_shims: bool,
}

#[derive(Debug, Args, Clone)]
pub(crate) struct SetupScratchArgs {
    /// ctx data root. Defaults to CTX_DATA_ROOT, then ~/.ctx.
    #[arg(long)]
    pub(crate) data_dir: Option<PathBuf>,
    /// Workspace display name.
    #[arg(long, default_value = "scratch")]
    pub(crate) name: String,
    /// Do not install git/gh capture shims.
    #[arg(long)]
    pub(crate) no_shims: bool,
}

#[derive(Debug, Args, Clone)]
pub(crate) struct SetupStatusArgs {
    /// ctx data root. Defaults to CTX_DATA_ROOT, then ~/.ctx.
    #[arg(long)]
    pub(crate) data_dir: Option<PathBuf>,
}

#[derive(Debug, Args, Clone)]
pub(crate) struct SetupUninstallArgs {
    /// ctx data root. Defaults to CTX_DATA_ROOT, then ~/.ctx.
    #[arg(long)]
    pub(crate) data_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkspaceRoot {
    root: PathBuf,
    vcs_kind: VcsKind,
}

pub(crate) async fn run(command: SetupCommand) -> Result<()> {
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    run_with_writer(command, &mut stdout).await
}

async fn run_with_writer(command: SetupCommand, writer: &mut dyn Write) -> Result<()> {
    match command.command {
        SetupSubcommand::Workspace(args) => setup_workspace(args, writer).await,
        SetupSubcommand::Scratch(args) => setup_scratch(args, writer).await,
        SetupSubcommand::Status(args) => setup_status(args, writer).await,
        SetupSubcommand::Uninstall(args) => setup_uninstall(args, writer).await,
    }
}

async fn setup_workspace(args: SetupWorkspaceArgs, writer: &mut dyn Write) -> Result<()> {
    let data_root = resolve_data_root(args.data_dir.as_deref())?;
    let root = workspace_root_for_path(&args.path)?;
    let manager = StoreManager::open(&data_root)
        .await
        .with_context(|| format!("opening ctx store at {}", data_root.display()))?;
    let workspace = register_or_reuse_workspace(
        &manager,
        root.root.clone(),
        args.name.as_deref(),
        root.vcs_kind,
    )
    .await?;
    manager.workspace(workspace.id).await?;
    let shim_result = if args.no_shims {
        ShimInstallResult::Skipped
    } else {
        install_shims(&data_root)?
    };

    writeln!(writer, "workspace: {}", workspace.id.0)?;
    writeln!(writer, "name: {}", workspace.name)?;
    writeln!(writer, "root: {}", workspace.root_path)?;
    writeln!(writer, "data_root: {}", data_root.display())?;
    write_shim_status(writer, &data_root, &shim_result)?;
    Ok(())
}

async fn setup_scratch(args: SetupScratchArgs, writer: &mut dyn Write) -> Result<()> {
    let data_root = resolve_data_root(args.data_dir.as_deref())?;
    let root = create_scratch_git_workspace(&data_root)?;
    let manager = StoreManager::open(&data_root)
        .await
        .with_context(|| format!("opening ctx store at {}", data_root.display()))?;
    let workspace =
        register_or_reuse_workspace(&manager, root.clone(), Some(&args.name), VcsKind::Git).await?;
    manager.workspace(workspace.id).await?;
    let shim_result = if args.no_shims {
        ShimInstallResult::Skipped
    } else {
        install_shims(&data_root)?
    };

    writeln!(writer, "workspace: {}", workspace.id.0)?;
    writeln!(writer, "name: {}", workspace.name)?;
    writeln!(writer, "root: {}", workspace.root_path)?;
    writeln!(writer, "data_root: {}", data_root.display())?;
    writeln!(writer, "scratch: true")?;
    write_shim_status(writer, &data_root, &shim_result)?;
    Ok(())
}

async fn setup_status(args: SetupStatusArgs, writer: &mut dyn Write) -> Result<()> {
    let data_root = resolve_data_root(args.data_dir.as_deref())?;
    let manager = StoreManager::open(&data_root)
        .await
        .with_context(|| format!("opening ctx store at {}", data_root.display()))?;
    let workspaces = manager.global().list_workspaces().await?;
    writeln!(writer, "data_root: {}", data_root.display())?;
    writeln!(writer, "workspaces: {}", workspaces.len())?;
    for workspace in workspaces {
        writeln!(
            writer,
            "- {} {} ({})",
            workspace.id.0, workspace.name, workspace.root_path
        )?;
    }
    for tool in SHIM_TOOLS {
        let path = shim_path(&data_root, tool);
        let state = if is_ctx_owned_shim(&path)? {
            "installed"
        } else if path.exists() {
            "not-owned"
        } else {
            "missing"
        };
        writeln!(writer, "shim.{tool}: {state} ({})", path.display())?;
    }
    Ok(())
}

async fn setup_uninstall(args: SetupUninstallArgs, writer: &mut dyn Write) -> Result<()> {
    let data_root = resolve_data_root(args.data_dir.as_deref())?;
    let removed = uninstall_shims(&data_root)?;
    writeln!(writer, "data_root: {}", data_root.display())?;
    writeln!(writer, "removed_shims: {}", removed.removed)?;
    writeln!(writer, "missing_shims: {}", removed.missing)?;
    writeln!(writer, "skipped_not_owned: {}", removed.skipped_not_owned)?;
    Ok(())
}

fn resolve_data_root(data_dir: Option<&Path>) -> Result<PathBuf> {
    let raw = match data_dir {
        Some(path) => path.to_path_buf(),
        None => match std::env::var("CTX_DATA_ROOT") {
            Ok(value) if !value.trim().is_empty() => PathBuf::from(value),
            _ => {
                let base = BaseDirs::new().context("resolving home dir")?;
                base.home_dir().join(".ctx")
            }
        },
    };
    ctx_http_auth::daemon::prepare_daemon_data_root(raw)
}

fn workspace_root_for_path(path: &Path) -> Result<WorkspaceRoot> {
    let canonical = fs::canonicalize(path)
        .with_context(|| format!("resolving workspace path {}", path.display()))?;
    if let Some(root) = git_root_for_path(&canonical)? {
        return Ok(WorkspaceRoot {
            root,
            vcs_kind: VcsKind::Git,
        });
    }
    Ok(WorkspaceRoot {
        root: canonical,
        vcs_kind: VcsKind::Other,
    })
}

fn git_root_for_path(path: &Path) -> Result<Option<PathBuf>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .arg("rev-parse")
        .arg("--show-toplevel")
        .output();
    let output = match output {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).context("running git rev-parse"),
    };
    if !output.status.success() {
        return Ok(None);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let root = stdout.trim();
    if root.is_empty() {
        return Ok(None);
    }
    Ok(Some(PathBuf::from(root)))
}

async fn register_or_reuse_workspace(
    manager: &StoreManager,
    root: PathBuf,
    name: Option<&str>,
    vcs_kind: VcsKind,
) -> Result<Workspace> {
    let root = normalize_path_string(&root)?;
    let workspaces = manager.global().list_workspaces().await?;
    if let Some(existing) = workspaces
        .into_iter()
        .find(|workspace| same_path_string(&workspace.root_path, &root))
    {
        return Ok(existing);
    }
    let name = name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| default_workspace_name(Path::new(&root)));
    manager
        .global()
        .create_workspace(name, root, vcs_kind)
        .await
}

fn create_scratch_git_workspace(data_root: &Path) -> Result<PathBuf> {
    let root = data_root
        .join("workspaces")
        .join("scratch")
        .join(format!("scratch-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;
    let status = Command::new("git")
        .arg("init")
        .arg("--quiet")
        .arg(&root)
        .status()
        .context("running git init for scratch workspace")?;
    if !status.success() {
        bail!("git init failed for scratch workspace {}", root.display());
    }
    Ok(root)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShimInstallResult {
    Installed,
    Skipped,
}

#[derive(Debug, Default)]
struct ShimUninstallResult {
    removed: usize,
    missing: usize,
    skipped_not_owned: usize,
}

fn install_shims(data_root: &Path) -> Result<ShimInstallResult> {
    let bin_dir = data_root.join("bin");
    fs::create_dir_all(&bin_dir).with_context(|| format!("creating {}", bin_dir.display()))?;
    let ctx_exe = std::env::current_exe().context("resolving current ctx executable")?;
    for tool in SHIM_TOOLS {
        let path = shim_path(data_root, tool);
        if path.exists() && !is_ctx_owned_shim(&path)? {
            bail!(
                "refusing to overwrite non-ctx file at {}; remove it or choose another data root",
                path.display()
            );
        }
        write_owned_shim(&path, &shim_script(&ctx_exe, data_root, tool)?)
            .with_context(|| format!("writing {}", path.display()))?;
        make_executable(&path)?;
    }
    Ok(ShimInstallResult::Installed)
}

fn uninstall_shims(data_root: &Path) -> Result<ShimUninstallResult> {
    let mut result = ShimUninstallResult::default();
    for tool in SHIM_TOOLS {
        let path = shim_path(data_root, tool);
        if !path.exists() {
            result.missing += 1;
            continue;
        }
        if !is_ctx_owned_shim(&path)? {
            result.skipped_not_owned += 1;
            continue;
        }
        fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
        result.removed += 1;
    }
    Ok(result)
}

fn write_shim_status(
    writer: &mut dyn Write,
    data_root: &Path,
    result: &ShimInstallResult,
) -> Result<()> {
    match result {
        ShimInstallResult::Installed => {
            writeln!(writer, "shims: installed")?;
            writeln!(writer, "shim_dir: {}", data_root.join("bin").display())?;
            writeln!(
                writer,
                "activate: export PATH={}:$PATH",
                shell_quote(&data_root.join("bin"))
            )?;
        }
        ShimInstallResult::Skipped => {
            writeln!(writer, "shims: skipped")?;
        }
    }
    Ok(())
}

fn shim_path(data_root: &Path, tool: &str) -> PathBuf {
    data_root.join("bin").join(tool)
}

fn write_owned_shim(path: &Path, script: &str) -> Result<()> {
    if path
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        bail!(
            "refusing to overwrite symlink at {}; remove it or choose another data root",
            path.display()
        );
    }
    let temp_path = path.with_extension(format!("tmp-{}", Uuid::new_v4()));
    fs::write(&temp_path, script).with_context(|| format!("writing {}", temp_path.display()))?;
    fs::rename(&temp_path, path)
        .with_context(|| format!("moving {} into place", path.display()))?;
    Ok(())
}

fn shim_script(ctx_exe: &Path, data_root: &Path, tool: &str) -> Result<String> {
    Ok(format!(
        "#!/bin/sh\n\
         {SHIM_HEADER}\n\
         {SHIM_DATA_ROOT_PREFIX}{data_root}\n\
         case \"$0\" in */*) _ctx_script_dir=${{0%/*}} ;; *) _ctx_script_dir=. ;; esac\n\
         CTX_WORK_SHIM_DIR=$(CDPATH= cd -- \"$_ctx_script_dir\" && pwd)\n\
         export CTX_WORK_SHIM_DIR\n\
         _ctx_status=0\n\
         _ctx_path_without_shim=\"\"\n\
         _ctx_old_ifs=$IFS\n\
         IFS=:\n\
         for _ctx_part in $PATH; do\n\
           [ -n \"$_ctx_part\" ] || _ctx_part=.\n\
           [ \"$_ctx_part\" = \"$CTX_WORK_SHIM_DIR\" ] && continue\n\
           if [ -z \"$_ctx_path_without_shim\" ]; then _ctx_path_without_shim=$_ctx_part; else _ctx_path_without_shim=$_ctx_path_without_shim:$_ctx_part; fi\n\
         done\n\
         IFS=$_ctx_old_ifs\n\
         _ctx_real=$(PATH=\"$_ctx_path_without_shim\" command -v {tool} || true)\n\
         if [ -z \"$_ctx_real\" ]; then echo \"ctx shim: real {tool} not found\" >&2; exit 127; fi\n\
         \"$_ctx_real\" \"$@\"\n\
         _ctx_status=$?\n\
         (\n\
           _ctx_first=1\n\
           for _ctx_arg in \"$@\"; do\n\
             [ \"$_ctx_first\" = 1 ] || printf '\\0'\n\
             _ctx_first=0\n\
             printf '%s' \"$_ctx_arg\"\n\
           done\n\
         ) | PATH=\"$_ctx_path_without_shim\" CTX_WORK_SHIM_DIR= {ctx} work capture command --data-dir {data_root} --tool {tool} --exit-code \"$_ctx_status\" --cwd \"$PWD\" --argv0-stdin >/dev/null 2>/dev/null || true\n\
         exit \"$_ctx_status\"\n",
        ctx = shell_quote(ctx_exe),
        data_root = shell_quote(data_root)
    ))
}

fn is_ctx_owned_shim(path: &Path) -> Result<bool> {
    let metadata = match path.symlink_metadata() {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
    };
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Ok(false);
    }
    let contents =
        fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(contents.lines().next() == Some("#!/bin/sh")
        && contents.lines().nth(1) == Some(SHIM_HEADER)
        && contents
            .lines()
            .nth(2)
            .is_some_and(|line| line.starts_with(SHIM_DATA_ROOT_PREFIX))
        && contents.contains("work capture command")
        && contents.contains("CTX_WORK_SHIM_DIR"))
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)
        .with_context(|| format!("reading {}", path.display()))?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
        .with_context(|| format!("marking {} executable", path.display()))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

fn normalize_path_string(path: &Path) -> Result<String> {
    Ok(path
        .canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_string())
}

fn same_path_string(a: &str, b: &str) -> bool {
    Path::new(a)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(a))
        == Path::new(b)
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from(b))
}

fn default_workspace_name(root: &Path) -> String {
    root.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("workspace")
        .to_string()
}

fn shell_quote(path: &Path) -> String {
    let value = path.to_string_lossy();
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn setup_workspace_registers_existing_git_root_and_reuses_it() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_git_repo(repo.path());
        let nested = repo.path().join("a/b");
        fs::create_dir_all(&nested).unwrap();
        let mut output = Vec::new();

        run_with_writer(
            SetupCommand {
                command: SetupSubcommand::Workspace(SetupWorkspaceArgs {
                    data_dir: Some(data.path().to_path_buf()),
                    path: nested,
                    name: Some("demo".to_string()),
                    no_shims: true,
                }),
            },
            &mut output,
        )
        .await
        .unwrap();
        run_with_writer(
            SetupCommand {
                command: SetupSubcommand::Workspace(SetupWorkspaceArgs {
                    data_dir: Some(data.path().to_path_buf()),
                    path: repo.path().to_path_buf(),
                    name: Some("ignored".to_string()),
                    no_shims: true,
                }),
            },
            &mut Vec::new(),
        )
        .await
        .unwrap();

        let manager = StoreManager::open(data.path()).await.unwrap();
        let workspaces = manager.global().list_workspaces().await.unwrap();
        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].name, "demo");
        assert!(String::from_utf8(output)
            .unwrap()
            .contains("shims: skipped"));
    }

    #[tokio::test]
    async fn setup_scratch_creates_user_local_git_workspace() {
        let data = TempDir::new().unwrap();
        let mut output = Vec::new();

        run_with_writer(
            SetupCommand {
                command: SetupSubcommand::Scratch(SetupScratchArgs {
                    data_dir: Some(data.path().to_path_buf()),
                    name: "scratch".to_string(),
                    no_shims: true,
                }),
            },
            &mut output,
        )
        .await
        .unwrap();

        let manager = StoreManager::open(data.path()).await.unwrap();
        let workspaces = manager.global().list_workspaces().await.unwrap();
        assert_eq!(workspaces.len(), 1);
        assert!(Path::new(&workspaces[0].root_path).join(".git").exists());
        assert!(workspaces[0]
            .root_path
            .contains("/workspaces/scratch/scratch-"));
    }

    #[test]
    fn shims_are_idempotent_and_uninstall_only_owned_files() {
        let data = TempDir::new().unwrap();
        install_shims(data.path()).unwrap();
        install_shims(data.path()).unwrap();
        assert!(is_ctx_owned_shim(&shim_path(data.path(), "git")).unwrap());

        let result = uninstall_shims(data.path()).unwrap();
        assert_eq!(result.removed, 2);
        let result = uninstall_shims(data.path()).unwrap();
        assert_eq!(result.missing, 2);

        let git_path = shim_path(data.path(), "git");
        fs::create_dir_all(git_path.parent().unwrap()).unwrap();
        fs::write(&git_path, "#!/bin/sh\nexit 0\n").unwrap();
        let result = uninstall_shims(data.path()).unwrap();
        assert_eq!(result.skipped_not_owned, 1);
        assert!(git_path.exists());
    }

    #[test]
    fn shim_ownership_rejects_marker_spoofing() {
        let data = TempDir::new().unwrap();
        let git_path = shim_path(data.path(), "git");
        fs::create_dir_all(git_path.parent().unwrap()).unwrap();
        fs::write(
            &git_path,
            format!("#!/bin/sh\n# random file mentioning {SHIM_HEADER}\nexit 0\n"),
        )
        .unwrap();

        assert!(!is_ctx_owned_shim(&git_path).unwrap());
        let error = install_shims(data.path()).unwrap_err().to_string();
        assert!(error.contains("refusing to overwrite non-ctx file"));
    }

    #[cfg(unix)]
    #[test]
    fn shim_install_refuses_symlink_targets() {
        use std::os::unix::fs::symlink;

        let data = TempDir::new().unwrap();
        let outside = data.path().join("outside");
        fs::write(&outside, "outside").unwrap();
        let git_path = shim_path(data.path(), "git");
        fs::create_dir_all(git_path.parent().unwrap()).unwrap();
        symlink(&outside, &git_path).unwrap();

        assert!(!is_ctx_owned_shim(&git_path).unwrap());
        let error = install_shims(data.path()).unwrap_err().to_string();
        assert!(error.contains("refusing to overwrite non-ctx file"));
    }

    #[test]
    fn shim_script_preserves_status_and_records_best_effort() {
        let script =
            shim_script(Path::new("/usr/bin/ctx"), Path::new("/tmp/ctx data"), "git").unwrap();
        assert!(script.contains(SHIM_HEADER));
        assert!(script.contains("# ctx-data-root: '/tmp/ctx data'"));
        assert!(script.contains("\"$_ctx_real\" \"$@\""));
        assert!(script.contains("_ctx_status=$?"));
        assert!(script.contains("PATH=\"$_ctx_path_without_shim\""));
        assert!(script.contains("work capture command --data-dir '/tmp/ctx data' --tool git"));
        assert!(script.contains("--argv0-stdin"));
        assert!(!script.contains("-- \"$@\""));
        assert!(script.contains("|| true"));
        assert!(script.contains("exit \"$_ctx_status\""));
    }

    #[cfg(unix)]
    #[test]
    fn executable_shim_scrubs_path_preserves_status_and_sends_argv_on_stdin() {
        let bash = Path::new("/usr/bin/bash");
        if !bash.exists() {
            eprintln!("skipping executable shim stdin test; /usr/bin/bash is unavailable");
            return;
        }
        let temp = TempDir::new().unwrap();
        let data_root = temp.path().join("ctx data");
        let shim_dir = data_root.join("bin");
        let real_bin = temp.path().join("real-bin");
        fs::create_dir_all(&shim_dir).unwrap();
        fs::create_dir_all(&real_bin).unwrap();

        let fake_git_log = temp.path().join("fake-git.log");
        let fake_ctx_log = temp.path().join("fake-ctx.log");
        let fake_ctx_stdin = temp.path().join("fake-ctx.stdin");
        let fake_git = real_bin.join("git");
        fs::write(
            &fake_git,
            "#!/bin/sh\nprintf 'PATH=%s\\n' \"$PATH\" > \"$CTX_FAKE_GIT_LOG\"\nexit 17\n",
        )
        .unwrap();
        make_executable(&fake_git).unwrap();
        let fake_ctx = temp.path().join("fake ctx");
        fs::write(
            &fake_ctx,
            format!(
                "#!{}\nprintf 'PATH=%s\\n' \"$PATH\" > \"$CTX_FAKE_CTX_LOG\"\nprintf 'ARGS=%s\\n' \"$*\" >> \"$CTX_FAKE_CTX_LOG\"\n/bin/cat > \"$CTX_FAKE_CTX_STDIN\"\nexit 0\n",
                bash.display()
            ),
        )
        .unwrap();
        make_executable(&fake_ctx).unwrap();
        let shim = shim_dir.join("git");
        write_owned_shim(&shim, &shim_script(&fake_ctx, &data_root, "git").unwrap()).unwrap();
        make_executable(&shim).unwrap();

        let path = format!("{}:{}", shim_dir.display(), real_bin.display());
        let output = Command::new(&shim)
            .arg("status")
            .arg("--token")
            .arg("super-secret")
            .env("PATH", path)
            .env("CTX_FAKE_GIT_LOG", &fake_git_log)
            .env("CTX_FAKE_CTX_LOG", &fake_ctx_log)
            .env("CTX_FAKE_CTX_STDIN", &fake_ctx_stdin)
            .current_dir(temp.path())
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(17));
        let ctx_log = fs::read_to_string(&fake_ctx_log).unwrap();
        assert!(ctx_log.contains(&format!("--data-dir {}", data_root.display())));
        assert!(ctx_log.contains("--tool git"));
        assert!(ctx_log.contains("--exit-code 17"));
        assert!(ctx_log.contains("--argv0-stdin"));
        assert!(!ctx_log.contains("super-secret"));
        let path_line = ctx_log
            .lines()
            .find(|line| line.starts_with("PATH="))
            .unwrap();
        assert!(!path_line.contains(&shim_dir.to_string_lossy().to_string()));
        assert!(path_line.contains(&real_bin.to_string_lossy().to_string()));
        let captured_argv = fs::read(&fake_ctx_stdin).unwrap();
        assert_eq!(captured_argv, b"status\0--token\0super-secret");
    }

    #[cfg(unix)]
    #[test]
    fn executable_shim_reports_missing_real_tool_without_capture() {
        let temp = TempDir::new().unwrap();
        let data_root = temp.path().join("ctx");
        let shim_dir = data_root.join("bin");
        fs::create_dir_all(&shim_dir).unwrap();
        let fake_ctx_log = temp.path().join("fake-ctx.log");
        let fake_ctx = temp.path().join("fake-ctx");
        fs::write(
            &fake_ctx,
            "#!/bin/sh\nprintf 'unexpected capture\\n' > \"$CTX_FAKE_CTX_LOG\"\nexit 0\n",
        )
        .unwrap();
        make_executable(&fake_ctx).unwrap();
        let shim = shim_dir.join("git");
        write_owned_shim(&shim, &shim_script(&fake_ctx, &data_root, "git").unwrap()).unwrap();
        make_executable(&shim).unwrap();

        let output = Command::new(&shim)
            .env("PATH", &shim_dir)
            .env("CTX_FAKE_CTX_LOG", &fake_ctx_log)
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(127));
        assert!(String::from_utf8_lossy(&output.stderr).contains("real git not found"));
        assert!(!fake_ctx_log.exists());
    }

    fn init_git_repo(path: &Path) {
        let status = Command::new("git")
            .arg("init")
            .arg("--quiet")
            .arg(path)
            .status()
            .unwrap();
        assert!(status.success());
    }
}
