## Moxby v2.3.0 `moxby_chats.db` fixture

This fixture is synthetic and sanitized. Its schema is based on the public
`ChainAI-Org/moxby-agent-releases` `Moxby_x64.app.tar.gz` v2.3.0 artifact:

- release: `Moxby v2.3.0`, published 2026-07-05
- artifact SHA-256:
  `270842c5e632c8d6ab9885a45c57fff935948f79cb1a5043b770f40cf5fd0cbe`
- bundle id from `Contents/Info.plist`: `com.moxby.agent`
- bundled sidecar strings identify `MOXBY_STATE_DIR`, `Application Support`,
  and durable chat storage in `moxby_chats.db`

The rows use fake ids, timestamps, workspace names, model names, and message
text. No real Moxby user data is included.
