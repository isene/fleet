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
phone app rings for. While nothing changes it writes nothing.
"""
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
HOST = socket.gethostname()
WAIT = 20  # seconds a site may take to answer


def proc(*text: str) -> str:
    want = " ".join(text)
    for d in Path("/proc").iterdir():
        if not d.name.isdigit() or d.name == str(os.getpid()):
            continue
        try:
            cmd = (d / "cmdline").read_bytes().replace(b"\0", b" ").decode(errors="replace")
        except OSError:
            continue
        if want in cmd:
            return ""
    return "not running"


def url(address: str, *text: str) -> str:
    want = " ".join(text)
    try:
        with urllib.request.urlopen(address, timeout=WAIT) as r:
            page = r.read(1_000_000).decode(errors="replace")
    except Exception as e:
        return str(e)[:80]
    return "" if want in page else f"the page lacks '{want}'"


def disk(path: str, most: str) -> str:
    use = shutil.disk_usage(path)
    full = 100 * use.used // use.total
    return "" if full <= int(most) else f"{full}% full"


def cert(host: str, days: str) -> str:
    try:
        with socket.create_connection((host, 443), timeout=WAIT) as s:
            with ssl.create_default_context().wrap_socket(s, server_hostname=host) as t:
                end = ssl.cert_time_to_seconds(t.getpeercert()["notAfter"])
    except Exception as e:
        return str(e)[:80]
    left = int((end - time.time()) // 86400)
    return "" if left >= int(days) else f"{left} days left"


def fresh(pattern: str, hours: str) -> str:
    files = glob.glob(os.path.expanduser(pattern))
    if not files:
        return "no such file"
    age = time.time() - max(os.path.getmtime(f) for f in files)
    return "" if age <= float(hours) * 3600 else f"{int(age // 3600)} hours old"


KINDS = {"proc": proc, "url": url, "disk": disk, "cert": cert, "fresh": fresh}


def run(lines: list) -> dict:
    """Each check's name, and what is wrong with it ("" when it works)."""
    out = {}
    for line in lines:
        f = line.split()
        if len(f) < 3 or f[0] not in KINDS:
            continue
        try:
            out[f[1]] = KINDS[f[0]](*f[2:])
        except Exception as e:  # a broken line fails, so it gets seen
            out[f[1]] = f"bad line: {e}"[:80]
    return out


def write(path: Path, data: dict) -> None:
    tmp = path.with_name("." + path.name)
    tmp.write_text(json.dumps(data), encoding="utf-8")
    tmp.rename(path)  # the relay reads whole files only


def main() -> None:
    try:
        lines = [ln for ln in LIST.read_text(encoding="utf-8").splitlines() if not ln.startswith("#")]
    except OSError:
        return
    now = run(lines)
    view = OUT / f"server-{HOST}.json"
    try:
        before = json.loads(view.read_text(encoding="utf-8"))["checks"]
    except (OSError, ValueError, KeyError):
        before = {}
    failing = {n for n, why in now.items() if why}
    failed = {n for n, why in before.items() if why}
    if failing == failed and now.keys() == before.keys():
        return  # a new error text alone is no news
    t = int(time.time())
    write(view, {"t": t, "host": HOST, "checks": now})
    news = [f"{n}: {now[n]}" for n in sorted(failing - failed)]
    news += [f"{n} works again" for n in sorted(failed - failing) if n in now]
    if news:
        write(OUT / f"done-server-{HOST}.json",
              {"tag": HOST, "t": t, "kind": "alert", "line": f"{HOST}: " + "; ".join(news)})


if __name__ == "__main__":
    main()
