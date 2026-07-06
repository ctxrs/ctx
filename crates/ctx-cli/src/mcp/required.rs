#[allow(unused_imports)]
use super::*;

pub(crate) fn required_uuid(arguments: &Value, key: &str) -> Result<Uuid> {
    optional_uuid(arguments, key)?.ok_or_else(|| anyhow!("{key} is required"))
}
