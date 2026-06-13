# ctx-resource-utilization

Owns process, memory, CPU, and workspace-disk sampling models for the daemon.

The sampler is intentionally independent of `ctx-http`: API handlers can expose snapshots, and
runtime/governance code can consume them, but collection is not an HTTP concern.
