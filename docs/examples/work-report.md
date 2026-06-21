# Example Work Report

This is a redacted example of `ctx work report <work-id> --markdown`.

```markdown
# Fix Linux sandbox launch failure

- Work: `wrk_01abcdef`
- Trust: `partial`
- Next: Add verified provenance, fingerprints, artifacts, or citations.

## Evidence
- `wevdc_01` `observed_pass` `fresh` Observed `cargo test -p ctx-http` exited 0
- `wevdc_02` `observed_pass` `stale` Observed `pnpm -C core/apps/web lint` exited 0
- No raw transcript payloads are included by default.
```

Evidence is an observed local record, not proof by itself. Fresh local evidence
can still be `partial` when provenance is not independently verified. Rerun stale
evidence before relying on it in review.
