// Descriptor-relative, no-follow source opening for Unix broker admission.
//
// `O_NOFOLLOW` on the leaf is insufficient: a concurrent replacement of an
// ancestor can redirect a pathname after validation. Walk every component
// relative to the previously opened directory descriptor instead.

use std::{
    ffi::{CString, OsStr},
    fs::{File, Metadata},
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    },
    path::{Component, Path},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExpectedType {
    File,
    Directory,
}

#[derive(Debug)]
pub(crate) enum OpenedPath {
    File(File),
    Directory(File),
}

impl OpenedPath {
    pub(crate) fn into_file(self) -> File {
        match self {
            Self::File(file) | Self::Directory(file) => file,
        }
    }
}

pub(super) fn open_path(path: &Path, expected: ExpectedType) -> io::Result<File> {
    let opened = open_path_any(path)?;
    match (&opened, expected) {
        (OpenedPath::File(_), ExpectedType::File)
        | (OpenedPath::Directory(_), ExpectedType::Directory) => Ok(opened.into_file()),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "source path has an unsupported filesystem type",
        )),
    }
}

pub(crate) fn open_path_any(path: &Path) -> io::Result<OpenedPath> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "source path must be non-empty and traversal-free",
        ));
    }
    #[cfg(target_os = "macos")]
    let normalized = super::normalize_macos_fixed_root_alias(path);
    #[cfg(target_os = "macos")]
    let path = normalized.as_path();

    let mut components = path.components().peekable();
    let start = match components.peek() {
        Some(Component::RootDir) => {
            components.next();
            open_component(libc::AT_FDCWD, OsStr::new("/"), ExpectedType::Directory)?
        }
        Some(Component::Prefix(_)) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Unix source paths cannot contain a platform prefix",
            ));
        }
        _ => open_component(libc::AT_FDCWD, OsStr::new("."), ExpectedType::Directory)?,
    };
    let mut current = start;
    let mut opened_component = false;
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            continue;
        };
        if components.peek().is_some() {
            current = open_component(current.as_raw_fd(), name, ExpectedType::Directory)?;
        } else {
            return open_child(&current, name);
        }
        opened_component = true;
    }
    if opened_component || path == Path::new("/") || path == Path::new(".") {
        Ok(OpenedPath::Directory(current))
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "source path has no final component",
        ))
    }
}

pub(crate) fn open_child(parent: &File, name: &OsStr) -> io::Result<OpenedPath> {
    if name.is_empty()
        || name == OsStr::new(".")
        || name == OsStr::new("..")
        || name.as_bytes().contains(&b'/')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "source child name is invalid",
        ));
    }
    let name = CString::new(name.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "source path component contains a NUL byte",
        )
    })?;
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )
    };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    let file = unsafe { File::from_raw_fd(descriptor) };
    let metadata = file.metadata()?;
    if metadata.file_type().is_file() {
        Ok(OpenedPath::File(file))
    } else if metadata.file_type().is_dir() {
        Ok(OpenedPath::Directory(file))
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "source child has an unsupported filesystem type",
        ))
    }
}

fn open_component(parent: libc::c_int, name: &OsStr, expected: ExpectedType) -> io::Result<File> {
    let name = CString::new(name.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "source path component contains a NUL byte",
        )
    })?;
    let mut flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
    if expected == ExpectedType::Directory {
        flags |= libc::O_DIRECTORY;
    }
    let descriptor = unsafe { libc::openat(parent, name.as_ptr(), flags) };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    let file = unsafe { File::from_raw_fd(descriptor) };
    verify_type(&file.metadata()?, expected)?;
    Ok(file)
}

fn verify_type(metadata: &Metadata, expected: ExpectedType) -> io::Result<()> {
    let matches = match expected {
        ExpectedType::File => metadata.file_type().is_file(),
        ExpectedType::Directory => metadata.file_type().is_dir(),
    };
    if matches {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "source path has an unsupported filesystem type",
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Read;

    use super::{open_path, ExpectedType};

    #[test]
    fn descriptor_walk_reads_an_ordinary_file() {
        #[cfg(target_os = "macos")]
        for (input, expected) in [
            (
                "/var/folders/source.jsonl",
                "/private/var/folders/source.jsonl",
            ),
            ("/tmp/source.jsonl", "/private/tmp/source.jsonl"),
            ("/etc/source.jsonl", "/private/etc/source.jsonl"),
            ("/variable/source.jsonl", "/variable/source.jsonl"),
        ] {
            assert_eq!(
                super::super::normalize_lexical(std::path::Path::new(input)).as_deref(),
                Some(std::path::Path::new(expected))
            );
        }

        let temp = tempfile::tempdir().unwrap();
        let nested = temp.path().join("a/b");
        fs::create_dir_all(&nested).unwrap();
        let path = nested.join("source.jsonl");
        fs::write(&path, b"record\n").unwrap();

        let mut file = open_path(&path, ExpectedType::File).unwrap();
        let mut value = String::new();
        file.read_to_string(&mut value).unwrap();
        assert_eq!(value, "record\n");
    }

    #[test]
    fn source_root_safety_descriptor_walk_rejects_leaf_and_ancestor_symlinks() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        fs::create_dir(&real).unwrap();
        let source = real.join("source.jsonl");
        fs::write(&source, b"record\n").unwrap();

        let leaf = real.join("leaf-link");
        symlink(&source, &leaf).unwrap();
        assert!(open_path(&leaf, ExpectedType::File).is_err());

        let ancestor = temp.path().join("ancestor-link");
        symlink(&real, &ancestor).unwrap();
        #[cfg(target_os = "macos")]
        assert!(
            super::super::normalize_lexical(&ancestor)
                .is_some_and(|path| path.ends_with("ancestor-link")),
            "fixed-root normalization must not resolve arbitrary symlinks"
        );
        assert!(open_path(&ancestor.join("source.jsonl"), ExpectedType::File).is_err());
    }

    #[test]
    fn descriptor_walk_rejects_parent_traversal() {
        assert!(open_path(
            std::path::Path::new("safe/../source.jsonl"),
            ExpectedType::File
        )
        .is_err());
    }
}
