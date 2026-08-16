#!/usr/bin/env python3
"""Fake Yamaha R-series device — an SCP bring-up / capture harness.

Why this exists
---------------
`docs/yamaha-scp-r-remote.md` established, from static analysis of R Remote
6.0.0, that Yamaha's console-less controller drives Rio/RSio/RMio head amps over
**SCP on plain TCP** — not the `MBC` ConMon block a console uses. It recovered
the command set, the full parameter schema and the 359-command association
sequence. Two things it could NOT recover statically:

  1. **the TCP port** — an integer constant, not a string; and
  2. **the wire framing** of GET/SET — no capture was taken.

This tool gets both without a Rio, by making the *real* R Remote talk to it.

The three-stage plan
--------------------
**Stage 1 — the port, for free.** The fake does not need to work. Advertise
something R-series-shaped; when R Remote tries to connect, *the SYN alone reveals
the port*. Run with `--reply-mode none` and watch. If the port isn't in
`CANDIDATE_PORTS` we never see the connection, so also run the `tcpdump` line
this tool prints at startup — that catches SYNs to ports we never bound.

**Stage 2 — the framing.** With the port known (`--ports N`), accept and log.
Every byte R Remote sends is hex-dumped and, since SCP is ASCII, decoded as text
lines. Expect `DEVINFO` / `SCPMODE` negotiation first.

**Stage 3 — iterate into association.** Answer what it asks, watch what it asks
next, repeat. `--reply-mode guess` uses the table below; `--replies FILE` layers
a JSON file on top so replies can be edited between attempts without touching
this file. Success looks like R Remote proceeding into the association sequence —
33 parameter subscriptions expanded to 359 commands for a Rio — which confirms
the sequence and the framing together.

Run
---
    python3 tools/rio-fake/fake_rio.py --iface en0                 # stage 1
    python3 tools/rio-fake/fake_rio.py --iface en0 --ports 49280   # stage 2
    python3 tools/rio-fake/fake_rio.py --iface en0 --ports 49280 \
        --reply-mode guess --replies myreplies.json                # stage 3

Notes
-----
* Pure stdlib + macOS `dns-sd`, same as `tools/dt-fake`. No pip installs.
* `--iface` MUST be the real Dante interface. A beacon pinned to a VPN or VM
  interface is invisible to the Dante runtime — this has already cost one
  session.
* **Every reply in this file is a guess.** The response framing is precisely what
  we do not know; the defaults exist to be wrong in an informative way. Nothing
  here should be cited as evidence.
* Everything printed is data FROM the app. Treat it as untrusted; we only parse
  and display.
"""

from __future__ import annotations

import argparse
import json
import re
import select
import socket
import subprocess
import sys
from datetime import datetime

# --- config -----------------------------------------------------------------

DEVICE_NAME = "Rio3224-D2-FAKE"
CMC_PORT = 8700

# Candidate SCP ports, most likely first. 49280 is the documented Yamaha SCP port
# for the CL/QL line; whether R-series reuses it is UNVERIFIED — that is the
# question. The rest are cheap neighbours to bind while we find out.
CANDIDATE_PORTS = [49280, 49281, 49282, 49283, 49900]

# ConMon UDP ports, mirroring tools/dt-fake. R Remote also speaks ConMon for
# Dante-layer management (clocking, naming, firmware), so this catches that too.
CONMON_PORTS = [8704, 8705, 8706, 8707, 8708, 8709, 8710, 8800]

# Identity, with field widths taken from the DevInfo schema archived at
# docs/evidence/rremote-600-scp-schemas.xml. Values are plausible, not observed.
DEV_INFO = {
    "protocolver": "1.0",
    "paramsetver": "1.0",
    "version": "1.00",
    "productname": "Rio3224-D2",
    "manufacturer": "Yamaha",
    "category": "IO",
    "deviceid": "3",
    "devicename": DEVICE_NAME,
    "inputport": "32",
    "outputport": "16",
}

# Guessed reply table: (regex on the received line, response line).
# `{k}` expands to DEV_INFO[k] where the regex captured a key as group 1.
DEFAULT_REPLIES: list[tuple[str, str]] = [
    (r"^\s*devinfo\s+(\w+)", "OK devinfo {g1} \"{dev:g1}\""),
    (r"^\s*devstatus\s+(\w+)", "OK devstatus {g1} \"ok\""),
    (r"^\s*devmode\s+(.*)", "OK devmode {g1}"),
    (r"^\s*scpmode\s+keepalive\s+(\d+)", "OK scpmode keepalive {g1}"),
    (r"^\s*scpmode\s+(.*)", "OK scpmode {g1}"),
    (r"^\s*prmnum", "OK prmnum 33"),
    (r"^\s*ssnum", "OK ssnum 0"),
]


# --- helpers ----------------------------------------------------------------

def now() -> str:
    return datetime.now().strftime("%H:%M:%S.%f")[:-3]


def iface_ip(iface: str) -> str | None:
    try:
        out = subprocess.run(["ipconfig", "getifaddr", iface],
                             capture_output=True, text=True, timeout=3)
        return out.stdout.strip() or None
    except (OSError, subprocess.SubprocessError):
        return None


def primary_ip() -> str:
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.connect(("8.8.8.8", 80))
        return s.getsockname()[0]
    except OSError:
        return "127.0.0.1"
    finally:
        s.close()


def hexdump(data: bytes, indent: str = "    ") -> str:
    out = []
    for i in range(0, len(data), 16):
        chunk = data[i : i + 16]
        hexs = " ".join(f"{b:02x}" for b in chunk)
        text = "".join(chr(b) if 32 <= b < 127 else "." for b in chunk)
        out.append(f"{indent}{i:04x}  {hexs:<47}  {text}")
    return "\n".join(out)


def as_text(data: bytes) -> list[str]:
    """SCP is ASCII. Split on CR/LF and return the non-empty lines."""
    try:
        return [ln for ln in re.split(r"[\r\n]+", data.decode("ascii", "replace")) if ln]
    except Exception:
        return []


class Log:
    def __init__(self, path: str | None):
        self.fh = open(path, "a", encoding="utf-8") if path else None

    def __call__(self, msg: str) -> None:
        print(msg)
        if self.fh:
            self.fh.write(msg + "\n")
            self.fh.flush()

    def close(self) -> None:
        if self.fh:
            self.fh.close()


# --- reply engine -----------------------------------------------------------

class Replier:
    def __init__(self, mode: str, overrides: str | None):
        self.mode = mode
        self.rules: list[tuple[re.Pattern, str]] = []
        if mode == "guess":
            self.rules = [(re.compile(p, re.I), r) for p, r in DEFAULT_REPLIES]
        if overrides:
            with open(overrides, encoding="utf-8") as fh:
                extra = json.load(fh)
            # File rules take precedence: prepend them.
            self.rules = [(re.compile(p, re.I), r) for p, r in extra] + self.rules

    def reply_for(self, line: str) -> str | None:
        if self.mode == "none":
            return None
        for pat, tmpl in self.rules:
            m = pat.match(line)
            if not m:
                continue
            out = tmpl
            for i, g in enumerate(m.groups(), start=1):
                out = out.replace(f"{{dev:g{i}}}", DEV_INFO.get(g or "", ""))
                out = out.replace(f"{{g{i}}}", g or "")
            return out
        return None


# --- mDNS advertisement -----------------------------------------------------

def start_advertisement(ip: str, services: list[tuple[str, int]],
                        log: Log) -> list[subprocess.Popen]:
    """Register the fake over Bonjour, PINNED to one IP via `dns-sd -P`.

    Pinning matters: `dns-sd -R` advertises on every interface, so a machine with
    Tailscale/Parallels/VPN can hand the app an address it cannot route to. TXT
    keys mirror tools/dt-fake's, which the Audinate runtime accepted.
    """
    procs = []
    host = "rio-fake.local"
    txt = [
        "id=0102030405060708090a0b0c0d0e0f10",
        "process=1",
        f"dante={DEVICE_NAME}",
        "mf=Yamaha",
        "model=Rio3224-D2",
    ]
    for stype, port in services:
        cmd = ["dns-sd", "-P", DEVICE_NAME, stype, "local", str(port),
               host, ip, *txt]
        log(f"[{now()}] advertising {DEVICE_NAME} {stype} :{port} -> {ip}")
        procs.append(subprocess.Popen(cmd, stdout=subprocess.DEVNULL,
                                      stderr=subprocess.DEVNULL))
    return procs


# --- listeners --------------------------------------------------------------

def open_tcp(ports: list[int], log: Log) -> dict[socket.socket, int]:
    socks: dict[socket.socket, int] = {}
    for port in ports:
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        try:
            s.bind(("0.0.0.0", port))
            s.listen(4)
            s.setblocking(False)
            socks[s] = port
        except OSError as e:
            log(f"[{now()}] tcp/{port} unavailable ({e}); skipping")
    log(f"[{now()}] TCP listening on: {sorted(socks.values())}")
    return socks


def open_udp(ports: list[int], log: Log) -> dict[socket.socket, int]:
    socks: dict[socket.socket, int] = {}
    for port in ports:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        if hasattr(socket, "SO_REUSEPORT"):
            try:
                s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEPORT, 1)
            except OSError:
                pass
        try:
            s.bind(("0.0.0.0", port))
            s.setblocking(False)
            socks[s] = port
        except OSError as e:
            log(f"[{now()}] udp/{port} unavailable ({e}); skipping")
    log(f"[{now()}] UDP listening on: {sorted(socks.values())}")
    return socks


def main() -> int:
    ap = argparse.ArgumentParser(description="Fake Yamaha R-series SCP harness")
    ap.add_argument("--iface", default="en0",
                    help="interface to pin the advert to — MUST be the Dante NIC")
    ap.add_argument("--ip", default=None, help="override the pinned IP")
    ap.add_argument("--ports", type=int, nargs="*", default=None,
                    help=f"TCP ports to listen on (default: {CANDIDATE_PORTS})")
    ap.add_argument("--listen-only", action="store_true",
                    help="don't advertise over mDNS, just capture")
    ap.add_argument("--reply-mode", default="none", choices=["none", "guess"],
                    help="none=capture only (stage 1/2); guess=use the reply table")
    ap.add_argument("--replies", default=None,
                    help="JSON file of [[regex, response], ...] layered over the table")
    ap.add_argument("--extra-service", action="append", default=[],
                    metavar="TYPE:PORT",
                    help="additional mDNS service to advertise, e.g. _foo._udp:1234 "
                         "(add whatever Phase 2 shows R Remote browsing for)")
    ap.add_argument("--log", default=None, help="also append output to this file")
    args = ap.parse_args()

    log = Log(args.log)
    ip = args.ip or iface_ip(args.iface) or primary_ip()
    tcp_ports = args.ports if args.ports is not None else CANDIDATE_PORTS

    log(f"=== fake R-series SCP harness — pinned to {ip} (iface {args.iface}) ===")
    log(f"=== reply-mode={args.reply_mode} ===")
    log("Launch R Remote now. Ctrl-C to stop.\n")
    log("If no connection appears, the port is outside the candidate list.")
    log("Catch it with, in another terminal:")
    log(f"    sudo tcpdump -i {args.iface} -n \"tcp[tcpflags] & tcp-syn != 0 and host {ip}\"\n")

    services = [("_netaudio-cmc._udp", CMC_PORT), ("_netaudio-arc._udp", 4440)]
    for spec in args.extra_service:
        stype, _, port = spec.rpartition(":")
        if stype and port.isdigit():
            services.append((stype, int(port)))
        else:
            log(f"ignoring malformed --extra-service {spec!r} (want TYPE:PORT)")

    adverts = [] if args.listen_only else start_advertisement(ip, services, log)
    replier = Replier(args.reply_mode, args.replies)

    tcp_listen = open_tcp(tcp_ports, log)
    udp_socks = open_udp(CONMON_PORTS, log)
    if not tcp_listen and not udp_socks:
        log("No sockets available — is another Dante app holding them?")
        return 1

    conns: dict[socket.socket, tuple[str, int, int]] = {}  # sock -> (peer_ip, peer_port, local_port)

    try:
        while True:
            watch = list(tcp_listen) + list(udp_socks) + list(conns)
            ready, _, _ = select.select(watch, [], [], 1.0)
            for s in ready:
                # New SCP connection — this alone is the Stage 1 result.
                if s in tcp_listen:
                    try:
                        c, addr = s.accept()
                    except OSError:
                        continue
                    c.setblocking(False)
                    port = tcp_listen[s]
                    conns[c] = (addr[0], addr[1], port)
                    log(f"\n[{now()}] *** TCP CONNECT from {addr[0]}:{addr[1]} "
                        f"to port {port} — SCP PORT = {port} ***")
                    continue

                # ConMon datagram.
                if s in udp_socks:
                    try:
                        data, addr = s.recvfrom(65535)
                    except OSError:
                        continue
                    log(f"\n[{now()}] UDP {addr[0]}:{addr[1]} -> :{udp_socks[s]} "
                        f"({len(data)} bytes)")
                    log(hexdump(data))
                    continue

                # Data on an accepted SCP connection.
                peer_ip, peer_port, local_port = conns[s]
                try:
                    data = s.recv(65535)
                except OSError:
                    data = b""
                if not data:
                    log(f"[{now()}] TCP close {peer_ip}:{peer_port} (port {local_port})")
                    s.close()
                    del conns[s]
                    continue

                log(f"\n[{now()}] TCP {peer_ip}:{peer_port} -> :{local_port} "
                    f"({len(data)} bytes)")
                log(hexdump(data))
                for line in as_text(data):
                    log(f"  >> {line}")
                    resp = replier.reply_for(line)
                    if resp is None:
                        continue
                    try:
                        s.sendall((resp + "\n").encode("ascii", "replace"))
                        log(f"  << {resp}")
                    except OSError as e:
                        log(f"  << send failed: {e}")
    except KeyboardInterrupt:
        log("\nstopping.")
    finally:
        for p in adverts:
            p.terminate()
        for c in conns:
            c.close()
        for s in list(tcp_listen) + list(udp_socks):
            s.close()
        log.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
