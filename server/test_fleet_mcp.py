"""Tests for fleet-mcp. Run: pytest server/"""
import importlib.util
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


def test_the_session_list_comes_when_it_changed(tmp_path, monkeypatch):
    monkeypatch.setattr(fm, "BUS", tmp_path)
    # No list yet: the first ask gets an empty one at once.
    first = fm.sessions(-1, 0)
    assert (first["stamp"], first["sessions"]) == (0, [])
    (tmp_path / "sessions.json").write_text('{"t": 5, "sessions": [{"tag": "ci", "state": "YOURS"}]}')
    view = fm.sessions(first["stamp"], 0)
    assert view["sessions"][0]["tag"] == "ci" and view["stamp"] > 0
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
