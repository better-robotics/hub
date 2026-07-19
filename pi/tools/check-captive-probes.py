#!/usr/bin/env python3
"""Guard hubd's captive genuine-success arms against drift from the spec.

hubd (Pi hub) and wifi_portal.c (ESP32 hub) implement one captive design —
CONTRACT.md § Captive onboarding, the single spec both reconcile to. The drift
that bit on 2026-07-19: hubd had no `("GET", "/success.txt") if acked` arm the
ESP had, so a released Firefox client read as still-captive. It is invisible to
review — the other arms are all present and correct.

This is the Pi twin of robot/tools/check-captive-probes.py. Rust needs no
"registration" check (a match arm is reachable by existing — there is no dead
unregistered-handler class here as there is on ESP-IDF httpd), so this only
guards SPEC DRIFT: every canonical probe path must have a greeted (`if acked`)
arm, and every genuine-success body must be served byte-for-byte.

CONTRACT.md is the sole source of the path set; the exact body BYTES live here
(a markdown cell can't unambiguously encode "success\\n"), and the doc is
asserted to carry each one — so doc, this table, and hubd.rs all agree.

Run:  pi/tools/check-captive-probes.py
"""
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
HUBD = os.path.join(HERE, "..", "src", "bin", "hubd.rs")
CONTRACT = os.path.join(HERE, "..", "..", "CONTRACT.md")

# Genuine-success bytes each probe path returns to a GREETED client. None = a
# bare status, no body (Android's 204). Keys are the canonical probe set,
# asserted equal to CONTRACT.md's table.
EXPECTED = {
    "/generate_204":        None,
    "/hotspot-detect.html": "<HTML><HEAD><TITLE>Success</TITLE></HEAD><BODY>Success</BODY></HTML>",
    "/connecttest.txt":     "Microsoft Connect Test",
    "/ncsi.txt":            "Microsoft NCSI",
    "/success.txt":         "success\\n",
}


def die(msg):
    print(f"captive-probes: FAIL [drift] {msg}", file=sys.stderr)
    sys.exit(1)


def read(path, what):
    try:
        with open(path, encoding="utf-8") as f:
            return f.read()
    except OSError as e:
        print(f"captive-probes: cannot read {what}: {e}", file=sys.stderr)
        sys.exit(2)


def contract_paths(text):
    lines = text.splitlines()
    try:
        start = next(i for i, l in enumerate(lines)
                     if l.startswith("## Captive onboarding"))
    except StopIteration:
        die("CONTRACT.md has no '## Captive onboarding' section")
    paths, in_table = set(), False
    for l in lines[start:]:
        if l.startswith("| Probe path"):
            in_table = True
            continue
        if in_table:
            if not l.startswith("|"):
                break
            if set(l) <= set("| -"):
                continue
            m = re.match(r"`(/[^`]+)`", l.split("|")[1].strip())
            if m:
                paths.add(m.group(1))
    return paths


def acked_paths(text):
    """Probe paths served by a greeted (`if acked`) arm. An acked arm is a run of
    one or more ("GET", "/path") patterns immediately followed by `if acked =>`;
    several paths can share one arm (the Apple line lists two)."""
    served = set()
    for m in re.finditer(
            r'((?:\(\s*"GET"\s*,\s*"[^"]+"\s*\)\s*\|?\s*)+)\bif acked\s*=>', text):
        served.update(re.findall(r'"(/[^"]+)"', m.group(1)))
    return served


def main():
    contract = read(CONTRACT, "CONTRACT.md")
    hubd = read(HUBD, "pi/src/bin/hubd.rs")

    canon = set(EXPECTED)
    doc = contract_paths(contract)
    if canon != doc:
        die(f"CONTRACT.md path set {sorted(doc)} != this check's {sorted(canon)} "
            f"— update whichever is stale (they are the same spec)")
    for path, want in EXPECTED.items():
        if want and f"`{want}`" not in contract:
            die(f"CONTRACT.md is missing the genuine-success body for {path}: {want!r}")

    served = acked_paths(hubd)
    missing = canon - served
    if missing:
        die(f"hubd.rs has no greeted (`if acked`) arm for {sorted(missing)} "
            f"— a released client on those OSes reads as still-captive")

    for path, want in EXPECTED.items():
        if want and f'"{want}"' not in hubd:
            die(f"hubd.rs does not serve the genuine-success body for {path}: {want!r}")

    print(f"captive-probes: OK — {len(canon)} probe paths served greeted and "
          f"byte-matched to CONTRACT.md § Captive onboarding")


if __name__ == "__main__":
    main()
