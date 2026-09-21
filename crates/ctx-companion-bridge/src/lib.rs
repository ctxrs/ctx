//! Detached release verification retained for incoming legacy installation transactions.
//! No companion executable is discovered or launched by this crate.

mod error;
mod identity;
mod verifier;

pub use error::BridgeError;
pub use identity::Sha256Digest;
pub use verifier::{
    verify_signed_managed_pair_envelope, ManagedPairExpectations, ReleaseChannel,
    SignedManagedPairComponentIdentity, SignedManagedPairIdentity, SignedManagedPairTarget,
    MANAGED_PAIR_ENVELOPE_FILENAME, MANAGED_PAIR_STATE_FILENAME,
};

#[cfg(test)]
mod tests;
