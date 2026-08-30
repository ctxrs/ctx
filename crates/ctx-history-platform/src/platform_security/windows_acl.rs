//! Exact protected-DACL implementation for private ctx state.

#![allow(unsafe_code)]

use std::{
    ffi::c_void,
    fs::{self, File},
    io,
    mem::size_of,
    os::windows::{
        ffi::OsStrExt as _,
        fs::{MetadataExt as _, OpenOptionsExt as _},
        io::AsRawHandle as _,
    },
    path::Path,
    ptr::{addr_of, null_mut},
};

use windows_sys::Win32::{
    Foundation::{
        CloseHandle, GetLastError, LocalFree, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND,
        ERROR_INSUFFICIENT_BUFFER, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS, HANDLE,
    },
    Security::{
        AddAccessAllowedAceEx,
        Authorization::{GetSecurityInfo, SetSecurityInfo, SE_FILE_OBJECT},
        CreateWellKnownSid, EqualSid, GetAce, GetLengthSid, GetSecurityDescriptorControl,
        GetTokenInformation, InitializeAcl, InitializeSecurityDescriptor, IsValidSid,
        SetSecurityDescriptorControl, SetSecurityDescriptorDacl, TokenUser, WinLocalSystemSid,
        ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, DACL_SECURITY_INFORMATION,
        OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSID, SECURITY_ATTRIBUTES,
        SECURITY_DESCRIPTOR, SE_DACL_PROTECTED, TOKEN_QUERY, TOKEN_USER,
    },
    Storage::FileSystem::{
        CreateDirectoryW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_GENERIC_READ, FILE_SHARE_READ, FILE_SHARE_WRITE, READ_CONTROL, WRITE_DAC,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const SECURITY_MAX_SID_SIZE: usize = 68;
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const PRIVATE_ACCESS_MASK: u32 = 0x001f_01ff;
const PRIVATE_DIRECTORY_INHERITANCE: u8 = 0x03;
const SECURITY_DESCRIPTOR_REVISION: u32 = 1;

pub(super) fn create_private_directory_all(path: &Path) -> io::Result<()> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "private directory path must be non-empty and traversal-free",
        ));
    }
    let absolute = std::path::absolute(path)?;
    let mut component_paths: Vec<_> = absolute.ancestors().collect();
    component_paths.reverse();
    component_paths.retain(|candidate| !candidate.as_os_str().is_empty());

    let identities = PrivateIdentities::current()?;
    let mut acl = private_acl(&identities, ObjectKind::Directory)?;
    let mut descriptor = SECURITY_DESCRIPTOR::default();
    // SAFETY: descriptor is live and writable for this initialization.
    if unsafe {
        InitializeSecurityDescriptor((&raw mut descriptor).cast(), SECURITY_DESCRIPTOR_REVISION)
    } == 0
    {
        return Err(last_error());
    }
    // SAFETY: descriptor and ACL remain live for every synchronous create.
    if unsafe {
        SetSecurityDescriptorDacl((&raw mut descriptor).cast(), 1, acl.as_mut_ptr().cast(), 0)
    } == 0
    {
        return Err(last_error());
    }
    // Prevent CreateDirectoryW from merging inherited permissive entries.
    // SAFETY: descriptor is initialized and remains live.
    if unsafe {
        SetSecurityDescriptorControl(
            (&raw mut descriptor).cast(),
            SE_DACL_PROTECTED,
            SE_DACL_PROTECTED,
        )
    } == 0
    {
        return Err(last_error());
    }
    let attributes = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).map_err(|_| invalid_acl())?,
        lpSecurityDescriptor: (&raw mut descriptor).cast(),
        bInheritHandle: 0,
    };

    // Each open omits delete and write sharing. Retaining every ancestor handle
    // prevents a checked component from being replaced or converted to a
    // reparse point while descendant creation is in progress.
    let mut held = Vec::with_capacity(component_paths.len());
    let mut created_private_ancestor = false;
    for candidate in component_paths {
        let is_final = candidate == absolute;
        let mut raced_existing = false;
        let access = if is_final || created_private_ancestor {
            READ_CONTROL
        } else {
            0
        };
        let handle = match open_handle(candidate, ObjectKind::Directory, access) {
            Ok(handle) => handle,
            Err(error) if is_not_found(&error) => {
                let wide = wide_path(candidate)?;
                // The protected owner/SYSTEM DACL is installed by the create
                // itself; no inherited-permissive interval exists.
                if unsafe { CreateDirectoryW(wide.as_ptr(), &raw const attributes) } == 0 {
                    let code = last_error_code();
                    if code != ERROR_ALREADY_EXISTS {
                        return Err(win32_error(code));
                    }
                    raced_existing = true;
                } else {
                    created_private_ancestor = true;
                }
                open_handle(candidate, ObjectKind::Directory, READ_CONTROL)?
            }
            Err(error) => return Err(error),
        };
        validate_handle_type(&handle, ObjectKind::Directory).map_err(|error| {
            if error.kind() == io::ErrorKind::InvalidInput && !is_final {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "private state path traverses a reparse point",
                )
            } else {
                error
            }
        })?;
        if is_final || created_private_ancestor || raced_existing {
            verify_handle_with_identities(&handle, ObjectKind::Directory, &identities)?;
        }
        created_private_ancestor |= raced_existing;
        held.push(handle);
    }
    Ok(())
}

pub(super) fn restrict_private_directory(path: &Path) -> io::Result<()> {
    restrict(path, ObjectKind::Directory)
}

pub(super) fn restrict_private_file(path: &Path) -> io::Result<()> {
    restrict(path, ObjectKind::File)
}

pub(super) fn restrict_private_file_handle(handle: &File) -> io::Result<()> {
    restrict_handle(handle, ObjectKind::File)
}

pub(super) fn ensure_private_file(path: &Path) -> io::Result<()> {
    let object = OpenedPrivateObject::open(path, ObjectKind::File, true)?;
    ensure_private_file_handle(object.file())
}

pub(super) fn ensure_private_file_handle(handle: &File) -> io::Result<()> {
    let identities = PrivateIdentities::current()?;
    validate_handle_type(handle, ObjectKind::File)?;
    verify_handle_owner(handle, identities.user_sid())?;
    match verify_handle_with_identities(handle, ObjectKind::File, &identities) {
        Ok(()) => Ok(()),
        Err(_) => restrict_handle_with_identities(handle, ObjectKind::File, &identities),
    }
}

pub(super) fn verify_private_directory(path: &Path) -> io::Result<()> {
    verify(path, ObjectKind::Directory)
}

pub(super) fn verify_private_file(path: &Path) -> io::Result<()> {
    verify(path, ObjectKind::File)
}

pub(super) fn verify_private_directory_handle(handle: &File) -> io::Result<()> {
    verify_handle(handle, ObjectKind::Directory)
}

pub(super) fn verify_private_file_handle(handle: &File) -> io::Result<()> {
    verify_handle(handle, ObjectKind::File)
}

pub(super) fn open_verified_private_file(path: &Path) -> io::Result<File> {
    let object =
        OpenedPrivateObject::open_with_access(path, ObjectKind::File, false, FILE_GENERIC_READ)?;
    verify_handle(object.file(), ObjectKind::File)?;
    Ok(object.into_file())
}

#[derive(Clone, Copy)]
enum ObjectKind {
    Directory,
    File,
}

impl ObjectKind {
    const fn inheritance_flags(self) -> u8 {
        match self {
            Self::Directory => PRIVATE_DIRECTORY_INHERITANCE,
            Self::File => 0,
        }
    }
}

fn restrict(path: &Path, kind: ObjectKind) -> io::Result<()> {
    let object = OpenedPrivateObject::open(path, kind, true)?;
    restrict_handle(object.file(), kind)
}

fn restrict_handle(handle: &File, kind: ObjectKind) -> io::Result<()> {
    validate_handle_type(handle, kind)?;
    let identities = PrivateIdentities::current()?;
    restrict_handle_with_identities(handle, kind, &identities)
}

fn restrict_handle_with_identities(
    handle: &File,
    kind: ObjectKind,
    identities: &PrivateIdentities,
) -> io::Result<()> {
    let mut acl = private_acl(identities, kind)?;
    // SAFETY: the file owns a live handle with WRITE_DAC and the ACL remains
    // live for this synchronous call.
    let result = unsafe {
        SetSecurityInfo(
            handle.as_raw_handle().cast(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            acl.as_mut_ptr().cast(),
            null_mut(),
        )
    };
    if result != ERROR_SUCCESS {
        return Err(win32_error(result));
    }
    verify_handle_with_identities(handle, kind, identities)
}

fn verify_handle_owner(handle: &File, expected_owner: PSID) -> io::Result<()> {
    let mut owner = null_mut();
    let mut descriptor = null_mut();
    // SAFETY: all out pointers are valid and the returned descriptor is guarded.
    let result = unsafe {
        GetSecurityInfo(
            handle.as_raw_handle().cast(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &raw mut owner,
            null_mut(),
            null_mut(),
            null_mut(),
            &raw mut descriptor,
        )
    };
    if result != ERROR_SUCCESS {
        return Err(win32_error(result));
    }
    let _descriptor = LocalAllocation(descriptor);
    if owner.is_null() || unsafe { EqualSid(owner, expected_owner) } == 0 {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private state path is not owned by the current user",
        ))
    } else {
        Ok(())
    }
}

fn verify(path: &Path, kind: ObjectKind) -> io::Result<()> {
    let object = OpenedPrivateObject::open(path, kind, false)?;
    verify_handle(object.file(), kind)
}

fn verify_handle(handle: &File, kind: ObjectKind) -> io::Result<()> {
    validate_handle_type(handle, kind)?;
    let identities = PrivateIdentities::current()?;
    verify_handle_owner(handle, identities.user_sid())?;
    verify_handle_with_identities(handle, kind, &identities)
}

fn validate_handle_type(handle: &File, kind: ObjectKind) -> io::Result<()> {
    let metadata = handle.metadata()?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "private state path is a reparse point",
        ));
    }
    let expected = match kind {
        ObjectKind::Directory => metadata.is_dir(),
        ObjectKind::File => metadata.is_file(),
    };
    if expected {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "private state path has an unsafe file type",
        ))
    }
}

fn verify_handle_with_identities(
    handle: &File,
    kind: ObjectKind,
    identities: &PrivateIdentities,
) -> io::Result<()> {
    let mut dacl: *mut ACL = null_mut();
    let mut descriptor = null_mut();
    // SAFETY: all out pointers are valid and the returned descriptor is guarded.
    let result = unsafe {
        GetSecurityInfo(
            handle.as_raw_handle().cast(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &raw mut dacl,
            null_mut(),
            &raw mut descriptor,
        )
    };
    if result != ERROR_SUCCESS {
        return Err(win32_error(result));
    }
    let _descriptor = LocalAllocation(descriptor);
    if dacl.is_null() {
        return Err(invalid_acl());
    }
    let mut control = 0;
    let mut revision = 0;
    // SAFETY: the descriptor and scalar out pointers remain valid.
    if unsafe { GetSecurityDescriptorControl(descriptor, &raw mut control, &raw mut revision) } == 0
        || control & SE_DACL_PROTECTED == 0
    {
        return Err(invalid_acl());
    }

    let expected_count = if identities.user_is_system { 1 } else { 2 };
    // SAFETY: dacl points into the live descriptor.
    if usize::from(unsafe { (*dacl).AceCount }) != expected_count {
        return Err(invalid_acl());
    }
    let mut saw_user = false;
    let mut saw_system = false;
    for index in 0..expected_count {
        let mut raw_ace: *mut c_void = null_mut();
        let ace_index = u32::try_from(index).map_err(|_| invalid_acl())?;
        // SAFETY: index is bounded by the reported ACE count.
        if unsafe { GetAce(dacl, ace_index, &raw mut raw_ace) } == 0 || raw_ace.is_null() {
            return Err(last_error());
        }
        // SAFETY: GetAce returned an ACCESS_ALLOWED_ACE-sized pointer to inspect.
        let ace = unsafe { &*raw_ace.cast::<ACCESS_ALLOWED_ACE>() };
        if ace.Header.AceType != ACCESS_ALLOWED_ACE_TYPE
            || ace.Header.AceFlags != kind.inheritance_flags()
            || ace.Mask != PRIVATE_ACCESS_MASK
        {
            return Err(invalid_acl());
        }
        let sid = addr_of!(ace.SidStart).cast_mut().cast::<c_void>();
        // SAFETY: the ACE SID and identity SIDs remain live for this loop.
        if unsafe { EqualSid(sid, identities.user_sid()) } != 0 {
            saw_user = true;
        } else if unsafe { EqualSid(sid, identities.system_sid()) } != 0 {
            saw_system = true;
        } else {
            return Err(invalid_acl());
        }
    }
    if saw_user && (saw_system || identities.user_is_system) {
        Ok(())
    } else {
        Err(invalid_acl())
    }
}

/// Opens every pathname component without delete sharing, then validates and
/// operates on the final object through that same handle. This makes the
/// validation, DACL mutation, and verification one object-identity operation.
struct OpenedPrivateObject {
    _ancestors: Vec<File>,
    file: File,
}

impl OpenedPrivateObject {
    fn open(path: &Path, kind: ObjectKind, mutate: bool) -> io::Result<Self> {
        Self::open_with_access(path, kind, mutate, 0)
    }

    fn open_with_access(
        path: &Path,
        kind: ObjectKind,
        mutate: bool,
        additional_access: u32,
    ) -> io::Result<Self> {
        let absolute = std::path::absolute(path)?;
        let mut ancestor_paths: Vec<_> = absolute.ancestors().skip(1).collect();
        ancestor_paths.reverse();
        let mut ancestors = Vec::with_capacity(ancestor_paths.len());
        for ancestor in ancestor_paths {
            if ancestor.as_os_str().is_empty() {
                continue;
            }
            let handle = open_handle(ancestor, ObjectKind::Directory, 0)?;
            validate_handle_type(&handle, ObjectKind::Directory).map_err(|error| {
                if error.kind() == io::ErrorKind::InvalidInput {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "private state path traverses a reparse point",
                    )
                } else {
                    error
                }
            })?;
            ancestors.push(handle);
        }
        let access = READ_CONTROL | additional_access | if mutate { WRITE_DAC } else { 0 };
        let file = open_handle(&absolute, kind, access)?;
        validate_handle_type(&file, kind)?;
        Ok(Self {
            _ancestors: ancestors,
            file,
        })
    }

    fn file(&self) -> &File {
        &self.file
    }

    fn into_file(self) -> File {
        self.file
    }
}

fn open_handle(path: &Path, kind: ObjectKind, access: u32) -> io::Result<File> {
    let flags = FILE_FLAG_OPEN_REPARSE_POINT
        | match kind {
            ObjectKind::Directory => FILE_FLAG_BACKUP_SEMANTICS,
            ObjectKind::File => 0,
        };
    let mut options = fs::OpenOptions::new();
    let share = match kind {
        ObjectKind::Directory => FILE_SHARE_READ,
        ObjectKind::File => FILE_SHARE_READ | FILE_SHARE_WRITE,
    };
    options
        .access_mode(access)
        .share_mode(share)
        .custom_flags(flags)
        .open(path)
}

fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
    let mut wide: Vec<_> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "private state path contains a NUL character",
        ));
    }
    wide.push(0);
    Ok(wide)
}

fn is_not_found(error: &io::Error) -> bool {
    error
        .raw_os_error()
        .and_then(|code| u32::try_from(code).ok())
        .is_some_and(|code| code == ERROR_FILE_NOT_FOUND || code == ERROR_PATH_NOT_FOUND)
}

fn private_acl(identities: &PrivateIdentities, kind: ObjectKind) -> io::Result<AlignedBuffer> {
    let user_sid_size = sid_size(identities.user_sid())?;
    let system_sid_size = sid_size(identities.system_sid())?;
    let ace_header = size_of::<ACCESS_ALLOWED_ACE>() - size_of::<u32>();
    let ace_count = if identities.user_is_system { 1 } else { 2 };
    let bytes = size_of::<ACL>()
        .checked_add(ace_header + user_sid_size)
        .and_then(|size| {
            if ace_count == 1 {
                Some(size)
            } else {
                size.checked_add(ace_header + system_sid_size)
            }
        })
        .ok_or_else(invalid_acl)?;
    let mut acl = AlignedBuffer::new(bytes)?;
    let acl_bytes = u32::try_from(acl.byte_len()).map_err(|_| invalid_acl())?;
    // SAFETY: acl is aligned and writable for acl_bytes.
    if unsafe { InitializeAcl(acl.as_mut_ptr().cast(), acl_bytes, ACL_REVISION) } == 0 {
        return Err(last_error());
    }
    add_allowed_ace(&mut acl, kind, identities.user_sid())?;
    if !identities.user_is_system {
        add_allowed_ace(&mut acl, kind, identities.system_sid())?;
    }
    Ok(acl)
}

fn add_allowed_ace(acl: &mut AlignedBuffer, kind: ObjectKind, sid: PSID) -> io::Result<()> {
    // SAFETY: the ACL has sufficient capacity and sid is valid and live.
    if unsafe {
        AddAccessAllowedAceEx(
            acl.as_mut_ptr().cast(),
            ACL_REVISION,
            kind.inheritance_flags().into(),
            PRIVATE_ACCESS_MASK,
            sid,
        )
    } == 0
    {
        Err(last_error())
    } else {
        Ok(())
    }
}

fn sid_size(sid: PSID) -> io::Result<usize> {
    // SAFETY: callers supply SIDs backed by live buffers.
    if sid.is_null() || unsafe { IsValidSid(sid) } == 0 {
        return Err(invalid_acl());
    }
    // SAFETY: sid was validated immediately above.
    usize::try_from(unsafe { GetLengthSid(sid) }).map_err(|_| invalid_acl())
}

struct PrivateIdentities {
    _token: Handle,
    token_user: AlignedBuffer,
    system: AlignedBuffer,
    user_is_system: bool,
}

impl PrivateIdentities {
    fn current() -> io::Result<Self> {
        let mut token = null_mut();
        // SAFETY: token is a valid out pointer for the current process.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
            return Err(last_error());
        }
        let token = Handle(token);
        let mut required = 0;
        // SAFETY: a null first buffer requests the token-user size.
        let first =
            unsafe { GetTokenInformation(token.0, TokenUser, null_mut(), 0, &raw mut required) };
        if first != 0 || last_error_code() != ERROR_INSUFFICIENT_BUFFER || required == 0 {
            return Err(last_error());
        }
        let mut token_user =
            AlignedBuffer::new(usize::try_from(required).map_err(|_| invalid_acl())?)?;
        // SAFETY: the output buffer has the requested capacity.
        if unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                token_user.as_mut_ptr().cast(),
                required,
                &raw mut required,
            )
        } == 0
        {
            return Err(last_error());
        }
        let mut system = AlignedBuffer::new(SECURITY_MAX_SID_SIZE)?;
        let mut system_size = u32::try_from(system.byte_len()).map_err(|_| invalid_acl())?;
        // SAFETY: system is aligned and has SECURITY_MAX_SID_SIZE capacity.
        if unsafe {
            CreateWellKnownSid(
                WinLocalSystemSid,
                null_mut(),
                system.as_mut_ptr().cast(),
                &raw mut system_size,
            )
        } == 0
        {
            return Err(last_error());
        }
        let mut identities = Self {
            _token: token,
            token_user,
            system,
            user_is_system: false,
        };
        let _ = sid_size(identities.user_sid())?;
        let _ = sid_size(identities.system_sid())?;
        // SAFETY: both SIDs are valid and backed by identities.
        identities.user_is_system =
            unsafe { EqualSid(identities.user_sid(), identities.system_sid()) != 0 };
        Ok(identities)
    }

    fn user_sid(&self) -> PSID {
        // SAFETY: token_user contains a successful TOKEN_USER response.
        unsafe { (*self.token_user.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    }

    fn system_sid(&self) -> PSID {
        self.system.as_ptr().cast_mut().cast()
    }
}

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: this guard owns one live token handle.
        unsafe { CloseHandle(self.0) };
    }
}

struct LocalAllocation(*mut c_void);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: allocated by GetNamedSecurityInfoW and freed exactly once.
            unsafe { LocalFree(self.0) };
        }
    }
}

struct AlignedBuffer {
    words: Vec<usize>,
    byte_len: usize,
}

impl AlignedBuffer {
    fn new(byte_len: usize) -> io::Result<Self> {
        let word = size_of::<usize>();
        let words = byte_len.checked_add(word - 1).ok_or_else(invalid_acl)? / word;
        Ok(Self {
            words: vec![0; words],
            byte_len,
        })
    }

    const fn byte_len(&self) -> usize {
        self.byte_len
    }

    fn as_ptr(&self) -> *const usize {
        self.words.as_ptr()
    }

    fn as_mut_ptr(&mut self) -> *mut usize {
        self.words.as_mut_ptr()
    }
}

fn invalid_acl() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "private state ACL must be protected and allow only the current user and SYSTEM",
    )
}

fn last_error_code() -> u32 {
    // SAFETY: GetLastError has no preconditions.
    unsafe { GetLastError() }
}

fn last_error() -> io::Error {
    win32_error(last_error_code())
}

fn win32_error(code: u32) -> io::Error {
    io::Error::from_raw_os_error(i32::try_from(code).unwrap_or(i32::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_acl_diagnostic_names_the_current_user_and_system_policy() {
        assert_eq!(
            invalid_acl().to_string(),
            "private state ACL must be protected and allow only the current user and SYSTEM"
        );
    }

    fn set_permissive_null_dacl(handle: &File) -> io::Result<()> {
        // SAFETY: the file owns a live handle with WRITE_DAC. A null DACL is an
        // intentionally permissive fixture that ensure_private_file_handle
        // must replace before the file is used as private state.
        let result = unsafe {
            SetSecurityInfo(
                handle.as_raw_handle().cast(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
            )
        };
        if result == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(win32_error(result))
        }
    }

    fn world_sid() -> io::Result<AlignedBuffer> {
        use windows_sys::Win32::Security::WinWorldSid;

        let mut sid = AlignedBuffer::new(SECURITY_MAX_SID_SIZE)?;
        let mut size = u32::try_from(sid.byte_len()).map_err(|_| invalid_acl())?;
        // SAFETY: sid is aligned and has SECURITY_MAX_SID_SIZE capacity.
        if unsafe {
            CreateWellKnownSid(
                WinWorldSid,
                null_mut(),
                sid.as_mut_ptr().cast(),
                &raw mut size,
            )
        } == 0
        {
            Err(last_error())
        } else {
            Ok(sid)
        }
    }

    #[test]
    fn permissive_file_dacl_is_repaired_on_the_open_handle(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let parent = tempfile::tempdir()?;
        let path = parent.path().join("legacy-config.toml");
        fs::write(&path, b"legacy")?;
        let object = OpenedPrivateObject::open(&path, ObjectKind::File, true)?;
        set_permissive_null_dacl(object.file())?;
        assert!(verify_handle(object.file(), ObjectKind::File).is_err());

        ensure_private_file_handle(object.file())?;

        verify_handle(object.file(), ObjectKind::File)?;
        Ok(())
    }

    #[test]
    fn wrong_owner_is_rejected_before_acl_repair() -> Result<(), Box<dyn std::error::Error>> {
        let parent = tempfile::tempdir()?;
        let path = parent.path().join("wrong-owner.toml");
        fs::write(&path, b"legacy")?;
        let object = OpenedPrivateObject::open(&path, ObjectKind::File, true)?;
        set_permissive_null_dacl(object.file())?;
        let world = world_sid()?;

        let error =
            verify_handle_owner(object.file(), world.as_ptr().cast_mut().cast()).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("not owned by the current user"));
        assert!(verify_handle(object.file(), ObjectKind::File).is_err());
        Ok(())
    }

    #[test]
    fn reparse_handle_is_rejected_before_acl_repair() -> Result<(), Box<dyn std::error::Error>> {
        let parent = tempfile::tempdir()?;
        let target = parent.path().join("target");
        let junction = parent.path().join("junction");
        fs::create_dir(&target)?;
        let status = std::process::Command::new("cmd.exe")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&target)
            .status()?;
        if !status.success() {
            return Err("failed to create junction fixture".into());
        }
        let handle = open_handle(&junction, ObjectKind::Directory, READ_CONTROL | WRITE_DAC)?;

        let error = ensure_private_file_handle(&handle).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("reparse point"));
        Ok(())
    }

    #[test]
    fn verified_file_open_rejects_an_intermediate_junction(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let parent = tempfile::tempdir()?;
        let target = parent.path().join("target");
        let junction = parent.path().join("junction");
        fs::create_dir(&target)?;
        fs::write(target.join("secret.json"), b"secret")?;
        let status = std::process::Command::new("cmd.exe")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&target)
            .status()?;
        if !status.success() {
            return Err("failed to create junction fixture".into());
        }

        let error = open_verified_private_file(&junction.join("secret.json")).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("traverses a reparse point"));
        Ok(())
    }

    #[test]
    fn pathname_swap_cannot_redirect_handle_bound_acl_steps(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let parent = tempfile::tempdir()?;
        let path = parent.path().join("private.db");
        let replacement = parent.path().join("replacement.db");
        fs::write(&path, b"original")?;

        let object = OpenedPrivateObject::open(&path, ObjectKind::File, true)?;
        fs::rename(&path, &replacement)?;
        fs::write(&path, b"attacker replacement")?;
        restrict_handle(object.file(), ObjectKind::File)?;
        verify_private_file(&replacement)?;
        assert!(verify_private_file(&path).is_err());

        drop(object);
        Ok(())
    }

    #[test]
    fn directory_pathname_swap_cannot_redirect_handle_bound_acl_steps(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let parent = tempfile::tempdir()?;
        let path = parent.path().join("private");
        let replacement = parent.path().join("replacement");
        fs::create_dir(&path)?;

        let object = OpenedPrivateObject::open(&path, ObjectKind::Directory, true)?;
        fs::rename(&path, &replacement)?;
        fs::create_dir(&path)?;
        restrict_handle(object.file(), ObjectKind::Directory)?;
        verify_handle(object.file(), ObjectKind::Directory)?;
        verify_private_directory(&replacement)?;
        assert!(verify_private_directory(&path).is_err());

        drop(object);
        Ok(())
    }
}
