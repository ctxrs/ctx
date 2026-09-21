use ctx_attribution_model::{BlameAttribution, BlameCoverage, BlameCoverageUnit};

pub(crate) const fn outcome_heading(attribution: BlameAttribution) -> &'static str {
    match attribution {
        BlameAttribution::Proven => "Producer proven",
        BlameAttribution::Possible => "Possible producer found",
        BlameAttribution::Conflicting => "Producer evidence conflicts",
        BlameAttribution::None => "No producer proven",
    }
}

pub(crate) fn coverage_text(coverage: &BlameCoverage, evaluated_suffix: &str) -> String {
    format!(
        "{} {} evaluated{evaluated_suffix} · {} proven · {} possible · {} conflicting · {} none",
        coverage.evaluated,
        coverage_units(coverage.unit, coverage.evaluated),
        coverage.proven,
        coverage.possible,
        coverage.conflicting,
        coverage.none,
    )
}

const fn coverage_units(unit: BlameCoverageUnit, evaluated: u32) -> &'static str {
    let singular = evaluated == 1;
    match unit {
        BlameCoverageUnit::CommittedLine => {
            if singular {
                "committed line"
            } else {
                "committed lines"
            }
        }
        BlameCoverageUnit::CommitFact => {
            if singular {
                "commit fact"
            } else {
                "commit facts"
            }
        }
        BlameCoverageUnit::PullRequestRelationship => {
            if singular {
                "pull request relationship"
            } else {
                "pull request relationships"
            }
        }
    }
}
