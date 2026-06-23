# Hosted Sync Roadmap

Hosted sync is future product direction, not shipped scope for this public
ctx `0.1.0` candidate.

The public branch today is useful without any hosted account:

- records are created locally;
- evidence is stored locally;
- review output is generated locally;
- provider imports are explicit local commands;
- pull request publishing goes through the authenticated local `gh` CLI.

## What future hosted sync should preserve

The hosted roadmap should not undo the local-first posture:

- local recording must remain useful even when sync is disabled;
- redacted summaries should be the default sync shape;
- raw transcripts or raw evidence payloads should remain explicit opt-ins;
- sync should preserve stable record ids, provenance, and artifact references;
- users need clear device and retention controls before hosted sync is promoted
  as a default workflow.

## What is not claimed here

This preview does not claim:

- hosted accounts or team onboarding;
- hosted dashboards or organization analytics;
- centralized policy enforcement;
- hosted PR publishing or comment fan-out;
- production retention controls or legal hold;
- a public hosted work records API contract.

Those items need their own implementation proof, threat model, and release
approval before they can move from roadmap language into public product claims.

## Public wording guidance

When referencing future hosted direction in docs or previews:

- call it a roadmap, not a shipped feature;
- keep local recording as the primary path;
- say raw transcript sync is opt-in, not default;
- avoid suggesting that the current branch requires a hosted account;
- do not blur the boundary between this ctx preview and the separate
  ctx ADE or hosted team programs.
