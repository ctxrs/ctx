use super::super::*;

pub(in crate::api::ws) struct SessionCursor {
    pub(in crate::api::ws) last_sent: SessionReplayCursor,
}
