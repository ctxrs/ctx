# Antigravity, Gemini, Cursor Provider Evidence

Date: 2026-06-23

Scope: Antigravity CLI, Gemini CLI, Cursor, plus cheap GitHub Copilot CLI and
Factory Droid overlap classification.

## Local Probe

Commands checked on `PATH`: `agy`, `antigravity`, `gemini`, `cursor-agent`,
`cursor`, `copilot`, `droid`, and `factory`.

Result: none were installed in this environment.

Local directories checked:

- `~/.antigravity`
- `~/.gemini`
- `~/.cursor`
- `~/.config/Cursor`
- `~/.config/cursor`
- `~/.copilot`
- `~/.factory`
- `~/.droid`

Result: none were present in this environment.

Because neither CLI binaries nor local provider config/history directories were
available, live/gated E2E runs for these providers were blocked on this machine.

## Current Source Surfaces Checked

- Antigravity CLI GitHub README: `https://github.com/google-antigravity/antigravity-cli`
- Antigravity docs: `https://antigravity.google/docs/hooks`,
  `https://antigravity.google/docs/cli-using`
- Gemini CLI GitHub README: `https://github.com/google-gemini/gemini-cli`
- Gemini session/history docs:
  `https://geminicli.com/docs/cli/tutorials/session-management/`
- Gemini hooks docs: `https://geminicli.com/docs/hooks/`,
  `https://geminicli.com/docs/hooks/reference/`
- Gemini telemetry docs: `https://geminicli.com/docs/cli/telemetry/`
- Cursor CLI docs: `https://cursor.com/docs/cli/overview`,
  `https://cursor.com/docs/cli/headless`, `https://cursor.com/docs/subagents`
- GitHub Copilot CLI docs:
  `https://docs.github.com/en/copilot/how-tos/copilot-cli/use-copilot-cli/overview`,
  `https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-command-reference`
- Factory Droid docs: `https://docs.factory.ai/reference/cli-reference`,
  `https://docs.factory.ai/cli/getting-started/quickstart`,
  `https://factory.ai/product/cli`

## Branch Evidence

- Machine-readable rows: `docs/provider-support-matrix.json`
- Human support matrix: `docs/provider-support.md`
- Sanitized fixtures:
  - `tests/fixtures/provider/antigravity.jsonl`
  - `tests/fixtures/provider/gemini.jsonl`
  - `tests/fixtures/provider/cursor.jsonl`
- Focused tests:
  - `cargo test -p ctx --test cli provider_fixture_import_supports_additional_p0_fixture_providers`
  - `cargo test -p ctx-cli provider_fixture_import_supports_antigravity_gemini_and_cursor`
  - `cargo test -p ctx-cli import_local_providers_imports_codex_history_and_reports_unsupported_native_hooks`
  - `cargo test -p ctx-cli provider_fixture_import_supports_additional_p0_fixture_providers`
