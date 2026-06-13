# workspaceSetup

Workflow and controller helpers extracted from `WorkspaceSetupPage.tsx`.

- `remoteProfiles.ts`: remote SSH recent/profile storage + parsing.
- `launchProgress.ts`: execution launch phase/log/error/time projection helpers.
- `flowController.ts`: step-key navigation helpers and run-token guards for async flow transitions.
- `workflowTypes.ts` / `workflowReducer.ts`: workflow-owned draft state and setter helpers.
- `useWorkspaceSetupWorkflow.ts`: the main composition layer for draft state, route flow, provisioning services, and create handoff.
- `routePlanner.ts`: pure route-plan and onboarding-insertion derivation from provisioning snapshots.
- `createHandoff.ts`: explicit create-intent helpers and create-error step mapping.
- `launchHandoff.ts`: dedicated setup launch/prewarm adapter and launch-stream observation helpers.
