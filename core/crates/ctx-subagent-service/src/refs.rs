use base64::Engine;
use ctx_core::ids::{RunId, SessionId};

pub fn encode_agent_ref(session_id: SessionId) -> String {
    format!(
        "agent_{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(session_id.0.as_bytes())
    )
}

pub fn decode_agent_ref(raw: &str) -> Result<SessionId, String> {
    let encoded = raw
        .trim()
        .strip_prefix("agent_")
        .ok_or_else(|| "invalid agent_id".to_string())?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| "invalid agent_id".to_string())?;
    let uuid = uuid::Uuid::from_slice(&bytes).map_err(|_| "invalid agent_id".to_string())?;
    Ok(SessionId(uuid))
}

pub fn encode_run_ref(run_id: RunId) -> String {
    format!(
        "run_{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(run_id.0.as_bytes())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_refs_round_trip_and_reject_invalid_prefix() {
        let session_id = SessionId::new();
        assert_eq!(
            decode_agent_ref(&encode_agent_ref(session_id)),
            Ok(session_id)
        );
        assert_eq!(
            decode_agent_ref("session_notvalid")
                .as_ref()
                .map_err(String::as_str),
            Err("invalid agent_id")
        );
    }

    #[test]
    fn run_refs_use_run_prefix() {
        assert!(encode_run_ref(RunId::new()).starts_with("run_"));
    }
}
