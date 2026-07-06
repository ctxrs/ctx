#[allow(unused_imports)]
use super::*;

pub(crate) static ENV_LOCK: Mutex<()> = Mutex::new(());

pub(crate) struct EnvGuard {
    pub(crate) name: &'static str,
    pub(crate) original: Option<std::ffi::OsString>,
}

impl EnvGuard {
    pub(crate) fn set(name: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let original = env::var_os(name);
        env::set_var(name, value);
        Self { name, original }
    }

    pub(crate) fn remove(name: &'static str) -> Self {
        let original = env::var_os(name);
        env::remove_var(name);
        Self { name, original }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(value) = &self.original {
            env::set_var(self.name, value);
        } else {
            env::remove_var(self.name);
        }
    }
}
