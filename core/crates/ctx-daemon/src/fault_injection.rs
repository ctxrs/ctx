use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

static FAILPOINTS: OnceLock<Mutex<HashMap<&'static str, u32>>> = OnceLock::new();

fn failpoints() -> &'static Mutex<HashMap<&'static str, u32>> {
    FAILPOINTS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn clear_failpoints() {
    let mut guard = match failpoints().lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::warn!("fault injection mutex poisoned; recovering lock");
            poisoned.into_inner()
        }
    };
    guard.clear();
}

pub fn set_failpoint(point: &'static str, times: u32) {
    let mut guard = match failpoints().lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::warn!("fault injection mutex poisoned; recovering lock");
            poisoned.into_inner()
        }
    };
    if times == 0 {
        guard.remove(point);
    } else {
        guard.insert(point, times);
    }
}

pub fn maybe_fail(point: &'static str) -> anyhow::Result<()> {
    let mut guard = match failpoints().lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::warn!("fault injection mutex poisoned; recovering lock");
            poisoned.into_inner()
        }
    };
    let remaining = guard.get_mut(point);
    match remaining {
        Some(n) if *n > 0 => {
            *n -= 1;
            Err(anyhow::anyhow!("fault injection: {point}"))
        }
        _ => Ok(()),
    }
}
