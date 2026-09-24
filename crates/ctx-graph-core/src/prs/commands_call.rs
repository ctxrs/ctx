use super::*;

impl Commands<'_> {
    pub(super) fn call(&self, program: &str, args: Vec<String>) -> Result<String> {
        let left = std::time::Duration::from_secs(self.runtime.timeout_secs)
            .checked_sub(self.started.elapsed())
            .context("PR inspection timed out")?;
        ensure!(!left.is_zero(), "PR inspection timed out");
        run_command(&CommandAdapter { program: program.into(), args, output_file: false }, None, None,
            left.as_secs() + u64::from(left.subsec_nanos() > 0), self.runtime.max_output_bytes)
            .context("PR command failed; check installed gh/git and GitHub authentication (child output omitted)")
    }
    pub(super) fn git(&self, args: &[&str]) -> Result<String> {
        let cwd = self
            .runtime
            .cwd
            .canonicalize()
            .context("cannot resolve PR working directory")?;
        let cwd = cwd.to_str().context("PR working directory must be UTF-8")?;
        // CommandAdapter expands placeholders; reject them in filesystem argv.
        ensure!(
            !cwd.contains('{') && !cwd.contains('}'),
            "unsupported braces in PR working directory"
        );
        let mut argv = vec!["-C".into(), cwd.into()];
        argv.extend(args.iter().map(|s| (*s).into()));
        self.call(&self.runtime.git_program, argv)
    }
    pub(super) fn origin(&self) -> Result<String> {
        let raw = self.git(&["remote", "get-url", "origin"])?;
        let raw = raw.trim();
        let repo = raw
            .strip_prefix("https://github.com/")
            .or_else(|| raw.strip_prefix("git@github.com:"))
            .or_else(|| raw.strip_prefix("ssh://git@github.com/"))
            .context("origin is not a supported GitHub remote; use --repo OWNER/REPO")?;
        let repo = repo.strip_suffix(".git").unwrap_or(repo);
        validate_repo(repo)?;
        Ok(repo.to_owned())
    }
    pub(super) fn gh_json<T: serde::de::DeserializeOwned>(&self, args: Vec<String>) -> Result<T> {
        let raw = self.call(&self.runtime.gh_program, args)?;
        // serde's data errors may echo values from child output; deliberately omit them.
        serde_json::from_str(&raw)
            .map_err(|_| anyhow::anyhow!("gh returned invalid or unexpected JSON"))
    }
}
