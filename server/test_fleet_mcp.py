"""Tests for fleet-mcp. Run: pytest server/"""
import importlib.util
import subprocess
from pathlib import Path

spec = importlib.util.spec_from_file_location("fleet_mcp", Path(__file__).with_name("fleet-mcp.py"))
fm = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fm)


def test_a_tag_keeps_its_case(tmp_path, monkeypatch):
    monkeypatch.setattr(fm, "BUS", tmp_path)
    monkeypatch.setattr(fm, "poke_syncthing", lambda: None)
    assert fm.message_session("#DI", "hello").startswith("Delivered to 'DI'")
    assert [f.parent.name for f in tmp_path.glob("*/*.msg")] == ["DI"]
    assert fm.message_session("D I", "hello").startswith("Invalid tag")


def test_a_drop_is_a_file_no_session_reads(tmp_path, monkeypatch):
    monkeypatch.setattr(fm, "BUS", tmp_path)
    monkeypatch.setattr(fm, "poke_syncthing", lambda: None)
    assert fm.drop_item("fw-kimi", 3, " Restart kastrup? ").startswith("Dropped 3 from 'fw-kimi'")
    assert (tmp_path / ".drop/fw-kimi.3").read_text() == "Restart kastrup?"
    assert fm.drop_item("fw kimi", 3, "x").startswith("Invalid tag")
    assert fm.drop_item("ci", "three", "x").startswith("Invalid number")
    assert fm.drop_item("ci", 0, "x").startswith("Invalid number")
    # The laptop's Stop hook looks for messages as the shell sees relay/*/*.
    seen = subprocess.run(["sh", "-c", 'for f in "$0"/*/*; do [ -f "$f" ] && echo "$f"; done', str(tmp_path)],
                          capture_output=True, text=True).stdout
    assert seen == "", "a drop is no message"


def test_the_session_list_comes_when_it_changed(tmp_path, monkeypatch):
    monkeypatch.setattr(fm, "BUS", tmp_path)
    # No list yet: the first ask gets an empty one at once.
    first = fm.sessions(-1, 0)
    assert (first["stamp"], first["sessions"]) == (0, [])
    (tmp_path / "sessions.json").write_text('{"t": 5, "sessions": [{"tag": "ci", "state": "YOURS"}], "usage": {"week": 6}}')
    view = fm.sessions(first["stamp"], 0)
    assert view["sessions"][0]["tag"] == "ci" and view["stamp"] > 0
    assert view["usage"] == {"week": 6}
    # Unchanged: the stamp alone, after the wait.
    assert fm.sessions(view["stamp"], 0) == {"stamp": view["stamp"]}


def test_a_message_for_the_phone_ends_the_wait(tmp_path, monkeypatch):
    import os, time
    monkeypatch.setattr(fm, "BUS", tmp_path)
    box = tmp_path / "phone"
    box.mkdir()
    (box / "1-ci.msg").write_text("Old one.\n")
    (box / "2-DI.msg").write_text("\nTests pass.\n\n-- DI\n")
    os.utime(box / "1-ci.msg", (1, 1))
    # The phone knows of none: two wait, so the 5 s wait ends at once.
    start = time.time()
    assert fm.done(0, 5, 0) == [] and time.time() - start < 1
    assert (fm.waiting(), fm.newest_line()) == (2, "Tests pass.")
    # Read in the Claude app: the count is the one the phone knows.
    for f in box.iterdir():
        f.unlink()
    assert (fm.waiting(), fm.newest_line()) == (0, "")


def test_a_new_answer_changes_the_list_and_comes_in_full(tmp_path, monkeypatch):
    monkeypatch.setattr(fm, "BUS", tmp_path)
    (tmp_path / "sessions.json").write_text('{"t": 5, "sessions": [{"tag": "ci"}, {"tag": "../x"}]}')
    view = fm.sessions(-1, 0)
    assert [s["answer"] for s in view["sessions"]] == ["", ""]
    (tmp_path / "answer-ci.txt").write_text("Done\x1b[38;5;46m;\x1b[39m two wait.\n    child\n")
    again = fm.sessions(view["stamp"], 0)
    assert again["stamp"] != view["stamp"]
    assert again["sessions"][0]["answer"] == "Done; two wait.\n    child"
    full = fm.answer("ci")
    assert full["text"].startswith("Done\x1b[38;5;46m;") and full["t"] > 0
    assert fm.answer("../x") is None and fm.answer("DI") is None


def test_the_answer_comes_over_http(tmp_path, monkeypatch):
    # 0.3.62 shipped with do_GET's own `answer` variable hiding the
    # answer() function: every ?answer= request got an empty reply.
    import json, threading, urllib.request, urllib.error
    monkeypatch.setattr(fm, "BUS", tmp_path)
    (tmp_path / "answer-ci.txt").write_text("Done.\n")
    server = fm.ThreadingHTTPServer(("127.0.0.1", 0), fm.Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    base = "http://127.0.0.1:{}/".format(server.server_address[1])
    try:
        got = json.load(urllib.request.urlopen(base + "?answer=ci"))
        assert (got["tag"], got["text"]) == ("ci", "Done.\n")
        try:
            urllib.request.urlopen(base + "?answer=DI")
            assert False, "no answer kept: 404"
        except urllib.error.HTTPError as e:
            assert e.code == 404
        assert json.load(urllib.request.urlopen(base + "?done=0&wait=0&phone=0"))["waiting"] == 0
    finally:
        server.shutdown()
