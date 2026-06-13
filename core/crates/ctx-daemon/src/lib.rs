pub mod daemon;

#[cfg(feature = "fault_injection")]
pub mod fault_injection;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

#[cfg(not(feature = "fault_injection"))]
pub mod fault_injection {
    pub fn clear_failpoints() {}
    pub fn set_failpoint(_point: &'static str, _times: u32) {}
    pub fn maybe_fail(_point: &'static str) -> anyhow::Result<()> {
        Ok(())
    }
}
