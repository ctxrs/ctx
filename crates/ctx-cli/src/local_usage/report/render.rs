use serde_json::{json, Value};

use crate::pro::PRO_MONTHLY_PRICE_DISPLAY;

pub(crate) fn pro_conversion_action(access_state: Option<&str>) -> Option<Value> {
    match access_state {
        Some("trial") => Some(json!({
            "kind": "pro_monthly_conversion",
            "price": PRO_MONTHLY_PRICE_DISPLAY,
            "command": "ctx pro manage",
            "reason": "trial_active",
        })),
        Some("locked") => Some(json!({
            "kind": "pro_restore_access",
            "command": "ctx pro manage",
            "reason": "access_locked",
            "graph_preserved": true,
        })),
        Some("active" | "canceling_paid" | "offline_grace") | None | Some(_) => None,
    }
}
