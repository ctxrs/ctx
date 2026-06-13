#[path = "demo/dev_mode.rs"]
mod dev_mode;
#[path = "demo/seed_transcript.rs"]
mod seed_transcript;

pub(crate) use seed_transcript::dev_seed_session_transcript;
