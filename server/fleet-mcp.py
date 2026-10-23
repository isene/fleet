#!/usr/bin/env python3
"""fleet-mcp — remote MCP connector bridging the Claude phone app to fleet.

A minimal Streamable-HTTP MCP server, stdlib only (python 3.8). It runs
on the relay host (bound to 127.0.0.1, nginx terminates TLS and gates a
secret URL path) and exposes two tools:

  message_session(tag, text)  write a message into the synced bus folder
  check_messages()            read and consume messages addressed to the
                              phone

A plain GET of the same address with "?waiting" answers with the number
of messages that wait for the phone, and consumes nothing. A home-screen
widget on the phone can show a mark from it.

A GET with "?done=SINCE&wait=SECS" answers with the turns the sessions
finished after SINCE (a unix time): {"events": [{"tag", "t", "kind",
"eta", "line"}]}. The laptop's fleet hook writes one done-<tag>.json per
session into the relay. With none newer, the answer waits up to SECS
(80 at most, under nginx's 90 s proxy_read_timeout) for one to arrive,
so the phone's fleet app hears of a finished turn within seconds.
With "&phone=N", N being the number of messages the phone knows wait
for it, a different number ends the wait too, and the answer adds
"waiting" (that number now) and "line" (the newest message's first
line), so the phone's notice comes and goes with the messages.

A GET with "?sessions=STAMP&wait=SECS" answers with the laptop fleet's
session list, as fleet wrote it to sessions.json: {"stamp", "t",
"sessions": [...]}. STAMP is the file time of the list the phone has
(-1 for none). While the file is unchanged the answer waits up to SECS,
then says {"stamp": STAMP} alone. The phone asks only while its screen
shows the list. Each session in it carries "answer", the start of its
last answer without colours, and a new answer counts as a change.

A GET with "?answer=TAG" answers with that session's last answer in
full, as the laptop's fleet hook wrote it to answer-<tag>.txt: HyperList
with its terminal colours. {"tag", "t", "text"}, or 404 when none is kept.

Transport: ~/fleet-relay/ is a small dedicated Syncthing folder shared
with the laptop (as ~/.fleet/relay), so a written file lands in seconds.
The user's other synced folders stay receive-encrypted on this host;
only bus messages, which pass through this server in plaintext anyway,
and each session's last answer live here unencrypted.
On the laptop fleet wakes the target session for a message from here,
and the fleet-bus hook injects it on that session's next prompt. Replies travel the same road in
reverse: sessions write to the bus folder's phone/, this server serves them.

Security: nothing here executes anything. Messages are plain text files;
tags keep their case and are limited to [A-Za-z0-9_-]; the only reachable
route is behind the secret nginx location. Message text is injected into
Claude sessions as clearly-labelled DATA, never as instructions.
"""
import json
import os
import re
import urllib.parse
import urllib.request
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

BUS = Path.home() / "fleet-relay"
BIND = ("127.0.0.1", 8765)
MAX_TEXT = 4000
DONE_WAIT_MAX = 80
TAG = re.compile(r"[A-Za-z0-9_-]{1,32}")
SGR = re.compile(r"\x1b\[[0-9;]*m")
SNIPPET = 400
PROTOCOL = "2025-03-26"

TOOLS = [
    {
        "name": "message_session",
        "description": (
            "Send a short message to one of the user's Claude Code sessions "
            "on his laptop. fleet wakes that session with it once the "
            "session waits at its prompt; a running turn reads it when the "
            "turn ends. Use the session's tag: system, "
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
    # The tag keeps its case: a session tagged "DI" reads ~/.fleet/bus/DI,
    # and a lowercased "di" went to a folder nobody reads.
    tag = (tag or "").strip().lstrip("#")
    if not TAG.fullmatch(tag):
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
    return ("Delivered to '{}'. It syncs to the laptop in seconds, and "
            "fleet wakes that session with it.".format(tag))


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


def waiting():
    """How many messages wait for the phone. Counts names: no file is
    opened, and nothing is consumed."""
    try:
        return sum(1 for f in (BUS / "phone").iterdir() if f.name.endswith(".msg"))
    except OSError:
        return 0


def done_events(since):
    out = []
    for f in BUS.glob("done-*.json"):
        try:
            e = json.loads(f.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            continue
        if isinstance(e, dict) and isinstance(e.get("t"), int) and e["t"] > since:
            out.append(e)
    return sorted(out, key=lambda e: e["t"])


def newest_line():
    """The first line of the newest message for the phone, for its notice.
    Empty when none waits."""
    try:
        msgs = [f for f in (BUS / "phone").iterdir() if f.name.endswith(".msg")]
        newest = max(msgs, key=lambda f: f.stat().st_mtime_ns)
        for line in newest.read_text(encoding="utf-8").splitlines():
            if line.strip():
                return line.strip()[:200]
    except (OSError, ValueError):
        pass
    return ""


def mtime(path):
    try:
        return path.stat().st_mtime_ns
    except OSError:
        return 0


def done(since, wait, phone=None):
    """The turns finished after `since`, waiting up to `wait` seconds for
    one. Syncthing renames each file into place, which changes the
    folder's time, so a waiting request reads the files only after that
    changed: one stat every two seconds meanwhile, two with `phone`. Then
    a count of the messages for the phone that differs from `phone` ends
    the wait too."""
    end = time.time() + min(max(wait, 0), DONE_WAIT_MAX)
    seen = None
    while True:
        stamp = (mtime(BUS), mtime(BUS / "phone") if phone is not None else 0)
        if stamp != seen:
            seen = stamp
            events = done_events(since)
            if events or (phone is not None and waiting() != phone):
                return events
        if time.time() >= end:
            return []
        time.sleep(2)


def answer_file(tag):
    return BUS / "answer-{}.txt".format(tag)


def list_stamp():
    """The newest of the list and the answers beside it: a change in
    either changes what the phone's list shows."""
    return max([mtime(BUS / "sessions.json")] + [mtime(p) for p in BUS.glob("answer-*.txt")])


def snippet(tag):
    """The start of a session's last answer, without its colours."""
    if not TAG.fullmatch(tag or ""):
        return ""
    try:
        text = answer_file(tag).read_text(encoding="utf-8")
    except OSError:
        return ""
    return SGR.sub("", text)[:SNIPPET].strip()


def sessions(stamp, wait):
    """The laptop's session list, each with the start of its last answer,
    once the list or an answer changed after `stamp`, waiting up to `wait`
    seconds for that. Syncthing renames each file into place, so one stat
    of the folder every two seconds tells when to look again."""
    f = BUS / "sessions.json"
    end = time.time() + min(max(wait, 0), DONE_WAIT_MAX)
    seen = None
    while True:
        folder = mtime(BUS)
        if folder != seen:
            seen = folder
            now = list_stamp()
            if now != stamp:
                try:
                    view = json.loads(f.read_text(encoding="utf-8"))
                except (OSError, ValueError):
                    view = {"t": 0, "sessions": []}
                for one in view.get("sessions", []):
                    one["answer"] = snippet(one.get("tag"))
                view["stamp"] = now
                return view
        if time.time() >= end:
            return {"stamp": stamp}
        time.sleep(2)


def answer(tag):
    """A session's last answer in full, as the laptop shows it, or None."""
    if not TAG.fullmatch(tag or ""):
        return None
    f = answer_file(tag)
    try:
        return {"tag": tag, "t": int(f.stat().st_mtime), "text": f.read_text(encoding="utf-8")}
    except OSError:
        return None


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
        # Any other GET is an MCP client asking for an SSE stream, and
        # there is none: JSON responses only.
        query = self.path.partition("?")[2]
        if query.startswith("done="):
            q = urllib.parse.parse_qs(query)
            try:
                since = int(q["done"][0])
                wait = int(q.get("wait", ["0"])[0])
                phone = int(q["phone"][0]) if "phone" in q else None
            except (KeyError, ValueError):
                self._send(400, None)
                return
            reply = {"events": done(since, wait, phone)}
            if phone is not None:
                reply["waiting"] = waiting()
                reply["line"] = newest_line() if reply["waiting"] else ""
            self._send(200, reply)
            return
        if query.startswith("sessions="):
            q = urllib.parse.parse_qs(query)
            try:
                stamp = int(q["sessions"][0])
                wait = int(q.get("wait", ["0"])[0])
            except (KeyError, ValueError):
                self._send(400, None)
                return
            self._send(200, sessions(stamp, wait))
            return
        if query.startswith("answer="):
            tag = urllib.parse.parse_qs(query).get("answer", [""])[0]
            view = answer(tag)
            self._send(200 if view else 404, view)
            return
        if query != "waiting":
            self._send(405, None)
            return
        data = "{}\n".format(waiting()).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Cache-Control", "no-store")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_DELETE(self):
        self._send(200, None)            # session teardown: nothing to tear


if __name__ == "__main__":
    ThreadingHTTPServer(BIND, Handler).serve_forever()
