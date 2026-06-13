use super::super::*;

#[cfg(target_os = "macos")]
pub(in super::super) struct SharedVmResourceState {
    pub(super) host_port: libc::mach_port_t,
    pub(super) data_disk: SharedVmDataDiskState,
    pub(super) memory: SharedVmMemoryState,
}

#[cfg(target_os = "macos")]
pub(super) struct SharedVmDataDiskState {
    pub(super) next_check_at: std::time::Instant,
    pub(super) last_growth_blocked: bool,
}

#[cfg(target_os = "macos")]
pub(super) struct SharedVmMemoryState {
    pub(super) next_check_at: std::time::Instant,
    pub(super) ceiling_bytes: u64,
    pub(super) floor_bytes: u64,
    pub(super) decision_trace: SharedVmMemoryControllerDecisionTrace,
}

#[cfg(target_os = "macos")]
impl SharedVmResourceState {
    pub(in super::super) fn new(memory_ceiling_bytes: u64, memory_floor_bytes: u64) -> Self {
        Self {
            host_port: unsafe { mach_host_self() },
            data_disk: SharedVmDataDiskState {
                next_check_at: std::time::Instant::now(),
                last_growth_blocked: false,
            },
            memory: SharedVmMemoryState {
                next_check_at: std::time::Instant::now(),
                ceiling_bytes: memory_ceiling_bytes,
                floor_bytes: memory_floor_bytes,
                decision_trace: SharedVmMemoryControllerDecisionTrace::new(),
            },
        }
    }
}

#[cfg(target_os = "macos")]
pub(super) struct SharedVmMemoryControllerDecisionTrace {
    pub(super) trace_id: String,
    pub(super) epoch_millis: u64,
    next_sequence: u64,
}

#[cfg(target_os = "macos")]
impl SharedVmMemoryControllerDecisionTrace {
    fn new() -> Self {
        let epoch_millis = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        {
            Ok(duration) => duration.as_millis().min(u128::from(u64::MAX)) as u64,
            Err(_) => 0,
        };
        Self {
            trace_id: format!(
                "shared-vm-memory-controller-{}-{}",
                std::process::id(),
                epoch_millis
            ),
            epoch_millis,
            next_sequence: 0,
        }
    }

    pub(super) fn next_sequence(&mut self) -> u64 {
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.next_sequence
    }
}
