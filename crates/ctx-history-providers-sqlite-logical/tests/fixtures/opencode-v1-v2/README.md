# OpenCode v1 to v2 native migration fixture

Sanitized native output from official OpenCode 1.18.34 followed by OpenCode
2.0.21, using no-model user messages and the provider's own migration. The
checkpointed database contains two sessions and three messages in both the
legacy `message`/`part` and current `session_v2`/`session_message` layouts.

Message text is authored test content. Identifiers and workspace paths were
consistently sanitized without changing schema, rowids, storage types, ordering,
timestamps, or overlap relationships. Two independent sanitizations produced
the same bytes. This fixture does not cover live WAL/locking, assistant/tool
messages, or the pre-split-v2 upgrade path.

The import test opens an ordinary SQLite reader on its disposable copy and holds
it to establish WAL/SHM coordination before ctx reads. The checkpointed fixture
retains WAL mode; it is not an absent-sidecar or offline-source test.

Database SHA-256:
`43baa176d53c6d7392b8ec55fce6ff238f452d8272727e129c8378909dd6330e`.

The OpenCode schema is distributed under the adjacent upstream MIT license.
