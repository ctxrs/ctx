#[allow(unused_imports)]
use super::*;

text_enum! {
    pub enum SummaryKind {
        ImportedProviderSummary => "imported_provider_summary",
        CtxGenerated => "ctx_generated",
        AgentSupplied => "agent_supplied",
        HumanNote => "human_note",
    }
    default HumanNote
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureSourceDescriptor {
    pub kind: CaptureSourceKind,
    pub provider: CaptureProvider,
    pub machine_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_id: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_source_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_session_id: Option<String>,
}
