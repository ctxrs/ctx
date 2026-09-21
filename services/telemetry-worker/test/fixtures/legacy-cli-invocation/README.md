# Released CLI telemetry corpus

The compatibility test generates and freezes the complete stable version range
from `0.1.0` through `0.25.0`, then replays representative payloads through both
historical endpoints. It does not read Git history or the sibling public
repository at test time.

The Worker keeps these releases as one frozen `cli_invocation@1` family with
four envelope generations:

- `0.1.0`-`0.13.0`: install identity only.
- `0.14.0`-`0.17.0`: install and device identities.
- `0.18.0`-`0.24.0`: optional hosted-installer correlation.
- `0.25.0`: optional execution-capability snapshot.

Historical payloads are accepted only at ingress and normalized to the current
Neon row model. Unknown properties, raw content-shaped fields, unreleased
versions, and cross-generation envelope fields remain rejected.
