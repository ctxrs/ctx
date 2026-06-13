use ctx_core::models::SessionEventType;

#[derive(Debug, Clone)]
pub struct NormalizedEvent {
    pub event_type: SessionEventType,
    pub payload_json: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct ProviderUnknownEventObservation {
    pub protocol: &'static str,
    pub event_type: String,
    pub parse_error: String,
    pub raw: serde_json::Value,
    pub raw_truncated: bool,
    pub crp_channel: Option<String>,
    pub crp_seq: u64,
    pub timeline_notice_emitted: bool,
}
