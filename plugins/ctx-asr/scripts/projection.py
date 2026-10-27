"""Metadata-only projection of the documented Claude Code hook protocol.

All arbitrary text, paths, payload objects and unknown fields stop here.
Identifiers are native correlation keys, not permission or provenance claims.
"""

import hashlib
import hmac
import re

VERSION = "0.1.0"
META = "rs.ctx.claude-asr/source"
EVENTS = set("""SessionStart Setup UserPromptSubmit UserPromptExpansion PreToolUse
PermissionRequest PermissionDenied PostToolUse PostToolUseFailure PostToolBatch
Notification MessageDisplay SubagentStart SubagentStop TaskCreated TaskCompleted
Stop StopFailure TeammateIdle InstructionsLoaded ConfigChange CwdChanged
DirectoryAdded PreCompact PostCompact PreModelSwitch PostModelSwitch SessionEnd
Elicitation ElicitationResult""".split())
CORE_EVENTS = dict(zip(
    "PreToolUse PostToolUse UserPromptSubmit Stop SessionStart SessionEnd PreCompact Notification".split(),
    "pre_tool_use post_tool_use user_prompt_submit stop session_start session_end pre_compact notification".split()))
KINDS = {"Bash": "execute", "PowerShell": "execute", "Edit": "edit",
         "Write": "edit", "NotebookEdit": "edit", "Read": "read",
         "Glob": "search", "Grep": "search", "ToolSearch": "search",
         "WebFetch": "fetch", "WebSearch": "fetch"}
BUILTINS = set(KINDS) | set("""Agent Skill AskUserQuestion EnterPlanMode ExitPlanMode
EnterWorktree ExitWorktree TodoWrite TaskCreate TaskGet TaskList TaskUpdate
TaskOutput TaskStop CronCreate CronDelete CronList ScheduleWakeup Monitor
SendMessage SubagentHandback""".split())
PERMISSIONS = set("default plan acceptEdits auto dontAsk bypassPermissions".split())
EFFORTS = set("low medium high xhigh max".split())
LOSSES = ["inference", "usage", "cost", "timestamps", "reasoning",
          "reasoning.signature", "reasoning.opaque", "message.author",
          "tool_result.content", "tool_result.raw", "exec.exit_code",
          "display_content", "content.binary", "branches", "copies",
          "lineage", "revision_history", "provider.identity", "file_change", "native",
          "_rs.ctx.claude-asr/content_and_paths_omitted",
          "_rs.ctx.claude-asr/native_record_uuids_unavailable",
          "_rs.ctx.claude-asr/permission_decisions_incomplete",
          "_rs.ctx.claude-asr/assistant_messages_incomplete",
          "_rs.ctx.claude-asr/worktree_lifecycle_incomplete",
          "_rs.ctx.claude-asr/unexposed_and_unmapped_fields",
          "_rs.ctx.claude-asr/bounded_best_effort_capture"]


def identifier(value):
    if isinstance(value, str) and re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_-]{0,127}", value):
        return value
    return None


def omitted():
    return {"state": "omitted", "reason": "policy"}


def project(session, parent, event, state, observed):
    rows = []
    name = event["hook_event_name"]
    source = {"event": name, "sessionId": parent}
    for key, dest in [("prompt_id", "promptId"), ("tool_use_id", "toolUseId"),
                      ("agent_id", "agentId"), ("message_id", "displayMessageId")]:
        value = identifier(event.get(key))
        if value:
            source[dest] = value
    if "transcript_path" in event:
        source["transcriptPathOmitted"] = True

    def emit(record_type, **fields):
        seq = state["seq"]
        state["seq"] += 1
        row = {"type": record_type, "seq": seq, "id": "r" + str(seq), "idScope": "capture",
               "observedAt": observed, **fields}
        if record_type != "session":
            row.setdefault("_meta", {})[META] = dict(source)
            if source.get("promptId"):
                row["turnId"] = source["promptId"]
        rows.append(row)
        return row

    def derived():
        return "hook-r" + str(state["seq"])

    if state["seq"] == 0:
        header = emit("session", formatVersion="0.1", agent={"name": "claude-code"},
                      sessionId=session, namespace=None, profiles=["core"],
                      contentLevel="metadata", producer={"name": "claude-code-asr",
                          "version": VERSION, "method": "observer", "instance": state["instance"],
                          "source": {"format": "claude-code-hooks"}, "lossy": LOSSES},
                      extensions=[{"id": "rs.ctx.claude-asr/hooks", "version": "1", "required": False}])
        if session != parent:
            header["scope"] = "subagent"
            # The containing session does not identify the immediate spawner
            # of a nested child, teammate, or internal agent. Keep association
            # in source metadata, without inventing a direct lineage edge.
    # Display deltas aren't separate messages. At metadata level there are no
    # bytes to concatenate; emit one representation only on its final batch.
    if name == "MessageDisplay" and event.get("final") is not True:
        return rows

    hook = emit("hook", event=CORE_EVENTS.get(name, "_com.anthropic.claude-code/" + name),
                handlerKind="command", name="claude-code-asr")
    call = source.get("toolUseId")
    if call:
        hook["callId"] = call
    mode = event.get("permission_mode")
    if isinstance(mode, str) and mode in PERMISSIONS and mode != state.get("mode"):
        emit("config_change", snapshot=True, permissionMode=mode)
        state["mode"] = mode
    effort = event.get("effort")
    if isinstance(effort, dict) and effort.get("level") in EFFORTS:
        if effort["level"] != state.get("effort"):
            emit("config_change", snapshot=True, effort=effort["level"])
            state["effort"] = effort["level"]

    def path_root(path):
        # The private index retains only keyed digests, never path text. The
        # random key is deliberately absent from the ASR stream. Equal paths
        # reuse a root without disclosing guessable unkeyed path hashes.
        digest = hmac.new(bytes.fromhex(state["pathKey"]), path.encode("utf-8"), hashlib.sha256).hexdigest()
        root = state["roots"].get(digest)
        if root is None:
            root = "cwd-r" + str(state["seq"])
            emit("declaration", roots=[{"id": root, "path": None}])
            state["roots"][digest] = root
        return root

    path = event.get("new_cwd") if name == "CwdChanged" else event.get("cwd")
    if isinstance(path, str):
        root = path_root(path)
        if state.get("cwd") != root:
            emit("workspace_state", cwd={"root": root, "path": "."})
            state["cwd"] = root
    if name == "DirectoryAdded" and isinstance(event.get("directory"), str):
        path_root(event["directory"])
    if name == "SessionStart" and event.get("source") == "resume":
        emit("state", state="resumed")
    elif name == "SessionEnd":
        emit("state", state="ended")
    elif name == "StopFailure":
        emit("state", state="error")
    elif name == "UserPromptSubmit":
        # This is submitted text before expansion/policy checks, not proof of
        # model delivery or human authorship (schedules use this hook too).
        emit("message", messageId=derived(), idSource="derived", role="user", content=[],
             capture={"content": {"state": "absent"}, "displayContent": omitted()})
    elif name == "MessageDisplay":
        emit("message", messageId=derived(), idSource="derived", role="assistant",
             audience="user", content=[], assembly={"mode": "chunks"},
             capture={"content": {"state": "absent"}, "displayContent": omitted()})
    elif name == "Notification":
        emit("message", messageId=derived(), idSource="derived", author="harness",
             origin="notice", audience="user", content=[],
             capture={"role": {"state": "absent"}, "content": omitted()})
    elif name == "PreToolUse" and call:
        state["calls"][call] = state["calls"].get(call, 0) + 1
        tool = event.get("tool_name")
        capture_fields = {"input": omitted() if "tool_input" in event else {"state": "absent"}}
        record = emit("tool_call", callId=call, callIdSource="native", capture=capture_fields)
        if isinstance(tool, str) and tool in BUILTINS:
            record.update(name=tool, kind=KINDS.get(tool, "other"))
        else:
            capture_fields["name"] = omitted() if "tool_name" in event else {"state": "absent"}
    elif name in {"PostToolUse", "PostToolUseFailure"}:
        status = "completed" if name == "PostToolUse" else "failed"
        if name == "PostToolUseFailure" and event.get("is_interrupt") is True:
            status = "interrupted"
        record = emit("tool_result", linkage="none", status=status,
                      capture={"content": {"state": "absent"}, "raw": omitted()})
        if call:
            record.update(callId=call, linkage="ambiguous" if state["calls"].get(call, 0) > 1 else "exact")
        duration = event.get("duration_ms")
        if type(duration) in (int, float) and 0 <= duration <= 2**53 - 1:
            record["durationMs"] = duration
    elif name == "PermissionRequest":
        emit("permission", requestId=derived(), idSource="derived", status="pending")
    elif name == "PermissionDenied":
        record = emit("permission", requestId=derived(), idSource="derived",
                      status="resolved", decision="reject_once", decidedBy="auto_mode")
        if call:
            record["subject"] = {"callId": call}
    elif name == "PostCompact":
        record = emit("compaction", compactionId=derived(), idSource="derived",
                      kind="summary", status="completed", replaces={"state": "unknown"},
                      capture={"summary": omitted()})
        if event.get("trigger") in {"auto", "manual"}:
            record["trigger"] = event["trigger"]

    child = source.get("agentId")
    child_status = None
    if name == "SubagentStart":
        child_status = "running"
    elif name == "SubagentStop":
        # Other stop hooks may continue the child; this is not completion.
        child_status = "_rs.ctx.claude-asr/stop_requested"
    elif name == "PostToolUse" and event.get("tool_name") == "Agent":
        response = event.get("tool_response")
        if isinstance(response, dict) and response.get("status") in {"completed", "async_launched"}:
            child = identifier(response.get("agentId"))
            child_status = "completed" if response["status"] == "completed" else "running"
    if child and child_status:
        fields = dict(state["children"].get(child, {}))
        previous = fields.pop("id", None)
        fields.update(childSession="claude-code:" + child, status=child_status)
        if name == "PostToolUse" and call:
            fields["callId"] = call
        record = emit("subagent", **fields)
        if previous:
            record["supersedes"] = previous
        state["children"][child] = {**fields, "id": record["id"]}

    if name in {"Elicitation", "ElicitationResult"}:
        mode = event.get("mode", "form")
        if mode in {"form", "url"}:
            request = identifier(event.get("elicitation_id"))
            fields = {"requestId": request or derived(), "mode": mode}
            if request is None:
                fields["idSource"] = "derived"
            if name == "Elicitation":
                fields["status"] = "pending"
            else:
                status = {"accept": "answered", "decline": "declined", "cancel": "cancelled"}.get(event.get("action"))
                if status:
                    fields["status"] = status
                fields["capture"] = {"response": omitted()}
            record = emit("input_request", **fields)
            if request:
                if request in state["requests"]:
                    record["supersedes"] = state["requests"][request]
                state["requests"][request] = record["id"]
    return rows
