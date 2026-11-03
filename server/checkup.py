#!/usr/bin/env python3
"""checkup: check a server against a plain list, and ring the phone on a change.

cron runs it every 5 minutes. Each line of ~/.checkup is one check:

    proc   chat     weechat               a running command line holds this text
    url    site     https://example.org/  Welcome    the page loads and shows this text
    disk   disk     /  90                 the disk is at most this full, in percent
    cert   site     example.org  14       the certificate has this many days left
    fresh  news     ~/news/*.pdf  26      the newest of these files is this many hours old at most

When a check fails, or works again, checkup writes two files to the fleet
relay (~/fleet-relay, or $CHECKUP_OUT): server-<host>.json with every
check, and done-server-<host>.json, an event of kind "alert" that the
phone app rings for. While nothing changes it writes nothing there.

Each run also adds one line to ~/.checkup.log (or $CHECKUP_LOG) for the
graphs of a dashboard: the time, then per check "name=value=why", tab
apart. The value is the page's answer time in ms, the disk's percent,
the certificate's days or the file's hours, and empty for a process.
"""

from __future__ import annotations

import glob
import json
import os
import shutil
import socket
import ssl
import time
import urllib.request
from pathlib import Path

LIST = Path.home() / ".checkup"
OUT = Path(os.environ.get("CHECKUP_OUT", Path.home() / "fleet-relay"))
LOG = Path(os.environ.get("CHECKUP_LOG", Path.home() / ".checkup.log"))
LOG_MAX = 16_000_000  # bytes; past this the older half goes
HOST = socket.gethostname()
WAIT = 20  # seconds a site may take to answer


# Each kind returns what is wrong ("" when it works), and a number to graph.
def proc(*text: str) -> tuple[str, float | None]:
    want = " ".join(text)
    for d in Path("/proc").iterdir():
        if not d.name.isdigit() or d.name == str(os.getpid()):
            continue
        try:
            cmd = (
                (d / "cmdline")
                .read_bytes()
                .replace(b"\0", b" ")
                .decode(errors="replace")
            )
        except OSError:
            continue
        if want in cmd:
            return "", None
    return "not running", None


def url(address: str, *text: str) -> tuple[str, float | None]:
    want = " ".join(text)
    start = time.monotonic()
    try:
        with urllib.request.urlopen(address, timeout=WAIT) as r:
            page = r.read(1_000_000).decode(errors="replace")
    except Exception as e:  # noqa: BLE001  any error is a failed check
        return str(e)[:80], None
    ms = round((time.monotonic() - start) * 1000)
    return ("" if want in page else f"the page lacks '{want}'"), ms


def disk(path: str, most: str) -> tuple[str, float | None]:
    use = shutil.disk_usage(path)
    full = 100 * use.used // use.total
    return ("" if full <= int(most) else f"{full}% full"), full


def cert(host: str, days: str) -> tuple[str, float | None]:
    try:
        ctx = ssl.create_default_context()
        # Nested, not one with: the servers run Python 3.8.
        with socket.create_connection((host, 443), timeout=WAIT) as s:  # noqa: SIM117
            with ctx.wrap_socket(s, server_hostname=host) as t:
                end = ssl.cert_time_to_seconds(t.getpeercert()["notAfter"])
    except Exception as e:  # noqa: BLE001  any error is a failed check
        return str(e)[:80], None
    left = int((end - time.time()) // 86400)
    return ("" if left >= int(days) else f"{left} days left"), left


def fresh(pattern: str, hours: str) -> tuple[str, float | None]:
    files = glob.glob(os.path.expanduser(pattern))
    if not files:
        return "no such file", None
    age = (time.time() - max(os.path.getmtime(f) for f in files)) / 3600
    return ("" if age <= float(hours) else f"{int(age)} hours old"), round(age, 1)


KINDS = {"proc": proc, "url": url, "disk": disk, "cert": cert, "fresh": fresh}


def run(lines: list) -> tuple:
    """Each check's name, what is wrong with it ("" when it works), and its number."""
    why, value = {}, {}
    for line in lines:
        f = line.split()
        if len(f) < 3 or f[0] not in KINDS:
            continue
        if f[1] in why:
            why[f[1]] = "bad line: the name is used twice"
            continue
        try:
            why[f[1]], value[f[1]] = KINDS[f[0]](*f[2:])
        except Exception as e:  # noqa: BLE001  a broken line fails, so it gets seen
            why[f[1]], value[f[1]] = f"bad line: {e}"[:80], None
    return why, value


def write(path: Path, data: dict) -> None:
    tmp = path.with_name("." + path.name)
    tmp.write_text(json.dumps(data), encoding="utf-8")
    tmp.rename(path)  # the relay reads whole files only


def log(t: int, why: dict, value: dict) -> None:
    fields = [str(t)]
    for n, w in why.items():
        v = value.get(n)
        fields.append(f"{n}={'' if v is None else v}={' '.join(w.split())}")
    with LOG.open("a", encoding="utf-8") as f:
        f.write("\t".join(fields) + "\n")
    if LOG.stat().st_size > LOG_MAX:
        lines = LOG.read_text(encoding="utf-8").splitlines(keepends=True)
        tmp = LOG.with_name("." + LOG.name)
        tmp.write_text("".join(lines[len(lines) // 2 :]), encoding="utf-8")
        tmp.rename(LOG)


def main() -> None:
    try:
        lines = [
            ln
            for ln in LIST.read_text(encoding="utf-8").splitlines()
            if not ln.startswith("#")
        ]
    except OSError:
        return
    now, value = run(lines)
    t = int(time.time())
    log(t, now, value)
    view = OUT / f"server-{HOST}.json"
    try:
        before = json.loads(view.read_text(encoding="utf-8"))["checks"]
    except (OSError, ValueError, KeyError):
        before = {}
    failing = {n for n, why in now.items() if why}
    failed = {n for n, why in before.items() if why}
    if failing == failed and now.keys() == before.keys():
        return  # a new error text alone is no news
    write(view, {"t": t, "host": HOST, "checks": now})
    news = [f"{n}: {now[n]}" for n in sorted(failing - failed)]
    news += [f"{n} works again" for n in sorted(failed - failing) if n in now]
    if news:
        write(
            OUT / f"done-server-{HOST}.json",
            {
                "tag": HOST,
                "t": t,
                "kind": "alert",
                "line": f"{HOST}: " + "; ".join(news),
            },
        )


if __name__ == "__main__":
    main()
