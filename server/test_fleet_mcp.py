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
