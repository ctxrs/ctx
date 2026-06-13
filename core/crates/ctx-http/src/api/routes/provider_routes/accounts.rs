use crate::api::router::RouteState;

mod amp;
mod claude;
mod codex;
mod copilot;
mod cursor;
mod gemini;
mod kimi;
mod mistral;
mod qwen;

use amp::amp_account_routes;
use claude::claude_account_routes;
use codex::codex_account_routes;
use copilot::copilot_account_routes;
use cursor::cursor_account_routes;
use gemini::gemini_account_routes;
use kimi::kimi_account_routes;
use mistral::mistral_account_routes;
use qwen::qwen_account_routes;

pub(super) fn provider_account_routes() -> axum::Router<RouteState> {
    axum::Router::new()
        .merge(codex_account_routes())
        .merge(claude_account_routes())
        .merge(gemini_account_routes())
        .merge(qwen_account_routes())
        .merge(kimi_account_routes())
        .merge(amp_account_routes())
        .merge(mistral_account_routes())
        .merge(copilot_account_routes())
        .merge(cursor_account_routes())
}
