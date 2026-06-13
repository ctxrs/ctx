use super::*;

pub(in crate::api::tasks::lifecycle_tests) fn git(args: &[&str], cwd: &StdPath) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

fn git_output(args: &[&str], cwd: &StdPath) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("run git output");
    assert!(output.status.success(), "git {args:?} failed");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

pub(in crate::api::tasks::lifecycle_tests) fn init_git_workspace(root: &StdPath) -> String {
    git(&["init"], root);
    git(&["symbolic-ref", "HEAD", "refs/heads/main"], root);
    git(&["config", "user.email", "ctx@example.com"], root);
    git(&["config", "user.name", "Ctx Test"], root);
    std::fs::write(root.join("README.md"), "hello\n").expect("write readme");
    git(&["add", "README.md"], root);
    git(&["commit", "-m", "initial"], root);
    git_output(&["rev-parse", "HEAD"], root)
}

pub(in crate::api::tasks::lifecycle_tests) fn create_branch_lock(
    root: &StdPath,
    branch: &str,
) -> PathBuf {
    let lock_path = root
        .join(".git")
        .join("refs")
        .join("heads")
        .join(format!("{branch}.lock"));
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent).expect("create branch lock parent");
    }
    std::fs::write(&lock_path, "").expect("create branch lock");
    lock_path
}
