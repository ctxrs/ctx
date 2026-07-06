#[allow(unused_imports)]
use super::*;

pub fn new_id() -> Uuid {
    Uuid::now_v7()
}
