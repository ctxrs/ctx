use std::{
    fmt::Write as _,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    mem::{size_of, MaybeUninit},
    os::windows::{fs::OpenOptionsExt as _, io::AsRawHandle as _},
    path::{Path, PathBuf},
    process,
};

use anyhow::{anyhow, Context, Result};
use ring::digest::{Context as DigestContext, SHA256};
use serde_json::Value;
use windows_sys::Win32::{
    Foundation::{HANDLE, HMODULE},
    Storage::FileSystem::{
        FileIdInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
        GetFinalPathNameByHandleW, BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO, FILE_SHARE_READ,
        VOLUME_NAME_NT,
    },
    System::ProcessStatus::{EnumProcessModules, GetMappedFileNameW},
};

use super::process_handle::{WindowsProcess, WindowsProcessAccess};
use crate::process_executable_sha256;

pub(super) const OFFICIAL_V025_WINDOWS_X64_SHA256: &str =
    "32aa550cc5c56d4d2989d0f929bbc1e634d8b730219feb8e4a4ba770b02a9867";

pub(super) fn verify_lock_paths(
    value: &Value,
    data_root: &Path,
    expected_executable: &Path,
) -> Result<()> {
    let recorded_root = value
        .get("data_root")
        .and_then(Value::as_str)
        .map(Path::new)
        .ok_or_else(|| anyhow!("ctx daemon lock has no data-root identity"))?;
    if !same_windows_path(recorded_root, data_root) {
        return Err(anyhow!(
            "ctx daemon lock data-root identity does not match uninstall target"
        ));
    }
    let recorded_binary = value
        .get("binary")
        .and_then(Value::as_str)
        .map(Path::new)
        .ok_or_else(|| anyhow!("ctx daemon lock has no executable identity"))?;
    if !same_windows_path(recorded_binary, expected_executable) {
        return Err(anyhow!(
            "ctx daemon lock executable is not the installed ctx executable"
        ));
    }
    Ok(())
}

pub(super) fn verify_recorded_digest_identity(pid: u32, value: &Value) -> Result<()> {
    let recorded_sha256 = value
        .get("binary_sha256")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("ctx daemon lock has no executable digest identity"))?;
    let process_sha256 = process_executable_sha256(pid).ok_or_else(|| {
        anyhow!(
            "cannot verify executable image for residual ctx process {pid}; refusing to terminate"
        )
    })?;
    if process_sha256 != recorded_sha256 {
        return Err(anyhow!(
            "residual lock owner image does not match its held ctx daemon lock; refusing to terminate"
        ));
    }
    Ok(())
}

pub(super) enum LegacyProcessImageProof {
    Exited,
    Retained(RetainedProcessImage),
}

impl LegacyProcessImageProof {
    pub(super) fn verify(
        target: &WindowsProcess,
        expected_executable: &Path,
        known_legacy_sha256: &str,
    ) -> Result<Self> {
        if !target.is_running()? {
            return Ok(Self::Exited);
        }
        match RetainedProcessImage::verify(target, expected_executable, known_legacy_sha256) {
            Ok(proof) => Ok(Self::Retained(proof)),
            Err(_) if !target.is_running()? => Ok(Self::Exited),
            Err(error) => Err(error),
        }
    }

    /// Returns false only when the same retained process object has exited.
    pub(super) fn recheck(
        &mut self,
        target: &WindowsProcess,
        expected_executable: &Path,
    ) -> Result<bool> {
        if matches!(self, Self::Exited) || !target.is_running()? {
            return Ok(false);
        }
        let Self::Retained(proof) = self else {
            unreachable!();
        };
        match proof.recheck(target, expected_executable) {
            Ok(()) => Ok(true),
            Err(_) if !target.is_running()? => Ok(false),
            Err(error) => Err(error),
        }
    }
}

pub(super) struct RetainedProcessImage {
    observed_path: PathBuf,
    renamed: bool,
    image: RetainedImage,
    known_sha256: String,
}

impl RetainedProcessImage {
    fn verify(
        target: &WindowsProcess,
        expected_executable: &Path,
        known_sha256: &str,
    ) -> Result<Self> {
        let observed_path = process_image_path(target)?;
        let renamed = !same_windows_path(&observed_path, expected_executable);
        if renamed {
            verify_adjacent_rename(&observed_path, expected_executable)?;
            let current = WindowsProcess::open(process::id(), WindowsProcessAccess::Observe)?
                .ok_or_else(|| anyhow!("current ctx candidate is not running"))?;
            verify_exact_process_path(&current, expected_executable)
                .context("bind installed ctx.exe to the current uninstall candidate")?;
        }
        #[cfg(test)]
        swap_observed_image_for_test(&observed_path)?;

        let mut image = RetainedImage::open(&observed_path, target)
            .context("retain legacy ctx main-image file identity")?;
        if image.sha256 != known_sha256 {
            return Err(anyhow!(
                "residual image is not the published ctx v0.25.0 Windows artifact; refusing to terminate"
            ));
        }
        image.recheck(&observed_path, target)?;
        Ok(Self {
            observed_path,
            renamed,
            image,
            known_sha256: known_sha256.to_owned(),
        })
    }

    fn recheck(&mut self, target: &WindowsProcess, expected_executable: &Path) -> Result<()> {
        let current_path = process_image_path(target)?;
        if !same_windows_path(&current_path, &self.observed_path) {
            return Err(anyhow!(
                "residual ctx process image path changed during legacy verification"
            ));
        }
        if self.renamed {
            verify_adjacent_rename(&self.observed_path, expected_executable)?;
            let current = WindowsProcess::open(process::id(), WindowsProcessAccess::Observe)?
                .ok_or_else(|| anyhow!("current ctx candidate is not running"))?;
            verify_exact_process_path(&current, expected_executable)
                .context("recheck installed ctx.exe current-self identity")?;
        } else if !same_windows_path(&self.observed_path, expected_executable) {
            return Err(anyhow!("legacy ctx executable relationship changed"));
        }
        self.image.recheck(&self.observed_path, target)?;
        if self.image.sha256 != self.known_sha256 {
            return Err(anyhow!(
                "retained legacy ctx image changed before termination; refusing to terminate"
            ));
        }
        Ok(())
    }
}

fn process_image_path(target: &WindowsProcess) -> Result<PathBuf> {
    target.executable_path().ok_or_else(|| {
        anyhow!(
            "cannot verify executable path for residual ctx process {}; refusing to terminate",
            target.pid
        )
    })
}

fn verify_adjacent_rename(observed_path: &Path, expected_executable: &Path) -> Result<()> {
    if !expected_executable
        .file_name()
        .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("ctx.exe"))
    {
        return Err(anyhow!(
            "legacy renamed-image compatibility is restricted to installed ctx.exe"
        ));
    }
    let observed_parent = observed_path
        .parent()
        .ok_or_else(|| anyhow!("renamed legacy ctx image has no install directory"))?;
    let expected_parent = expected_executable
        .parent()
        .ok_or_else(|| anyhow!("installed ctx executable has no install directory"))?;
    if !same_windows_path(observed_parent, expected_parent) {
        return Err(anyhow!(
            "renamed residual ctx image is not adjacent to installed ctx.exe; refusing to terminate"
        ));
    }
    Ok(())
}

fn verify_exact_process_path(process: &WindowsProcess, expected: &Path) -> Result<()> {
    let observed = process_image_path(process)?;
    if !same_windows_path(&observed, expected) {
        return Err(anyhow!(
            "process image path {observed:?} does not match expected path {expected:?}"
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity {
    volume: u64,
    id: [u8; 16],
    links: u32,
}

struct RetainedImage {
    file: File,
    mapped_file: File,
    identity: FileIdentity,
    nt_path: String,
    sha256: String,
}

impl RetainedImage {
    fn open(path: &Path, target: &WindowsProcess) -> Result<Self> {
        let mut file = open_image(path)?;
        let identity = file_identity(&file)?;
        if identity.links != 1 {
            return Err(anyhow!(
                "legacy ctx image has {} hard links; refusing to terminate",
                identity.links
            ));
        }
        let nt_path = final_nt_path(&file)?;
        let mapped_path = mapped_main_image_nt_path(target)?;
        let mapped_file = open_mapped_image(&mapped_path)?;
        if file_identity(&mapped_file)? != identity {
            return Err(anyhow!(
                "residual process main mapped image file ID does not match retained legacy image"
            ));
        }
        if !mapped_path.eq_ignore_ascii_case(&nt_path) {
            return Err(anyhow!(
                "residual process main mapped image is outside the retained legacy image path scope"
            ));
        }
        let sha256 = hash_file(&mut file)?;
        let proof = Self {
            file,
            mapped_file,
            identity,
            nt_path,
            sha256,
        };
        proof.verify_named_file(path)?;
        Ok(proof)
    }

    fn recheck(&mut self, path: &Path, target: &WindowsProcess) -> Result<()> {
        if file_identity(&self.file)? != self.identity || final_nt_path(&self.file)? != self.nt_path
        {
            return Err(anyhow!("retained legacy ctx file identity changed"));
        }
        if file_identity(&self.mapped_file)? != self.identity {
            return Err(anyhow!("retained mapped main-image file identity changed"));
        }
        self.verify_named_file(path)?;
        self.recheck_mapped_main_image(target)?;
        let sha256 = hash_file(&mut self.file)?;
        if sha256 != self.sha256 {
            return Err(anyhow!("retained legacy ctx image bytes changed"));
        }
        Ok(())
    }

    fn verify_named_file(&self, path: &Path) -> Result<()> {
        let named = open_image(path)?;
        if file_identity(&named)? != self.identity || final_nt_path(&named)? != self.nt_path {
            return Err(anyhow!(
                "legacy ctx image path no longer names the retained mapped file"
            ));
        }
        Ok(())
    }

    fn recheck_mapped_main_image(&self, target: &WindowsProcess) -> Result<()> {
        let mapped = mapped_main_image_nt_path(target)?;
        if !mapped.eq_ignore_ascii_case(&self.nt_path) {
            return Err(anyhow!(
                "residual process main mapped image left the retained legacy path scope"
            ));
        }
        let current = open_mapped_image(&mapped)?;
        if file_identity(&current)? != self.identity {
            return Err(anyhow!(
                "residual process main mapped image file ID changed before termination"
            ));
        }
        Ok(())
    }
}

fn open_image(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .with_context(|| format!("open retained legacy ctx image {}", path.display()))?;
    let mut information = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
    if unsafe {
        GetFileInformationByHandle(file.as_raw_handle() as HANDLE, information.as_mut_ptr())
    } == 0
    {
        return Err(std::io::Error::last_os_error()).context("inspect retained image attributes");
    }
    let attributes = unsafe { information.assume_init() }.dwFileAttributes;
    if attributes & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DIRECTORY) != 0 {
        return Err(anyhow!(
            "legacy ctx image is a reparse point or directory; refusing to terminate"
        ));
    }
    Ok(file)
}

fn open_mapped_image(nt_path: &str) -> Result<File> {
    let device_prefix = r"\Device\HarddiskVolume";
    if nt_path.len() < device_prefix.len()
        || !nt_path[..device_prefix.len()].eq_ignore_ascii_case(device_prefix)
    {
        return Err(anyhow!(
            "residual process main mapped image is not on a local Windows volume"
        ));
    }
    let globalroot = PathBuf::from(format!(r"\\?\GLOBALROOT{nt_path}"));
    open_image(&globalroot).context("open residual process mapped main-image object")
}

fn file_identity(file: &File) -> Result<FileIdentity> {
    let mut id = MaybeUninit::<FILE_ID_INFO>::zeroed();
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as HANDLE,
            FileIdInfo,
            id.as_mut_ptr().cast(),
            u32::try_from(size_of::<FILE_ID_INFO>()).expect("FILE_ID_INFO size fits u32"),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error()).context("read retained image FILE_ID_INFO");
    }
    let mut standard = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
    if unsafe { GetFileInformationByHandle(file.as_raw_handle() as HANDLE, standard.as_mut_ptr()) }
        == 0
    {
        return Err(std::io::Error::last_os_error()).context("read retained image link count");
    }
    let id = unsafe { id.assume_init() };
    Ok(FileIdentity {
        volume: id.VolumeSerialNumber,
        id: id.FileId.Identifier,
        links: unsafe { standard.assume_init() }.nNumberOfLinks,
    })
}

fn final_nt_path(file: &File) -> Result<String> {
    let mut buffer = vec![0_u16; 32_768];
    let length = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle() as HANDLE,
            buffer.as_mut_ptr(),
            u32::try_from(buffer.len()).expect("NT path buffer fits u32"),
            VOLUME_NAME_NT,
        )
    };
    if length == 0 || usize::try_from(length).unwrap_or(usize::MAX) >= buffer.len() {
        return Err(std::io::Error::last_os_error()).context("read retained image NT path");
    }
    Ok(String::from_utf16_lossy(
        &buffer[..usize::try_from(length).expect("NT path length fits usize")],
    ))
}

fn mapped_main_image_nt_path(target: &WindowsProcess) -> Result<String> {
    let mut module: HMODULE = std::ptr::null_mut();
    let mut needed = 0_u32;
    if unsafe {
        EnumProcessModules(
            target.handle,
            &raw mut module,
            u32::try_from(size_of::<HMODULE>()).expect("HMODULE size fits u32"),
            &raw mut needed,
        )
    } == 0
        || module.is_null()
        || needed < u32::try_from(size_of::<HMODULE>()).expect("HMODULE size fits u32")
    {
        return Err(std::io::Error::last_os_error())
            .context("enumerate residual process main module");
    }
    let mut buffer = vec![0_u16; 32_768];
    let length = unsafe {
        GetMappedFileNameW(
            target.handle,
            module.cast_const(),
            buffer.as_mut_ptr(),
            u32::try_from(buffer.len()).expect("mapped path buffer fits u32"),
        )
    };
    if length == 0 || usize::try_from(length).unwrap_or(usize::MAX) >= buffer.len() {
        return Err(std::io::Error::last_os_error())
            .context("read residual process main mapped-file path");
    }
    Ok(String::from_utf16_lossy(
        &buffer[..usize::try_from(length).expect("mapped path length fits usize")],
    ))
}

fn hash_file(file: &mut File) -> Result<String> {
    file.seek(SeekFrom::Start(0))?;
    let mut digest = DigestContext::new(&SHA256);
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    let mut encoded = String::with_capacity(64);
    for byte in digest.finish().as_ref() {
        let _ = write!(&mut encoded, "{byte:02x}");
    }
    Ok(encoded)
}

pub(super) fn same_windows_path(left: &Path, right: &Path) -> bool {
    let normalize = |path: &Path| {
        fs::canonicalize(path)
            .ok()
            .map(|path| path.to_string_lossy().to_lowercase())
    };
    matches!((normalize(left), normalize(right)), (Some(left), Some(right)) if left == right)
}

#[cfg(test)]
const RELEASE_AFTER_PROOF_ENV: &str = "CTX_TEST_LEGACY_RELEASE_AFTER_IMAGE_PROOF";
#[cfg(test)]
const SWAP_OBSERVED_IMAGE_ENV: &str = "CTX_TEST_LEGACY_SWAP_OBSERVED_IMAGE";

#[cfg(test)]
pub(super) fn release_guard_after_image_proof_for_test(data_root: &Path) -> Result<()> {
    if std::env::var_os(RELEASE_AFTER_PROOF_ENV).is_none() {
        return Ok(());
    }
    fs::write(data_root.join("release-trigger"), b"release")?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !data_root.join("guard-released").exists() {
        if std::time::Instant::now() >= deadline {
            return Err(anyhow!(
                "test owner did not release guard after image proof"
            ));
        }
        std::thread::yield_now();
    }
    Ok(())
}

#[cfg(test)]
fn swap_observed_image_for_test(observed_path: &Path) -> Result<()> {
    let Some(replacement) = std::env::var_os(SWAP_OBSERVED_IMAGE_ENV) else {
        return Ok(());
    };
    let parked = observed_path.with_file_name("ctx.mapped-original.exe");
    fs::rename(observed_path, &parked).context("park mapped-image test object")?;
    fs::copy(replacement, observed_path).context("swap observed-image test path")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        env,
        process::{Child, Command, Stdio},
    };

    use serde_json::Value;

    use super::super::tests::{
        fixture_test_guard, spawn_fixture_child, wait_for_path, LegacyFixture,
        CHILD_EXPECT_ERROR_ENV, CHILD_MODE_ENV, CHILD_OPEN_IMAGE_ENV, CHILD_ROOT_ENV,
        CHILD_SHA_ENV, CHILD_TEST,
    };
    use super::super::{
        process_handle::process_access_rights,
        terminate_identity_verified_residual_daemon_owner_with_legacy_sha256,
    };
    use super::*;
    use crate::{daemon_lock_path, executable_sha256, observe_pid_advisory_lock};

    #[test]
    fn real_adjacent_rename_binds_mapped_image_and_terminates() {
        let _serial = fixture_test_guard();
        let mut fixture = LegacyFixture::start();
        let legacy_sha256 = executable_sha256(&fixture.active).expect("legacy fixture digest");
        let moved = fixture.active.with_file_name("ctx.v025-running.exe");
        fs::rename(&fixture.active, &moved).expect("rename running legacy image");
        fs::copy(
            env::current_exe().expect("current test image"),
            &fixture.active,
        )
        .expect("publish same-path candidate");

        let stable = WindowsProcess::open(fixture.owner.id(), WindowsProcessAccess::Observe)
            .expect("open renamed owner")
            .expect("renamed owner running");
        let observed = stable.executable_path().expect("query renamed owner path");
        assert!(
            same_windows_path(&observed, &moved),
            "QueryFullProcessImageNameW did not report the moved image: {observed:?}"
        );

        let mut takeover = spawn_takeover(
            &fixture.active,
            &fixture.root,
            Some(&legacy_sha256),
            None,
            None,
        );
        let status = takeover.wait().expect("join renamed-image takeover");
        assert!(status.success(), "{status}");
        assert!(!stable.is_running().expect("inspect renamed owner handle"));
        assert!(fixture
            .owner
            .try_wait()
            .expect("inspect renamed owner")
            .is_some());
    }

    #[test]
    fn renamed_same_directory_process_with_wrong_bytes_is_rejected() {
        let _serial = fixture_test_guard();
        let mut fixture = LegacyFixture::start();
        let moved = fixture.active.with_file_name("attacker-chosen.exe");
        fs::rename(&fixture.active, &moved).expect("rename arbitrary running image");
        fs::copy(
            env::current_exe().expect("current test image"),
            &fixture.active,
        )
        .expect("publish same-path candidate");

        let mut takeover = spawn_takeover(
            &fixture.active,
            &fixture.root,
            None,
            Some("not the published ctx v0.25.0 Windows artifact"),
            None,
        );
        assert!(takeover.wait().expect("join rejected takeover").success());
        assert!(fixture
            .owner
            .try_wait()
            .expect("inspect rejected owner")
            .is_none());
    }

    #[test]
    fn guard_owner_merely_opening_known_image_is_rejected() {
        let _serial = fixture_test_guard();
        let temp = tempfile::tempdir().expect("temporary unrelated owner fixture");
        let active = temp.path().join("ctx.exe");
        let unrelated = temp.path().join("unrelated.exe");
        let opened = temp.path().join("known-v025-image.exe");
        let root = temp.path().join("data");
        fs::create_dir_all(&root).expect("create unrelated owner root");
        let current = env::current_exe().expect("current test executable");
        fs::copy(&current, &active).expect("copy takeover executable");
        fs::copy(&current, &unrelated).expect("copy unrelated process executable");
        fs::write(
            &opened,
            b"known legacy image bytes not used as the main module",
        )
        .expect("write opened image fixture");
        let known_sha256 = executable_sha256(&opened).expect("opened image digest");

        let mut owner = Command::new(&unrelated)
            .args(["--exact", CHILD_TEST, "--nocapture"])
            .env(CHILD_MODE_ENV, "owner")
            .env(CHILD_ROOT_ENV, &root)
            .env(CHILD_OPEN_IMAGE_ENV, &opened)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn unrelated guard owner");
        wait_for_path(&root.join("owner-ready"));
        rewrite_recorded_binary(&root, &active);
        assert!(observe_pid_advisory_lock(&daemon_lock_path(&root)).is_some_and(|lock| lock.held));

        let mut takeover = spawn_takeover(
            &active,
            &root,
            Some(&known_sha256),
            Some("not the published ctx v0.25.0 Windows artifact"),
            None,
        );
        assert!(takeover
            .wait()
            .expect("join unrelated-owner rejection")
            .success());
        assert!(owner.try_wait().expect("inspect unrelated owner").is_none());
        owner.kill().expect("stop unrelated owner");
        owner.wait().expect("join unrelated owner");
    }

    #[test]
    fn hard_link_alias_cannot_supply_the_renamed_image_identity() {
        let _serial = fixture_test_guard();
        let temp = tempfile::tempdir().expect("temporary hard-link fixture");
        let active = temp.path().join("ctx.exe");
        let alias = temp.path().join("ctx-alias.exe");
        let root = temp.path().join("data");
        fs::create_dir_all(&root).expect("create hard-link root");
        fs::copy(
            env::current_exe().expect("current test executable"),
            &active,
        )
        .expect("copy hard-link fixture executable");
        fs::hard_link(&active, &alias).expect("create executable hard link");
        let sha256 = executable_sha256(&active).expect("hard-link image digest");
        let mut owner = spawn_fixture_child(&alias, &root, "owner");
        wait_for_path(&root.join("owner-ready"));
        rewrite_recorded_binary(&root, &active);

        let mut takeover = spawn_takeover(&active, &root, Some(&sha256), Some("hard links"), None);
        assert!(takeover.wait().expect("join alias rejection").success());
        assert!(owner.try_wait().expect("inspect alias owner").is_none());
        owner.kill().expect("stop alias owner");
        owner.wait().expect("join alias owner");
    }

    #[test]
    fn legacy_process_start_mismatch_is_rejected_before_image_proof() {
        let _serial = fixture_test_guard();
        let mut fixture = LegacyFixture::start();
        let mut value = crate::read_pid_lock_json(&daemon_lock_path(&fixture.root))
            .expect("legacy fixture metadata");
        value["started_at_ms"] = Value::from(1);
        fs::write(
            daemon_lock_path(&fixture.root),
            serde_json::to_vec(&value).expect("encode start-mismatch metadata"),
        )
        .expect("publish start-mismatch metadata");
        let sha256 = executable_sha256(&fixture.active).expect("legacy fixture digest");
        let error = terminate_identity_verified_residual_daemon_owner_with_legacy_sha256(
            &fixture.root,
            &fixture.active,
            None,
            &sha256,
        )
        .expect_err("process-start mismatch must fail closed");
        assert!(error.to_string().contains("PID was reused"), "{error:#}");
        assert!(fixture
            .owner
            .try_wait()
            .expect("inspect start owner")
            .is_none());
    }

    #[test]
    fn binary_sha256_key_with_null_blocks_legacy_fallback() {
        let _serial = fixture_test_guard();
        let mut fixture = LegacyFixture::start();
        let mut value = crate::read_pid_lock_json(&daemon_lock_path(&fixture.root))
            .expect("legacy fixture metadata");
        value["binary_sha256"] = Value::Null;
        fs::write(
            daemon_lock_path(&fixture.root),
            serde_json::to_vec(&value).expect("encode null-digest metadata"),
        )
        .expect("publish null-digest metadata");
        let error = super::super::terminate_identity_verified_residual_daemon(
            &fixture.root,
            &fixture.active,
        )
        .expect_err("present null digest must not enter legacy fallback");
        assert!(
            error.to_string().contains("no executable digest identity"),
            "{error:#}"
        );
        assert!(fixture
            .owner
            .try_wait()
            .expect("inspect null-digest owner")
            .is_none());
    }

    #[test]
    fn final_guard_release_after_image_hash_uses_clean_wait_path() {
        let _serial = fixture_test_guard();
        let mut fixture = LegacyFixture::start();
        let target = WindowsProcess::open(fixture.owner.id(), WindowsProcessAccess::Observe)
            .expect("open final-release owner handle")
            .expect("final-release owner running");
        let sha256 = executable_sha256(&fixture.active).expect("legacy fixture digest");
        let _release = ScopedEnv::set(RELEASE_AFTER_PROOF_ENV, "1");

        terminate_identity_verified_residual_daemon_owner_with_legacy_sha256(
            &fixture.root,
            &fixture.active,
            None,
            &sha256,
        )
        .expect("wait after final guard release");
        assert!(!target.is_running().expect("inspect final-release handle"));
        let status = fixture
            .owner
            .try_wait()
            .expect("inspect final-release owner")
            .expect("owner exited before return");
        assert!(status.success(), "{status}");
        assert!(fixture.root.join("clean-exit").exists());
    }

    #[test]
    fn mapped_main_file_id_rejects_observed_path_swap() {
        let _serial = fixture_test_guard();
        let mut fixture = LegacyFixture::start();
        let legacy_sha256 = executable_sha256(&fixture.active).expect("legacy fixture digest");
        let moved = fixture.active.with_file_name("ctx.v025-running.exe");
        let replacement = fixture.active.with_file_name("mapped-swap-replacement.exe");
        fs::rename(&fixture.active, &moved).expect("rename running legacy image");
        fs::copy(
            env::current_exe().expect("current test image"),
            &fixture.active,
        )
        .expect("publish same-path candidate");
        fs::copy(&fixture.active, &replacement).expect("copy mapped swap replacement");

        let mut takeover = spawn_takeover(
            &fixture.active,
            &fixture.root,
            Some(&legacy_sha256),
            Some("main mapped image file ID does not match"),
            Some(&replacement),
        );
        assert!(takeover.wait().expect("join mapped-ID rejection").success());
        assert!(fixture
            .owner
            .try_wait()
            .expect("inspect mapped owner")
            .is_none());
    }

    #[test]
    fn modern_termination_rights_do_not_request_vm_read() {
        use windows_sys::Win32::System::Threading::{
            PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
            PROCESS_TERMINATE, PROCESS_VM_READ,
        };
        let modern = process_access_rights(WindowsProcessAccess::ModernTerminate);
        assert_eq!(
            modern,
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE
        );
        assert_eq!(modern & (PROCESS_QUERY_INFORMATION | PROCESS_VM_READ), 0);
        let legacy = process_access_rights(WindowsProcessAccess::LegacyTerminate);
        assert_ne!(legacy & PROCESS_QUERY_INFORMATION, 0);
        assert_ne!(legacy & PROCESS_VM_READ, 0);
    }

    fn spawn_takeover(
        binary: &Path,
        root: &Path,
        sha256: Option<&str>,
        expected_error: Option<&str>,
        swap_image: Option<&Path>,
    ) -> Child {
        let mut command = Command::new(binary);
        command
            .args(["--exact", CHILD_TEST, "--nocapture"])
            .env(CHILD_MODE_ENV, "takeover")
            .env(CHILD_ROOT_ENV, root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(sha256) = sha256 {
            command.env(CHILD_SHA_ENV, sha256);
        }
        if let Some(error) = expected_error {
            command.env(CHILD_EXPECT_ERROR_ENV, error);
        }
        if let Some(path) = swap_image {
            command.env(SWAP_OBSERVED_IMAGE_ENV, path);
        }
        command.spawn().expect("spawn legacy takeover child")
    }

    struct ScopedEnv {
        name: &'static str,
        prior: Option<std::ffi::OsString>,
    }

    impl ScopedEnv {
        fn set(name: &'static str, value: &str) -> Self {
            let prior = env::var_os(name);
            env::set_var(name, value);
            Self { name, prior }
        }
    }

    impl Drop for ScopedEnv {
        fn drop(&mut self) {
            if let Some(prior) = &self.prior {
                env::set_var(self.name, prior);
            } else {
                env::remove_var(self.name);
            }
        }
    }

    fn rewrite_recorded_binary(root: &Path, expected: &Path) {
        let mut value =
            crate::read_pid_lock_json(&daemon_lock_path(root)).expect("guard-owner metadata");
        value["binary"] = Value::String(expected.to_string_lossy().into_owned());
        fs::write(
            daemon_lock_path(root),
            serde_json::to_vec(&value).expect("encode rewritten lock"),
        )
        .expect("rewrite recorded executable path");
    }
}
