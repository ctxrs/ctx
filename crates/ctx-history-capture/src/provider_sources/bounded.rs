#[allow(unused_imports)]
use super::*;

impl BoundedProbe {
    pub(crate) fn from_bool(value: bool) -> Self {
        if value {
            Self::Found
        } else {
            Self::NotFound
        }
    }
}
