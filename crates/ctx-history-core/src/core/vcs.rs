#[allow(unused_imports)]
use super::*;

text_enum! {
    pub enum VcsKind {
        Git => "git",
        Jj => "jj",
    }
    default Git
}

text_enum! {
    pub enum VcsHost {
        Github => "github",
        Gitlab => "gitlab",
        Bitbucket => "bitbucket",
        Local => "local",
        Unknown => "unknown",
    }
    default Unknown
}

text_enum! {
    pub enum VcsChangeKind {
        GitCommit => "git_commit",
        GitBranch => "git_branch",
        GitWorktree => "git_worktree",
        JjChange => "jj_change",
        JjBookmark => "jj_bookmark",
        Patch => "patch",
        WorkingCopy => "working_copy",
    }
    default WorkingCopy
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VcsChange {
    pub id: Uuid,
    pub vcs_workspace_id: Uuid,
    pub kind: VcsChangeKind,
    pub change_id: String,
    #[serde(default)]
    pub parent_change_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_or_bookmark: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_time: Option<DateTime<Utc>>,
    #[serde(default)]
    pub confidence: Confidence,
    #[serde(flatten)]
    pub timestamps: EntityTimestamps,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<Uuid>,
    #[serde(flatten)]
    pub sync: SyncMetadata,
}
