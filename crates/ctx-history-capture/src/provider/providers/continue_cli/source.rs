use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::Value;

use crate::common::io::{ensure_regular_provider_transcript_file, read_text_file_limited};
use crate::{fnv1a64, CaptureError, Result, MAX_PROVIDER_JSONL_LINE_BYTES};

use super::{CONTINUE_CAPTURE_REVISION, CONTINUE_POLICY_REVISION};

#[derive(Debug, Clone, PartialEq, Eq)]
struct ContinueFrozenFile {
    path: PathBuf,
    length: u64,
    modified: SystemTime,
    readonly: bool,
    device: Option<u64>,
    inode: Option<u64>,
    #[cfg(windows)]
    windows_change_token: ContinueWindowsFileChangeToken,
}

impl ContinueFrozenFile {
    fn read(path: &Path) -> Result<Self> {
        ensure_regular_provider_transcript_file(path)?;
        let metadata = fs::symlink_metadata(path)?;
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        #[cfg(unix)]
        let (device, inode) = (Some(metadata.dev()), Some(metadata.ino()));
        #[cfg(not(unix))]
        let (device, inode) = (None, None);
        #[cfg(windows)]
        let windows_change_token = continue_windows_file_change_token(path)?;

        Ok(Self {
            path: path.to_path_buf(),
            length: metadata.len(),
            modified: metadata.modified()?,
            readonly: metadata.permissions().readonly(),
            device,
            inode,
            #[cfg(windows)]
            windows_change_token,
        })
    }

    fn revision_component(&self, output: &mut String) {
        let (side, seconds, nanos) = match self.modified.duration_since(UNIX_EPOCH) {
            Ok(duration) => ('+', duration.as_secs(), duration.subsec_nanos()),
            Err(error) => {
                let duration = error.duration();
                ('-', duration.as_secs(), duration.subsec_nanos())
            }
        };
        output.push_str(&format!(
            "{:?}\0{}\0{side}{seconds}.{nanos:09}\0{}\0{:?}\0{:?}\n",
            self.path.as_os_str(),
            self.length,
            self.readonly,
            self.device,
            self.inode,
        ));
        #[cfg(windows)]
        output.push_str(&format!(
            "windows-file-info-v1\0{}\0{:02x?}\0{}\0{}\0{}\n",
            self.windows_change_token.volume_serial_number,
            self.windows_change_token.file_id,
            self.windows_change_token.last_write_time_100ns,
            self.windows_change_token.change_time_100ns,
            self.windows_change_token.length,
        ));
        #[cfg(not(any(unix, windows)))]
        {
            // Targets without stable file identity or change-time metadata keep
            // the metadata-only SystemTime/length token above. Unix and Windows
            // use stronger identity and rewrite detection on supported targets.
        }
    }
}

#[cfg(windows)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct ContinueWindowsFileChangeToken {
    volume_serial_number: u64,
    file_id: [u8; 16],
    last_write_time_100ns: i64,
    change_time_100ns: i64,
    length: u64,
}

#[cfg(windows)]
fn continue_windows_file_change_token(path: &Path) -> Result<ContinueWindowsFileChangeToken> {
    use std::{ffi::c_void, fs::File, mem::size_of, os::windows::io::AsRawHandle};

    #[repr(C)]
    #[derive(Default)]
    struct FileBasicInfo {
        _creation_time: i64,
        _last_access_time: i64,
        last_write_time: i64,
        change_time: i64,
        _file_attributes: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct FileId128 {
        identifier: [u8; 16],
    }

    #[repr(C)]
    #[derive(Default)]
    struct FileIdInfo {
        volume_serial_number: u64,
        file_id: FileId128,
    }

    #[link(name = "Kernel32")]
    unsafe extern "system" {
        #[link_name = "GetFileInformationByHandleEx"]
        fn get_file_information_by_handle_ex(
            file: *mut c_void,
            info_class: i32,
            info: *mut c_void,
            size: u32,
        ) -> i32;
    }

    const FILE_BASIC_INFO_CLASS: i32 = 0;
    const FILE_ID_INFO_CLASS: i32 = 18;

    let file = File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Continue CLI session path is not a regular file",
        });
    }
    let handle = file.as_raw_handle();
    let mut basic_info = FileBasicInfo::default();
    let basic_result = unsafe {
        get_file_information_by_handle_ex(
            handle,
            FILE_BASIC_INFO_CLASS,
            (&mut basic_info as *mut FileBasicInfo).cast(),
            size_of::<FileBasicInfo>() as u32,
        )
    };
    if basic_result == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut id_info = FileIdInfo::default();
    let id_result = unsafe {
        get_file_information_by_handle_ex(
            handle,
            FILE_ID_INFO_CLASS,
            (&mut id_info as *mut FileIdInfo).cast(),
            size_of::<FileIdInfo>() as u32,
        )
    };
    if id_result == 0 {
        return Err(std::io::Error::last_os_error().into());
    }

    Ok(ContinueWindowsFileChangeToken {
        volume_serial_number: id_info.volume_serial_number,
        file_id: id_info.file_id.identifier,
        last_write_time_100ns: basic_info.last_write_time,
        change_time_100ns: basic_info.change_time,
        length: metadata.len(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ContinueIndexObservation {
    path: PathBuf,
    file: Option<ContinueFrozenFile>,
    content_hash: Option<u64>,
}

impl ContinueIndexObservation {
    fn read(path: PathBuf) -> Result<(Self, Box<[Value]>)> {
        let file = Self::read_file(&path)?;
        Self::read_with_file(path, file)
    }

    fn read_file(path: &Path) -> Result<Option<ContinueFrozenFile>> {
        Ok(match fs::symlink_metadata(path) {
            Ok(_) => Some(ContinueFrozenFile::read(path)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        })
    }

    fn read_with_file(
        path: PathBuf,
        file: Option<ContinueFrozenFile>,
    ) -> Result<(Self, Box<[Value]>)> {
        let Some(file) = file else {
            return Ok((
                Self {
                    path,
                    file: None,
                    content_hash: None,
                },
                Box::default(),
            ));
        };

        let (content_hash, entries) = match read_text_file_limited(
            &path,
            MAX_PROVIDER_JSONL_LINE_BYTES,
            "Continue CLI sessions index",
        ) {
            Ok(text) => {
                let content_hash = Some(fnv1a64(text.as_bytes()));
                let entries = match serde_json::from_str::<Value>(&text) {
                    Ok(Value::Array(entries)) => entries.into_boxed_slice(),
                    Ok(_) | Err(_) => Box::default(),
                };
                (content_hash, entries)
            }
            Err(_) => (None, Box::default()),
        };
        Ok((
            Self {
                path,
                file: Some(file),
                content_hash,
            },
            entries,
        ))
    }

    fn revision_component(&self, output: &mut String) {
        match &self.file {
            Some(file) => file.revision_component(output),
            None => output.push_str(&format!("{:?}\0missing\n", self.path.as_os_str())),
        }
        match self.content_hash {
            Some(hash) => output.push_str(&format!("content-hash\0{hash:016x}\n")),
            None => output.push_str("content-hash\0unavailable\n"),
        }
    }
}

#[derive(Default)]
pub(super) struct ContinueIndexCache {
    observation: Option<ContinueIndexObservation>,
    entries: Box<[Value]>,
}

impl ContinueIndexCache {
    fn observe(&mut self, parent: &Path) -> Result<ContinueIndexObservation> {
        let path = parent.join("sessions.json");
        let file = ContinueIndexObservation::read_file(&path)?;
        if let Some(observation) = self
            .observation
            .as_ref()
            .filter(|observation| observation.path == path && observation.file == file)
        {
            return Ok(observation.clone());
        }
        let (observation, entries) = ContinueIndexObservation::read_with_file(path, file)?;
        self.observation = Some(observation.clone());
        self.entries = entries;
        Ok(observation)
    }

    pub(super) fn metadata(
        &self,
        observation: &ContinueIndexObservation,
        session_id: &str,
    ) -> Result<Option<Value>> {
        if self.observation.as_ref() != Some(observation) {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        Ok(self
            .entries
            .iter()
            .find(|entry| {
                entry
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .is_some_and(|candidate| candidate == session_id)
            })
            .cloned())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ContinueSessionObservation {
    canonical_path: PathBuf,
    session_file: ContinueFrozenFile,
    sibling_index: ContinueIndexObservation,
}

impl ContinueSessionObservation {
    pub(super) fn read(path: &Path, index_cache: &mut ContinueIndexCache) -> Result<Self> {
        let session_file = ContinueFrozenFile::read(path)?;
        let canonical_path = fs::canonicalize(path)?;
        let parent = path
            .parent()
            .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "Continue CLI session path has no parent directory",
            })?;
        let sibling_index = index_cache.observe(parent)?;
        Ok(Self {
            canonical_path,
            session_file,
            sibling_index,
        })
    }

    pub(super) fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }

    pub(super) fn session_path(&self) -> &Path {
        &self.session_file.path
    }

    pub(super) fn session_length(&self) -> u64 {
        self.session_file.length
    }

    pub(super) fn sibling_index(&self) -> &ContinueIndexObservation {
        &self.sibling_index
    }

    pub(super) fn source_revision(&self) -> String {
        let mut input = format!(
            "continue-session-file-v2\0capture={CONTINUE_CAPTURE_REVISION}\0policy={CONTINUE_POLICY_REVISION}\n"
        );
        self.session_file.revision_component(&mut input);
        self.sibling_index.revision_component(&mut input);
        format!(
            "continue-session-file-v2:fnv1a64:{:016x}",
            fnv1a64(input.as_bytes())
        )
    }

    pub(super) fn revalidate(&self) -> Result<bool> {
        let session_file = match ContinueFrozenFile::read(&self.session_file.path) {
            Ok(file) => file,
            Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(false);
            }
            Err(CaptureError::InvalidProviderTranscriptPath { .. }) => return Ok(false),
            Err(error) => return Err(error),
        };
        let (sibling_index, _) = ContinueIndexObservation::read(self.sibling_index.path.clone())?;
        Ok(session_file == self.session_file
            && sibling_index == self.sibling_index
            && fs::canonicalize(&self.session_file.path)? == self.canonical_path)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::{json, Value};

    use crate::test_support_paths::tempdir;

    use super::*;

    fn write_session(path: &Path, session_id: &str, text: &str) {
        fs::write(
            path,
            serde_json::to_vec(&json!({
                "sessionId": session_id,
                "history": [{
                    "message": {
                        "role": "user",
                        "content": text,
                    }
                }]
            }))
            .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn sibling_index_content_participates_in_each_file_revision() {
        let temp = tempdir().unwrap();
        let session_path = temp.path().join("session.json");
        let index_path = temp.path().join("sessions.json");
        write_session(&session_path, "session", "hello");
        fs::write(&index_path, br#"[{"sessionId":"session","title":"first"}]"#).unwrap();
        let mut cache = ContinueIndexCache::default();
        let first = ContinueSessionObservation::read(&session_path, &mut cache).unwrap();
        let first_revision = first.source_revision();
        assert_eq!(
            cache
                .metadata(first.sibling_index(), "session")
                .unwrap()
                .and_then(|entry| entry.get("title").cloned()),
            Some(Value::String("first".to_owned()))
        );

        fs::write(
            &index_path,
            br#"[{"sessionId":"session","title":"second-and-longer"}]"#,
        )
        .unwrap();
        let second = ContinueSessionObservation::read(&session_path, &mut cache).unwrap();

        assert_ne!(first_revision, second.source_revision());
        assert_eq!(
            cache
                .metadata(second.sibling_index(), "session")
                .unwrap()
                .and_then(|entry| entry.get("title").cloned()),
            Some(Value::String("second-and-longer".to_owned()))
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_frozen_file_token_detects_same_length_replacement() {
        let temp = tempdir().unwrap();
        let session_path = temp.path().join("session.json");
        let replacement_path = temp.path().join("replacement.json");
        fs::write(&session_path, b"first").unwrap();
        fs::write(&replacement_path, b"other").unwrap();

        let first = ContinueFrozenFile::read(&session_path).unwrap();
        fs::remove_file(&session_path).unwrap();
        fs::rename(&replacement_path, &session_path).unwrap();
        let replacement = ContinueFrozenFile::read(&session_path).unwrap();

        assert_eq!(first.length, replacement.length);
        assert_ne!(
            (
                first.windows_change_token.volume_serial_number,
                first.windows_change_token.file_id,
            ),
            (
                replacement.windows_change_token.volume_serial_number,
                replacement.windows_change_token.file_id,
            ),
            "Windows file identity must reject same-length path replacement"
        );
        assert_ne!(first, replacement);
    }

    #[cfg(windows)]
    #[test]
    fn windows_frozen_file_token_detects_same_identity_rewrite() {
        let temp = tempdir().unwrap();
        let session_path = temp.path().join("session.json");
        fs::write(&session_path, b"first").unwrap();
        let first = ContinueFrozenFile::read(&session_path).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&session_path, b"other").unwrap();
        let rewritten = ContinueFrozenFile::read(&session_path).unwrap();

        assert_eq!(first.length, rewritten.length);
        assert_eq!(
            (
                first.windows_change_token.volume_serial_number,
                first.windows_change_token.file_id,
            ),
            (
                rewritten.windows_change_token.volume_serial_number,
                rewritten.windows_change_token.file_id,
            ),
            "rewriting an openable path must retain its Windows file identity"
        );
        assert_ne!(
            first.windows_change_token.change_time_100ns,
            rewritten.windows_change_token.change_time_100ns,
            "Windows ChangeTime must detect a same-length rewrite"
        );
        assert_ne!(first, rewritten);
    }
}
