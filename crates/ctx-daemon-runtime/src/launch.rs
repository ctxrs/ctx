use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
};

#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
#[cfg(windows)]
use std::os::windows::process::CommandExt as _;

#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
#[cfg(windows)]
const DETACHED_PROCESS: u32 = 0x0000_0008;

/// A complete, immutable process launch assembled by product composition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedLaunch {
    program: PathBuf,
    args: Vec<OsString>,
    environment: BTreeMap<OsString, OsString>,
}

impl NormalizedLaunch {
    pub fn new(
        program: PathBuf,
        args: Vec<OsString>,
        environment: BTreeMap<OsString, OsString>,
    ) -> Self {
        Self {
            program,
            args,
            environment,
        }
    }

    pub fn program(&self) -> &Path {
        &self.program
    }

    pub fn args(&self) -> impl Iterator<Item = &OsStr> {
        self.args.iter().map(OsString::as_os_str)
    }

    pub fn get_args(&self) -> impl Iterator<Item = &OsStr> {
        self.args()
    }

    pub fn environment(&self) -> impl Iterator<Item = (&OsStr, &OsStr)> {
        self.environment
            .iter()
            .map(|(key, value)| (key.as_os_str(), value.as_os_str()))
    }

    pub fn get_envs(&self) -> impl Iterator<Item = (&OsStr, Option<&OsStr>)> {
        self.environment().map(|(name, value)| (name, Some(value)))
    }
}

/// Spawns an already-normalized launch without inheriting stdio.
///
/// This is deliberately the common part of both daemon launch policies.  In
/// particular, a finite worker still receives the sanitized environment and
/// null stdio, but remains in its caller's terminal/console group.
fn spawn_with(launch: NormalizedLaunch, configure: impl FnOnce(&mut Command)) -> io::Result<Child> {
    let mut command = Command::new(launch.program);
    command
        .args(launch.args)
        .env_clear()
        .envs(launch.environment)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure(&mut command);
    command.spawn()
}

/// Spawns a persistent daemon detached from the invoking terminal session.
pub fn spawn_detached(launch: NormalizedLaunch) -> io::Result<Child> {
    spawn_with(launch, |command| {
        #[cfg(unix)]
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        #[cfg(windows)]
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    })
}

/// Spawns a finite foreground worker in the caller's terminal/console group.
///
/// The worker deliberately stays in the invoking terminal/console, but has a
/// private process group.  That gives the foreground client a precise graceful
/// interrupt target without granting it authority over a pre-existing daemon.
pub fn spawn_attached(launch: NormalizedLaunch) -> io::Result<Child> {
    spawn_with(launch, |command| {
        #[cfg(unix)]
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        #[cfg(windows)]
        command.creation_flags(CREATE_NEW_PROCESS_GROUP);
    })
}

/// Delivers the finite worker's graceful interrupt to the private group made
/// by [`spawn_attached`]. Callers must first prove that `child` is their exact
/// direct child and reap it through every completion race.
pub fn interrupt_attached_child_group(child: &Child) -> io::Result<()> {
    #[cfg(unix)]
    {
        let pid = i32::try_from(child.id())
            .map_err(|_| io::Error::other("finite worker pid is out of range"))?;
        // A negative pid names exactly the process group whose leader is the
        // direct child. The worker was placed in that group before exec.
        if unsafe { libc::kill(-pid, libc::SIGINT) } == -1 {
            return Err(io::Error::last_os_error());
        }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{GenerateConsoleCtrlEvent, CTRL_BREAK_EVENT};

        // CREATE_NEW_PROCESS_GROUP makes the child pid its console process
        // group id. CTRL_BREAK_EVENT is the targeted console analogue of the
        // Unix group SIGINT and does not require detaching the child.
        if unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, child.id()) } == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_launch_is_an_immutable_snapshot() {
        let mut environment = BTreeMap::from([(OsString::from("A"), OsString::from("before"))]);
        let launch = NormalizedLaunch::new(
            PathBuf::from("daemon"),
            vec![OsString::from("run")],
            environment.clone(),
        );
        environment.insert(OsString::from("A"), OsString::from("after"));

        assert_eq!(launch.program(), Path::new("daemon"));
        assert_eq!(launch.args().collect::<Vec<_>>(), [OsStr::new("run")]);
        assert_eq!(
            launch.environment().collect::<Vec<_>>(),
            [(OsStr::new("A"), OsStr::new("before"))]
        );
    }
}
