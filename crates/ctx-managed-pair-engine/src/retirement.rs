use std::path::Path;

use anyhow::Result;

use crate::filesystem::{self, Layout, Slot};

/// Removes only the three retired installation slots, never user data.
///
/// The caller must hold the installation lock and validate the hosted marker,
/// unified executable and terminal scheduler before calling. A later daemon
/// restart does not consume these slots or acquire installation ownership.
/// Pair publication still owns its files until its pending record is gone.
pub fn retire_managed_pair_files_under_installation_lock(root: &Path) -> Result<()> {
    retire_with_fault(root, &mut |_| Ok(()))
}

pub(crate) fn retire_with_fault(
    root: &Path,
    fault: &mut dyn FnMut(&'static str) -> Result<()>,
) -> Result<()> {
    let layout = Layout::open(root, true)?;
    filesystem::require_absent(
        &layout.active_transaction(),
        "pending installation transaction",
    )?;
    crate::cleanup_orphaned_managed_pair_candidate_under_installation_lock(root)?;
    // Inspect every slot before deleting any. Missing files are an interrupted
    // cleanup, while links, substituted directories and unsafe files are errors.
    let slots = [Slot::Companion, Slot::State, Slot::Envelope];
    let observed = slots
        .into_iter()
        .map(|slot| {
            filesystem::stamp_optional(&layout.target(slot), crate::max_bytes(slot), slot.label())
                .map(|stamp| (slot, stamp))
        })
        .collect::<Result<Vec<_>>>()?;
    for (slot, stamp) in observed {
        if let Some(stamp) = stamp {
            filesystem::remove_if_exact(
                &layout.target(slot),
                &stamp,
                crate::max_bytes(slot),
                slot.label(),
            )?;
            fault(slot.label())?;
        }
    }
    layout.revalidate()
}
