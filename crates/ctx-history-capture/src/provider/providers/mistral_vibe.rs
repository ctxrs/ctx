pub(crate) mod native_path;
mod schema;
mod source;

pub(super) const MISTRAL_VIBE_CAPTURE_REVISION: u32 = 4;
pub(super) const MISTRAL_VIBE_POLICY_REVISION: u32 = 8;
const MISTRAL_VIBE_MAX_ID_BYTES: usize = 4 * 1024;
