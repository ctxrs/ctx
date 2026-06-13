pub fn clear_failpoints() {
    ctx_daemon::fault_injection::clear_failpoints();
}

pub fn set_failpoint(point: &'static str, times: u32) {
    ctx_daemon::fault_injection::set_failpoint(point, times);
}

pub fn maybe_fail(point: &'static str) -> anyhow::Result<()> {
    ctx_daemon::fault_injection::maybe_fail(point)
}
