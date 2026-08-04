use std::path::Path;

use ctx_history_core::RepositoryAbstentionReason;

use super::{
    analyze, bounded_outcome_operation, bounded_outcome_plan, BoundedCommitProducer,
    BoundedOutcomeOperation, BoundedOutcomePlanDisposition,
};

#[test]
fn outcome_recognition_is_bounded_and_alias_free() {
    assert_eq!(
        bounded_outcome_operation("git commit -m exact && git rev-parse --verify HEAD"),
        Some(BoundedOutcomeOperation::Commit {
            producer: BoundedCommitProducer::Commit,
            rewrites_history: false,
            exact_oid_output: true,
        })
    );
    assert!(bounded_outcome_operation("git ci -m alias").is_none());
    assert!(bounded_outcome_operation("git commit -m exact && echo $HEAD").is_none());
    assert!(bounded_outcome_operation("bash -lc 'git commit -m hidden'").is_none());
    assert_eq!(
        bounded_outcome_operation("git add file && git commit -m $'line one\\nline two'"),
        Some(BoundedOutcomeOperation::Commit {
            producer: BoundedCommitProducer::Commit,
            rewrites_history: false,
            exact_oid_output: false,
        })
    );
    assert_eq!(
        bounded_outcome_operation("git add file\ngit commit -m exact"),
        Some(BoundedOutcomeOperation::Commit {
            producer: BoundedCommitProducer::Commit,
            rewrites_history: false,
            exact_oid_output: false,
        })
    );
    assert!(bounded_outcome_operation("git commit -m exact\ngit rev-parse HEAD").is_none());
    assert!(bounded_outcome_operation("git add file; git commit -m exact").is_none());
    for command in [
        "gh pr create --help",
        "gh pr create -h",
        "gh pr create --web",
        "gh pr create -w",
        "gh pr create --dry-run",
    ] {
        assert!(bounded_outcome_operation(command).is_none(), "{command}");
    }
    assert_eq!(
        bounded_outcome_operation("git merge --no-ff feature"),
        Some(BoundedOutcomeOperation::Commit {
            producer: BoundedCommitProducer::Merge,
            rewrites_history: false,
            exact_oid_output: false,
        })
    );
    assert!(bounded_outcome_operation("git merge feature").is_none());
}

#[test]
fn cd_routing_ignores_ambient_cdpath_by_construction() {
    let base = Path::new("/workspace/control");
    for (command, expected) in [
        ("cd /repo && git status", "/repo"),
        ("cd ./repo && git status", "/workspace/control/repo"),
        ("cd ../repo && git status", "/workspace/repo"),
        ("cd -- ./repo && git status", "/workspace/control/repo"),
    ] {
        let analysis = analyze(Some(command), Some(base));
        assert_eq!(
            analysis.derived_effective_cwd.as_deref(),
            Some(Path::new(expected)),
            "{command}"
        );
        assert_eq!(analysis.repository_paths.len(), 1, "{command}");
        assert!(analysis.abstentions.is_empty(), "{command}");
    }

    for command in ["cd repo && git status", "cd -- repo && git status"] {
        let analysis = analyze(Some(command), Some(base));
        assert!(analysis.derived_effective_cwd.is_none(), "{command}");
        assert!(analysis.repository_paths.is_empty(), "{command}");
        assert!(analysis.abstentions.iter().any(|abstention| {
            abstention.reason == RepositoryAbstentionReason::DynamicPath
                && abstention.detail == "unsupported_or_dynamic_cd"
        }));
    }
}

#[test]
fn literal_git_c_and_wrappers_are_candidates_but_wrappers_are_not_authority() {
    let base = Path::new("/workspace/control");
    for command in [
        "git -C repo status",
        "env -- A=1 git -C ../repo status",
        "command -- git -C ../repo status",
        "time -p git -C ../repo status",
        "timeout 5s git -C ../repo status",
    ] {
        let analysis = analyze(Some(command), Some(base));
        assert_eq!(analysis.repository_paths.len(), 1, "{command}");
        assert!(analysis.abstentions.is_empty(), "{command}");
    }

    assert!(matches!(
        bounded_outcome_plan(
            "git -C ../repo commit -m exact && git -C ../repo rev-parse HEAD",
            base,
        ),
        BoundedOutcomePlanDisposition::Planned(_)
    ));
    assert!(matches!(
        bounded_outcome_plan(
            "env -- git -C ../repo commit -m exact && git -C ../repo rev-parse HEAD",
            base,
        ),
        BoundedOutcomePlanDisposition::Abstained {
            reason: RepositoryAbstentionReason::UnknownWrapper,
            ..
        }
    ));
}

#[test]
fn exact_oid_plan_rejects_dry_runs_and_unsafe_intervening_commands() {
    let base = Path::new("/repo");
    assert!(matches!(
        bounded_outcome_plan("git commit --dry-run && git rev-parse --verify HEAD", base,),
        BoundedOutcomePlanDisposition::Abstained {
            reason: RepositoryAbstentionReason::OutcomeResultInadmissible,
            ..
        }
    ));
    assert!(matches!(
        bounded_outcome_plan(
            "git commit --dry-run=true && git rev-parse --verify HEAD",
            base,
        ),
        BoundedOutcomePlanDisposition::Abstained {
            reason: RepositoryAbstentionReason::OutcomeResultInadmissible,
            ..
        }
    ));

    for command in [
        "git commit -m exact && git reset --hard HEAD^ && git rev-parse HEAD",
        "git commit -m exact && git checkout other && git rev-parse HEAD",
        "git commit -m exact && git switch other && git rev-parse HEAD",
        "git commit -m exact && git pull && git rev-parse HEAD",
        "git commit -m exact && git branch --show-current && git rev-parse HEAD",
        "git commit -m exact && git commit --allow-empty -m second && git rev-parse HEAD",
        "git commit -m exact && git rev-parse HEAD && git commit --allow-empty -m second",
        "git commit -m exact && custom-command && git rev-parse HEAD",
    ] {
        assert!(
            matches!(
                bounded_outcome_plan(command, base),
                BoundedOutcomePlanDisposition::Abstained {
                    reason: RepositoryAbstentionReason::Ambiguous,
                    ..
                }
            ),
            "{command}"
        );
    }

    assert_eq!(
        bounded_outcome_operation(
            "git commit -m exact && git status --short && git rev-parse --verify HEAD"
        ),
        Some(BoundedOutcomeOperation::Commit {
            producer: BoundedCommitProducer::Commit,
            rewrites_history: false,
            exact_oid_output: true,
        })
    );
    assert_eq!(
        bounded_outcome_operation(
            "git commit -m exact && git status --short && git rev-parse HEAD && sed -n '12,18p' src/lib.rs"
        ),
        Some(BoundedOutcomeOperation::Commit {
            producer: BoundedCommitProducer::Commit,
            rewrites_history: false,
            exact_oid_output: true,
        })
    );
    assert_eq!(
        bounded_outcome_operation(
            "git commit -m exact -- --dry-run && git rev-parse --verify HEAD"
        ),
        Some(BoundedOutcomeOperation::Commit {
            producer: BoundedCommitProducer::Commit,
            rewrites_history: false,
            exact_oid_output: true,
        })
    );
}

#[test]
fn exact_oid_inspection_commands_are_never_commit_producers() {
    let base = Path::new("/repo");
    let oid = "d50d84a3e609d1ed30a435adbf2c19db35448b52";

    for command in [
        format!("git show --no-patch --format=%H {oid}"),
        format!("git log -1 --format=%H {oid}"),
        format!("git rev-parse --verify {oid}^{{commit}}"),
        format!("git branch --contains {oid}"),
    ] {
        assert!(
            !matches!(
                bounded_outcome_plan(&command, base),
                BoundedOutcomePlanDisposition::Planned(_)
            ),
            "inspection command was recognized as a producer: {command}"
        );
    }
}
