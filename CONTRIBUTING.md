# Contributing

Thanks for improving ctx.

This staged export is intended to become the public source home for the ctx Agentic Development Environment (ADE). Until the release candidate and repository permissions are approved, do not treat this tree as publishable.

## What belongs here

- ADE daemon, desktop, and web workbench source
- Source-build documentation for local development
- Public docs and README media from `ctxrs/ctx`
- Issue templates and repository metadata

Hosted services and private operations belong outside this repo. Control-plane source should live separately at https://github.com/ctxrs/control-plane when that repository is available.

## Working style

- Prefer small, reviewable changes.
- Keep public build paths local-first and source-buildable.
- Do not add maintainer-only infrastructure, hosted operations, signing flows, credential tooling, remote-runner pools, or unpublished artifact systems as required public build steps.
- Do not include generated build outputs, local worktrees, personal paths, secrets, or private operational docs.
