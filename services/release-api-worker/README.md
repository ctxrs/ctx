# Release API Worker

This Worker serves ctx and legacy ADE release metadata and artifact routes.
Its public source includes the route parser, storage adapter, pinned public
trust material, tests, dependency lock and Wrangler configuration. Deployment
credentials are injected separately.

```sh
pnpm --dir services/release-api-worker install --frozen-lockfile
pnpm --dir services/release-api-worker test
pnpm --dir services/release-api-worker typecheck
scripts/bazelw test //services/release-api-worker:release_api_worker_tests --config=ci
```

The stable `/functions/v2/releases/stable/ctx-release-metadata.env[.sig]`
route selects `releases/stable/current-v2.json`. Original v1 aliases select
`current.json`, frozen at 1.3.2. Both use the existing signed schema and immutable
versioned objects; v2 has no fallback. Staging and dogfood retain v1. ADE,
provider-matrix, download and storage compatibility routes remain available.

For ctx 1.5, ordinary metadata identifies one unified executable. Additional
signed pair fields are a projection for released older updaters, using the
same final platform-signed bytes under their required filenames. The Worker
does not depend on an account, entitlement or commercial service. Publish and
verify immutable artifacts, metadata and signatures before advancing v2; keep
frozen v1 and previous release objects intact.

Wrangler's `staging` environment binds only `ctx-releases-staging`. The checked-in
staging public key and fingerprint must match the operator's signing key.
Production deployment uses `pnpm --dir services/release-api-worker deploy:prod`.
Source changes and fixture tests are not evidence that a Worker or release has
been deployed.
