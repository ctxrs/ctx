use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ctx_desktop_ipc::DesktopDeepLinkToken;

#[derive(Default)]
pub(crate) struct DeepLinkTokenStore {
    tokens: std::sync::Mutex<HashMap<String, Instant>>,
}

const DEEP_LINK_TOKEN_TTL: Duration = Duration::from_secs(600);

impl DeepLinkTokenStore {
    fn mint(&self) -> DesktopDeepLinkToken {
        let token = uuid::Uuid::new_v4().to_string();
        let expires_at_ms = SystemTime::now()
            .checked_add(DEEP_LINK_TOKEN_TTL)
            .unwrap_or_else(SystemTime::now)
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        if let Ok(mut tokens) = self.tokens.lock() {
            tokens.insert(token.clone(), Instant::now() + DEEP_LINK_TOKEN_TTL);
        } else {
            eprintln!("failed to acquire deep link token lock; token will not be persisted");
        }
        DesktopDeepLinkToken {
            token,
            expires_at_ms,
        }
    }

    pub(super) fn is_valid(&self, token: &str) -> bool {
        let mut tokens = match self.tokens.lock() {
            Ok(tokens) => tokens,
            Err(_) => return false,
        };
        let now = Instant::now();
        tokens.retain(|_, expiry| *expiry > now);
        tokens
            .get(token)
            .map(|expiry| *expiry > now)
            .unwrap_or(false)
    }
}

#[tauri::command]
pub(crate) fn desktop_get_deep_link_token(
    store: tauri::State<'_, DeepLinkTokenStore>,
) -> DesktopDeepLinkToken {
    store.mint()
}
