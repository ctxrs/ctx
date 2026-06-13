use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ctx_providers::adapters::ProviderAdapter;
use ctx_providers::crp::Tier1CrpAdapter;

pub fn python_binary() -> Option<PathBuf> {
    which::which("python3")
        .or_else(|_| which::which("python"))
        .ok()
}

pub fn write_crp_fixture_runtime(root: &Path) -> PathBuf {
    let script_dir = root
        .join("providers")
        .join("agent-servers")
        .join("crp-fixtures")
        .join("fake");
    std::fs::create_dir_all(&script_dir).unwrap();
    let script_path = script_dir.join("crp_fixture_runtime.py");

    // This CRP runtime replays deterministic fixtures keyed by CTX_PROVIDER_ID.
    // Fixtures live under $CTX_TEST_FIXTURES_DIR/<provider_id>/<scenario>.json.
    let script = r#"
import json
import os
import sys
import uuid

SEQ = 1
TOOL_OUTPUT_EMIT_MAX_BYTES = 64 * 1024
TOOL_OUTPUT_EMIT_MAX_CHUNKS = 64
TOOL_OUTPUT_COMPLETION_MAX_BYTES = 8 * 1024
PROVIDER_ID = os.environ.get("CTX_PROVIDER_ID")
FIXTURE_ROOT = os.environ.get("CTX_TEST_FIXTURES_DIR")
SCENARIO = os.environ.get("CTX_TEST_SCENARIO") or "basic"
COMMAND_LOG = os.environ.get("CTX_TEST_CRP_COMMAND_LOG")

scenario = {"turns": []}
try:
    if not PROVIDER_ID:
        raise RuntimeError("missing CTX_PROVIDER_ID")
    if not FIXTURE_ROOT:
        raise RuntimeError("missing CTX_TEST_FIXTURES_DIR")
    fixture_path = os.path.join(FIXTURE_ROOT, PROVIDER_ID, SCENARIO + ".json")
    with open(fixture_path, "r") as f:
        scenario = json.load(f)
except Exception as e:
    sys.stderr.write("fixture load failed: %r\n" % (e,))
    sys.stderr.flush()

provider_session_id = PROVIDER_ID + "-thread"

def default_models(provider_id):
    if provider_id == "codex":
        return [
            {"id": "gpt-5.4/medium", "name": "GPT-5.4 (Medium)"},
            {"id": "gpt-5.4/xhigh", "name": "GPT-5.4 (Extra High)"},
        ]
    if provider_id == "claude-crp":
        return [
            {"id": "default/medium", "name": "Default (Medium)"},
            {"id": "default/high", "name": "Default (High)"},
        ]
    return [{"id": "fake-model", "name": "fake-model"}]

models = scenario.get("models")
if not isinstance(models, list) or not models:
    models = default_models(PROVIDER_ID)
current_model_id = (
    scenario.get("current_model_id")
    or (models[0].get("id") if models and isinstance(models[0], dict) else None)
    or "fake-model"
)
session_status = scenario.get("session_status")
if not isinstance(session_status, dict):
    session_status = {
        "quiescent": True,
        "busy_reasons": [],
        "loaded_thread_ids": [],
        "active_thread_ids": [],
    }

def send(msg, channel="control"):
    global SEQ
    msg["seq"] = SEQ
    SEQ += 1
    msg["channel"] = channel
    sys.stdout.write(json.dumps(msg))
    sys.stdout.write("\n")
    sys.stdout.flush()

def tool_output_delta_type():
    # Codex CRP currently emits the underscored tag in production logs.
    if PROVIDER_ID == "codex":
        return "tool.output_delta"
    return "tool.output.delta"

def send_tool(session_id, turn_id, tool):
    tool_call_id = str(uuid.uuid4())
    tool_name = tool.get("tool_name") or "exec_command"
    tool_input = tool.get("input")
    output_chunks = tool.get("output_chunks")
    if output_chunks is None:
        repeated_chunk = tool.get("output_chunk")
        repeat_count = int(tool.get("output_repeat") or 0)
        if repeated_chunk is not None and repeat_count > 0:
            output_chunks = [repeated_chunk] * repeat_count
    if output_chunks is not None:
        tool_output = "".join(str(chunk) for chunk in output_chunks)
    else:
        tool_output = tool.get("output") or "ok"
    tool_output = str(tool_output)[:TOOL_OUTPUT_COMPLETION_MAX_BYTES]
    completed_output = tool.get("completed_output")
    if completed_output is None:
        if tool_name == "exec_command":
            completed_output = {
                "rawOutput": {
                    "aggregated_output": tool_output,
                }
            }
        else:
            completed_output = {"text": tool_output}
    send({
        "type": "tool.started",
        "session_id": session_id,
        "turn_id": turn_id,
        "tool_call_id": tool_call_id,
        "tool_name": tool_name,
        "input": tool_input,
    }, channel="data")
    if output_chunks is not None:
        emitted_bytes = 0
        emitted_chunks = 0
        for chunk in output_chunks:
            if emitted_bytes >= TOOL_OUTPUT_EMIT_MAX_BYTES or emitted_chunks >= TOOL_OUTPUT_EMIT_MAX_CHUNKS:
                break
            chunk = str(chunk)
            remaining = TOOL_OUTPUT_EMIT_MAX_BYTES - emitted_bytes
            if remaining <= 0:
                break
            chunk = chunk[:remaining]
            if not chunk:
                break
            send({
                "type": tool_output_delta_type(),
                "session_id": session_id,
                "turn_id": turn_id,
                "tool_call_id": tool_call_id,
                "chunk": chunk,
            }, channel="data")
            emitted_bytes += len(chunk.encode("utf-8"))
            emitted_chunks += 1
    else:
        send({
            "type": tool_output_delta_type(),
            "session_id": session_id,
            "turn_id": turn_id,
            "tool_call_id": tool_call_id,
            "chunk": tool_output,
        }, channel="data")
    send({
        "type": "tool.completed",
        "session_id": session_id,
        "turn_id": turn_id,
        "tool_call_id": tool_call_id,
        "tool_name": tool_name,
        "status": "success",
        "output": completed_output,
    }, channel="data")

def send_turn(session_id, turn_id, turn):
    context_window = turn.get("context_window")
    if turn.get("events"):
        msg_id = str(uuid.uuid4())
        final = turn.get("final") or ("hola from " + PROVIDER_ID)
        for step in (turn.get("events") or []):
            kind = step.get("kind")
            if kind == "thought":
                item_id = str(uuid.uuid4())
                thought = step.get("content") or ""
                if not thought:
                    continue
                send({
                    "type": "reasoning.trace",
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "chunk": thought,
                    "item_id": item_id,
                    "summary_index": 0,
                }, channel="data")
                send({
                    "type": "reasoning.trace.final",
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "content": thought,
                    "item_id": item_id,
                    "summary_index": 0,
                }, channel="data")
                continue
            if kind == "tool":
                send_tool(session_id, turn_id, step)
                continue
            if kind == "delta":
                send({
                    "type": "message.delta",
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "message_id": msg_id,
                    "delta": step.get("content") or "",
                }, channel="data")
                continue
            if kind == "final":
                send({
                    "type": "message.final",
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "message_id": msg_id,
                    "content": step.get("content") or final,
                }, channel="data")
                completed = {
                    "type": "turn.completed",
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "status": "success",
                }
                if context_window is not None:
                    completed["context_window"] = context_window
                send(completed, channel="control")
                return
        send({
            "type": "message.final",
            "session_id": session_id,
            "turn_id": turn_id,
            "message_id": msg_id,
            "content": final,
        }, channel="data")
        completed = {
            "type": "turn.completed",
            "session_id": session_id,
            "turn_id": turn_id,
            "status": "success",
        }
        if context_window is not None:
            completed["context_window"] = context_window
        send(completed, channel="control")
        return

    thought = turn.get("thought")
    if thought:
        item_id = str(uuid.uuid4())
        send({
            "type": "reasoning.trace",
            "session_id": session_id,
            "turn_id": turn_id,
            "chunk": thought,
            "item_id": item_id,
            "summary_index": 0,
        }, channel="data")
        send({
            "type": "reasoning.trace.final",
            "session_id": session_id,
            "turn_id": turn_id,
            "content": thought,
            "item_id": item_id,
            "summary_index": 0,
        }, channel="data")

    for tool in (turn.get("tools") or []):
        send_tool(session_id, turn_id, tool)

    msg_id = str(uuid.uuid4())
    final = turn.get("final") or ("hola from " + PROVIDER_ID)
    for chunk in (turn.get("deltas") or []):
        send({
            "type": "message.delta",
            "session_id": session_id,
            "turn_id": turn_id,
            "message_id": msg_id,
            "delta": chunk,
        }, channel="data")
    send({
        "type": "message.final",
        "session_id": session_id,
        "turn_id": turn_id,
        "message_id": msg_id,
        "content": final,
    }, channel="data")
    completed = {
        "type": "turn.completed",
        "session_id": session_id,
        "turn_id": turn_id,
        "status": "success",
    }
    if context_window is not None:
        completed["context_window"] = context_window
    send(completed, channel="control")

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    if COMMAND_LOG:
        try:
            with open(COMMAND_LOG, "a") as f:
                f.write(line + "\n")
        except Exception:
            pass
    try:
        cmd = json.loads(line)
    except Exception:
        continue
    t = cmd.get("type")

    if t == "models.list":
        send({
            "type": "models.list",
            "models": models,
            "current_model_id": current_model_id,
        })
        continue

    if t == "session.open":
        session_id = cmd.get("session_id") or "sess_1"
        send({
            "type": "session.opened",
            "session_id": session_id,
            "provider_session_id": provider_session_id,
            "current_model_id": current_model_id,
            "models": models,
        })
        continue

    if t == "session.authenticate":
        session_id = cmd.get("session_id") or "sess_1"
        send({
            "type": "session.notice",
            "session_id": session_id,
            "code": "authenticated",
            "severity": "info",
            "message": "fixture authentication complete",
            "details": {"provider_id": PROVIDER_ID},
            "transient": False,
        })
        continue

    if t == "session.set_model":
        session_id = cmd.get("session_id") or "sess_1"
        requested_model_id = (cmd.get("model_id") or "").strip()
        if not requested_model_id:
            send({
                "type": "session.notice",
                "session_id": session_id,
                "code": "session_model_update_failed",
                "severity": "error",
                "message": "missing model_id",
                "details": {"model_id": requested_model_id},
                "transient": False,
            })
            continue
        current_model_id = requested_model_id
        send({
            "type": "session.notice",
            "session_id": session_id,
            "code": "session_model_updated",
            "severity": "info",
            "message": "session model updated to " + requested_model_id,
            "details": {"model_id": requested_model_id},
            "transient": False,
        })
        continue

    if t == "session.status":
        session_id = cmd.get("session_id") or "sess_1"
        send({
            "type": "session.notice",
            "session_id": session_id,
            "code": "session_status",
            "severity": "info",
            "message": "fixture session status",
            "details": session_status,
            "transient": False,
        })
        continue

    if t == "session.prompt":
        session_id = cmd.get("session_id") or "sess_1"
        turn_id = cmd.get("turn_id") or "turn_1"
        try:
            turn_index = 0
            turns = scenario.get("turns") or []
            if turns:
                turn_index = min(int(cmd.get("turn_index") or 0), len(turns) - 1)
                send_turn(session_id, turn_id, turns[turn_index])
            else:
                send_turn(session_id, turn_id, {})
        except Exception as e:
            sys.stderr.write("fixture turn failed: %r\n" % (e,))
            sys.stderr.flush()
            send({
                "type": "turn.completed",
                "session_id": session_id,
                "turn_id": turn_id,
                "status": "error",
                "error": {"message": str(e), "kind": "fixture_turn_error"},
            }, channel="control")
        continue
"#;

    std::fs::write(&script_path, script.trim()).unwrap();
    script_path
}

pub fn build_crp_fixture_providers(
    provider_ids: &[&str],
    python: &Path,
    script_path: &Path,
) -> HashMap<String, Arc<dyn ProviderAdapter>> {
    let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
    for provider_id in provider_ids {
        let adapter: Arc<Tier1CrpAdapter> = Arc::new(Tier1CrpAdapter::from_raw(
            provider_id,
            python.to_string_lossy().to_string(),
            vec![script_path.to_string_lossy().to_string()],
        ));
        providers.insert((*provider_id).to_string(), adapter);
    }
    providers
}
