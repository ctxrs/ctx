# OMP 18.0.10 `parentSession` fixtures

These fixtures were captured from isolated sessions created by the installed OMP 18.0.10 binary.

`ordinary-id-fork` was produced with OMP's startup `--fork` flow, which calls `SessionManager.forkFrom` and writes the source session ID to `parentSession`.

`path-branch` was produced by the RPC `branch` operation, which calls `createBranchedSession` for the selected non-root entry and writes the source session path to `parentSession`.

`missing-parent` is a sanitized copy of the ordinary fork whose parent ID is not present in the admitted fixture inventory.

Session, entry, timestamp, cwd, prompt, and path values were replaced with deterministic synthetic values. The physical 256-byte OMP title slot and record shapes were retained. Tests replace the sanitized path reference with the canonical path of the copied parent fixture before import.
