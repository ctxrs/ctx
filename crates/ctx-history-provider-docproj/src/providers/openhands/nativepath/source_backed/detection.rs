use std::path::{Component, Path, PathBuf};

use super::{OpenHandsSourceBackedErrorV2, OpenHandsSourceBackedResultV2};

pub(super) struct OpenHandsEventPath {
    pub(super) conversation_id: String,
    pub(super) conversation_root: PathBuf,
}

/// Recognizes the two bounded OpenHands event-file layouts.
///
/// Legacy V1 permits JSON leaves below `v1_conversations/<id>/` for released
/// compatibility. Current CLI history is intentionally narrower and admits
/// only `<conversation-root>/<id>/events/event-*.json`; the official direct
/// conversation-root override does not constrain the root directory's name.
pub(super) fn openhands_event_path(
    path: &Path,
) -> OpenHandsSourceBackedResultV2<Option<OpenHandsEventPath>> {
    if let Some(event) = openhands_legacy_event_path(path)? {
        return Ok(Some(event));
    }
    openhands_current_event_path(path)
}

pub(super) fn openhands_legacy_event_path(
    path: &Path,
) -> OpenHandsSourceBackedResultV2<Option<OpenHandsEventPath>> {
    let components = path.components().collect::<Vec<_>>();
    for index in 0..components.len() {
        let legacy = components[index].as_os_str() == "v1_conversations"
            && components.len() >= index.saturating_add(3)
            && path.extension().and_then(|extension| extension.to_str()) == Some("json");
        if legacy {
            return Ok(Some(OpenHandsEventPath {
                conversation_id: normal_component(path, &components, index.saturating_add(1))?,
                conversation_root: path_through(&components, index.saturating_add(1)),
            }));
        }
    }
    Ok(None)
}

pub(super) fn openhands_current_event_path(
    path: &Path,
) -> OpenHandsSourceBackedResultV2<Option<OpenHandsEventPath>> {
    let components = path.components().collect::<Vec<_>>();
    let Some(conversation_index) = components.len().checked_sub(3) else {
        return Ok(None);
    };
    let current = components
        .get(conversation_index.saturating_add(1))
        .is_some_and(|component| component.as_os_str() == "events")
        && current_cli_event_file(path);
    if current {
        return Ok(Some(OpenHandsEventPath {
            conversation_id: normal_component(path, &components, conversation_index)?,
            conversation_root: path_through(&components, conversation_index),
        }));
    }
    Ok(None)
}

fn normal_component(
    path: &Path,
    components: &[Component<'_>],
    index: usize,
) -> OpenHandsSourceBackedResultV2<String> {
    components
        .get(index)
        .and_then(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(
            || OpenHandsSourceBackedErrorV2::MissingConversationCoordinate {
                path: path.to_path_buf(),
            },
        )
}

fn path_through(components: &[Component<'_>], inclusive_index: usize) -> PathBuf {
    let mut root = PathBuf::new();
    for component in components.iter().take(inclusive_index.saturating_add(1)) {
        root.push(component.as_os_str());
    }
    root
}

fn current_cli_event_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("event-") && name.ends_with(".json"))
}
