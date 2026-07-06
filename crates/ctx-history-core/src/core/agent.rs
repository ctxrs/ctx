#[allow(unused_imports)]
use super::*;

text_enum! {
    pub enum AgentType {
        Primary => "primary",
        Subagent => "subagent",
        AgentTeamMember => "agent_team_member",
        Reviewer => "reviewer",
        Implementer => "implementer",
        Unknown => "unknown",
    }
    default Unknown
}
