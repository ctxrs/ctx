//! Bounded source discovery, immutable reads, and structured-container scanning.

use std::{
    collections::BTreeSet,
    fs::{self, File, Metadata},
    io::Read,
    path::{Component, Path, PathBuf},
};

use quick_xml::{events::Event as XmlEvent, Reader as XmlReader};
use serde_json::Value;

use ctx_history_core::CaptureProvider;

use super::{
    CompleteContentError, CompleteContentErrorKind, CompleteMessageRequest, ResolutionBudget,
    COMPLETE_CONTENT_MAX_BODY_BYTES,
};

pub(super) fn selected_roots(
    request: &CompleteMessageRequest,
    budget: &mut ResolutionBudget,
) -> std::result::Result<Vec<PathBuf>, CompleteContentError> {
    let mut roots = BTreeSet::new();
    if request
        .raw_source_path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|ext| matches!(ext, "json5" | "xml"))
    {
        let bytes = read_frozen_file(request, &request.raw_source_path, budget)?;
        let configured = match request
            .raw_source_path
            .extension()
            .and_then(|value| value.to_str())
        {
            Some("json5") => profile_roots_from_json5(request, &bytes, budget)?,
            Some("xml") => profile_roots_from_xml(request, &bytes, budget)?,
            _ => Vec::new(),
        };
        for root in configured {
            if path_allowed_by_root(request, &root) {
                roots.insert(root);
            }
        }
    } else {
        roots.insert(request.raw_source_path.clone());
    }
    if let Some(root) = request.source_root.as_ref() {
        roots.insert(root.clone());
    }
    if roots.is_empty() {
        return Err(error(request, CompleteContentErrorKind::SourceMissing));
    }
    Ok(roots.into_iter().collect())
}

pub(super) fn candidate_files(
    request: &CompleteMessageRequest,
    roots: &[PathBuf],
    budget: &mut ResolutionBudget,
) -> std::result::Result<Vec<PathBuf>, CompleteContentError> {
    let mut paths = BTreeSet::new();
    for root in roots {
        if !path_allowed_by_root(request, root) {
            return Err(error(
                request,
                CompleteContentErrorKind::ContentVerificationFailed,
            ));
        }
        match fs::symlink_metadata(root) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(error(request, CompleteContentErrorKind::SourceChanged));
            }
            Ok(metadata) if metadata.is_file() => {
                budget.observe_file(request)?;
                paths.insert(root.clone());
            }
            Ok(metadata) if metadata.is_dir() => {
                collect_files(request, root, 0, &mut paths, budget)?;
            }
            Ok(_) => {}
            Err(error_value) if error_value.kind() == std::io::ErrorKind::NotFound => {}
            Err(error_value) if error_value.kind() == std::io::ErrorKind::PermissionDenied => {
                return Err(error(request, CompleteContentErrorKind::SourceUnreadable));
            }
            Err(_) => return Err(error(request, CompleteContentErrorKind::SourceUnreadable)),
        }
    }
    Ok(paths.into_iter().collect())
}

fn collect_files(
    request: &CompleteMessageRequest,
    root: &Path,
    depth: usize,
    paths: &mut BTreeSet<PathBuf>,
    budget: &mut ResolutionBudget,
) -> std::result::Result<(), CompleteContentError> {
    if depth > budget.bounds.max_depth {
        return Err(error(request, CompleteContentErrorKind::ContentTooLarge));
    }
    budget.check(request)?;
    let entries = fs::read_dir(root)
        .map_err(|_| error(request, CompleteContentErrorKind::SourceUnreadable))?;
    for entry in entries {
        budget.observe_entries(request, 1)?;
        let entry =
            entry.map_err(|_| error(request, CompleteContentErrorKind::SourceUnreadable))?;
        let file_type = entry
            .file_type()
            .map_err(|_| error(request, CompleteContentErrorKind::SourceUnreadable))?;
        if file_type.is_symlink() {
            return Err(error(request, CompleteContentErrorKind::SourceChanged));
        }
        let path = entry.path();
        if file_type.is_dir() {
            collect_files(request, &path, depth.saturating_add(1), paths, budget)?;
        } else if file_type.is_file() {
            budget.observe_file(request)?;
            paths.insert(path);
        }
    }
    Ok(())
}

fn path_allowed_by_root(request: &CompleteMessageRequest, path: &Path) -> bool {
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return false;
    }
    match request.source_root.as_ref() {
        Some(root) => path.starts_with(root) || path == request.raw_source_path,
        None => true,
    }
}

pub(super) fn whole_json_path_candidate(provider: CaptureProvider, path: &Path) -> bool {
    let file_name = path.file_name().and_then(|value| value.to_str());
    match provider {
        CaptureProvider::Auggie => {
            path.extension().and_then(|value| value.to_str()) == Some("json")
        }
        CaptureProvider::Continue => {
            path.extension().and_then(|value| value.to_str()) == Some("json")
                && file_name != Some("sessions.json")
        }
        CaptureProvider::RovoDev => file_name == Some("session_context.json"),
        _ => false,
    }
}

#[derive(Debug)]
struct FrozenFile {
    metadata: Metadata,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    ctime: i64,
    #[cfg(unix)]
    ctime_nsec: i64,
}

impl FrozenFile {
    fn from_metadata(metadata: Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Self {
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
            #[cfg(unix)]
            ctime: metadata.ctime(),
            #[cfg(unix)]
            ctime_nsec: metadata.ctime_nsec(),
            metadata,
        }
    }

    fn same(&self, other: &Self) -> bool {
        let common = self.metadata.len() == other.metadata.len()
            && self.metadata.modified().ok() == other.metadata.modified().ok();
        #[cfg(unix)]
        return common
            && self.device == other.device
            && self.inode == other.inode
            && self.ctime == other.ctime
            && self.ctime_nsec == other.ctime_nsec;
        #[cfg(not(unix))]
        common
    }
}

pub(super) fn read_frozen_file(
    request: &CompleteMessageRequest,
    path: &Path,
    budget: &mut ResolutionBudget,
) -> std::result::Result<Vec<u8>, CompleteContentError> {
    read_frozen_file_with_limit(request, path, budget, COMPLETE_CONTENT_MAX_BODY_BYTES)
}

pub(super) fn read_frozen_file_with_limit(
    request: &CompleteMessageRequest,
    path: &Path,
    budget: &mut ResolutionBudget,
    max_file_bytes: usize,
) -> std::result::Result<Vec<u8>, CompleteContentError> {
    ensure_no_symlink_components(request, path)?;
    let before = fs::symlink_metadata(path).map_err(|value| match value.kind() {
        std::io::ErrorKind::NotFound => error(request, CompleteContentErrorKind::SourceMissing),
        std::io::ErrorKind::PermissionDenied => {
            error(request, CompleteContentErrorKind::SourceUnreadable)
        }
        _ => error(request, CompleteContentErrorKind::SourceUnreadable),
    })?;
    if before.file_type().is_symlink() || !before.is_file() {
        return Err(error(request, CompleteContentErrorKind::SourceChanged));
    }
    let file_bytes = usize::try_from(before.len()).unwrap_or(usize::MAX);
    budget.observe_bytes(request, file_bytes)?;
    if file_bytes > max_file_bytes {
        return Err(error(request, CompleteContentErrorKind::ContentTooLarge));
    }
    let before = FrozenFile::from_metadata(before);
    let mut file = open_no_follow(path)
        .map_err(|_| error(request, CompleteContentErrorKind::SourceUnreadable))?;
    let opened = FrozenFile::from_metadata(
        file.metadata()
            .map_err(|_| error(request, CompleteContentErrorKind::SourceUnreadable))?,
    );
    if !before.same(&opened) {
        return Err(error(request, CompleteContentErrorKind::SourceChanged));
    }
    let mut bytes = Vec::with_capacity(before.metadata.len() as usize);
    file.by_ref()
        .take((max_file_bytes as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| error(request, CompleteContentErrorKind::SourceUnreadable))?;
    if bytes.len() > max_file_bytes {
        return Err(error(request, CompleteContentErrorKind::ContentTooLarge));
    }
    let opened_after = FrozenFile::from_metadata(
        file.metadata()
            .map_err(|_| error(request, CompleteContentErrorKind::SourceUnreadable))?,
    );
    let path_after = FrozenFile::from_metadata(
        fs::symlink_metadata(path)
            .map_err(|_| error(request, CompleteContentErrorKind::SourceChanged))?,
    );
    if !opened.same(&opened_after)
        || !opened_after.same(&path_after)
        || u64::try_from(bytes.len()).ok() != Some(opened.metadata.len())
    {
        return Err(error(request, CompleteContentErrorKind::SourceChanged));
    }
    Ok(bytes)
}

#[cfg(unix)]
fn open_no_follow(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn open_no_follow(path: &Path) -> std::io::Result<File> {
    File::open(path)
}

fn ensure_no_symlink_components(
    request: &CompleteMessageRequest,
    path: &Path,
) -> std::result::Result<(), CompleteContentError> {
    let parent_count = path.components().count().saturating_sub(1);
    let mut current = PathBuf::new();
    for component in path.components().take(parent_count) {
        current.push(component.as_os_str());
        if current.as_os_str().is_empty() {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(error(request, CompleteContentErrorKind::SourceChanged));
            }
            Ok(_) => {}
            Err(value) if value.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(error(request, CompleteContentErrorKind::SourceUnreadable)),
        }
    }
    Ok(())
}

pub(super) fn parse_bounded_json(
    request: &CompleteMessageRequest,
    bytes: &[u8],
    budget: &mut ResolutionBudget,
) -> std::result::Result<Value, CompleteContentError> {
    let value = serde_json::from_slice(bytes)
        .map_err(|_| error(request, CompleteContentErrorKind::SourceChanged))?;
    validate_json_shape(request, &value, budget, 0)?;
    Ok(value)
}

pub(super) fn validate_json_shape(
    request: &CompleteMessageRequest,
    value: &Value,
    budget: &mut ResolutionBudget,
    depth: usize,
) -> std::result::Result<(), CompleteContentError> {
    if depth > budget.bounds.max_json_depth {
        return Err(error(request, CompleteContentErrorKind::ContentTooLarge));
    }
    budget.observe_entries(request, 1)?;
    match value {
        Value::Array(items) => {
            for item in items {
                validate_json_shape(request, item, budget, depth.saturating_add(1))?;
            }
        }
        Value::Object(object) => {
            for value in object.values() {
                validate_json_shape(request, value, budget, depth.saturating_add(1))?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub(super) struct TaskJsonRecord<'a> {
    pub(super) native_index: usize,
    pub(super) bytes: &'a [u8],
    pub(super) value: Value,
}

pub(super) fn task_json_records<'a>(
    request: &CompleteMessageRequest,
    bytes: &'a [u8],
    budget: &mut ResolutionBudget,
) -> std::result::Result<Vec<TaskJsonRecord<'a>>, CompleteContentError> {
    let range = locate_task_array(bytes)
        .ok_or_else(|| error(request, CompleteContentErrorKind::SourceChanged))?;
    let mut records = Vec::new();
    let mut cursor = range.start;
    while cursor < range.end {
        cursor = skip_json_whitespace(bytes, cursor);
        if cursor >= range.end {
            break;
        }
        let end = scan_json_value_end(bytes, cursor, range.end)
            .ok_or_else(|| error(request, CompleteContentErrorKind::SourceChanged))?;
        let raw = &bytes[cursor..end];
        let value = parse_bounded_json(request, raw, budget)?;
        records.push(TaskJsonRecord {
            native_index: records.len(),
            bytes: raw,
            value,
        });
        budget.observe_entries(request, 1)?;
        cursor = skip_json_whitespace(bytes, end);
        if cursor < range.end {
            if bytes[cursor] != b',' {
                return Err(error(request, CompleteContentErrorKind::SourceChanged));
            }
            cursor = cursor.saturating_add(1);
        }
    }
    Ok(records)
}

fn locate_task_array(bytes: &[u8]) -> Option<std::ops::Range<usize>> {
    let start = skip_json_whitespace(bytes, 0);
    match *bytes.get(start)? {
        b'[' => matching_container_range(bytes, start, b'[', b']'),
        b'{' => locate_named_task_array(bytes, start),
        _ => None,
    }
}

fn locate_named_task_array(bytes: &[u8], object_start: usize) -> Option<std::ops::Range<usize>> {
    let mut cursor = object_start.checked_add(1)?;
    loop {
        cursor = skip_json_whitespace(bytes, cursor);
        if bytes.get(cursor) == Some(&b'}') {
            return None;
        }
        let key_end = scan_json_value_end(bytes, cursor, bytes.len())?;
        let key = serde_json::from_slice::<String>(bytes.get(cursor..key_end)?).ok()?;
        cursor = skip_json_whitespace(bytes, key_end);
        if bytes.get(cursor) != Some(&b':') {
            return None;
        }
        cursor = skip_json_whitespace(bytes, cursor.checked_add(1)?);
        if matches!(key.as_str(), "messages" | "history") && bytes.get(cursor) == Some(&b'[') {
            return matching_container_range(bytes, cursor, b'[', b']');
        }
        cursor = scan_json_value_end(bytes, cursor, bytes.len())?;
        cursor = skip_json_whitespace(bytes, cursor);
        match bytes.get(cursor) {
            Some(b',') => cursor = cursor.checked_add(1)?,
            Some(b'}') | None => return None,
            _ => return None,
        }
    }
}

fn matching_container_range(
    bytes: &[u8],
    start: usize,
    open: u8,
    close: u8,
) -> Option<std::ops::Range<usize>> {
    let mut depth = 0_usize;
    let mut quoted = false;
    let mut escaped = false;
    for (offset, byte) in bytes.get(start..)?.iter().copied().enumerate() {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
            continue;
        }
        if byte == b'"' {
            quoted = true;
        } else if byte == open {
            depth = depth.saturating_add(1);
        } else if byte == close {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(start + 1..start + offset);
            }
        }
    }
    None
}

fn scan_json_value_end(bytes: &[u8], start: usize, limit: usize) -> Option<usize> {
    let first = *bytes.get(start)?;
    if matches!(first, b'{' | b'[') {
        let close = if first == b'{' { b'}' } else { b']' };
        return matching_container_range(bytes.get(..limit)?, start, first, close)
            .map(|range| range.end + 1);
    }
    if first == b'"' {
        let mut escaped = false;
        for (cursor, byte) in bytes
            .iter()
            .copied()
            .enumerate()
            .take(limit)
            .skip(start + 1)
        {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                return Some(cursor + 1);
            }
        }
        return None;
    }
    (start..limit)
        .find(|cursor| matches!(bytes[*cursor], b',' | b']' | b'}'))
        .map(|cursor| {
            let mut end = cursor;
            while end > start && bytes[end - 1].is_ascii_whitespace() {
                end -= 1;
            }
            end
        })
        .or(Some(limit))
}

fn skip_json_whitespace(bytes: &[u8], mut cursor: usize) -> usize {
    while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor = cursor.saturating_add(1);
    }
    cursor
}

fn profile_roots_from_json5(
    request: &CompleteMessageRequest,
    bytes: &[u8],
    budget: &mut ResolutionBudget,
) -> std::result::Result<Vec<PathBuf>, CompleteContentError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| error(request, CompleteContentErrorKind::SourceChanged))?;
    let value: Value = json5::from_str(text)
        .map_err(|_| error(request, CompleteContentErrorKind::SourceChanged))?;
    validate_json_shape(request, &value, budget, 0)?;
    let mut roots = Vec::new();
    collect_profile_root_values(&value, &mut roots, budget.bounds.max_json_depth, 0);
    Ok(roots)
}

fn collect_profile_root_values(
    value: &Value,
    roots: &mut Vec<PathBuf>,
    max_depth: usize,
    depth: usize,
) {
    if depth > max_depth {
        return;
    }
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if matches!(
                    key.as_str(),
                    "path" | "storagePath" | "globalStoragePath" | "userDataDir"
                ) {
                    if let Some(path) = value.as_str().filter(|path| !path.trim().is_empty()) {
                        roots.push(PathBuf::from(path));
                    }
                }
                collect_profile_root_values(value, roots, max_depth, depth.saturating_add(1));
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_profile_root_values(item, roots, max_depth, depth.saturating_add(1));
            }
        }
        _ => {}
    }
}

fn profile_roots_from_xml(
    request: &CompleteMessageRequest,
    bytes: &[u8],
    budget: &mut ResolutionBudget,
) -> std::result::Result<Vec<PathBuf>, CompleteContentError> {
    let mut reader = XmlReader::from_reader(bytes);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut roots = Vec::new();
    let mut selected_text = false;
    let mut depth = 0_usize;
    loop {
        budget.observe_entries(request, 1)?;
        match reader.read_event_into(&mut buffer) {
            Ok(XmlEvent::Start(start)) => {
                depth = depth.saturating_add(1);
                if depth > budget.bounds.max_json_depth {
                    return Err(error(request, CompleteContentErrorKind::ContentTooLarge));
                }
                let name = start.name();
                selected_text = matches!(
                    name.as_ref(),
                    b"path" | b"storagePath" | b"globalStoragePath" | b"userDataDir"
                );
                collect_xml_root_attributes(request, &start, &mut roots)?;
            }
            Ok(XmlEvent::Empty(start)) => {
                collect_xml_root_attributes(request, &start, &mut roots)?;
                selected_text = false;
            }
            Ok(XmlEvent::Text(text)) if selected_text => {
                let value = decode_xml_text(text.as_ref())
                    .ok_or_else(|| error(request, CompleteContentErrorKind::SourceChanged))?;
                if !value.trim().is_empty() {
                    roots.push(PathBuf::from(value));
                }
            }
            Ok(XmlEvent::DocType(_)) | Ok(XmlEvent::GeneralRef(_)) => {
                return Err(error(request, CompleteContentErrorKind::SourceChanged));
            }
            Ok(XmlEvent::Eof) => break,
            Ok(XmlEvent::End(_)) => {
                selected_text = false;
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| error(request, CompleteContentErrorKind::SourceChanged))?;
            }
            Ok(_) => {}
            Err(_) => return Err(error(request, CompleteContentErrorKind::SourceChanged)),
        }
        buffer.clear();
    }
    Ok(roots)
}

fn collect_xml_root_attributes(
    request: &CompleteMessageRequest,
    start: &quick_xml::events::BytesStart<'_>,
    roots: &mut Vec<PathBuf>,
) -> std::result::Result<(), CompleteContentError> {
    for attribute in start.attributes().with_checks(true) {
        let attribute =
            attribute.map_err(|_| error(request, CompleteContentErrorKind::SourceChanged))?;
        if matches!(
            attribute.key.as_ref(),
            b"path" | b"storagePath" | b"globalStoragePath" | b"userDataDir"
        ) {
            let value = decode_xml_text(attribute.value.as_ref())
                .ok_or_else(|| error(request, CompleteContentErrorKind::SourceChanged))?;
            if !value.trim().is_empty() {
                roots.push(PathBuf::from(value));
            }
        }
    }
    Ok(())
}

fn decode_xml_text(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut output = String::with_capacity(text.len());
    let mut cursor = 0;
    while let Some(relative) = text.get(cursor..)?.find('&') {
        let amp = cursor + relative;
        output.push_str(text.get(cursor..amp)?);
        let semicolon = text.get(amp..)?.find(';')? + amp;
        let entity = text.get(amp + 1..semicolon)?;
        match entity {
            "amp" => output.push('&'),
            "lt" => output.push('<'),
            "gt" => output.push('>'),
            "quot" => output.push('"'),
            "apos" => output.push('\''),
            _ if entity.starts_with("#x") => {
                output.push(char::from_u32(u32::from_str_radix(&entity[2..], 16).ok()?)?);
            }
            _ if entity.starts_with('#') => {
                output.push(char::from_u32(entity[1..].parse().ok()?)?);
            }
            _ => return None,
        }
        cursor = semicolon + 1;
    }
    output.push_str(text.get(cursor..)?);
    Some(output)
}

pub(super) fn error(
    request: &CompleteMessageRequest,
    kind: CompleteContentErrorKind,
) -> CompleteContentError {
    CompleteContentError::new(kind, request.event_id)
}
