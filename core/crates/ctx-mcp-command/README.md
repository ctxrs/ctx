# ctx-mcp-command

Selects and stages the `ctx-mcp` runtime command injected into provider harness environments.

This code lives outside `ctx-http` so provider execution can depend on a focused runtime-command
helper without depending on HTTP routes, daemon state, or API tests.
