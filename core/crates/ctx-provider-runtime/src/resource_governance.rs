#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ResourceGovernanceMode {
    Auto,
    Custom,
}

#[derive(Debug, Clone)]
pub struct SystemSnapshot {
    pub memory_total_bytes: u64,
    pub memory_used_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct ProviderMemorySample {
    pub provider_id: String,
    pub label: String,
    pub pid: u32,
    pub memory_bytes: u64,
    pub tool_memory_bytes: u64,
}
