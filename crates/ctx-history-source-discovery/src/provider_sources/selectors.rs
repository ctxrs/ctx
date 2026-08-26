use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    io::ErrorKind,
    path::{Path, PathBuf},
};

use quick_xml::{encoding::Decoder, events::Event, Reader};
use serde_json::Value;
use thiserror::Error;

use ctx_history_capture_model::provider_root_path_within_limit;

use ctx_history_source_io::{
    open_provider_source_file, open_provider_source_path, OpenedProviderSourcePath, SourceIoError,
};

pub(super) const MAX_SELECTOR_FILE_BYTES: usize = 1024 * 1024;
pub(super) const MAX_SELECTOR_FILES_PER_PROVIDER: usize = 64;
pub(super) const MAX_CONFIG_INCLUDE_DEPTH: usize = 4;
pub(super) const MAX_CONFIG_INCLUDE_FILES: usize = 16;
pub(super) const MAX_PARSED_NESTING_DEPTH: usize = 32;
pub(super) const MAX_FINITE_SELECTOR_ENTRIES: usize = 128;
pub(super) const MAX_DIRECT_DIRECTORY_ENTRIES: usize = 1024;
pub(super) const MAX_PROJECT_ANCESTORS: usize = 64;
pub(super) const MAX_SOURCE_CANDIDATES_PER_PROVIDER: usize = 256;
pub(super) const MAX_RENDERED_DIAGNOSTIC_BYTES: usize = 512;

#[cfg(test)]
thread_local! {
    static SELECTOR_FILE_OPEN_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        std::cell::RefCell::new(None);
    static DIRECT_ENTRIES_ROOT_OPEN_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        std::cell::RefCell::new(None);
    static DIRECT_ENTRIES_FIRST_PASS_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        std::cell::RefCell::new(None);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SelectorFormat {
    Json,
    Jsonc,
    Json5,
    Toml,
    Yaml,
    Xml,
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub(super) enum SelectorReadError {
    #[error("selector file is unavailable or is not an ordinary no-follow file")]
    Unavailable,
    #[error("selector source root is unsupported or unsafe")]
    UnsupportedRoot,
    #[error("selector file exceeds the fixed byte limit")]
    FileTooLarge,
    #[error("selector file count exceeds the per-provider limit")]
    FileLimit,
    #[error("selector document could not be parsed")]
    Parse,
    #[error("selector nesting exceeds the fixed depth limit")]
    NestingDepth,
    #[error("selector list or XML document exceeds the fixed entry limit")]
    EntryLimit,
    #[error("allowlisted directory exceeds the direct-entry limit")]
    DirectoryLimit,
    #[error("XML document types and entity declarations are not accepted")]
    XmlDoctype,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SourcePathKind {
    File,
    Directory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SourcePathError {
    Missing,
    Unsupported,
    Unavailable(ErrorKind),
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum SelectorDocument {
    Structured(Value),
    Xml(XmlSelectorDocument),
}

impl SelectorDocument {
    #[cfg(test)]
    pub(super) fn string(&self, path: &[&str]) -> Option<&str> {
        let Self::Structured(value) = self else {
            return None;
        };
        value_at(value, path).and_then(Value::as_str)
    }

    #[cfg(test)]
    pub(super) fn strings(&self, path: &[&str]) -> Option<Vec<&str>> {
        let Self::Structured(value) = self else {
            return None;
        };
        let values = value_at(value, path)?.as_array()?;
        if values.len() > MAX_FINITE_SELECTOR_ENTRIES {
            return None;
        }
        values.iter().map(Value::as_str).collect()
    }

    pub(super) fn xml(&self) -> Option<&XmlSelectorDocument> {
        match self {
            Self::Xml(document) => Some(document),
            Self::Structured(_) => None,
        }
    }
}

#[cfg(test)]
fn value_at<'a>(mut value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    for component in path {
        value = value.as_object()?.get(*component)?;
    }
    Some(value)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct XmlSelectorEntry {
    path: Vec<String>,
    attributes: BTreeMap<String, String>,
    text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct XmlSelectorDocument {
    entries: Vec<XmlSelectorEntry>,
}

impl XmlSelectorDocument {
    /// Returns bounded values from elements at an exact path. When `attribute`
    /// is `Some`, the exact attribute value is returned; otherwise element text
    /// is returned. No descendant or wildcard matching is performed.
    pub(super) fn values(&self, path: &[&str], attribute: Option<&str>) -> Vec<&str> {
        self.entries
            .iter()
            .filter(|entry| {
                entry.path.len() == path.len()
                    && entry
                        .path
                        .iter()
                        .zip(path)
                        .all(|(actual, expected)| actual == expected)
            })
            .filter_map(|entry| match attribute {
                Some(name) => entry.attributes.get(name).map(String::as_str),
                None => Some(entry.text.as_str()),
            })
            .take(MAX_FINITE_SELECTOR_ENTRIES)
            .collect()
    }
}

#[derive(Debug, Default)]
pub(super) struct SelectorReader {
    files_read: usize,
}

#[derive(Debug, Default)]
pub(super) struct SelectorIncludeBudget {
    files: usize,
}

impl SelectorIncludeBudget {
    pub(super) fn admit(&mut self, depth: usize) -> Result<(), SelectorReadError> {
        if depth > MAX_CONFIG_INCLUDE_DEPTH {
            return Err(SelectorReadError::NestingDepth);
        }
        if self.files >= MAX_CONFIG_INCLUDE_FILES {
            return Err(SelectorReadError::FileLimit);
        }
        self.files += 1;
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn files(&self) -> usize {
        self.files
    }
}

impl SelectorReader {
    pub(super) fn read(
        &mut self,
        path: &Path,
        format: SelectorFormat,
    ) -> Result<SelectorDocument, SelectorReadError> {
        if self.files_read >= MAX_SELECTOR_FILES_PER_PROVIDER {
            return Err(SelectorReadError::FileLimit);
        }
        self.files_read += 1;

        let text = read_selector_text(path)?;
        let document = match format {
            SelectorFormat::Json => SelectorDocument::Structured(
                serde_json::from_str(&text).map_err(|_| SelectorReadError::Parse)?,
            ),
            SelectorFormat::Jsonc => SelectorDocument::Structured(
                jsonc_parser::parse_to_serde_value(&text, &Default::default())
                    .map_err(|_| SelectorReadError::Parse)?,
            ),
            SelectorFormat::Json5 => {
                validate_json5_nesting(&text)?;
                SelectorDocument::Structured(
                    json5::from_str(&text).map_err(|_| SelectorReadError::Parse)?,
                )
            }
            SelectorFormat::Toml => SelectorDocument::Structured(
                toml_edit::de::from_str(&text).map_err(|_| SelectorReadError::Parse)?,
            ),
            SelectorFormat::Yaml => SelectorDocument::Structured(
                serde_yaml::from_str(&text).map_err(|_| SelectorReadError::Parse)?,
            ),
            SelectorFormat::Xml => SelectorDocument::Xml(parse_xml(&text)?),
        };
        if let SelectorDocument::Structured(value) = &document {
            validate_nesting(value, 1)?;
        }
        Ok(document)
    }

    pub(super) fn files_read(&self) -> usize {
        self.files_read
    }
}

fn read_selector_text(path: &Path) -> Result<String, SelectorReadError> {
    let file = open_provider_source_file(path).map_err(selector_open_error)?;
    #[cfg(test)]
    SELECTOR_FILE_OPEN_HOOK.with(|hook| {
        if let Some(hook) = hook.borrow_mut().take() {
            hook();
        }
    });
    if file.len() > MAX_SELECTOR_FILE_BYTES as u64 {
        return Err(SelectorReadError::FileTooLarge);
    }
    let bytes = file
        .read_all_bounded(MAX_SELECTOR_FILE_BYTES)
        .map_err(selector_open_error)?;
    String::from_utf8(bytes).map_err(|_| SelectorReadError::Parse)
}

fn validate_nesting(value: &Value, depth: usize) -> Result<(), SelectorReadError> {
    if depth > MAX_PARSED_NESTING_DEPTH {
        return Err(SelectorReadError::NestingDepth);
    }
    match value {
        Value::Array(values) => {
            for value in values {
                validate_nesting(value, depth.saturating_add(1))?;
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                validate_nesting(value, depth.saturating_add(1))?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_json5_nesting(text: &str) -> Result<(), SelectorReadError> {
    #[derive(Clone, Copy)]
    enum State {
        Normal,
        String(char),
        LineComment,
        BlockComment,
    }

    let mut state = State::Normal;
    let mut stack = ['\0'; MAX_PARSED_NESTING_DEPTH];
    let mut depth = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        match state {
            State::Normal => match character {
                '\'' | '"' => state = State::String(character),
                '/' if chars.peek() == Some(&'/') => {
                    chars.next();
                    state = State::LineComment;
                }
                '/' if chars.peek() == Some(&'*') => {
                    chars.next();
                    state = State::BlockComment;
                }
                '{' | '[' => {
                    if depth == stack.len() {
                        return Err(SelectorReadError::NestingDepth);
                    }
                    stack[depth] = character;
                    depth += 1;
                }
                '}' | ']' => {
                    let expected = if character == '}' { '{' } else { '[' };
                    if depth == 0 || stack[depth - 1] != expected {
                        return Err(SelectorReadError::Parse);
                    }
                    depth -= 1;
                }
                _ => {}
            },
            State::String(quote) => match character {
                '\\' => {
                    chars.next();
                }
                character if character == quote => state = State::Normal,
                _ => {}
            },
            State::LineComment => {
                if matches!(character, '\n' | '\r' | '\u{2028}' | '\u{2029}') {
                    state = State::Normal;
                }
            }
            State::BlockComment => {
                if character == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    state = State::Normal;
                }
            }
        }
    }
    match state {
        State::Normal | State::LineComment if depth == 0 => Ok(()),
        State::Normal | State::LineComment | State::String(_) | State::BlockComment => {
            Err(SelectorReadError::Parse)
        }
    }
}

#[derive(Debug)]
struct XmlFrame {
    name: String,
    attributes: BTreeMap<String, String>,
    text: String,
}

fn parse_xml(text: &str) -> Result<XmlSelectorDocument, SelectorReadError> {
    let mut reader = Reader::from_str(text);
    reader.config_mut().trim_text(true);
    let mut frames = Vec::<XmlFrame>::new();
    let mut entries = Vec::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) => {
                if frames.len() >= MAX_PARSED_NESTING_DEPTH {
                    return Err(SelectorReadError::NestingDepth);
                }
                frames.push(XmlFrame {
                    name: decode_xml_bytes(element.name().as_ref())?,
                    attributes: xml_attributes(&element, reader.decoder())?,
                    text: String::new(),
                });
            }
            Ok(Event::Empty(element)) => {
                if frames.len() >= MAX_PARSED_NESTING_DEPTH {
                    return Err(SelectorReadError::NestingDepth);
                }
                push_xml_entry(
                    &mut entries,
                    &frames,
                    XmlFrame {
                        name: decode_xml_bytes(element.name().as_ref())?,
                        attributes: xml_attributes(&element, reader.decoder())?,
                        text: String::new(),
                    },
                )?;
            }
            Ok(Event::Text(value)) => {
                if let Some(frame) = frames.last_mut() {
                    let decoded = value.decode().map_err(|_| SelectorReadError::Parse)?;
                    frame.text.push_str(&decoded);
                }
            }
            Ok(Event::CData(value)) => {
                if let Some(frame) = frames.last_mut() {
                    let decoded = value.decode().map_err(|_| SelectorReadError::Parse)?;
                    frame.text.push_str(&decoded);
                }
            }
            Ok(Event::End(_)) => {
                let Some(frame) = frames.pop() else {
                    return Err(SelectorReadError::Parse);
                };
                push_xml_entry(&mut entries, &frames, frame)?;
            }
            Ok(Event::DocType(_) | Event::GeneralRef(_)) => {
                return Err(SelectorReadError::XmlDoctype);
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(_) => return Err(SelectorReadError::Parse),
        }
    }
    if !frames.is_empty() {
        return Err(SelectorReadError::Parse);
    }
    Ok(XmlSelectorDocument { entries })
}

fn push_xml_entry(
    entries: &mut Vec<XmlSelectorEntry>,
    parents: &[XmlFrame],
    frame: XmlFrame,
) -> Result<(), SelectorReadError> {
    if entries.len() >= MAX_FINITE_SELECTOR_ENTRIES {
        return Err(SelectorReadError::EntryLimit);
    }
    let mut path = parents
        .iter()
        .map(|parent| parent.name.clone())
        .collect::<Vec<_>>();
    path.push(frame.name);
    entries.push(XmlSelectorEntry {
        path,
        attributes: frame.attributes,
        text: frame.text,
    });
    Ok(())
}

fn xml_attributes(
    element: &quick_xml::events::BytesStart<'_>,
    decoder: Decoder,
) -> Result<BTreeMap<String, String>, SelectorReadError> {
    let mut values = BTreeMap::new();
    for attribute in element.attributes() {
        let attribute = attribute.map_err(|_| SelectorReadError::Parse)?;
        values.insert(
            decode_xml_bytes(attribute.key.as_ref())?,
            quick_xml::escape::unescape(
                &decoder
                    .decode(attribute.value.as_ref())
                    .map_err(|_| SelectorReadError::Parse)?,
            )
            .map_err(|_| SelectorReadError::Parse)?
            .into_owned(),
        );
    }
    Ok(values)
}

fn decode_xml_bytes(bytes: &[u8]) -> Result<String, SelectorReadError> {
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| SelectorReadError::Parse)
}

pub(super) fn encoded_path_sort_key(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        path.as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect()
    }
    #[cfg(not(any(unix, windows)))]
    {
        path.as_os_str().to_string_lossy().as_bytes().to_vec()
    }
}

pub(super) fn encoded_path_within_limit(path: &Path) -> bool {
    provider_root_path_within_limit(path)
}

pub(super) fn sort_paths(paths: &mut [PathBuf]) {
    paths.sort_by_cached_key(|path| encoded_path_sort_key(path));
}

pub(super) fn source_path_kind(path: &Path) -> Result<SourcePathKind, SourcePathError> {
    let opened = open_provider_source_path(path).map_err(source_path_error)?;
    let kind = match &opened {
        OpenedProviderSourcePath::File(_) => SourcePathKind::File,
        OpenedProviderSourcePath::Directory(_) => SourcePathKind::Directory,
    };
    revalidate_opened_path(&opened).map_err(source_path_error)?;
    Ok(kind)
}

pub(super) fn ordinary_file(path: &Path) -> bool {
    source_path_kind(path) == Ok(SourcePathKind::File)
}

pub(super) fn ordinary_directory(path: &Path) -> bool {
    source_path_kind(path) == Ok(SourcePathKind::Directory)
}

pub(super) fn ordinary_path(path: &Path) -> bool {
    source_path_kind(path).is_ok()
}

pub(super) fn ordinary_empty_file(path: &Path) -> Result<bool, SelectorReadError> {
    let file = open_provider_source_file(path).map_err(selector_open_error)?;
    let empty = file.is_empty();
    file.revalidate().map_err(selector_open_error)?;
    Ok(empty)
}

pub(super) fn read_bounded_bytes(
    path: &Path,
    maximum_bytes: usize,
) -> Result<Vec<u8>, SelectorReadError> {
    let file = open_provider_source_file(path).map_err(selector_open_error)?;
    if file.len() > u64::try_from(maximum_bytes).map_err(|_| SelectorReadError::FileTooLarge)? {
        return Err(SelectorReadError::FileTooLarge);
    }
    file.read_all_bounded(maximum_bytes)
        .map_err(selector_open_error)
}

pub(super) fn direct_entries(path: &Path) -> Result<Vec<PathBuf>, SelectorReadError> {
    direct_entries_filtered(path, |_| true, false)
}

/// Enumerates one bounded directory by name, then opens and revalidates only
/// selected children. Non-matching entries still count toward the directory
/// bound and the closing name-set check, but cannot make an allowlisted
/// registry unreadable merely because they are links or other unsafe objects.
pub(super) fn direct_regular_files_matching(
    path: &Path,
    matches_name: impl Fn(&OsStr) -> bool,
) -> Result<Vec<PathBuf>, SelectorReadError> {
    direct_entries_filtered(path, matches_name, true)
}

fn direct_entries_filtered(
    path: &Path,
    matches_name: impl Fn(&OsStr) -> bool,
    regular_files_only: bool,
) -> Result<Vec<PathBuf>, SelectorReadError> {
    let opened = open_provider_source_path(path).map_err(selector_open_error)?;
    let OpenedProviderSourcePath::Directory(directory) = opened else {
        return Err(SelectorReadError::Unavailable);
    };
    let authority = directory.authority_root();
    #[cfg(test)]
    DIRECT_ENTRIES_ROOT_OPEN_HOOK.with(|hook| {
        if let Some(hook) = hook.borrow_mut().take() {
            hook();
        }
    });
    let names = directory
        .entries(MAX_DIRECT_DIRECTORY_ENTRIES.saturating_add(1))
        .map_err(selector_open_error)?;
    if names.len() > MAX_DIRECT_DIRECTORY_ENTRIES {
        return Err(SelectorReadError::DirectoryLimit);
    }

    let matching_names = names
        .iter()
        .filter(|name| matches_name(name.as_os_str()))
        .cloned()
        .collect::<Vec<_>>();
    let mut fingerprints: Vec<(OsString, [u8; 32], bool)> =
        Vec::with_capacity(matching_names.len());
    for name in matching_names {
        let child = directory.open_child(&name).map_err(selector_open_error)?;
        revalidate_opened_path(&child).map_err(selector_open_error)?;
        let is_regular_file = matches!(&child, OpenedProviderSourcePath::File(_));
        fingerprints.push((name, child.authority_fingerprint(), is_regular_file));
    }
    directory.revalidate().map_err(selector_open_error)?;
    authority.revalidate().map_err(selector_open_error)?;

    #[cfg(test)]
    DIRECT_ENTRIES_FIRST_PASS_HOOK.with(|hook| {
        if let Some(hook) = hook.borrow_mut().take() {
            hook();
        }
    });

    let closing_names = directory
        .entries(MAX_DIRECT_DIRECTORY_ENTRIES.saturating_add(1))
        .map_err(selector_open_error)?;
    if closing_names != names {
        return Err(SelectorReadError::Unavailable);
    }
    for (name, fingerprint, is_regular_file) in &fingerprints {
        let child = directory.open_child(name).map_err(selector_open_error)?;
        if child.authority_fingerprint() != *fingerprint
            || matches!(&child, OpenedProviderSourcePath::File(_)) != *is_regular_file
        {
            return Err(SelectorReadError::Unavailable);
        }
        revalidate_opened_path(&child).map_err(selector_open_error)?;
    }
    directory.revalidate().map_err(selector_open_error)?;
    authority.revalidate().map_err(selector_open_error)?;

    let mut paths = fingerprints
        .into_iter()
        .filter(|(_, _, is_regular_file)| !regular_files_only || *is_regular_file)
        .map(|(name, _, _)| path.join(name))
        .collect::<Vec<_>>();
    sort_paths(&mut paths);
    Ok(paths)
}

fn revalidate_opened_path(opened: &OpenedProviderSourcePath) -> ctx_history_source_io::Result<()> {
    match opened {
        OpenedProviderSourcePath::File(file) => file.revalidate(),
        OpenedProviderSourcePath::Directory(directory) => {
            directory.revalidate()?;
            directory.authority_root().revalidate()
        }
    }
}

fn source_path_error(error: SourceIoError) -> SourcePathError {
    match error {
        SourceIoError::Io(error) if error.kind() == ErrorKind::NotFound => SourcePathError::Missing,
        SourceIoError::Io(error) => SourcePathError::Unavailable(error.kind()),
        SourceIoError::InvalidProviderTranscriptPath { .. } => SourcePathError::Unsupported,
        _ => SourcePathError::Unavailable(ErrorKind::Other),
    }
}

fn selector_open_error(error: SourceIoError) -> SelectorReadError {
    match source_path_error(error) {
        SourcePathError::Unsupported => SelectorReadError::UnsupportedRoot,
        SourcePathError::Missing | SourcePathError::Unavailable(_) => {
            SelectorReadError::Unavailable
        }
    }
}

#[cfg(test)]
mod tests;
