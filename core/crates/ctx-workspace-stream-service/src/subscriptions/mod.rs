pub mod application;
pub mod planning;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceStreamSubscriptionResolutionError {
    Hydration,
    Resolution,
}
