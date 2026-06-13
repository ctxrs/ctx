mod artifact_access;
mod artifacts;
pub mod ask_user;
pub mod auth;
pub mod command_dispatch;
mod control_route;
mod demo_route;
mod demo_seed;
mod handle;
mod message_attachment_signatures;
mod message_commands;
#[cfg(test)]
mod message_commands_tests;
mod message_route;
pub mod model_catalog;
mod model_switch;
mod model_target_bridge;
mod read_models;
mod route_contract;
mod scheduler_host;
pub mod subagents;
mod subagents_route;
pub mod title_generation;
mod title_model_mode_route;
pub mod vcs;
mod vcs_route;

pub use artifact_access::SessionImageBlobStoreError;
pub use artifacts::SessionArtifactDownload;
pub use demo_seed::{
    DemoSeedTranscript, DemoSeedTranscriptError, DemoSeedTranscriptHandle, DemoSeedTranscriptTurn,
};
pub use handle::GenerateSessionTitleError;
pub use message_commands::{PostUserMessageError, PostUserMessageInput};
pub use model_switch::{SetSessionModelError, SetSessionModelErrorKind, SetSessionModelRequest};
pub use model_target_bridge::SetSessionModeError;
pub(in crate::daemon) use scheduler_host::SessionSchedulerWorkerHostFactory;
