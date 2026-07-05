CREATE TABLE schema_version (
  version integer not null
);

INSERT INTO schema_version VALUES (1);

CREATE TABLE sessions (
  id text primary key,
  title text,
  cwd text,
  created_at integer,
  updated_at integer
);

INSERT INTO sessions VALUES (
  'codestudio-session-1',
  'Code Studio fixture',
  '/workspace/codestudio',
  1783267200000,
  1783267201000
);

CREATE TABLE turns (
  id text primary key,
  session_id text not null,
  seq integer not null,
  role text not null,
  content text not null,
  created_at integer,
  metadata_json text
);

INSERT INTO turns VALUES (
  'codestudio-user-1',
  'codestudio-session-1',
  1,
  'user',
  'CODESTUDIO_ORACLE_USER_TEXT orange prism',
  1783267200000,
  '{"fixture":true}'
);

INSERT INTO turns VALUES (
  'codestudio-assistant-1',
  'codestudio-session-1',
  2,
  'assistant',
  'CODESTUDIO_ORACLE_ASSISTANT_TEXT cobalt prism',
  1783267201000,
  '{"fixture":true}'
);
