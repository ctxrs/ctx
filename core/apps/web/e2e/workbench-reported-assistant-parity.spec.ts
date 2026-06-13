import { expect, test } from "./fixtures";
import {
  measureAssistantParity,
  openWorkbenchShell,
} from "./utils/pretextParity";

const REPORTED_ASSISTANT_CANARY_MESSAGE = `Yes. That is the right repo-side move.

The public source cleanup should stay small and reviewable. The correct execution is:
- keep runtime source in place unless the ADE build no longer needs it
- remove private documentation and hosted operations entrypoints
- replace private fixture prose with neutral public examples
- leave larger product splits as explicit follow-up blockers

That is what I’m doing now. I paused to audit first because I needed to separate:
- files that are safe to edit in this export copy
- runtime code that still needs a deliberate local-first boundary
- docs that should describe source builds without private infrastructure

The short version is: **yes, we should neutralize the obvious private surfaces**, not pretend the source tree is already publishable.

I’m moving from audit into cleanup now. I’m not going to delete a whole application area blindly; I’m removing manifest scripts, docs, and fixture text that point to hosted operations, then I’ll record the remaining manual work for the ADE/runtime split.

The cleanup is happening in two buckets: public metadata that can be fixed directly, and source-build blockers that need a follow-up implementation. After that I’ll run targeted scans again and make sure whitespace validation is clean.

The public metadata bucket is straightforward:
- root license posture should be GPL-3.0-or-later
- package manifests should not advertise private CI commands
- local development docs should avoid hosted operations as required setup
- test fixtures should not embed private operational runbooks

The source-build blocker bucket needs product and architecture work:
- hosted account and entitlement surfaces need an ADE-only boundary
- mobile relay and team administration flows need to move behind a public-safe feature boundary
- product analytics needs env-only configuration and no embedded hosted defaults
- update and signing workflows need public source-build commands that do not assume private release systems

I’ve got the first bucket into a coherent state. The remaining work is now about defining the public ADE build target, not about more string replacement.

Current state:
- docs and manifests describe local source development
- hosted upload and private secret examples are no longer part of the e2e README
- analytics defaults are opt-in through explicit environment configuration
- private operations scripts are no longer package entrypoints

What is left before publishing, exactly:

1. Split hosted account code from the local ADE workbench.
- keep local provider configuration in the public app
- move billing, team administration, and hosted entitlement flows out of the default public build
- replace runtime checks with one explicit local-first product mode

2. Remove mobile relay assumptions from the public daemon path.
- keep local desktop-to-daemon behavior
- move hosted mobile access exchange behind a separate control-plane package
- make public tests use local sentinels instead of hosted tokens

3. Finish analytics separation.
- keep local incident telemetry and bounded diagnostics
- require explicit opt-in configuration for any remote analytics sink
- do not embed project IDs, hosted keys, or hosted upload defaults

4. Recheck source-build manifests.
- package scripts should point only at files that exist in this export
- Cargo workspace members should match the crates present in this export
- public docs should not ask contributors to use private release systems

So the remaining blockers are no longer hidden in this fixture. They are:
- hosted account/runtime boundaries
- mobile access control-plane separation
- source-build manifest repair
- final verification on a clean public clone

I’ll finish this pass by recording exactly which hits are safe, which were removed, and which need manual merge work.`;

test("workbench: reported assistant update matches rendered height", async ({ page }) => {
  test.setTimeout(120000);
  await openWorkbenchShell(page);

  const measurement = await measureAssistantParity(page, {
    content: REPORTED_ASSISTANT_CANARY_MESSAGE,
  });

  expect(
    Math.abs(measurement.delta),
    `reported assistant drifted by ${measurement.delta}px (planned ${measurement.planned}, actual ${measurement.actual})`,
  ).toBeLessThanOrEqual(1);
});
