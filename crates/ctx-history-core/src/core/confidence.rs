#[allow(unused_imports)]
use super::*;

text_enum! {
    pub enum Confidence {
        Explicit => "explicit",
        High => "high",
        Medium => "medium",
        Low => "low",
        Unknown => "unknown",
    }
    default Unknown
}
