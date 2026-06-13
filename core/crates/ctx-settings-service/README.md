# ctx-settings-service

Owns persisted runtime settings load/save, runtime secret sidecar handling, environment overrides, and host-execution policy enforcement.

This crate depends on `ctx-settings-model` and `ctx-store`. It should not know about Axum routes, API DTOs, daemon `AppState`, or HTTP status mapping. Callers should map service errors at their boundary.
