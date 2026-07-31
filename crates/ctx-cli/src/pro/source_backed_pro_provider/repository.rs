use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fmt::Write as _,
    fs,
    io::Read as _,
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use ctx_pro_host_protocol::{SourceRepositoryContext, SourceWorktreeRootLocator};
use sha2::{Digest as _, Sha256};
use url::Url;
use zeroize::Zeroize as _;

const MAX_GIT_OUTPUT_BYTES: usize = 128 * 1024;
const GIT_TIMEOUT: Duration = Duration::from_secs(3);
const POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug, thiserror::Error)]
pub(crate) enum RepositoryAuthorityError {
    #[error("repository_authority_unavailable: Git repository inspection failed")]
    Inspection,
    #[error("repository_authority_stale: Git repository identity changed during inspection")]
    Stale,
}

#[derive(Debug)]
pub(super) struct RepositoryAuthority {
    git: FileWitness,
    cache: BTreeMap<PathBuf, Option<SourceRepositoryContext>>,
}

impl RepositoryAuthority {
    pub(super) fn discover() -> Option<Self> {
        let git = crate::pro::client::git_executable().ok()?;
        Some(Self {
            git: FileWitness::file(&git).ok()?,
            cache: BTreeMap::new(),
        })
    }

    pub(super) fn context_for(
        &mut self,
        cwd: Option<&str>,
    ) -> Result<Option<SourceRepositoryContext>, RepositoryAuthorityError> {
        let Some(cwd) = cwd else {
            return Ok(None);
        };
        if cwd.is_empty()
            || cwd.len() > ctx_pro_host_protocol::MAX_SOURCE_PATH_BYTES
            || cwd.contains('\0')
        {
            return Ok(None);
        }
        let cwd = Path::new(cwd);
        if !cwd.is_absolute() {
            return Ok(None);
        }
        let Ok(cwd) = cwd.canonicalize() else {
            return Ok(None);
        };
        let metadata =
            fs::symlink_metadata(&cwd).map_err(|_| RepositoryAuthorityError::Inspection)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Ok(None);
        }
        if let Some(cached) = self.cache.get(&cwd) {
            return Ok(cached.clone());
        }

        let context = inspect(&self.git, &cwd)?;
        self.cache.insert(cwd, context.clone());
        Ok(context)
    }
}

fn inspect(
    git: &FileWitness,
    cwd: &Path,
) -> Result<Option<SourceRepositoryContext>, RepositoryAuthorityError> {
    git.verify()?;
    let identity = run_git(
        git.path(),
        cwd,
        [
            "-C",
            path_text(cwd)?,
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
            "--git-common-dir",
            "--show-object-format",
        ],
    )?;
    if !identity.status.success() {
        git.verify()?;
        return Ok(None);
    }
    let identity = parse_lines(&identity.stdout)?;
    let [root, common_dir, object_format] = identity.as_slice() else {
        return Err(RepositoryAuthorityError::Inspection);
    };
    if !matches!(object_format.as_str(), "sha1" | "sha256") {
        return Err(RepositoryAuthorityError::Inspection);
    }
    let root = canonical_directory(root)?;
    let common_dir = canonical_directory(common_dir)?;
    if !cwd.starts_with(&root) {
        return Err(RepositoryAuthorityError::Inspection);
    }
    let root_witness = FileWitness::directory(&root)?;
    let checkout_witness = FileWitness::directory(&common_dir)?;
    root_witness.verify()?;
    checkout_witness.verify()?;

    let mut remotes = run_git(
        git.path(),
        &root,
        [
            "-C",
            path_text(&root)?,
            "config",
            "--local",
            "--no-includes",
            "--get-regexp",
            r"^remote\..*\.url$",
        ],
    )?;
    let repository_id = if remotes.status.success() {
        repository_id_from_remotes(&remotes.stdout)
            .unwrap_or_else(|| local_repository_id(&checkout_witness))
    } else {
        local_repository_id(&checkout_witness)
    };
    remotes.stdout.zeroize();

    root_witness.verify()?;
    checkout_witness.verify()?;
    git.verify()?;
    let root_text = root
        .to_str()
        .ok_or(RepositoryAuthorityError::Inspection)?
        .to_owned();
    let context = SourceRepositoryContext {
        repository_id,
        checkout_id: Some(identity_id("checkout", &checkout_witness)),
        worktree_id: Some(identity_id("worktree", &root_witness)),
        object_format: Some(object_format.clone()),
        worktree_root: Some(
            SourceWorktreeRootLocator::new(root_text)
                .map_err(|_| RepositoryAuthorityError::Inspection)?,
        ),
    };
    Ok(Some(context))
}

fn canonical_directory(value: &str) -> Result<PathBuf, RepositoryAuthorityError> {
    let path = Path::new(value);
    if !path.is_absolute() {
        return Err(RepositoryAuthorityError::Inspection);
    }
    let canonical = path
        .canonicalize()
        .map_err(|_| RepositoryAuthorityError::Inspection)?;
    let metadata =
        fs::symlink_metadata(&canonical).map_err(|_| RepositoryAuthorityError::Inspection)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(RepositoryAuthorityError::Inspection);
    }
    Ok(canonical)
}

fn path_text(path: &Path) -> Result<&str, RepositoryAuthorityError> {
    path.to_str().ok_or(RepositoryAuthorityError::Inspection)
}

fn parse_lines(bytes: &[u8]) -> Result<Vec<String>, RepositoryAuthorityError> {
    let text = std::str::from_utf8(bytes).map_err(|_| RepositoryAuthorityError::Inspection)?;
    let lines = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if lines.iter().any(|line| line.contains(['\0', '\r'])) {
        return Err(RepositoryAuthorityError::Inspection);
    }
    Ok(lines)
}

fn repository_id_from_remotes(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut origin = BTreeSet::new();
    let mut all = BTreeSet::new();
    for line in text.lines() {
        let (key, value) = line.split_once(char::is_whitespace)?;
        let value = canonical_forge_repository(value.trim())?;
        if key == "remote.origin.url" {
            origin.insert(value.clone());
        }
        all.insert(value);
    }
    if origin.len() == 1 {
        return origin.into_iter().next();
    }
    (all.len() == 1).then(|| all.into_iter().next()).flatten()
}

fn canonical_forge_repository(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.contains(['\0', '\n', '\r']) {
        return None;
    }
    if let Some(rest) = value.strip_prefix("git@") {
        let (host, path) = rest.split_once(':')?;
        return forge_repository(host, path);
    }
    let parsed = Url::parse(value).ok()?;
    if !matches!(parsed.scheme(), "http" | "https" | "ssh" | "git")
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return None;
    }
    forge_repository(parsed.host_str()?, parsed.path())
}

fn forge_repository(host: &str, path: &str) -> Option<String> {
    let host = host.trim().to_ascii_lowercase();
    let path = path
        .trim_matches('/')
        .strip_suffix(".git")
        .unwrap_or(path.trim_matches('/'));
    if host.is_empty()
        || !host.contains('.')
        || path.is_empty()
        || host.contains(['@', '/', '\\'])
        || path.contains(['@', '\\', '?', '#'])
        || path
            .split('/')
            .any(|component| component.is_empty() || matches!(component, "." | ".."))
        || contains_secret_marker(path)
    {
        return None;
    }
    Some(format!("forge:{host}/{path}"))
}

fn contains_secret_marker(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    [
        "token=",
        "password=",
        "passwd=",
        "secret=",
        "ghp_",
        "github_pat_",
        "glpat-",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn local_repository_id(checkout: &FileWitness) -> String {
    format!(
        "local-{}",
        digest_parts(b"ctx-pro-local-repository-v1\0", checkout.identity_bytes())
    )
}

fn identity_id(kind: &str, witness: &FileWitness) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-pro-source-repository-identity-v1\0");
    digest.update((kind.len() as u64).to_be_bytes());
    digest.update(kind.as_bytes());
    digest.update(witness.identity_bytes());
    format!("{kind}-{}", lower_hex(&digest.finalize()))
}

fn digest_parts(domain: &[u8], value: Vec<u8>) -> String {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
    lower_hex(&digest.finalize())
}

fn lower_hex(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut encoded, "{byte:02x}").expect("writing into String cannot fail");
    }
    encoded
}

struct GitOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
}

fn run_git<const N: usize>(
    executable: &Path,
    current_dir: &Path,
    args: [&str; N],
) -> Result<GitOutput, RepositoryAuthorityError> {
    let mut command = Command::new(executable);
    command
        .args(args)
        .current_dir(current_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (key, _) in std::env::vars_os().filter(|(key, _)| git_environment_key(key)) {
        command.env_remove(key);
    }
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_COUNT", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat");
    let mut child = command
        .spawn()
        .map_err(|_| RepositoryAuthorityError::Inspection)?;
    let stdout = child
        .stdout
        .take()
        .ok_or(RepositoryAuthorityError::Inspection)?;
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .take(u64::try_from(MAX_GIT_OUTPUT_BYTES + 1).unwrap_or(u64::MAX))
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let deadline = Instant::now() + GIT_TIMEOUT;
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|_| RepositoryAuthorityError::Inspection)?
        {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return Err(RepositoryAuthorityError::Inspection);
        }
        thread::sleep(POLL_INTERVAL);
    };
    let stdout = reader
        .join()
        .map_err(|_| RepositoryAuthorityError::Inspection)?
        .map_err(|_| RepositoryAuthorityError::Inspection)?;
    if stdout.len() > MAX_GIT_OUTPUT_BYTES {
        return Err(RepositoryAuthorityError::Inspection);
    }
    Ok(GitOutput { status, stdout })
}

fn git_environment_key(key: &OsStr) -> bool {
    key.to_string_lossy()
        .to_ascii_uppercase()
        .starts_with("GIT_")
}

#[derive(Debug)]
struct FileWitness {
    path: PathBuf,
    canonical: PathBuf,
    kind: WitnessKind,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(windows)]
    windows_id: (u32, u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WitnessKind {
    File,
    Directory,
}

impl FileWitness {
    fn file(path: &Path) -> Result<Self, RepositoryAuthorityError> {
        Self::new(path, WitnessKind::File)
    }

    fn directory(path: &Path) -> Result<Self, RepositoryAuthorityError> {
        Self::new(path, WitnessKind::Directory)
    }

    fn new(path: &Path, kind: WitnessKind) -> Result<Self, RepositoryAuthorityError> {
        let named = fs::symlink_metadata(path).map_err(|_| RepositoryAuthorityError::Inspection)?;
        let named_matches_kind = match kind {
            WitnessKind::File => named.is_file(),
            WitnessKind::Directory => named.is_dir(),
        };
        if !named_matches_kind || named.file_type().is_symlink() || is_reparse_point(&named) {
            return Err(RepositoryAuthorityError::Inspection);
        }
        let canonical = path
            .canonicalize()
            .map_err(|_| RepositoryAuthorityError::Inspection)?;
        let metadata =
            fs::symlink_metadata(&canonical).map_err(|_| RepositoryAuthorityError::Inspection)?;
        let matches_kind = match kind {
            WitnessKind::File => metadata.is_file(),
            WitnessKind::Directory => metadata.is_dir(),
        };
        if !matches_kind || metadata.file_type().is_symlink() || is_reparse_point(&metadata) {
            return Err(RepositoryAuthorityError::Inspection);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            Ok(Self {
                path: canonical.clone(),
                canonical,
                kind,
                device: metadata.dev(),
                inode: metadata.ino(),
            })
        }
        #[cfg(windows)]
        {
            Ok(Self {
                path: canonical.clone(),
                canonical,
                kind,
                windows_id: windows_file_identity(&canonical, kind)?,
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            Ok(Self {
                path: canonical.clone(),
                canonical,
                kind,
            })
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn verify(&self) -> Result<(), RepositoryAuthorityError> {
        let current = Self::new(&self.path, self.kind)?;
        if self.identity_bytes() == current.identity_bytes() && self.canonical == current.canonical
        {
            Ok(())
        } else {
            Err(RepositoryAuthorityError::Stale)
        }
    }

    fn identity_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(self.canonical.as_os_str().as_encoded_bytes());
        #[cfg(unix)]
        {
            bytes.extend_from_slice(&self.device.to_be_bytes());
            bytes.extend_from_slice(&self.inode.to_be_bytes());
        }
        #[cfg(windows)]
        {
            bytes.extend_from_slice(&self.windows_id.0.to_be_bytes());
            bytes.extend_from_slice(&self.windows_id.1.to_be_bytes());
        }
        bytes
    }
}

#[cfg(windows)]
fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
const fn is_reparse_point(_metadata: &fs::Metadata) -> bool {
    false
}

#[cfg(windows)]
fn windows_file_identity(
    path: &Path,
    kind: WitnessKind,
) -> Result<(u32, u64), RepositoryAuthorityError> {
    use std::{
        fs::OpenOptions,
        mem::MaybeUninit,
        os::windows::{fs::OpenOptionsExt as _, io::AsRawHandle as _},
    };
    use windows_sys::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{
            GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS,
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        },
    };

    let mut options = OpenOptions::new();
    options
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    if kind == WitnessKind::Directory {
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS);
    }
    let file = options
        .open(path)
        .map_err(|_| RepositoryAuthorityError::Inspection)?;
    let mut information = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
    if unsafe {
        GetFileInformationByHandle(file.as_raw_handle() as HANDLE, information.as_mut_ptr())
    } == 0
    {
        return Err(RepositoryAuthorityError::Inspection);
    }
    let information = unsafe { information.assume_init() };
    let index =
        (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow);
    Ok((information.dwVolumeSerialNumber, index))
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn certifies_https_repository_without_serializing_credentials() {
        let fixture = git_fixture("https://user:ghp_secret@example.com/ctxrs/repository.git");
        let git = FileWitness::file(Path::new(&git_program())).unwrap();
        let context = inspect(&git, &fixture).unwrap().unwrap();

        assert_eq!(context.repository_id, "forge:example.com/ctxrs/repository");
        assert!(context
            .checkout_id
            .as_deref()
            .unwrap()
            .starts_with("checkout-"));
        assert!(context
            .worktree_id
            .as_deref()
            .unwrap()
            .starts_with("worktree-"));
        assert!(matches!(
            context.object_format.as_deref(),
            Some("sha1" | "sha256")
        ));
        let encoded = serde_json::to_string(&context).unwrap();
        assert!(!encoded.contains("user"));
        assert!(!encoded.contains("ghp_secret"));
        assert_eq!(
            context
                .worktree_root
                .as_ref()
                .map(|locator| Path::new(&locator.absolute_path)),
            Some(fixture.as_path())
        );
    }

    #[test]
    fn missing_or_non_repository_working_directory_abstains() {
        let git = FileWitness::file(Path::new(&git_program())).unwrap();
        let temp = tempdir().unwrap();
        assert_eq!(inspect(&git, temp.path()).unwrap(), None);
    }

    #[test]
    fn ambiguous_remotes_fall_back_to_opaque_local_identity() {
        let fixture = git_fixture("https://example.com/ctxrs/one.git");
        git(&fixture, ["remote", "remove", "origin"]);
        git(
            &fixture,
            [
                "remote",
                "add",
                "first",
                "https://example.com/ctxrs/one.git",
            ],
        );
        git(
            &fixture,
            [
                "remote",
                "add",
                "second",
                "https://other.example/ctxrs/two.git",
            ],
        );
        let git = FileWitness::file(Path::new(&git_program())).unwrap();
        let context = inspect(&git, &fixture).unwrap().unwrap();
        assert!(context.repository_id.starts_with("local-"));
        assert!(!context.repository_id.contains(fixture.to_str().unwrap()));
    }

    fn git_fixture(remote: &str) -> PathBuf {
        let temp = tempdir().unwrap();
        let root = temp.keep();
        git(&root, ["init", "-q"]);
        git(&root, ["remote", "add", "origin", remote]);
        root.canonicalize().unwrap()
    }

    fn git<const N: usize>(root: &Path, args: [&str; N]) {
        let status = Command::new(git_program())
            .args(["-C", root.to_str().unwrap()])
            .args(args)
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn git_program() -> String {
        crate::pro::client::git_executable()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }
}
