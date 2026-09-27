#!/usr/bin/env python3
"""Isolated native child fixture with explicit launch and owner-pipe shutdown barriers."""
import http.server
import json
import pathlib
import sys
import threading
import time

root = pathlib.Path.cwd()
with (root / "starts").open("a") as starts:
    starts.write("started\n")

def wait_for(name):
    while not (root / name).exists():
        time.sleep(0.005)

class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = json.dumps(info).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args):
        pass

server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
info = {
    "schema_version": 15,
    "protocol_version": 2,
    "server_version": "1.0.0",
    "workspace_id": (root / "identity").read_text() if (root / "identity").exists() else root.name,
    "bind_addr": f"127.0.0.1:{server.server_port}",
    "authentication": "bearer",
    "shutdown_on_stdin_close": True,
    "capabilities": dict.fromkeys([
        "session_catalog", "durable_session_reopen", "bounded_transcript_replay",
        "canonical_conversation_display", "image_attachments", "typed_tool_artifact_retrieval",
        "conversation_recovery", "durable_event_replay", "live_events", "approval",
        "durable_user_input", "cancellation", "terminal_task_cancel", "task_pause",
        "verification", "task_integration", "intent_stack", "run_context",
        "agent_activity", "support_diagnostics", "provider_connections", "provider_setup",
    ], True),
}
threading.Thread(target=server.serve_forever, daemon=True).start()
wait_for("allow-start")
print(json.dumps(info), flush=True)
sys.stdin.buffer.read()
(root / "stopping").touch()
wait_for("allow-stop")
(root / "stopped").touch()
