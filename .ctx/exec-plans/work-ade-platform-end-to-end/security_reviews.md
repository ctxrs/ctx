# Security Reviews

Record plugin, import/export, path, redaction, and capability security reviews.

## Pending

- Initial plugin threat model review.
- Import/export/redaction review.
- Final security review before full local validation.

## Work CLI Review-Hardening Slice

- Finding: transcript-like event payloads could retain raw text in redaction
  previews when the record shape used event fields instead of message fields.
  Resolution: event-aware omission now treats transcript-like `event_type`
  values and nested payload keys such as `content`, `delta`, `message`, `text`,
  `thought`, and `transcript` as content-bearing fields to omit.
- Finding: plugin manifest validation accepted shallow manifests with unknown
  fields before the daemon/plugin runtime saw them. Resolution: the CLI now
  rejects unknown public v1 manifest fields and delegates structural validation
  to the Rust `PluginManifest` model.
- Finding: shifted-left CLI smoke coverage did not exercise `work-bundle`
  schema output or negative path traversal fixtures. Resolution: the Bazel bin
  smoke test now covers `work-bundle` and rejects `../` bundle object paths.
- Residual risk: local plugin manifests still represent trusted local code once
  installed. The final plugin threat model must explicitly review root escape,
  env leakage, command timeout/output caps, provider ID collisions, and
  diagnostics visibility.
