use std::collections::HashMap;
use std::time::{Duration, Instant};

use ctx_core::ids::{SessionId, WorkspaceId, WorktreeId};
use sha2::Digest;
use tokio::sync::Mutex;

const PROVIDER_SESSION_MCP_AUTH_TTL: Duration = Duration::from_secs(12 * 60 * 60);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct McpAuthCapabilities {
    pub subagents: bool,
    pub artifacts: bool,
    pub merge_queue_submit: bool,
}

impl McpAuthCapabilities {
    pub fn provider_session() -> Self {
        Self {
            subagents: true,
            artifacts: true,
            merge_queue_submit: false,
        }
    }

    pub fn provider_turn_default() -> Self {
        Self {
            subagents: true,
            artifacts: true,
            merge_queue_submit: true,
        }
    }

    pub fn names(self) -> Vec<&'static str> {
        let mut values = Vec::new();
        if self.subagents {
            values.push("subagents");
        }
        if self.artifacts {
            values.push("artifacts");
        }
        if self.merge_queue_submit {
            values.push("merge_queue_submit");
        }
        values
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct McpAuthContext {
    pub session_id: SessionId,
    pub workspace_id: WorkspaceId,
    pub worktree_id: WorktreeId,
    pub capabilities: McpAuthCapabilities,
}

impl McpAuthContext {
    pub fn provider_session(
        session_id: SessionId,
        workspace_id: WorkspaceId,
        worktree_id: WorktreeId,
        capabilities: McpAuthCapabilities,
    ) -> Self {
        Self {
            session_id,
            workspace_id,
            worktree_id,
            capabilities,
        }
    }

    pub fn allows_subagents(self, session_id: SessionId) -> bool {
        self.capabilities.subagents && self.session_id == session_id
    }

    pub fn allows_artifacts(self, session_id: SessionId) -> bool {
        self.capabilities.artifacts && self.session_id == session_id
    }

    pub fn allows_merge_queue_submit(self, session_id: SessionId, worktree_id: WorktreeId) -> bool {
        self.capabilities.merge_queue_submit
            && self.session_id == session_id
            && self.worktree_id == worktree_id
    }
}

#[derive(Debug)]
pub struct IssuedMcpAuthToken {
    pub token: String,
    pub context: McpAuthContext,
    pub replaced_count: usize,
}

#[derive(Debug)]
pub struct McpAuthRegistry {
    ttl: Duration,
    entries: Mutex<HashMap<String, TimedEntry<McpAuthContext>>>,
}

impl Default for McpAuthRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl McpAuthRegistry {
    pub fn new() -> Self {
        Self {
            ttl: PROVIDER_SESSION_MCP_AUTH_TTL,
            entries: Mutex::new(HashMap::new()),
        }
    }

    pub async fn issue_provider_session_token(
        &self,
        session_id: SessionId,
        workspace_id: WorkspaceId,
        worktree_id: WorktreeId,
    ) -> IssuedMcpAuthToken {
        self.issue_provider_session_token_with_capabilities(
            session_id,
            workspace_id,
            worktree_id,
            McpAuthCapabilities::provider_session(),
        )
        .await
    }

    pub async fn issue_provider_session_token_with_capabilities(
        &self,
        session_id: SessionId,
        workspace_id: WorkspaceId,
        worktree_id: WorktreeId,
        capabilities: McpAuthCapabilities,
    ) -> IssuedMcpAuthToken {
        let token = format!("ctxmcp_{}", uuid::Uuid::new_v4().simple());
        let token_hash = mcp_token_hash(&token);
        let context =
            McpAuthContext::provider_session(session_id, workspace_id, worktree_id, capabilities);

        let mut entries = self.entries.lock().await;
        self.prune_expired_entries(&mut entries);
        let replaced_count = revoke_matching_provider_session_tokens(&mut entries, context);
        entries.insert(token_hash, TimedEntry::new(context));

        IssuedMcpAuthToken {
            token,
            context,
            replaced_count,
        }
    }

    pub async fn revoke_provider_session_token(&self, token: &str) -> Option<McpAuthContext> {
        let token = token.trim();
        if token.is_empty() {
            return None;
        }
        let token_hash = mcp_token_hash(token);
        let mut entries = self.entries.lock().await;
        self.prune_expired_entries(&mut entries);
        entries.remove(&token_hash).map(|entry| entry.value)
    }

    pub async fn verify_token(&self, token: &str) -> Option<McpAuthContext> {
        let token_hash = mcp_token_hash(token);
        let mut entries = self.entries.lock().await;
        self.prune_expired_entries(&mut entries);
        let entry = entries.get_mut(&token_hash)?;
        entry.touch();
        Some(entry.value)
    }

    fn prune_expired_entries(&self, entries: &mut HashMap<String, TimedEntry<McpAuthContext>>) {
        entries.retain(|_, entry| entry.last_access.elapsed() <= self.ttl);
    }

    #[cfg(test)]
    fn new_with_ttl(ttl: Duration) -> Self {
        Self {
            ttl,
            entries: Mutex::new(HashMap::new()),
        }
    }

    #[cfg(test)]
    async fn set_token_age(&self, token: &str, age: Duration) {
        let token_hash = mcp_token_hash(token);
        let mut entries = self.entries.lock().await;
        let entry = entries
            .get_mut(&token_hash)
            .expect("test token should exist");
        entry.last_access = Instant::now() - age;
    }

    #[cfg(test)]
    async fn token_age(&self, token: &str) -> Duration {
        let token_hash = mcp_token_hash(token);
        let entries = self.entries.lock().await;
        entries
            .get(&token_hash)
            .expect("test token should exist")
            .last_access
            .elapsed()
    }
}

#[derive(Debug)]
struct TimedEntry<T> {
    value: T,
    last_access: Instant,
}

impl<T> TimedEntry<T> {
    fn new(value: T) -> Self {
        Self {
            value,
            last_access: Instant::now(),
        }
    }

    fn touch(&mut self) {
        self.last_access = Instant::now();
    }
}

fn mcp_token_hash(token: &str) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(b"ctx-mcp-auth|");
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

fn revoke_matching_provider_session_tokens(
    entries: &mut HashMap<String, TimedEntry<McpAuthContext>>,
    ctx: McpAuthContext,
) -> usize {
    let before = entries.len();
    entries.retain(|_, entry| {
        entry.value.session_id != ctx.session_id
            || entry.value.workspace_id != ctx.workspace_id
            || entry.value.worktree_id != ctx.worktree_id
    });
    before.saturating_sub(entries.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> (SessionId, WorkspaceId, WorktreeId) {
        (SessionId::new(), WorkspaceId::new(), WorktreeId::new())
    }

    #[test]
    fn token_hash_is_deterministic_and_domain_separated() {
        let token = "ctxmcp_test-token";
        let hash = mcp_token_hash(token);

        assert_eq!(hash, mcp_token_hash(token));
        assert_ne!(hash, token);
        assert_ne!(hash, hex::encode(sha2::Sha256::digest(token.as_bytes())));
        assert_ne!(hash, mcp_token_hash("ctxmcp_other-token"));
    }

    #[tokio::test]
    async fn issued_provider_session_token_verifies_with_context() {
        let registry = McpAuthRegistry::new();
        let (session_id, workspace_id, worktree_id) = ids();

        let issued = registry
            .issue_provider_session_token(session_id, workspace_id, worktree_id)
            .await;

        assert!(issued.token.starts_with("ctxmcp_"));
        assert_eq!(issued.replaced_count, 0);
        assert_eq!(
            registry.verify_token(&issued.token).await,
            Some(McpAuthContext::provider_session(
                session_id,
                workspace_id,
                worktree_id,
                McpAuthCapabilities::provider_session(),
            )),
        );
    }

    #[tokio::test]
    async fn unknown_token_does_not_verify() {
        let registry = McpAuthRegistry::new();

        assert_eq!(registry.verify_token("ctxmcp_missing").await, None);
    }

    #[tokio::test]
    async fn issuing_replacement_revokes_same_session_workspace_worktree_token() {
        let registry = McpAuthRegistry::new();
        let (session_id, workspace_id, worktree_id) = ids();

        let first = registry
            .issue_provider_session_token(session_id, workspace_id, worktree_id)
            .await;
        let second = registry
            .issue_provider_session_token(session_id, workspace_id, worktree_id)
            .await;

        assert_eq!(second.replaced_count, 1);
        assert_eq!(registry.verify_token(&first.token).await, None);
        assert_eq!(
            registry.verify_token(&second.token).await,
            Some(second.context),
        );
    }

    #[tokio::test]
    async fn issuing_replacement_does_not_revoke_different_scope() {
        let registry = McpAuthRegistry::new();
        let (session_id, workspace_id, worktree_id) = ids();
        let other_worktree_id = WorktreeId::new();

        let first = registry
            .issue_provider_session_token(session_id, workspace_id, worktree_id)
            .await;
        let second = registry
            .issue_provider_session_token(session_id, workspace_id, other_worktree_id)
            .await;

        assert_eq!(second.replaced_count, 0);
        assert_eq!(
            registry.verify_token(&first.token).await,
            Some(first.context)
        );
        assert_eq!(
            registry.verify_token(&second.token).await,
            Some(second.context),
        );
    }

    #[tokio::test]
    async fn explicit_revocation_removes_token() {
        let registry = McpAuthRegistry::new();
        let (session_id, workspace_id, worktree_id) = ids();
        let issued = registry
            .issue_provider_session_token(session_id, workspace_id, worktree_id)
            .await;

        assert_eq!(
            registry.revoke_provider_session_token(&issued.token).await,
            Some(issued.context),
        );
        assert_eq!(registry.verify_token(&issued.token).await, None);
        assert_eq!(registry.revoke_provider_session_token("").await, None);
    }

    #[tokio::test]
    async fn expired_tokens_are_pruned_before_verify() {
        let registry = McpAuthRegistry::new_with_ttl(Duration::from_secs(10));
        let (session_id, workspace_id, worktree_id) = ids();
        let issued = registry
            .issue_provider_session_token(session_id, workspace_id, worktree_id)
            .await;

        registry
            .set_token_age(&issued.token, Duration::from_secs(11))
            .await;

        assert_eq!(registry.verify_token(&issued.token).await, None);
    }

    #[tokio::test]
    async fn verify_touches_token_last_access() {
        let registry = McpAuthRegistry::new_with_ttl(Duration::from_secs(60));
        let (session_id, workspace_id, worktree_id) = ids();
        let issued = registry
            .issue_provider_session_token(session_id, workspace_id, worktree_id)
            .await;
        registry
            .set_token_age(&issued.token, Duration::from_secs(30))
            .await;

        assert_eq!(
            registry.verify_token(&issued.token).await,
            Some(issued.context)
        );
        assert!(registry.token_age(&issued.token).await < Duration::from_secs(1));
    }

    #[test]
    fn capability_names_are_stable() {
        assert_eq!(
            McpAuthCapabilities::provider_turn_default().names(),
            vec!["subagents", "artifacts", "merge_queue_submit"],
        );
    }

    #[test]
    fn capability_policy_is_scoped_to_expected_session_and_worktree() {
        let (session_id, _workspace_id, worktree_id) = ids();
        let context = McpAuthContext::provider_session(
            session_id,
            WorkspaceId::new(),
            worktree_id,
            McpAuthCapabilities::provider_turn_default(),
        );

        assert!(context.allows_subagents(session_id));
        assert!(context.allows_artifacts(session_id));
        assert!(context.allows_merge_queue_submit(session_id, worktree_id));
        assert!(!context.allows_subagents(SessionId::new()));
        assert!(!context.allows_artifacts(SessionId::new()));
        assert!(!context.allows_merge_queue_submit(session_id, WorktreeId::new()));
    }
}
