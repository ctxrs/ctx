use std::{io, path::Path};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtomicPublicationStage {
    Preparation,
    Validation,
    Replacement,
    Synchronization,
}

impl From<crate::AtomicWriteStage> for AtomicPublicationStage {
    fn from(stage: crate::AtomicWriteStage) -> Self {
        match stage {
            crate::AtomicWriteStage::BeforeTemporaryWrite => Self::Preparation,
            crate::AtomicWriteStage::AfterTemporarySyncBeforeReplace => Self::Validation,
            crate::AtomicWriteStage::BeforeReplace => Self::Replacement,
            crate::AtomicWriteStage::AfterReplaceBeforeDirectorySync => Self::Synchronization,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicationIoProbe {
    CandidateMetadata(AtomicPublicationStage),
    CertificationSidecar(AtomicPublicationStage),
    ActivePointer(AtomicPublicationStage),
    OtherAtomicPublication(AtomicPublicationStage),
    CandidateGenerationSync,
    TerminalSealOpen,
}

impl PublicationIoProbe {
    pub(crate) fn atomic(path: &Path, stage: crate::AtomicWriteStage) -> Self {
        let stage = stage.into();
        match path.file_name().and_then(|name| name.to_str()) {
            Some("meta.json") => Self::CandidateMetadata(stage),
            Some("active-generation.json") => Self::ActivePointer(stage),
            Some(name) if name.ends_with(".physical-certification.json") => {
                Self::CertificationSidecar(stage)
            }
            _ => Self::OtherAtomicPublication(stage),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum PublicationIoEvent<'a> {
    Atomic(crate::AtomicWriteStage, &'a Path),
    CandidateGenerationSync,
    #[cfg(windows)]
    TerminalSealOpen,
}

impl PublicationIoEvent<'_> {
    fn redacted(self) -> PublicationIoProbe {
        match self {
            Self::Atomic(stage, path) => PublicationIoProbe::atomic(path, stage),
            Self::CandidateGenerationSync => PublicationIoProbe::CandidateGenerationSync,
            #[cfg(windows)]
            Self::TerminalSealOpen => PublicationIoProbe::TerminalSealOpen,
        }
    }
}

type PublicationIoHook = Box<dyn for<'a> FnMut(PublicationIoEvent<'a>) -> io::Result<()>>;

thread_local! {
    static PUBLICATION_IO_HOOK: std::cell::RefCell<Option<PublicationIoHook>> =
        const { std::cell::RefCell::new(None) };
}

pub struct PublicationIoProbeGuard(Option<PublicationIoHook>);

impl PublicationIoProbeGuard {
    pub fn set(mut hook: impl FnMut(PublicationIoProbe) -> io::Result<()> + 'static) -> Self {
        Self::set_raw(move |event| hook(event.redacted()))
    }

    pub(crate) fn set_raw(
        hook: impl for<'a> FnMut(PublicationIoEvent<'a>) -> io::Result<()> + 'static,
    ) -> Self {
        Self(PUBLICATION_IO_HOOK.with(|active| active.replace(Some(Box::new(hook)))))
    }
}

impl Drop for PublicationIoProbeGuard {
    fn drop(&mut self) {
        PUBLICATION_IO_HOOK.with(|active| active.replace(self.0.take()));
    }
}

pub(crate) fn publication_io_checkpoint(event: PublicationIoEvent<'_>) -> io::Result<()> {
    PUBLICATION_IO_HOOK.with(|active| match active.borrow_mut().as_mut() {
        Some(hook) => hook(event),
        None => Ok(()),
    })
}
