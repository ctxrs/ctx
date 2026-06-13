use serde::Deserialize;

use super::common::TaskRouteError;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateTaskTitleRouteRequest {
    title: String,
}

impl UpdateTaskTitleRouteRequest {
    pub fn validated_title(self) -> Result<String, TaskRouteError> {
        let title = self.title.trim().to_string();
        if title.is_empty() {
            return Err(TaskRouteError::bad_request("title is required"));
        }
        if title.len() > 120 {
            return Err(TaskRouteError::bad_request("title is too long"));
        }
        Ok(title)
    }
}
