use std::{path::Path, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManagedUnlinkStage {
    BeforeUnlink,
    AfterUnlink,
    ArtifactSnapshot,
}

pub(super) type Hook = Arc<dyn Fn(ManagedUnlinkStage, &Path) + Send + Sync>;

thread_local! {
    static HOOK: std::cell::RefCell<Option<Hook>> = const { std::cell::RefCell::new(None) };
}

/// Captured by the candidate directory and shared with its actual GC workers.
pub struct ManagedUnlinkTestGuard(Option<Hook>);

impl ManagedUnlinkTestGuard {
    pub fn set(hook: impl Fn(ManagedUnlinkStage, &Path) + Send + Sync + 'static) -> Self {
        Self(HOOK.with(|active| active.replace(Some(Arc::new(hook)))))
    }
}

impl Drop for ManagedUnlinkTestGuard {
    fn drop(&mut self) {
        HOOK.with(|active| active.replace(self.0.take()));
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn capture() -> Option<Hook> {
    HOOK.with(|active| active.borrow().clone())
}
