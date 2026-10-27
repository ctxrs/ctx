#!/usr/bin/env python3
"""Silent, bounded hook entry point. Only the projection crosses onto disk."""

import os
import signal
import sys


def main():
    # A missing dependency, full disk, malformed input or timeout must never
    # return a hook decision, a prompt, or a blocking exit code to Claude.
    try:
        signal.signal(signal.SIGALRM, lambda *_: os._exit(0))
        signal.setitimer(signal.ITIMER_REAL, 0.075)
        sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
        from projection import EVENTS, identifier
        from storage import capture, decode

        payload = sys.stdin.buffer.read(1024 * 1024 + 1)
        if len(payload) > 1024 * 1024:
            return
        event = decode(payload)
        if not isinstance(event, dict) or event.get("hook_event_name") not in EVENTS:
            return
        parent = identifier(event.get("session_id"))
        if parent is None:
            return
        base = os.environ.get("CTX_ASR_DIR")
        if not base:
            data = os.environ.get("CLAUDE_PLUGIN_DATA")
            if not data:
                return
            base = os.path.join(data, "asr")
        if not os.path.isabs(base):
            return
        child = identifier(event.get("agent_id"))
        lifecycle = event["hook_event_name"] in {"SubagentStart", "SubagentStop"}
        # Lifecycle callbacks announce a child in the parent. Ordinary hooks
        # executing inside the child belong in that child's own stream.
        capture(base, parent if lifecycle else child or parent, event, parent)
    except BaseException:
        pass
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)


if __name__ == "__main__":
    main()
