# Web E2E Suites

The public ADE export keeps browser e2e tests focused on local workbench
behavior. Tests should run against a local daemon or mocked browser fixtures and
must not require private CI, hosted secrets, or organization-only infrastructure.

Run local browser contracts from `core/`:

```bash
pnpm -C apps/web exec playwright test -c playwright.premerge.config.ts
```

Visual tests may write local screenshots under the test output directory. Do
not configure this public copy to upload screenshots or diagnostics to hosted
services by default.

Provider tests that require real third-party credentials are opt-in. Keep those
credentials in your local environment and avoid committing provider tokens,
service-account JSON, browser profiles, or generated diagnostic bundles.
