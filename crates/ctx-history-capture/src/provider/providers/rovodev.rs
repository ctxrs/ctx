mod event;
mod native_path;
mod source;

pub(crate) use event::rovodev_result_content;
pub(crate) use native_path::import_rovodev_native_path;

pub(crate) const ROVODEV_RESULT_CONTENT_PROFILE: &str = "rovodev.result-body.v1";

#[cfg(test)]
mod tests;
