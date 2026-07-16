#!/usr/bin/env python3
"""Run a command on the Pi hub over its USB serial console (autologin shell):
`pi-serial.py 'systemctl status hubd' [wait-seconds]`. The workstation-side
escape hatch when the network path to the Pi is blocked (e.g. client-isolated
Wi-Fi). Needs `pip install pyserial`.

The console's /dev/cu.usbmodem* name renumbers on any USB re-enumeration, so
by default every candidate port is probed for a shell that answers `hostname`
with "hub"; set PI_SERIAL_PORT to skip the probe.

Staging files through this console: multi-line heredocs get mangled — send
`echo <base64> | base64 -d > /path` instead.

EXIT STATUS IS THE REMOTE COMMAND'S. This used to print `[exit 6]` as decorative
text and exit 0 regardless, which made every caller's error handling a no-op:
deploy-hubd.sh runs under `set -euo pipefail` and still sailed past a remote
curl that had failed to resolve github.com, then announced the new binary was
live while the Pi kept serving the old one (scar 2026-07-16 — the uplink radio
had dropped, and nothing anywhere said so). A wrapper that reports a command's
failure only as a string it hopes someone reads is not reporting it.
124 = the command never finished inside `wait` (GNU timeout's convention): no
marker came back, so its real status is unknown and unknown is not success."""
import glob, os, re, serial, sys, time

TIMEOUT_RC = 124

MARK = 'XCMDONEX'
# A done line is MARK + exit code at end of line. The shell echoes the command
# itself (which contains "echo XCMDONEX$?") before running it, so every check
# also requires the line not to contain the command text.
DONE = re.compile(MARK + r'(\d+)\s*$')

def _exec(port, cmd, wait):
    p = serial.Serial(port, 115200, timeout=1)
    p.write(b'\n')          # wake the shell / trigger autologin
    time.sleep(1.5)
    p.reset_input_buffer()
    p.write((cmd + f' ; echo {MARK}$?\n').encode())
    end = time.time() + wait
    buf = b''
    lines = []
    while time.time() < end:
        buf += p.read(4096)
        lines = buf.decode('utf-8', 'replace').splitlines()
        if any(DONE.search(l) and cmd not in l for l in lines):
            break
    p.close()
    out = []
    # None until the marker proves otherwise: a command whose completion never
    # came back has an UNKNOWN status, which must not read as 0.
    code = TIMEOUT_RC
    for l in lines:
        if cmd in l or not l.strip():
            continue
        m = DONE.search(l)
        if m:
            code = int(m.group(1))
            out.append(f'[exit {m.group(1)}]')
            break
        out.append(l)
    return '\n'.join(out), code

def find_pi():
    for port in sorted(glob.glob('/dev/cu.usbmodem*')):
        try:
            # any-line match: the shell prefixes control noise (e.g. the
            # bracketed-paste toggle `[?2004l`) before the real output
            if any(l.strip() == 'hub' for l in _exec(port, 'hostname', 6)[0].splitlines()):
                return port
        except Exception:
            continue
    sys.exit('no Pi console found (set PI_SERIAL_PORT)')

def run(cmd, wait=8):
    """(output, exit_code). Callers that care must check the code — or use the
    CLI below, which turns it into this process's status so `set -e` can."""
    return _exec(os.environ.get('PI_SERIAL_PORT') or find_pi(), cmd, wait)

if __name__ == '__main__':
    wait = float(sys.argv[2]) if len(sys.argv) > 2 else 8
    out, code = run(sys.argv[1], wait)
    print(out)
    if code == TIMEOUT_RC:
        print(f'pi-serial: no completion marker in {wait:g}s — remote status unknown',
              file=sys.stderr)
    sys.exit(code)
