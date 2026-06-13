pub mod boolish;
pub mod env;
pub mod ids;
pub mod models;
pub mod provider_ids;
pub mod provider_policy;
pub mod redaction;
pub mod session_projection;

#[cfg(test)]
mod tests {
    use super::models::*;

    #[test]
    fn task_status_serializes_snake_case() {
        let v = serde_json::to_string(&TaskStatus::Pending).unwrap();
        assert_eq!(v, "\"pending\"");
    }
}
