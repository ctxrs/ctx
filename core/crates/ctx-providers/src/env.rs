use std::collections::HashMap;

pub fn data_root_for_host(env: &HashMap<String, String>) -> Option<String> {
    env.get("CTX_DATA_ROOT_HOST")
        .cloned()
        .or_else(|| env.get("CTX_DATA_ROOT").cloned())
}

pub fn data_root_for_child(env: &HashMap<String, String>) -> Option<String> {
    env.get("CTX_DATA_ROOT")
        .cloned()
        .or_else(|| data_root_for_host(env))
}
