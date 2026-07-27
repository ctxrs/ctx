//! Bounded extraction of structured-container roots from JSON5 and XML manifests.

use std::path::PathBuf;

use quick_xml::{events::Event as XmlEvent, Reader as XmlReader};
use serde_json::Value;

use super::{
    error, parsing::validate_json_shape, AdmissionContext, CompleteContentError,
    CompleteContentErrorKind, ResolutionBudget,
};

pub(super) fn profile_roots_from_json5(
    request: &AdmissionContext<'_>,
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

pub(super) fn profile_roots_from_xml(
    request: &AdmissionContext<'_>,
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
    request: &AdmissionContext<'_>,
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
