use std::{cell::RefCell, rc::Rc};

use tempfile::tempdir;

use super::*;
use crate::{AtomicPublicationStage, PublicationIoProbe, PublicationIoProbeGuard};

#[test]
fn probe_classification_is_precise_and_redacted() {
    let temp = tempdir().unwrap();
    let directory = DurableMmapDirectory::open(temp.path()).unwrap();
    let observed = Rc::new(RefCell::new(Vec::new()));
    let hook_observed = Rc::clone(&observed);
    let hook = PublicationIoProbeGuard::set(move |probe| {
        hook_observed.borrow_mut().push(probe);
        Ok(())
    });

    directory
        .atomic_write(Path::new("meta.json"), b"candidate")
        .unwrap();
    drop(hook);
    assert_eq!(
        *observed.borrow(),
        [
            PublicationIoProbe::CandidateMetadata(AtomicPublicationStage::Preparation),
            PublicationIoProbe::CandidateMetadata(AtomicPublicationStage::Validation),
            PublicationIoProbe::CandidateMetadata(AtomicPublicationStage::Replacement),
            PublicationIoProbe::CandidateMetadata(AtomicPublicationStage::Synchronization),
        ]
    );
    assert!(!format!("{:?}", observed.borrow()).contains(&temp.path().display().to_string()));
}
