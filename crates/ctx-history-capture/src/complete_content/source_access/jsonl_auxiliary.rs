//! Broker-owned admission for identity-affecting JSONL auxiliary files.
//!
//! Exact provider bindings consume only captured metadata and bounded bytes
//! from this module. Provider code never receives a path it can reopen.

#[cfg(unix)]
use std::io;
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
};

use ctx_history_core::CaptureProvider;
use uuid::Uuid;

use crate::{
    complete_content::{
        jsonl::ExactJsonlSourceBinding, CompleteContentError, CompleteContentErrorKind,
    },
    provider::{
        provider_path_identity,
        providers::{codebuddy, cursor, kimi, mistral_vibe, openclaw},
    },
    CODEBUDDY_SOURCE_FORMAT, CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT, KIMI_CODE_CLI_SOURCE_FORMAT,
    MAX_OPENCLAW_SESSION_INDEX_BYTES, MISTRAL_VIBE_SOURCE_FORMAT, OPENCLAW_SOURCE_FORMAT,
};

use super::{
    jsonl::read_exact_at, map_capture_error, map_io_error, normalize_lexical,
    AuthorizedSourceRoute, FrozenFile,
};

#[cfg(unix)]
use super::open_brokered_file;
#[cfg(target_os = "windows")]
use super::windows;

const JSONL_AUXILIARY_MAX_COMPONENT_BYTES: usize = 16 * 1024 * 1024;

pub(super) struct BrokeredJsonlAuxiliary {
    path: PathBuf,
    file: Option<File>,
    metadata: Option<fs::Metadata>,
    frozen: Option<FrozenFile>,
}

impl BrokeredJsonlAuxiliary {
    pub(super) fn revalidate(
        &self,
        containment_root: Option<&Path>,
        event_id: Uuid,
    ) -> Result<bool, CompleteContentError> {
        revalidate_brokered_regular_file(
            &self.path,
            self.file.as_ref(),
            self.frozen.as_ref(),
            containment_root,
            event_id,
        )
    }
}

pub(super) fn admit_exact_jsonl_binding(
    route: &AuthorizedSourceRoute,
    selected_path: &Path,
    main_file: &File,
    main_metadata: &fs::Metadata,
    event_id: Uuid,
) -> Result<(Option<ExactJsonlSourceBinding>, Vec<BrokeredJsonlAuxiliary>), CompleteContentError> {
    let exact = matches!(
        (route.provider, route.source_format.as_str()),
        (CaptureProvider::CodeBuddy, CODEBUDDY_SOURCE_FORMAT)
            | (
                CaptureProvider::Cursor,
                CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT
            )
            | (CaptureProvider::MistralVibe, MISTRAL_VIBE_SOURCE_FORMAT)
            | (CaptureProvider::OpenClaw, OPENCLAW_SOURCE_FORMAT)
            | (CaptureProvider::KimiCodeCli, KIMI_CODE_CLI_SOURCE_FORMAT)
    );
    if !exact {
        return Ok((None, Vec::new()));
    }

    // Canonical identity is captured once inside the broker, then immediately
    // checked against the already-opened no-follow handle.
    let canonical_path =
        fs::canonicalize(selected_path).map_err(|cause| map_io_error(event_id, cause))?;
    let path_identity = provider_path_identity(&canonical_path)
        .map_err(|cause| map_capture_error(event_id, cause))?;
    let main_frozen = FrozenFile::from_file(main_file, main_metadata)
        .map_err(|cause| map_io_error(event_id, cause))?;
    if !revalidate_brokered_regular_file(
        selected_path,
        Some(main_file),
        Some(&main_frozen),
        route.source_root.as_deref(),
        event_id,
    )? {
        return Err(changed(event_id));
    }

    let mut auxiliaries = Vec::new();
    let observed = match route.provider {
        CaptureProvider::CodeBuddy => {
            codebuddy::codebuddy_cli_complete_content_source_from_admitted(
                main_metadata,
                path_identity,
            )
        }
        CaptureProvider::Cursor => {
            cursor::cursor_complete_content_source_from_admitted(main_metadata, path_identity)
        }
        CaptureProvider::MistralVibe => {
            let metadata_path = sibling(selected_path, "meta.json", event_id)?;
            let (metadata, _) = admit_optional_auxiliary(route, metadata_path, None, event_id)?;
            let admitted_metadata = metadata
                .metadata
                .as_ref()
                .ok_or_else(|| changed(event_id))?;
            let result = mistral_vibe::mistral_vibe_complete_content_source_from_admitted(
                admitted_metadata,
                main_metadata,
                path_identity,
            );
            auxiliaries.push(metadata);
            result
        }
        CaptureProvider::OpenClaw => {
            let index_path = sibling(selected_path, "sessions.json", event_id)?;
            let (index, index_bytes) = admit_optional_auxiliary(
                route,
                index_path,
                Some(MAX_OPENCLAW_SESSION_INDEX_BYTES),
                event_id,
            )?;
            let admitted_index = index.metadata.as_ref().zip(index_bytes.as_deref());
            let result = openclaw::openclaw_complete_content_source_from_admitted(
                selected_path,
                main_metadata,
                admitted_index,
                path_identity,
            );
            auxiliaries.push(index);
            result
        }
        CaptureProvider::KimiCodeCli => {
            let (state_path, index_path) =
                kimi::kimi_complete_content_auxiliary_paths(selected_path)
                    .map_err(|cause| map_capture_error(event_id, cause))?;
            let (state, state_bytes) = admit_optional_auxiliary(
                route,
                state_path,
                Some(JSONL_AUXILIARY_MAX_COMPONENT_BYTES),
                event_id,
            )?;
            let (index, index_bytes) = admit_optional_auxiliary(
                route,
                index_path,
                Some(JSONL_AUXILIARY_MAX_COMPONENT_BYTES),
                event_id,
            )?;
            let admitted_state = state.metadata.as_ref().zip(state_bytes.as_deref());
            let admitted_index = index.metadata.as_ref().zip(index_bytes.as_deref());
            let result = kimi::kimi_complete_content_source_from_admitted(
                selected_path,
                route.source_root.as_deref(),
                canonical_path,
                main_metadata,
                admitted_state,
                admitted_index,
                path_identity,
            );
            auxiliaries.push(state);
            auxiliaries.push(index);
            result
        }
        _ => unreachable!("exact provider set checked above"),
    };
    let (revision, identity) = observed.map_err(|cause| map_capture_error(event_id, cause))?;
    for auxiliary in &auxiliaries {
        if !auxiliary.revalidate(route.source_root.as_deref(), event_id)? {
            return Err(changed(event_id));
        }
    }
    Ok((
        Some(ExactJsonlSourceBinding::new(&revision, &identity)),
        auxiliaries,
    ))
}

fn sibling(
    selected_path: &Path,
    name: &str,
    event_id: Uuid,
) -> Result<PathBuf, CompleteContentError> {
    selected_path
        .parent()
        .map(|parent| parent.join(name))
        .ok_or_else(|| {
            CompleteContentError::new(CompleteContentErrorKind::HydrationUnsupported, event_id)
        })
}

fn validate_auxiliary_route(
    route: &AuthorizedSourceRoute,
    path: &Path,
    event_id: Uuid,
) -> Result<(), CompleteContentError> {
    let path = normalize_lexical(path).ok_or_else(|| {
        CompleteContentError::new(CompleteContentErrorKind::SourceUnreadable, event_id)
    })?;
    if let Some(root) = route.source_root.as_deref().and_then(normalize_lexical) {
        #[cfg(target_os = "windows")]
        let contained = windows::lexical_path_is_within(&path, &root);
        #[cfg(not(target_os = "windows"))]
        let contained = path == root || path.starts_with(&root);
        if !contained {
            return Err(CompleteContentError::new(
                CompleteContentErrorKind::SourceUnreadable,
                event_id,
            ));
        }
    }
    #[cfg(target_os = "windows")]
    windows::validate_local_qualified_path(&path, event_id)?;
    Ok(())
}

#[cfg(unix)]
fn admit_optional_auxiliary(
    route: &AuthorizedSourceRoute,
    path: PathBuf,
    content_limit: Option<usize>,
    event_id: Uuid,
) -> Result<(BrokeredJsonlAuxiliary, Option<Vec<u8>>), CompleteContentError> {
    validate_auxiliary_route(route, &path, event_id)?;
    let file = match open_brokered_file(&path) {
        Ok(file) => file,
        Err(cause) if cause.kind() == io::ErrorKind::NotFound => {
            return Ok((absent(path), None));
        }
        Err(cause) => return Err(map_io_error(event_id, cause)),
    };
    finish_auxiliary_admission(route, path, file, content_limit, event_id)
}

#[cfg(target_os = "windows")]
fn admit_optional_auxiliary(
    route: &AuthorizedSourceRoute,
    path: PathBuf,
    content_limit: Option<usize>,
    event_id: Uuid,
) -> Result<(BrokeredJsonlAuxiliary, Option<Vec<u8>>), CompleteContentError> {
    validate_auxiliary_route(route, &path, event_id)?;
    let Some(admitted) =
        windows::admit_optional_regular_file(&path, route.source_root.as_deref(), event_id)?
    else {
        return Ok((absent(path), None));
    };
    finish_auxiliary_admission(route, path, admitted.file, content_limit, event_id)
}

fn absent(path: PathBuf) -> BrokeredJsonlAuxiliary {
    BrokeredJsonlAuxiliary {
        path,
        file: None,
        metadata: None,
        frozen: None,
    }
}

fn finish_auxiliary_admission(
    route: &AuthorizedSourceRoute,
    path: PathBuf,
    file: File,
    content_limit: Option<usize>,
    event_id: Uuid,
) -> Result<(BrokeredJsonlAuxiliary, Option<Vec<u8>>), CompleteContentError> {
    let metadata = file
        .metadata()
        .map_err(|cause| map_io_error(event_id, cause))?;
    let frozen =
        FrozenFile::from_file(&file, &metadata).map_err(|cause| map_io_error(event_id, cause))?;
    let bytes = content_limit
        .map(|maximum| read_bounded(&file, &metadata, maximum, event_id))
        .transpose()?;
    if !revalidate_brokered_regular_file(
        &path,
        Some(&file),
        Some(&frozen),
        route.source_root.as_deref(),
        event_id,
    )? {
        return Err(changed(event_id));
    }
    Ok((
        BrokeredJsonlAuxiliary {
            path,
            file: Some(file),
            metadata: Some(metadata),
            frozen: Some(frozen),
        },
        bytes,
    ))
}

fn read_bounded(
    file: &File,
    metadata: &fs::Metadata,
    maximum: usize,
    event_id: Uuid,
) -> Result<Vec<u8>, CompleteContentError> {
    let length = usize::try_from(metadata.len())
        .ok()
        .filter(|length| *length <= maximum)
        .ok_or_else(|| {
            CompleteContentError::new(CompleteContentErrorKind::ContentTooLarge, event_id)
        })?;
    let mut bytes = vec![0_u8; length];
    if !bytes.is_empty() {
        read_exact_at(file, &mut bytes, 0).map_err(|cause| map_io_error(event_id, cause))?;
    }
    Ok(bytes)
}

#[cfg(unix)]
pub(super) fn revalidate_brokered_regular_file(
    path: &Path,
    held: Option<&File>,
    frozen: Option<&FrozenFile>,
    _containment_root: Option<&Path>,
    _event_id: Uuid,
) -> Result<bool, CompleteContentError> {
    let held_frozen = held
        .and_then(|file| file.metadata().ok().map(|metadata| (file, metadata)))
        .and_then(|(file, metadata)| FrozenFile::from_file(file, &metadata).ok());
    let selected_frozen = match open_brokered_file(path) {
        Ok(file) => match file
            .metadata()
            .ok()
            .and_then(|metadata| FrozenFile::from_file(&file, &metadata).ok())
        {
            Some(frozen) => Some(frozen),
            None => return Ok(false),
        },
        Err(cause) if cause.kind() == io::ErrorKind::NotFound => None,
        Err(_) => return Ok(false),
    };
    Ok(held_frozen.as_ref() == frozen && selected_frozen.as_ref() == frozen)
}

#[cfg(target_os = "windows")]
pub(super) fn revalidate_brokered_regular_file(
    path: &Path,
    held: Option<&File>,
    frozen: Option<&FrozenFile>,
    containment_root: Option<&Path>,
    event_id: Uuid,
) -> Result<bool, CompleteContentError> {
    let held_frozen = held
        .and_then(|file| file.metadata().ok().map(|metadata| (file, metadata)))
        .and_then(|(file, metadata)| FrozenFile::from_file(file, &metadata).ok());
    let selected_frozen =
        match windows::admit_optional_regular_file(path, containment_root, event_id) {
            Ok(Some(file)) => match FrozenFile::from_file(&file.file, &file.metadata) {
                Ok(frozen) => Some(frozen),
                Err(_) => return Ok(false),
            },
            Ok(None) => None,
            Err(_) => return Ok(false),
        };
    Ok(held_frozen.as_ref() == frozen && selected_frozen.as_ref() == frozen)
}

fn changed(event_id: Uuid) -> CompleteContentError {
    CompleteContentError::new(CompleteContentErrorKind::SourceChanged, event_id)
}
