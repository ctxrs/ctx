#[derive(Clone, Copy, Debug)]
pub(super) enum SessionHeadKind {
    Active,
    Archived,
}

pub(super) fn disable_head_materialization_writes_for(kind: SessionHeadKind) -> bool {
    matches!(kind, SessionHeadKind::Active)
}
