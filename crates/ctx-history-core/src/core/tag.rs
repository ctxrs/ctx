#[allow(unused_imports)]
use super::*;

text_enum! {
    pub enum TagKind {
        User => "user",
        System => "system",
        Inferred => "inferred",
    }
    default User
}
