use std::{
    io,
    path::{Component, Path, PathBuf},
};

use crate::{
    common::io::{OpenedProviderSourcePath, ProviderSourceDirectory, ProviderSourceRoot},
    CaptureError,
};

use super::{
    capture_source_error, is_component_local_error, source_access, ClineNativePathError,
    TaskJsonNativeDialect,
};

pub(super) fn resolve_data_root(
    path: &Path,
    dialect: TaskJsonNativeDialect,
) -> Result<(PathBuf, ProviderSourceRoot), ClineNativePathError> {
    let requested = normalized_task_json_authority_path(path)?;
    let (data_root, selected_route) =
        selected_task_json_route(&requested, dialect).ok_or_else(|| {
            ClineNativePathError::UnsupportedRoot {
                path: requested.clone(),
            }
        })?;
    let authority = ProviderSourceRoot::open(&data_root)
        .map_err(|error| capture_source_error(&data_root, "open task-json data root", error))?;
    let tasks = match authority.open_directory(Path::new("tasks")) {
        Ok(tasks) => tasks,
        Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Err(ClineNativePathError::UnsupportedRoot { path: requested });
        }
        Err(error) => {
            return Err(capture_source_error(
                &data_root.join("tasks"),
                "open selected tasks directory",
                error,
            ));
        }
    };
    tasks.revalidate().map_err(|error| {
        capture_source_error(
            &data_root.join("tasks"),
            "revalidate selected tasks directory",
            error,
        )
    })?;
    match selected_route {
        SelectedTaskJsonRoute::DataRoot | SelectedTaskJsonRoute::TasksRoot => {}
        SelectedTaskJsonRoute::TaskDirectory(relative) => {
            let directory = authority.open_directory(&relative).map_err(|error| {
                capture_source_error(&requested, "open selected task directory", error)
            })?;
            if !task_dir_has_component(&directory, &requested, dialect)? {
                return Err(ClineNativePathError::UnsupportedRoot { path: requested });
            }
            directory.revalidate().map_err(|error| {
                capture_source_error(&requested, "revalidate selected task directory", error)
            })?;
        }
        SelectedTaskJsonRoute::File(relative) => {
            authority
                .open_file(&relative)
                .and_then(|file| file.revalidate())
                .map_err(|error| {
                    capture_source_error(&requested, "open selected task-json file", error)
                })?;
        }
    }
    authority.revalidate().map_err(|error| {
        capture_source_error(&data_root, "revalidate task-json data root", error)
    })?;
    Ok((authority.named_path().to_path_buf(), authority))
}

enum SelectedTaskJsonRoute {
    DataRoot,
    TasksRoot,
    TaskDirectory(PathBuf),
    File(PathBuf),
}

fn selected_task_json_route(
    requested: &Path,
    dialect: TaskJsonNativeDialect,
) -> Option<(PathBuf, SelectedTaskJsonRoute)> {
    let file_name = requested.file_name().and_then(|value| value.to_str());
    if file_name.is_some_and(|name| dialect.root_index_file == Some(name)) {
        let data_root = requested.parent()?.parent()?.to_path_buf();
        let relative = requested.strip_prefix(&data_root).ok()?.to_path_buf();
        return Some((data_root, SelectedTaskJsonRoute::File(relative)));
    }
    if file_name.is_some_and(|name| {
        dialect
            .all_task_files()
            .any(|(candidate, _)| candidate == name)
    }) {
        let task_dir = requested.parent()?;
        let data_root = task_dir_data_root(task_dir)?;
        let relative = requested.strip_prefix(&data_root).ok()?.to_path_buf();
        return Some((data_root, SelectedTaskJsonRoute::File(relative)));
    }
    if file_name == Some("tasks") {
        return Some((
            requested.parent()?.to_path_buf(),
            SelectedTaskJsonRoute::TasksRoot,
        ));
    }
    if requested
        .parent()
        .and_then(Path::file_name)
        .and_then(|value| value.to_str())
        == Some("tasks")
    {
        let data_root = task_dir_data_root(requested)?;
        let relative = requested.strip_prefix(&data_root).ok()?.to_path_buf();
        return Some((data_root, SelectedTaskJsonRoute::TaskDirectory(relative)));
    }
    Some((requested.to_path_buf(), SelectedTaskJsonRoute::DataRoot))
}

fn task_dir_data_root(task_dir: &Path) -> Option<PathBuf> {
    let tasks = task_dir
        .parent()
        .filter(|path| path.file_name().and_then(|value| value.to_str()) == Some("tasks"))?;
    tasks.parent().map(Path::to_path_buf)
}

fn task_dir_has_component(
    directory: &ProviderSourceDirectory,
    path: &Path,
    dialect: TaskJsonNativeDialect,
) -> Result<bool, ClineNativePathError> {
    for (file, _) in dialect.all_task_files() {
        let component = path.join(file);
        match directory.open_child(std::ffi::OsStr::new(file)) {
            Ok(OpenedProviderSourcePath::File(opened)) => {
                opened.revalidate().map_err(|error| {
                    capture_source_error(&component, "revalidate task component", error)
                })?;
                return Ok(true);
            }
            Ok(OpenedProviderSourcePath::Directory(opened)) => {
                opened.revalidate().map_err(|error| {
                    capture_source_error(&component, "revalidate task component directory", error)
                })?;
                return Ok(true);
            }
            Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                let classified = capture_source_error(&component, "inspect task component", error);
                if is_component_local_error(&classified) {
                    return Ok(true);
                }
                return Err(classified);
            }
        }
    }
    Ok(false)
}

fn normalized_task_json_authority_path(path: &Path) -> Result<PathBuf, ClineNativePathError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| source_access(path, error))?
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(ClineNativePathError::UnsupportedRoot {
                        path: path.to_path_buf(),
                    });
                }
            }
        }
    }
    Ok(normalized)
}
