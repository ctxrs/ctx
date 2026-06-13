use sysinfo::{Pid, Signal, System};

pub(super) const PROCESS_KILL_SIGNAL: Signal = Signal::Kill;

pub(super) fn signal_pids(pids: &[u32], signal: Signal) -> usize {
    let mut system = System::new();
    system.refresh_processes();
    let mut killed = 0usize;
    for pid in pids {
        if let Some(process) = system.process(Pid::from_u32(*pid)) {
            if process.kill_with(signal).unwrap_or(false) {
                killed += 1;
            }
        }
    }
    killed
}
