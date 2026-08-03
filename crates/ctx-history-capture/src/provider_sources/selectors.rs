use std::{
    collections::BTreeMap,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use quick_xml::{encoding::Decoder, events::Event, Reader};
use serde_json::Value;
use thiserror::Error;

use crate::{
    common::io::{open_provider_source_file, open_provider_source_path, OpenedProviderSourcePath},
    CaptureError,
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
pub(super) const MAX_ENCODED_PATH_BYTES: usize = 16 * 1024;
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
    encoded_path_sort_key(path).len() <= MAX_ENCODED_PATH_BYTES
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

    let mut fingerprints = Vec::with_capacity(names.len());
    for name in &names {
        let child = directory.open_child(name).map_err(selector_open_error)?;
        revalidate_opened_path(&child).map_err(selector_open_error)?;
        fingerprints.push(child.authority_fingerprint());
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
    for (name, fingerprint) in names.iter().zip(fingerprints) {
        let child = directory.open_child(name).map_err(selector_open_error)?;
        if child.authority_fingerprint() != fingerprint {
            return Err(SelectorReadError::Unavailable);
        }
        revalidate_opened_path(&child).map_err(selector_open_error)?;
    }
    directory.revalidate().map_err(selector_open_error)?;
    authority.revalidate().map_err(selector_open_error)?;

    let mut paths = names
        .into_iter()
        .map(|name| path.join(name))
        .collect::<Vec<_>>();
    sort_paths(&mut paths);
    Ok(paths)
}

fn revalidate_opened_path(opened: &OpenedProviderSourcePath) -> crate::Result<()> {
    match opened {
        OpenedProviderSourcePath::File(file) => file.revalidate(),
        OpenedProviderSourcePath::Directory(directory) => {
            directory.revalidate()?;
            directory.authority_root().revalidate()
        }
    }
}

fn source_path_error(error: CaptureError) -> SourcePathError {
    match error {
        CaptureError::Io(error) if error.kind() == ErrorKind::NotFound => SourcePathError::Missing,
        CaptureError::Io(error) => SourcePathError::Unavailable(error.kind()),
        CaptureError::InvalidProviderTranscriptPath { .. } => SourcePathError::Unsupported,
        _ => SourcePathError::Unavailable(ErrorKind::Other),
    }
}

fn selector_open_error(error: CaptureError) -> SelectorReadError {
    match source_path_error(error) {
        SourcePathError::Unsupported => SelectorReadError::UnsupportedRoot,
        SourcePathError::Missing | SourcePathError::Unavailable(_) => {
            SelectorReadError::Unavailable
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tempdir() -> tempfile::TempDir {
        crate::test_support_paths::tempdir()
            .expect("system temporary directory should support selector fixtures")
    }

    #[test]
    fn fixed_limits_match_the_reviewed_discovery_contract() {
        assert_eq!(MAX_SELECTOR_FILE_BYTES, 1024 * 1024);
        assert_eq!(MAX_SELECTOR_FILES_PER_PROVIDER, 64);
        assert_eq!(MAX_CONFIG_INCLUDE_DEPTH, 4);
        assert_eq!(MAX_CONFIG_INCLUDE_FILES, 16);
        assert_eq!(MAX_PARSED_NESTING_DEPTH, 32);
        assert_eq!(MAX_FINITE_SELECTOR_ENTRIES, 128);
        assert_eq!(MAX_DIRECT_DIRECTORY_ENTRIES, 1024);
        assert_eq!(MAX_PROJECT_ANCESTORS, 64);
        assert_eq!(MAX_SOURCE_CANDIDATES_PER_PROVIDER, 256);
        assert_eq!(MAX_ENCODED_PATH_BYTES, 16 * 1024);
        assert_eq!(MAX_RENDERED_DIAGNOSTIC_BYTES, 512);
    }

    #[test]
    fn structured_helpers_require_exact_scalar_and_list_shapes() {
        let document = SelectorDocument::Structured(serde_json::json!({
            "options": {"root": "/tmp/history", "profiles": ["one", "two"]}
        }));
        assert_eq!(document.string(&["options", "root"]), Some("/tmp/history"));
        assert_eq!(
            document.strings(&["options", "profiles"]),
            Some(vec!["one", "two"])
        );
        assert_eq!(document.string(&["root"]), None);
    }

    #[test]
    fn bounded_reader_parses_each_allowlisted_selector_format() {
        let temp = tempdir();
        let fixtures = [
            (
                SelectorFormat::Json,
                "selector.json",
                r#"{"root":"json"}"#,
                "json",
            ),
            (
                SelectorFormat::Jsonc,
                "selector.jsonc",
                "{/* comment */\"root\":\"jsonc\"}",
                "jsonc",
            ),
            (
                SelectorFormat::Json5,
                "selector.json5",
                "{root: 'json5',}",
                "json5",
            ),
            (
                SelectorFormat::Toml,
                "selector.toml",
                "root = \"toml\"\n",
                "toml",
            ),
            (
                SelectorFormat::Yaml,
                "selector.yaml",
                "root: yaml\n",
                "yaml",
            ),
        ];
        for (format, name, body, expected) in fixtures {
            let path = temp.path().join(name);
            std::fs::write(&path, body).unwrap();
            let document = SelectorReader::default().read(&path, format).unwrap();
            assert_eq!(document.string(&["root"]), Some(expected));
        }

        let xml = temp.path().join("selector.xml");
        std::fs::write(
            &xml,
            r#"<application><component><option name="root" value="xml"/></component></application>"#,
        )
        .unwrap();
        let document = SelectorReader::default()
            .read(&xml, SelectorFormat::Xml)
            .unwrap();
        assert_eq!(
            document
                .xml()
                .unwrap()
                .values(&["application", "component", "option"], Some("value")),
            vec!["xml"]
        );
    }

    #[test]
    fn bounded_reader_enforces_byte_file_and_depth_limits() {
        let temp = tempdir();
        let valid = temp.path().join("valid.json");
        std::fs::write(&valid, "{}").unwrap();
        let mut reader = SelectorReader::default();
        for _ in 0..MAX_SELECTOR_FILES_PER_PROVIDER {
            reader.read(&valid, SelectorFormat::Json).unwrap();
        }
        assert_eq!(reader.files_read(), MAX_SELECTOR_FILES_PER_PROVIDER);
        assert_eq!(
            reader.read(&valid, SelectorFormat::Json),
            Err(SelectorReadError::FileLimit)
        );

        let oversized = temp.path().join("oversized.json");
        std::fs::write(&oversized, vec![b' '; MAX_SELECTOR_FILE_BYTES + 1]).unwrap();
        assert_eq!(
            SelectorReader::default().read(&oversized, SelectorFormat::Json),
            Err(SelectorReadError::FileTooLarge)
        );

        let deep = temp.path().join("deep.json");
        let body = format!(
            "{}null{}",
            "[".repeat(MAX_PARSED_NESTING_DEPTH),
            "]".repeat(MAX_PARSED_NESTING_DEPTH)
        );
        std::fs::write(&deep, body).unwrap();
        assert_eq!(
            SelectorReader::default().read(&deep, SelectorFormat::Json),
            Err(SelectorReadError::NestingDepth)
        );

        let adversarial = temp.path().join("adversarial.json5");
        std::fs::write(
            &adversarial,
            format!("{}null{}", "[".repeat(4096), "]".repeat(4096)),
        )
        .unwrap();
        assert_eq!(
            SelectorReader::default().read(&adversarial, SelectorFormat::Json5),
            Err(SelectorReadError::NestingDepth)
        );
    }

    #[test]
    fn json5_depth_scan_ignores_tokens_in_strings_and_comments() {
        let decoys = "[{}]".repeat(MAX_PARSED_NESTING_DEPTH + 1);
        let text =
            format!("{{literal: '{decoys}', block: /* {decoys} */ true, // {decoys}\n value: 1}}");
        assert_eq!(validate_json5_nesting(&text), Ok(()));
        assert_eq!(
            validate_json5_nesting(&format!(
                "// decoy\u{2028}{}null",
                "[".repeat(MAX_PARSED_NESTING_DEPTH + 1)
            )),
            Err(SelectorReadError::NestingDepth)
        );
    }

    #[test]
    fn include_budget_enforces_both_reviewed_include_limits() {
        let mut budget = SelectorIncludeBudget::default();
        for _ in 0..MAX_CONFIG_INCLUDE_FILES {
            budget.admit(MAX_CONFIG_INCLUDE_DEPTH).unwrap();
        }
        assert_eq!(budget.files(), MAX_CONFIG_INCLUDE_FILES);
        assert_eq!(
            budget.admit(MAX_CONFIG_INCLUDE_DEPTH),
            Err(SelectorReadError::FileLimit)
        );
        assert_eq!(
            SelectorIncludeBudget::default().admit(MAX_CONFIG_INCLUDE_DEPTH + 1),
            Err(SelectorReadError::NestingDepth)
        );
    }

    #[cfg(unix)]
    #[test]
    fn bounded_reader_does_not_follow_selector_symlinks() {
        use std::os::unix::fs::symlink;

        let temp = tempdir();
        let target = temp.path().join("target.json");
        let link = temp.path().join("selector.json");
        std::fs::write(&target, "{}").unwrap();
        symlink(target, &link).unwrap();
        assert_eq!(
            SelectorReader::default().read(&link, SelectorFormat::Json),
            Err(SelectorReadError::UnsupportedRoot)
        );
    }

    #[test]
    fn root_handle_discovery_selector_swap_fails_final_revalidation() {
        let temp = tempdir();
        let selector = temp.path().join("selector.json");
        let moved = temp.path().join("opened-selector.json");
        let replacement = temp.path().join("replacement.json");
        fs::write(&selector, r#"{"root":"opened"}"#).unwrap();
        fs::write(&replacement, r#"{"root":"replacement"}"#).unwrap();

        let selector_for_hook = selector.clone();
        let moved_for_hook = moved.clone();
        SELECTOR_FILE_OPEN_HOOK.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                fs::rename(&selector_for_hook, &moved_for_hook).unwrap();
                fs::rename(&replacement, &selector_for_hook).unwrap();
            }));
        });

        assert_eq!(
            SelectorReader::default().read(&selector, SelectorFormat::Json),
            Err(SelectorReadError::UnsupportedRoot)
        );
        assert_eq!(fs::read_to_string(moved).unwrap(), r#"{"root":"opened"}"#);
    }

    #[test]
    fn root_handle_discovery_directory_swap_fails_final_revalidation() {
        let temp = tempdir();
        let root = temp.path().join("root");
        let moved = temp.path().join("opened-root");
        let replacement = temp.path().join("replacement");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&replacement).unwrap();
        fs::write(root.join("opened.json"), "{}").unwrap();
        fs::write(replacement.join("replacement.json"), "{}").unwrap();

        let root_for_hook = root.clone();
        let moved_for_hook = moved.clone();
        DIRECT_ENTRIES_ROOT_OPEN_HOOK.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                fs::rename(&root_for_hook, &moved_for_hook).unwrap();
                fs::rename(&replacement, &root_for_hook).unwrap();
            }));
        });

        assert_eq!(
            direct_entries(&root),
            Err(SelectorReadError::UnsupportedRoot)
        );
        assert!(moved.join("opened.json").is_file());
    }

    #[test]
    fn direct_entries_rejects_same_name_child_replacement_between_bounded_passes() {
        let temp = tempdir();
        let root = temp.path().join("root");
        let child = root.join("child");
        let moved = root.join("opened-child");
        let replacement = root.join("replacement");
        fs::create_dir_all(&child).unwrap();
        fs::create_dir_all(&replacement).unwrap();

        let child_for_hook = child.clone();
        let moved_for_hook = moved.clone();
        let replacement_for_hook = replacement.clone();
        DIRECT_ENTRIES_FIRST_PASS_HOOK.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                fs::rename(&child_for_hook, &moved_for_hook).unwrap();
                fs::rename(&replacement_for_hook, &child_for_hook).unwrap();
            }));
        });

        assert_eq!(direct_entries(&root), Err(SelectorReadError::Unavailable));
        assert!(moved.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn direct_entries_rejects_link_roots_and_children() {
        use std::os::unix::fs::symlink;

        let temp = tempdir();
        let outside = temp.path().join("outside");
        let entries = temp.path().join("entries");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(&entries).unwrap();
        std::fs::write(outside.join("external.json"), "{}").unwrap();
        std::fs::write(entries.join("ordinary.json"), "{}").unwrap();
        symlink(&outside, entries.join("linked-child")).unwrap();
        let linked_root = temp.path().join("linked-root");
        symlink(&entries, &linked_root).unwrap();

        assert_eq!(
            direct_entries(&entries),
            Err(SelectorReadError::UnsupportedRoot)
        );
        assert_eq!(
            direct_entries(&linked_root),
            Err(SelectorReadError::UnsupportedRoot)
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn direct_entries_rejects_windows_reparse_roots_and_children() {
        use std::{io::ErrorKind, os::windows::fs::symlink_dir};

        let temp = tempdir();
        let outside = temp.path().join("outside");
        let entries = temp.path().join("entries");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(&entries).unwrap();
        std::fs::write(entries.join("ordinary.json"), "{}").unwrap();
        if let Err(error) = symlink_dir(&outside, entries.join("linked-child")) {
            if error.kind() == ErrorKind::PermissionDenied || error.raw_os_error() == Some(1314) {
                return;
            }
            panic!("failed to create Windows child reparse point: {error}");
        }
        let linked_root = temp.path().join("linked-root");
        symlink_dir(&entries, &linked_root)
            .unwrap_or_else(|error| panic!("failed to create Windows root reparse point: {error}"));

        assert_eq!(
            direct_entries(&entries),
            Err(SelectorReadError::UnsupportedRoot)
        );
        assert_eq!(
            direct_entries(&linked_root),
            Err(SelectorReadError::UnsupportedRoot)
        );
    }

    #[test]
    fn xml_reader_rejects_document_types_and_entity_references() {
        let temp = tempdir();
        let xml = temp.path().join("selector.xml");
        std::fs::write(
            &xml,
            r#"<!DOCTYPE config [<!ENTITY external SYSTEM "file:///etc/passwd">]><config>&external;</config>"#,
        )
        .unwrap();
        assert_eq!(
            SelectorReader::default().read(&xml, SelectorFormat::Xml),
            Err(SelectorReadError::XmlDoctype)
        );
    }

    #[test]
    fn xml_reader_unescapes_attribute_entity_paths() {
        let temp = tempdir();
        let xml = temp.path().join("selector.xml");
        std::fs::write(
            &xml,
            r#"<application><component><option value="/tmp/A&amp;B/&#x8DEF;&#24452;"/></component></application>"#,
        )
        .unwrap();
        let document = SelectorReader::default()
            .read(&xml, SelectorFormat::Xml)
            .unwrap();
        assert_eq!(
            document
                .xml()
                .unwrap()
                .values(&["application", "component", "option"], Some("value")),
            vec!["/tmp/A&B/路径"]
        );
    }

    #[test]
    fn xml_reader_rejects_duplicate_attributes_after_many_unique_names() {
        let temp = tempdir();
        let xml = temp.path().join("selector.xml");
        let attributes = (0..32)
            .map(|index| format!(r#" key{index}="{index}""#))
            .collect::<String>();
        std::fs::write(&xml, format!(r#"<option{attributes} key0="duplicate"/>"#)).unwrap();

        assert_eq!(
            SelectorReader::default().read(&xml, SelectorFormat::Xml),
            Err(SelectorReadError::Parse)
        );
    }
}
