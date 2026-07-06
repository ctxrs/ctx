#[allow(unused_imports)]
use super::*;

text_enum! {
    pub enum Fidelity {
        Full => "full",
        Partial => "partial",
        Imported => "imported",
        Inferred => "inferred",
        SummaryOnly => "summary_only",
    }
    default Partial
}
