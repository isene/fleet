#!/usr/bin/env python3
"""fleet-mcp — remote MCP connector bridging the Claude phone app to fleet.

A minimal Streamable-HTTP MCP server, stdlib only (python 3.8). It runs
on the relay host (bound to 127.0.0.1, nginx terminates TLS and gates a
secret URL path) and exposes two tools:

  message_session(tag, text)  write a message into the synced bus folder
  check_messages()            read and consume messages addressed to the
                              phone

Transport: ~/fleet-relay/ is a small dedicated Syncthing folder shared
with the laptop (as ~/.fleet/relay), so a written file lands in seconds.
The user's other synced folders stay receive-encrypted on this host;
only bus messages, which pass through this server in plaintext anyway,
live here unencrypted.
On the laptop the fleet-bus UserPromptSubmit hook injects it into the
target session on its next user prompt. Replies travel the same road in
reverse: sessions write to fleet-bus/phone/, this server serves them.

Security: nothing here executes anything. Messages are plain text files;
tags are sanitised to [a-z0-9_-]; the only reachable route is behind the
secret nginx location. Message text is injected into Claude sessions as
clearly-labelled DATA, never as instructions.
"""
import json
import os
import re
import urllib.request
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

BUS = Path.home() / "fleet-relay"
BIND = ("127.0.0.1", 8765)
MAX_TEXT = 4000
PROTOCOL = "2025-03-26"

TOOLS = [
    {
        "name": "message_session",
        "description": (
            "Send a short message to one of the user's Claude Code sessions "
            "on his laptop. The message is delivered as context on that "
            "session's next user prompt (minutes to hours; it never "
            "interrupts a running turn). Use the session's tag: system, "
            "asm, pf, pip, nomad, freewill, trade and similar. Use "
            "tag 'system' when unsure; that session routes things onward."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "tag": {
                    "type": "string",
                    "description": "Target session tag, e.g. 'system' or 'asm'",
                },
                "text": {
                    "type": "string",
                    "description": "The message, plain text, max 4000 chars",
                },
            },
            "required": ["tag", "text"],
        },
        "annotations": {"readOnlyHint": False, "openWorldHint": False},
    },
    {
        "name": "check_messages",
        "description": (
            "Fetch messages the laptop sessions have sent to the phone. "
            "Returns and consumes everything waiting; call it when the "
            "user asks whether there is anything from the laptop."
        ),
        "inputSchema": {"type": "object", "properties": {}},
        "annotations": {"readOnlyHint": False, "openWorldHint": False},
    },
]


def message_session(tag, text):
    tag = (tag or "").strip().lower()
    if not re.fullmatch(r"[a-z0-9_-]{1,32}", tag):
        return ("Invalid tag. Use a short session tag like 'system' "
                "or 'asm' (letters, digits, - and _).")
    text = (text or "").strip()
    if not text:
        return "Empty message; nothing sent."
    if len(text) > MAX_TEXT:
        return "Message too long (max 4000 characters); nothing sent."
    d = BUS / tag
    d.mkdir(parents=True, exist_ok=True)
    path = d / "{}-phone.msg".format(int(time.time()))
    path.write_text(text + "\n\n/phone\n", encoding="utf-8")
    poke_syncthing()
    return ("Delivered to '{}'. It syncs to the laptop in seconds and "
            "reaches that session on its next user prompt.".format(tag))


def poke_syncthing():
    """Tell the local Syncthing to index the relay now.

    The folder watcher missed files written here (messages sat until the
    hourly rescan, which is not "in seconds"), so the write is followed
    by an explicit scan of that one folder. Best effort: a failure only
    means the message waits for the next rescan, as before.
    """
    try:
        cfg = Path.home() / ".local/state/syncthing/config.xml"
        m = re.search(r"<apikey>([^<]+)</apikey>", cfg.read_text(encoding="utf-8"))
        if not m:
            return
        req = urllib.request.Request(
            "http://127.0.0.1:8384/rest/db/scan?folder=fleet-relay",
            method="POST", headers={"X-API-Key": m.group(1)})
        urllib.request.urlopen(req, timeout=5).read()
    except Exception:
        pass


def check_messages():
    d = BUS / "phone"
    if not d.is_dir():
        return "No messages waiting."
    out = []
    for f in sorted(d.iterdir()):
        if not f.name.endswith(".msg"):
            continue
        try:
            out.append(f.read_text(encoding="utf-8").strip())
            f.unlink()
        except OSError:
            continue
    if not out:
        return "No messages waiting."
    return "Messages from the laptop sessions:\n\n" + "\n---\n".join(out)


def rpc_result(rid, result):
    return {"jsonrpc": "2.0", "id": rid, "result": result}


def rpc_error(rid, code, msg):
    return {"jsonrpc": "2.0", "id": rid, "error": {"code": code, "message": msg}}


def handle(msg):
    """One JSON-RPC message in, one reply dict out (None = no reply)."""
    method = msg.get("method")
    rid = msg.get("id")
    if method == "initialize":
        proto = (msg.get("params") or {}).get("protocolVersion") or PROTOCOL
        return rpc_result(rid, {
            "protocolVersion": proto,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "fleet-bus", "version": "0.1.0"},
        })
    if method == "notifications/initialized":
        return None
    if method == "ping":
        return rpc_result(rid, {})
    if method == "tools/list":
        return rpc_result(rid, {"tools": TOOLS})
    if method == "tools/call":
        params = msg.get("params") or {}
        name = params.get("name")
        args = params.get("arguments") or {}
        if name == "message_session":
            text = message_session(args.get("tag"), args.get("text"))
        elif name == "check_messages":
            text = check_messages()
        else:
            return rpc_error(rid, -32602, "Unknown tool: {}".format(name))
        return rpc_result(rid, {
            "content": [{"type": "text", "text": text}],
            "isError": False,
        })
    if rid is None:
        return None                      # unknown notification: ignore
    return rpc_error(rid, -32601, "Method not found: {}".format(method))


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        pass                             # quiet; nginx has the access log

    def _send(self, code, body):
        data = json.dumps(body).encode() if body is not None else b""
        self.send_response(code)
        if data:
            self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        if data:
            self.wfile.write(data)

    def do_POST(self):
        try:
            n = int(self.headers.get("Content-Length") or 0)
            msg = json.loads(self.rfile.read(n).decode("utf-8"))
        except (ValueError, OSError):
            self._send(400, rpc_error(None, -32700, "Parse error"))
            return
        try:
            reply = handle(msg)
        except Exception as e:  # noqa: BLE001 — a tool bug must not kill the server
            reply = rpc_error(msg.get("id"), -32603, "Internal error: {}".format(e))
        if reply is None:
            self._send(202, None)
        else:
            self._send(200, reply)

    def do_GET(self):
        self._send(405, None)            # no SSE stream; JSON responses only

    def do_DELETE(self):
        self._send(200, None)            # session teardown: nothing to tear


if __name__ == "__main__":
    ThreadingHTTPServer(BIND, Handler).serve_forever()
