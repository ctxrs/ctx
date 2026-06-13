use std::collections::HashMap;
use std::sync::Arc;

use crate::daemon::DaemonState;

pub(super) async fn provider_session_counts(state: &Arc<DaemonState>) -> HashMap<String, u64> {
    let session_ids = state.sessions.list_running_sessions().await;
    let mut counts: HashMap<String, u64> = HashMap::new();
    for session_id in session_ids {
        let session = match state.store_for_session(session_id).await {
            Ok(store) => store.get_session(session_id).await.ok().flatten(),
            Err(_) => None,
        };
        let Some(session) = session else {
            continue;
        };
        *counts.entry(session.provider_id).or_insert(0) += 1;
    }
    counts
}
