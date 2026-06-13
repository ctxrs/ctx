# ctx-settings-model

Owns the runtime settings schema, defaults, normalization, public redaction projection, and update merge logic.

This crate must stay free of daemon IO, Axum/API response types, stores, and daemon state. Route handlers and services may consume these types, but settings model code should not call back into `ctx-http`.
