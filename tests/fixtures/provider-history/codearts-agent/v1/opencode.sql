CREATE TABLE session (
  id text primary key,
  parent_id text,
  title text not null,
  directory text not null,
  model text,
  agent text,
  time_created integer not null,
  time_updated integer not null,
  tokens_input integer not null,
  tokens_output integer not null,
  tokens_reasoning integer not null,
  tokens_cache_read integer not null,
  tokens_cache_write integer not null
);

INSERT INTO session VALUES (
  'codearts-root',
  NULL,
  'CodeArts fixture',
  '/workspace/codearts',
  '{"id":"codearts-model"}',
  'primary',
  1783263600000,
  1783263601000,
  1,
  1,
  0,
  0,
  0
);

CREATE TABLE session_message (
  id text primary key,
  session_id text not null,
  type text not null,
  seq integer not null,
  time_created integer not null,
  time_updated integer not null,
  data text not null
);

INSERT INTO session_message VALUES (
  'codearts-user-1',
  'codearts-root',
  'user',
  1,
  1783263600000,
  1783263600000,
  '{"time":{"created":1783263600000},"text":"CODEARTS_ORACLE_USER_TEXT copper ridge"}'
);

INSERT INTO session_message VALUES (
  'codearts-assistant-1',
  'codearts-root',
  'assistant',
  2,
  1783263601000,
  1783263601000,
  '{"time":{"created":1783263601000},"text":"CODEARTS_ORACLE_ASSISTANT_TEXT jade ridge"}'
);
