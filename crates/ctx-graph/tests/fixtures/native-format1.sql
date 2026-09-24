-- Synthetic native index written by the preceding Graf storage format.
-- Source: provider.py defines target; consumer.py imports it as relay and calls missing().
-- Only the stored project root was changed to the portable literal fixture.
PRAGMA foreign_keys=ON;
BEGIN;
CREATE TABLE metadata (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    generation INTEGER NOT NULL, kind TEXT NOT NULL,
    root TEXT, coverage TEXT NOT NULL, graph_metadata TEXT NOT NULL
, search_version INTEGER NOT NULL DEFAULT 0);
CREATE TABLE files (
    path TEXT PRIMARY KEY, hash TEXT NOT NULL, module TEXT NOT NULL, diagnostics TEXT NOT NULL
);
CREATE TABLE nodes (
    id TEXT PRIMARY KEY, label TEXT NOT NULL, qualified_name TEXT, binding_key TEXT,
    file TEXT NOT NULL, owner_file TEXT REFERENCES files(path) ON DELETE CASCADE,
    payload TEXT NOT NULL, search TEXT NOT NULL
);
CREATE TABLE refs (
    id TEXT PRIMARY KEY, source TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    owner_file TEXT NOT NULL REFERENCES files(path) ON DELETE CASCADE,
    relation TEXT NOT NULL, payload TEXT NOT NULL,
    resolved_target TEXT, resolution_reason TEXT NOT NULL
);
CREATE TABLE ref_keys (
    ref_id TEXT NOT NULL REFERENCES refs(id) ON DELETE CASCADE,
    priority INTEGER NOT NULL, binding_key TEXT NOT NULL,
    PRIMARY KEY(ref_id, priority)
);
CREATE TABLE edges (
    id TEXT PRIMARY KEY,
    source TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    target TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    relation TEXT NOT NULL, directed INTEGER NOT NULL CHECK(directed IN (0, 1)),
    owner_file TEXT REFERENCES files(path) ON DELETE CASCADE,
    ref_id TEXT UNIQUE REFERENCES refs(id) ON DELETE CASCADE,
    payload TEXT NOT NULL
);
CREATE TABLE node_aliases (
        node_id TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        binding_key TEXT NOT NULL, PRIMARY KEY(node_id,binding_key)
    );
INSERT INTO metadata(rowid,singleton,generation,kind,root,coverage,graph_metadata,search_version) VALUES(1,1,1,'native','fixture','{"supported_files":2,"unsupported_files":0,"unchanged_files":0}','{"graf_index_options":{"code_only":false,"include_generated":false,"ingest":{"allow_private_urls":false,"arxiv_api_endpoint":null,"converters":{},"max_input_bytes":33554432,"max_text_bytes":4194304,"semantic":null,"timeout_secs":60,"transcript_cache_dir":null,"tweet_oembed_endpoint":null},"max_semantic_calls":null,"max_semantic_files":32,"max_semantic_output_tokens":null,"no_gitignore":false,"python_source_roots":[],"robot_python":null,"semantic_code":false,"swift_modules":{}}}',5);
INSERT INTO files(rowid,path,hash,module,diagnostics) VALUES(1,'consumer.py','python-v11:terminal-v2-8575f788ce0942c69cb90d361c7c73fd18f702e2032e7e2732cfdf907f691ad5:ae14d4e2408520ab37435b74b0922fd2937d74786f536b5461c782f78e40f4a8','consumer','[]');
INSERT INTO files(rowid,path,hash,module,diagnostics) VALUES(2,'provider.py','python-v11:terminal-v2-52432fad07b36d8e088fb8554ef55fe610902e561e398cf911acdc9c4e88614f:c79a19f520979afc32c695561aa07db25d7006e4b1c582ca79d69140fa536393','provider','[]');
INSERT INTO nodes(rowid,id,label,qualified_name,binding_key,file,owner_file,payload,search) VALUES(1,'python:consumer.py:module','consumer','consumer','module:consumer','consumer.py','consumer.py','{"id":"python:consumer.py:module","label":"consumer","kind":"module","file":"consumer.py","line":1,"end_line":7,"qualified_name":"consumer","binding_key":"module:consumer","metadata":{"binding_aliases":["file:consumer.py"],"python_all":null,"python_blocked_exports":[],"python_definitions":{"caller":"python:consumer:caller","unknown":"python:consumer:unknown"},"python_exports":{"relay":{"line":1,"target":"python:provider:target"}},"python_stars":[],"python_uncertain":false}}','python:consumer.py:module consumer consumer consumer.py python:consumer.py:module consumer consumer consumer.py');
INSERT INTO nodes(rowid,id,label,qualified_name,binding_key,file,owner_file,payload,search) VALUES(2,'python:consumer.py:caller@38','caller','caller','python:consumer:caller','consumer.py','consumer.py','{"id":"python:consumer.py:caller@38","label":"caller","kind":"function","file":"consumer.py","line":3,"end_line":4,"qualified_name":"caller","binding_key":"python:consumer:caller","metadata":{"binding_aliases":["symbol:caller","symbol:consumer.py::caller"]}}','python:consumer.py:caller@38 caller caller consumer.py python:consumer.py:caller@38 caller caller consumer.py');
INSERT INTO nodes(rowid,id,label,qualified_name,binding_key,file,owner_file,payload,search) VALUES(3,'python:consumer.py:unknown@73','unknown','unknown','python:consumer:unknown','consumer.py','consumer.py','{"id":"python:consumer.py:unknown@73","label":"unknown","kind":"function","file":"consumer.py","line":6,"end_line":7,"qualified_name":"unknown","binding_key":"python:consumer:unknown","metadata":{"binding_aliases":["symbol:consumer.py::unknown","symbol:unknown"]}}','python:consumer.py:unknown@73 unknown unknown consumer.py python:consumer.py:unknown@73 unknown unknown consumer.py');
INSERT INTO nodes(rowid,id,label,qualified_name,binding_key,file,owner_file,payload,search) VALUES(4,'python:provider.py:module','provider','provider','module:provider','provider.py','provider.py','{"id":"python:provider.py:module","label":"provider","kind":"module","file":"provider.py","line":1,"end_line":2,"qualified_name":"provider","binding_key":"module:provider","metadata":{"binding_aliases":["file:provider.py"],"python_all":null,"python_blocked_exports":[],"python_definitions":{"target":"python:provider:target"},"python_exports":{},"python_stars":[],"python_uncertain":false}}','python:provider.py:module provider provider provider.py python:provider.py:module provider provider provider.py');
INSERT INTO nodes(rowid,id,label,qualified_name,binding_key,file,owner_file,payload,search) VALUES(5,'python:provider.py:target@0','target','target','python:provider:target','provider.py','provider.py','{"id":"python:provider.py:target@0","label":"target","kind":"function","file":"provider.py","line":1,"end_line":2,"qualified_name":"target","binding_key":"python:provider:target","metadata":{"binding_aliases":["symbol:provider.py::target","symbol:target"]}}','python:provider.py:target@0 target target provider.py python:provider.py:target@0 target target provider.py');
INSERT INTO refs(rowid,id,source,owner_file,relation,payload,resolved_target,resolution_reason) VALUES(1,'import:python:consumer.py:module:21-36','python:consumer.py:module','consumer.py','imports','{"id":"import:python:consumer.py:module:21-36","source":"python:consumer.py:module","label":"provider.target","relation":"imports","file":"consumer.py","line":1,"candidate_keys":["python:provider:target"],"reason":"import target is unavailable or ambiguous"}','python:provider.py:target@0','');
INSERT INTO refs(rowid,id,source,owner_file,relation,payload,resolved_target,resolution_reason) VALUES(2,'call:python:consumer.py:caller@38:63-71','python:consumer.py:caller@38','consumer.py','calls','{"id":"call:python:consumer.py:caller@38:63-71","source":"python:consumer.py:caller@38","label":"relay","relation":"calls","file":"consumer.py","line":4,"candidate_keys":["python:provider:target"],"reason":"static target is unavailable or ambiguous"}','python:provider.py:target@0','');
INSERT INTO refs(rowid,id,source,owner_file,relation,payload,resolved_target,resolution_reason) VALUES(3,'call:python:consumer.py:unknown@73:99-108','python:consumer.py:unknown@73','consumer.py','calls','{"id":"call:python:consumer.py:unknown@73:99-108","source":"python:consumer.py:unknown@73","label":"missing","relation":"calls","file":"consumer.py","line":7,"candidate_keys":[],"reason":"dynamic, shadowed, or uncertain Python binding"}',NULL,'dynamic, shadowed, or uncertain Python binding');
INSERT INTO refs(rowid,id,source,owner_file,relation,payload,resolved_target,resolution_reason) VALUES(4,'reexport:python:consumer.py:module:relay','python:consumer.py:module','consumer.py','re_exports','{"id":"reexport:python:consumer.py:module:relay","source":"python:consumer.py:module","label":"relay","relation":"re_exports","file":"consumer.py","line":1,"candidate_keys":["module:provider"],"reason":"explicit public import target is unavailable or ambiguous"}','python:provider.py:module','');
INSERT INTO ref_keys(rowid,ref_id,priority,binding_key) VALUES(1,'import:python:consumer.py:module:21-36',0,'python:provider:target');
INSERT INTO ref_keys(rowid,ref_id,priority,binding_key) VALUES(2,'call:python:consumer.py:caller@38:63-71',0,'python:provider:target');
INSERT INTO ref_keys(rowid,ref_id,priority,binding_key) VALUES(3,'reexport:python:consumer.py:module:relay',0,'module:provider');
INSERT INTO edges(rowid,id,source,target,relation,directed,owner_file,ref_id,payload) VALUES(1,'contains:python:consumer.py:caller@38','python:consumer.py:module','python:consumer.py:caller@38','contains',1,'consumer.py',NULL,'{"id":"contains:python:consumer.py:caller@38","source":"python:consumer.py:module","target":"python:consumer.py:caller@38","relation":"contains","directed":true,"file":"consumer.py","line":3,"confidence":"static","metadata":null}');
INSERT INTO edges(rowid,id,source,target,relation,directed,owner_file,ref_id,payload) VALUES(2,'contains:python:consumer.py:unknown@73','python:consumer.py:module','python:consumer.py:unknown@73','contains',1,'consumer.py',NULL,'{"id":"contains:python:consumer.py:unknown@73","source":"python:consumer.py:module","target":"python:consumer.py:unknown@73","relation":"contains","directed":true,"file":"consumer.py","line":6,"confidence":"static","metadata":null}');
INSERT INTO edges(rowid,id,source,target,relation,directed,owner_file,ref_id,payload) VALUES(3,'contains:python:provider.py:target@0','python:provider.py:module','python:provider.py:target@0','contains',1,'provider.py',NULL,'{"id":"contains:python:provider.py:target@0","source":"python:provider.py:module","target":"python:provider.py:target@0","relation":"contains","directed":true,"file":"provider.py","line":1,"confidence":"static","metadata":null}');
INSERT INTO edges(rowid,id,source,target,relation,directed,owner_file,ref_id,payload) VALUES(4,'reference:call:python:consumer.py:caller@38:63-71','python:consumer.py:caller@38','python:provider.py:target@0','calls',1,'consumer.py','call:python:consumer.py:caller@38:63-71','{"id":"reference:call:python:consumer.py:caller@38:63-71","source":"python:consumer.py:caller@38","target":"python:provider.py:target@0","relation":"calls","directed":true,"file":"consumer.py","line":4,"confidence":"statically_resolved","metadata":{"reference_id":"call:python:consumer.py:caller@38:63-71"}}');
INSERT INTO edges(rowid,id,source,target,relation,directed,owner_file,ref_id,payload) VALUES(5,'reference:import:python:consumer.py:module:21-36','python:consumer.py:module','python:provider.py:target@0','imports',1,'consumer.py','import:python:consumer.py:module:21-36','{"id":"reference:import:python:consumer.py:module:21-36","source":"python:consumer.py:module","target":"python:provider.py:target@0","relation":"imports","directed":true,"file":"consumer.py","line":1,"confidence":"statically_resolved","metadata":{"reference_id":"import:python:consumer.py:module:21-36"}}');
INSERT INTO edges(rowid,id,source,target,relation,directed,owner_file,ref_id,payload) VALUES(6,'reference:reexport:python:consumer.py:module:relay','python:consumer.py:module','python:provider.py:module','re_exports',1,'consumer.py','reexport:python:consumer.py:module:relay','{"id":"reference:reexport:python:consumer.py:module:relay","source":"python:consumer.py:module","target":"python:provider.py:module","relation":"re_exports","directed":true,"file":"consumer.py","line":1,"confidence":"statically_resolved","metadata":{"reference_id":"reexport:python:consumer.py:module:relay"}}');
INSERT INTO node_aliases(rowid,node_id,binding_key) VALUES(1,'python:consumer.py:module','file:consumer.py');
INSERT INTO node_aliases(rowid,node_id,binding_key) VALUES(2,'python:consumer.py:caller@38','symbol:caller');
INSERT INTO node_aliases(rowid,node_id,binding_key) VALUES(3,'python:consumer.py:caller@38','symbol:consumer.py::caller');
INSERT INTO node_aliases(rowid,node_id,binding_key) VALUES(4,'python:consumer.py:unknown@73','symbol:consumer.py::unknown');
INSERT INTO node_aliases(rowid,node_id,binding_key) VALUES(5,'python:consumer.py:unknown@73','symbol:unknown');
INSERT INTO node_aliases(rowid,node_id,binding_key) VALUES(6,'python:provider.py:module','file:provider.py');
INSERT INTO node_aliases(rowid,node_id,binding_key) VALUES(7,'python:provider.py:target@0','symbol:provider.py::target');
INSERT INTO node_aliases(rowid,node_id,binding_key) VALUES(8,'python:provider.py:target@0','symbol:target');
CREATE INDEX edges_owner ON edges(owner_file) WHERE owner_file IS NOT NULL;
CREATE INDEX edges_source ON edges(source, id);
CREATE INDEX edges_source_direction ON edges(source, id) WHERE directed=0;
CREATE INDEX edges_source_direction_relation ON edges(source, relation, id) WHERE directed=0;
CREATE INDEX edges_source_relation ON edges(source, relation, id);
CREATE INDEX edges_target ON edges(target, id);
CREATE INDEX edges_target_direction ON edges(target, id) WHERE directed=0;
CREATE INDEX edges_target_direction_relation ON edges(target, relation, id) WHERE directed=0;
CREATE INDEX edges_target_relation ON edges(target, relation, id);
CREATE INDEX node_aliases_binding ON node_aliases(binding_key,node_id);
CREATE INDEX nodes_binding ON nodes(binding_key, id) WHERE binding_key IS NOT NULL;
CREATE INDEX nodes_file ON nodes(file, id);
CREATE INDEX nodes_label ON nodes(label, id);
CREATE INDEX nodes_owner ON nodes(owner_file) WHERE owner_file IS NOT NULL;
CREATE INDEX nodes_qualified ON nodes(qualified_name, id) WHERE qualified_name IS NOT NULL;
CREATE INDEX ref_keys_binding ON ref_keys(binding_key, ref_id);
CREATE INDEX refs_owner ON refs(owner_file);
CREATE INDEX refs_source ON refs(source, id);
CREATE INDEX refs_unresolved_relation ON refs(source, relation, id) WHERE resolved_target IS NULL;
CREATE INDEX refs_unresolved_source ON refs(source, id) WHERE resolved_target IS NULL;
CREATE VIRTUAL TABLE node_search USING fts5(text);
INSERT INTO node_search(rowid,text) SELECT rowid,search FROM nodes;
CREATE TRIGGER nodes_delete AFTER DELETE ON nodes BEGIN
    DELETE FROM node_search WHERE rowid = old.rowid;
END;
CREATE TRIGGER nodes_insert AFTER INSERT ON nodes BEGIN
    INSERT INTO node_search(rowid, text) VALUES(new.rowid, new.search);
END;
PRAGMA application_id=1196572998;
PRAGMA user_version=1;
COMMIT;
