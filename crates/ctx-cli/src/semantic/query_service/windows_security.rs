pub(super) const WINDOWS_DAEMON_QUERY_PIPE_ACCESS_MASK: u32 =
    windows_sys::Win32::Storage::FileSystem::FILE_GENERIC_READ
        | windows_sys::Win32::Storage::FileSystem::FILE_GENERIC_WRITE;

pub(super) struct WindowsDaemonQueryPipeSecurity {
    identities: WindowsDaemonQueryPipeIdentities,
    _acl: AlignedBuffer,
    descriptor: Box<windows_sys::Win32::Security::SECURITY_DESCRIPTOR>,
}

impl WindowsDaemonQueryPipeSecurity {
    pub(super) fn for_current_user_and_system() -> std::io::Result<Self> {
        use windows_sys::Win32::Security::{
            InitializeSecurityDescriptor, IsValidSecurityDescriptor, SetSecurityDescriptorControl,
            SetSecurityDescriptorDacl, SetSecurityDescriptorOwner, SECURITY_DESCRIPTOR,
            SE_DACL_PROTECTED,
        };

        let identities = WindowsDaemonQueryPipeIdentities::current()?;
        let mut acl = build_pipe_acl(&identities)?;
        let mut descriptor = Box::<SECURITY_DESCRIPTOR>::default();
        if unsafe { InitializeSecurityDescriptor((&raw mut *descriptor).cast(), 1) } == 0 {
            return Err(last_error());
        }
        if unsafe {
            SetSecurityDescriptorOwner((&raw mut *descriptor).cast(), identities.user_sid(), 0)
        } == 0
        {
            return Err(last_error());
        }
        if unsafe {
            SetSecurityDescriptorDacl((&raw mut *descriptor).cast(), 1, acl.as_mut_ptr().cast(), 0)
        } == 0
        {
            return Err(last_error());
        }
        if unsafe {
            SetSecurityDescriptorControl(
                (&raw mut *descriptor).cast(),
                SE_DACL_PROTECTED,
                SE_DACL_PROTECTED,
            )
        } == 0
        {
            return Err(last_error());
        }
        if unsafe { IsValidSecurityDescriptor((&raw mut *descriptor).cast()) } == 0 {
            return Err(invalid_acl());
        }
        Ok(Self {
            identities,
            _acl: acl,
            descriptor,
        })
    }

    pub(super) fn attributes(
        &mut self,
    ) -> std::io::Result<windows_sys::Win32::Security::SECURITY_ATTRIBUTES> {
        use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;

        Ok(SECURITY_ATTRIBUTES {
            nLength: u32::try_from(std::mem::size_of::<SECURITY_ATTRIBUTES>())
                .map_err(|_| invalid_acl())?,
            lpSecurityDescriptor: (&raw mut *self.descriptor).cast(),
            bInheritHandle: 0,
        })
    }

    pub(super) fn verify_handle(
        &self,
        handle: windows_sys::Win32::Foundation::HANDLE,
    ) -> std::io::Result<()> {
        use std::{ffi::c_void, ptr::addr_of};
        use windows_sys::Win32::{
            Foundation::{LocalFree, ERROR_SUCCESS},
            Security::{
                Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT},
                EqualSid, GetAce, GetSecurityDescriptorControl, IsValidAcl, ACCESS_ALLOWED_ACE,
                ACL, DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, SE_DACL_PROTECTED,
            },
        };

        struct Descriptor(*mut c_void);
        impl Drop for Descriptor {
            fn drop(&mut self) {
                if !self.0.is_null() {
                    unsafe {
                        let _ = LocalFree(self.0);
                    }
                }
            }
        }

        let mut owner = std::ptr::null_mut();
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut descriptor = std::ptr::null_mut();
        let result = unsafe {
            GetSecurityInfo(
                handle,
                SE_KERNEL_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &raw mut owner,
                std::ptr::null_mut(),
                &raw mut dacl,
                std::ptr::null_mut(),
                &raw mut descriptor,
            )
        };
        if result != ERROR_SUCCESS {
            return Err(std::io::Error::from_raw_os_error(
                i32::try_from(result).unwrap_or(i32::MAX),
            ));
        }
        let _descriptor = Descriptor(descriptor);
        if owner.is_null()
            || dacl.is_null()
            || unsafe { EqualSid(owner, self.identities.user_sid()) } == 0
            || unsafe { IsValidAcl(dacl) } == 0
        {
            return Err(invalid_acl());
        }
        let mut control = 0;
        let mut revision = 0;
        if unsafe { GetSecurityDescriptorControl(descriptor, &raw mut control, &raw mut revision) }
            == 0
            || control & SE_DACL_PROTECTED == 0
        {
            return Err(invalid_acl());
        }

        let expected_count = if self.identities.user_is_system { 1 } else { 2 };
        if usize::from(unsafe { (*dacl).AceCount }) != expected_count {
            return Err(invalid_acl());
        }
        let mut saw_user = false;
        let mut saw_system = false;
        for index in 0..expected_count {
            let mut raw_ace = std::ptr::null_mut();
            if unsafe { GetAce(dacl, index as u32, &raw mut raw_ace) } == 0 || raw_ace.is_null() {
                return Err(invalid_acl());
            }
            let ace = unsafe { &*raw_ace.cast::<ACCESS_ALLOWED_ACE>() };
            let sid = addr_of!(ace.SidStart).cast_mut().cast::<c_void>();
            let sid_size = sid_size(sid)?;
            let expected_ace_size =
                std::mem::size_of::<ACCESS_ALLOWED_ACE>() - std::mem::size_of::<u32>() + sid_size;
            if ace.Header.AceType != 0
                || ace.Header.AceFlags != 0
                || ace.Mask != WINDOWS_DAEMON_QUERY_PIPE_ACCESS_MASK
                || usize::from(ace.Header.AceSize) != expected_ace_size
            {
                return Err(invalid_acl());
            }
            if unsafe { EqualSid(sid, self.identities.user_sid()) } != 0 {
                if saw_user {
                    return Err(invalid_acl());
                }
                saw_user = true;
            } else if unsafe { EqualSid(sid, self.identities.system_sid()) } != 0 {
                if saw_system {
                    return Err(invalid_acl());
                }
                saw_system = true;
            } else {
                return Err(invalid_acl());
            }
        }
        if !saw_user || (!self.identities.user_is_system && !saw_system) {
            return Err(invalid_acl());
        }
        Ok(())
    }
}

struct WindowsDaemonQueryPipeIdentities {
    _token: TokenHandle,
    token_user: AlignedBuffer,
    system: AlignedBuffer,
    user_is_system: bool,
}

impl WindowsDaemonQueryPipeIdentities {
    fn current() -> std::io::Result<Self> {
        use windows_sys::Win32::{
            Foundation::ERROR_INSUFFICIENT_BUFFER,
            Security::{
                CreateWellKnownSid, EqualSid, GetTokenInformation, TokenUser, WinLocalSystemSid,
                TOKEN_QUERY,
            },
            System::Threading::{GetCurrentProcess, OpenProcessToken},
        };

        let mut token = std::ptr::null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
            return Err(last_error());
        }
        let token = TokenHandle(token);
        let mut required = 0;
        let first = unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                std::ptr::null_mut(),
                0,
                &raw mut required,
            )
        };
        if first != 0
            || unsafe { windows_sys::Win32::Foundation::GetLastError() }
                != ERROR_INSUFFICIENT_BUFFER
            || required == 0
        {
            return Err(last_error());
        }
        let mut token_user = AlignedBuffer::new(required as usize)?;
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
        let mut system = AlignedBuffer::new(68)?;
        let mut system_size = 68;
        if unsafe {
            CreateWellKnownSid(
                WinLocalSystemSid,
                std::ptr::null_mut(),
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
        identities.user_is_system =
            unsafe { EqualSid(identities.user_sid(), identities.system_sid()) } != 0;
        Ok(identities)
    }

    fn user_sid(&self) -> windows_sys::Win32::Security::PSID {
        use windows_sys::Win32::Security::TOKEN_USER;
        unsafe { (*self.token_user.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    }

    fn system_sid(&self) -> windows_sys::Win32::Security::PSID {
        self.system.as_ptr().cast_mut().cast()
    }
}

fn build_pipe_acl(identities: &WindowsDaemonQueryPipeIdentities) -> std::io::Result<AlignedBuffer> {
    use windows_sys::Win32::Security::{
        AddAccessAllowedAceEx, InitializeAcl, ACCESS_ALLOWED_ACE, ACL, ACL_REVISION,
    };

    let ace_header = std::mem::size_of::<ACCESS_ALLOWED_ACE>() - std::mem::size_of::<u32>();
    let user_size = sid_size(identities.user_sid())?;
    let system_size = sid_size(identities.system_sid())?;
    let bytes = std::mem::size_of::<ACL>()
        .checked_add(ace_header + user_size)
        .and_then(|size| {
            if identities.user_is_system {
                Some(size)
            } else {
                size.checked_add(ace_header + system_size)
            }
        })
        .ok_or_else(invalid_acl)?;
    let mut acl = AlignedBuffer::new(bytes)?;
    if unsafe { InitializeAcl(acl.as_mut_ptr().cast(), bytes as u32, ACL_REVISION) } == 0 {
        return Err(last_error());
    }
    for sid in [identities.user_sid(), identities.system_sid()]
        .into_iter()
        .take(if identities.user_is_system { 1 } else { 2 })
    {
        if unsafe {
            AddAccessAllowedAceEx(
                acl.as_mut_ptr().cast(),
                ACL_REVISION,
                0,
                WINDOWS_DAEMON_QUERY_PIPE_ACCESS_MASK,
                sid,
            )
        } == 0
        {
            return Err(last_error());
        }
    }
    Ok(acl)
}

fn sid_size(sid: windows_sys::Win32::Security::PSID) -> std::io::Result<usize> {
    use windows_sys::Win32::Security::{GetLengthSid, IsValidSid};
    if sid.is_null() || unsafe { IsValidSid(sid) } == 0 {
        return Err(invalid_acl());
    }
    Ok(unsafe { GetLengthSid(sid) } as usize)
}

struct TokenHandle(windows_sys::Win32::Foundation::HANDLE);

impl Drop for TokenHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

struct AlignedBuffer {
    words: Vec<usize>,
}

impl AlignedBuffer {
    fn new(byte_len: usize) -> std::io::Result<Self> {
        let word = std::mem::size_of::<usize>();
        let words = byte_len.checked_add(word - 1).ok_or_else(invalid_acl)? / word;
        Ok(Self {
            words: vec![0; words],
        })
    }

    fn as_ptr(&self) -> *const usize {
        self.words.as_ptr()
    }

    fn as_mut_ptr(&mut self) -> *mut usize {
        self.words.as_mut_ptr()
    }
}

fn invalid_acl() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "daemon query named pipe ACL is invalid",
    )
}

fn last_error() -> std::io::Error {
    std::io::Error::last_os_error()
}
