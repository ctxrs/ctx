use super::{SegmentMaterializerError, locking};
use std::path::Path;
pub fn prepare_root(root: &Path) -> Result<(), SegmentMaterializerError> {
    locking::prepare_private_root(root)
}
