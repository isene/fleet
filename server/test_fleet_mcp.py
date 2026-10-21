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
