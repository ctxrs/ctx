#[path = "build/assembly.rs"]
mod assembly;
#[path = "build/encoding.rs"]
mod encoding;
#[path = "build/indexing.rs"]
mod indexing;
#[path = "build/planning.rs"]
mod planning;

pub(crate) use assembly::DirectCandidate;
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use planning::{PublicationTestHookGuard, PublicationTestWorkerReceipt};
#[cfg(test)]
pub(crate) use planning::{
    PublicationTransactionTestFault, install_publication_test_hook,
    install_publication_transaction_failure_test_hook,
};

#[cfg(test)]
#[path = "build/tests.rs"]
mod tests;
