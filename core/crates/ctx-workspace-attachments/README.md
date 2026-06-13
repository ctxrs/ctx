# ctx-workspace-attachments

Owns workspace attachment policy and mount behavior: attachment config
normalization, materialization, sync planning, host worktree mounts, native
container imports, AVF guest copies, mount cleanup, and mount-safety validation.

Daemon adapters supply workspace store access, worktree lookup, data-plane
resolution, container readiness, materialization task lifecycle, and route error
mapping. `ctx-http` remains transport-only and does not own attachment behavior.
