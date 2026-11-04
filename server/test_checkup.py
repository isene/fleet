"""Tests for checkup. Run: pytest server/"""

import importlib.util
import json
import os
import subprocess
import time
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "checkup", Path(__file__).with_name("checkup.py")
)
cu = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cu)


MARK = "checkup-test-marker"


def a_process():
    # checkup leaves out its own process, so the test starts one to find.
    return subprocess.Popen(["sh", "-c", f"sleep 30 # {MARK}"])


def test_each_kind_says_what_is_wrong(tmp_path):
    p = a_process()
    log = tmp_path / "ok"
    log.write_text("")
    old = tmp_path / "old"
    old.write_text("")
    os.utime(old, (time.time() - 30 * 3600,) * 2)
    got, value = cu.run(
        [
            "disk root / 100",
            "disk tight / 0",
            f"fresh new {log} 26",
            f"fresh stale {old} 26",
            f"fresh gone {tmp_path}/none-* 26",
            f"proc me {MARK}",
            "proc ghost no-such-program-runs-here",
            "url nowhere http://127.0.0.1:9/ x",
            "disk broken / many",
            "nonsense line here",
            "disk root / 50",
        ]
    )
    assert got["new"] == "" and got["me"] == ""
    assert got["root"] == "bad line: the name is used twice"
    assert got["tight"].endswith("% full")
    assert got["stale"] == "30 hours old"
    assert got["gone"] == "no such file"
    assert got["ghost"] == "not running"
    assert "refused" in got["nowhere"]
    assert got["broken"].startswith("bad line")
    assert "nonsense" not in got and "line" not in got
    assert 0 <= value["root"] <= 100 and value["stale"] == 30.0
    assert value["me"] is None and value["nowhere"] is None
    p.kill()


def test_only_a_change_rings(tmp_path, monkeypatch):
    rc = tmp_path / ".checkup"
    monkeypatch.setattr(cu, "LIST", rc)
    monkeypatch.setattr(cu, "OUT", tmp_path)
    monkeypatch.setattr(cu, "LOG", tmp_path / "log")
    monkeypatch.setattr(cu, "HOST", "box")
    event = tmp_path / "done-server-box.json"
    view = tmp_path / "server-box.json"
    rc.write_text("# the list\ndisk root / 100\n")
    cu.main()
    assert json.loads(view.read_text())["checks"] == {"root": ""}
    assert not event.exists(), "all is well: nothing rings"
    rc.write_text("disk root / 100\nproc ghost no-such-program-runs-here\n")
    cu.main()
    e = json.loads(event.read_text())
    assert (e["tag"], e["kind"], e["line"]) == (
        "box",
        "alert",
        "box: ghost: not running",
    )
    event.unlink()
    cu.main()
    assert not event.exists(), "still down: no second ring"
    p = a_process()
    rc.write_text(f"disk root / 100\nproc ghost {MARK}\n")
    cu.main()
    p.kill()
    assert json.loads(event.read_text())["line"] == "box: ghost works again"


def test_no_name_resolves_so_nothing_rings(tmp_path, monkeypatch):
    rc = tmp_path / ".checkup"
    rc.write_text("url site https://example.org/ x\ncert site-tls example.org 14\n")
    monkeypatch.setattr(cu, "LIST", rc)
    monkeypatch.setattr(cu, "OUT", tmp_path)
    monkeypatch.setattr(cu, "LOG", tmp_path / "log")
    monkeypatch.setattr(cu, "HOST", "box")
    why = {
        "site": "<urlopen error [Errno -3] Temporary failure in name resolution>",
        "site-tls": "[Errno -5] No address associated with hostname",
    }
    monkeypatch.setattr(cu, "run", lambda lines: (why, {}))
    cu.main()
    assert list(tmp_path.iterdir()) == [rc], "a dead network writes nothing"
    why["site-tls"] = "3 days left"
    cu.main()
    assert "site-tls: 3 days left" in (tmp_path / "done-server-box.json").read_text()


def test_each_run_adds_a_line_and_the_log_stays_small(tmp_path, monkeypatch):
    log = tmp_path / "log"
    monkeypatch.setattr(cu, "LOG", log)
    cu.log(100, {"disk": "", "web": "timed\tout\nnow"}, {"disk": 41, "web": None})
    assert log.read_text() == "100\tdisk=41=\tweb==timed out now\n"
    monkeypatch.setattr(cu, "LOG_MAX", 100)
    for t in range(101, 111):
        cu.log(t, {"disk": ""}, {"disk": 41})
    lines = log.read_text().splitlines()
    assert len(lines) < 11 and lines[-1] == "110\tdisk=41="
