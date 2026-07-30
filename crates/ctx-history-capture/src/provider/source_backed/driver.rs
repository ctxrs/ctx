mod execution;
mod lifecycle;
mod observation;
mod receipts;

pub(crate) use execution::captured_route_driver;
// Temporary compatibility for the landed OpenCode-family conversion. Remove
// this alias when that caller imports `certify_source_inventory` directly.
pub(crate) use super::inventory::certify_source_inventory as certify_captured_route_inventory;
pub(crate) use observation::ProviderCaptureSink;
pub use receipts::*;
