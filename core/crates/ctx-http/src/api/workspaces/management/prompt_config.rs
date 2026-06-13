#[path = "prompt_config/agent.rs"]
mod agent;
#[path = "prompt_config/subagent.rs"]
mod subagent;

pub(in crate::api) use agent::{get_agent_system_prompt, update_agent_system_prompt};
pub(in crate::api) use subagent::{get_subagent_system_prompt, update_subagent_system_prompt};
