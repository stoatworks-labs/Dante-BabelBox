#!/usr/bin/env python3
"""Fake Allen & Heath DT168 — a bring-up / capture harness, NOT a finished
emulator.

Why this exists
---------------
`crates/preamp-adapter-ah/src/dt.rs` implements the A&H `AllenHth` ConMon
*vendor payload* (gain / pad / +48V for 16 preamps). What it does NOT have is
the Audinate **ConMon transport** — the mDNS discovery record and the channel
handshake DAPI uses to reach a device. That layer was flagged [OPEN] in
`docs/allenheath-dt-preamp-over-dante.md` because it is Audinate's and we had no
capture.

This tool closes that gap the only way we can without hardware: it makes the
*real* DT Preamp Control app talk to us, and logs exactly what it says.

What it does
------------
1. Advertises a fake `_netaudio-cmc._udp` ConMon device (via macOS `dns-sd`) so
   the app's discovery lists a "DT168" and tries to connect to it.
2. Binds the advertised CMC port plus the well-known ConMon UDP ports and
   hex-dumps every datagram the app sends, scanning each for the 8-byte
   `AllenHth` vendor id and trying to decode the payload with the same rules as
   `dt.rs`.

The first runs will mostly *capture* the app (its mDNS resolve, its connection
attempt, and — the prize — a real `AllenHth` message with its ConMon envelope).
Each capture lets us fill in one [OPEN] and teach this tool to answer, until the
app both lists the fake and shows its values. It is a loop, not one shot.

Run
---
    python3 tools/dt-fake/fake_dt168.py            # advertise + listen
    python3 tools/dt-fake/fake_dt168.py --listen-only   # no mDNS, just capture

Notes
-----
* Pure stdlib + macOS `dns-sd`. No pip installs.
* Dante Virtual Soundcard / Controller may be running; that is fine (it gives the
  app a real Dante interface). If a port is already bound we skip it and carry on
  — we only strictly need the CMC port we advertise.
* Everything it prints is data FROM the app. Treat it as untrusted; we only
  parse and display.
"""

from __future__ import annotations

import argparse
import socket
import subprocess
import sys
import time
from datetime import datetime

# --- config -----------------------------------------------------------------

DEVICE_NAME = "DT168-FAKE"
CMC_PORT = 8700  # the port we advertise in the SRV record; the app connects here

# Well-known Audinate ConMon UDP ports, from the app's own immediates
# (0x2200..0x2211 = 8704..8721) plus the 8800 the MBC work saw. We bind these
# best-effort so we also catch traffic the app sends to fixed ports.
EXTRA_LISTEN_PORTS = [8704, 8705, 8706, 8707, 8708, 8709, 8710, 8800]

VENDOR_ID = b"AllenHth"

# Mirror of dt.rs — kept deliberately tiny so this stays a throwaway tool.
PREAMP_COUNT = 16
MSG_SET = 0x05
MSG_STATUS_FULL = 0x01
MSG_STATUS_FLAGS = 0x00
FLAG_PAD = 1 << 0
FLAG_PHANTOM = 1 << 1


# --- helpers ----------------------------------------------------------------

def now() -> str:
    return datetime.now().strftime("%H:%M:%S.%f")[:-3]


def primary_ip() -> str:
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.connect(("8.8.8.8", 80))
        return s.getsockname()[0]
    except OSError:
        return "127.0.0.1"
    finally:
        s.close()


def iface_ip(iface: str) -> str | None:
    """The IPv4 of a named interface (e.g. en0), via `ipconfig getifaddr`."""
    try:
        out = subprocess.run(["ipconfig", "getifaddr", iface],
                             capture_output=True, text=True, timeout=3)
        ip = out.stdout.strip()
        return ip or None
    except (OSError, subprocess.SubprocessError):
        return None


OUR_ID = bytes.fromhex("0102030405060708")  # our fake device's 8-byte conmon id


def parse_cmc_head(data: bytes) -> dict | None:
    """Parse the ConMon CMC packet head we reverse-engineered from Dante
    Controller's probes.

    Layout (offsets): [0:2]=0x1000 ver, [2:4]=len, [4:6]=seq, [6:8]=type
    (0x1001), [8:10]=sub/phase (0=probe, 1=carries the controller's channel
    endpoint), [10:18]=peer 8-byte conmon id."""
    if len(data) < 20 or data[0:2] != b"\x10\x00":
        return None
    h = {
        "len": int.from_bytes(data[2:4], "big"),
        "seq": int.from_bytes(data[4:6], "big"),
        "type": int.from_bytes(data[6:8], "big"),
        "sub": int.from_bytes(data[8:10], "big"),
        "peer_id": data[10:18],
    }
    # phase-1 (40-byte) packet embeds the controller's channel: ip@24, port@28
    if len(data) >= 30:
        h["ctrl_ip"] = ".".join(str(x) for x in data[24:28])
        h["ctrl_port"] = int.from_bytes(data[28:30], "big")
    return h


def build_device_announce(seq: int, our_ip: str, our_port: int) -> bytes:
    """A guess at the device -> controller announce a real DT unit would send to
    the controller's channel once it learns it (phase-1). Mirrors the probe head
    with our own id and advertises our CMC endpoint back."""
    ip = bytes(int(x) for x in our_ip.split("."))
    body = (
        b"\x10\x00"                      # version
        + b"\x00\x00"                    # len placeholder (fixed below)
        + seq.to_bytes(2, "big")
        + b"\x10\x01"                    # type
        + b"\x00\x01"                    # sub = 1 (announce)
        + OUR_ID
        + b"\x00\x00"
        + b"\x00\x02\x00\x00"
        + ip
        + our_port.to_bytes(2, "big")
        + b"\x00\x00\x00\x00"
        + our_port.to_bytes(2, "big")
        + b"\x00\x00"
    )
    b = bytearray(body)
    b[2:4] = len(b).to_bytes(2, "big")
    return bytes(b)


def hexdump(data: bytes, indent: str = "    ") -> str:
    out = []
    for i in range(0, len(data), 16):
        chunk = data[i : i + 16]
        hexs = " ".join(f"{b:02x}" for b in chunk)
        text = "".join(chr(b) if 32 <= b < 127 else "." for b in chunk)
        out.append(f"{indent}{i:04x}  {hexs:<47}  {text}")
    return "\n".join(out)


def try_decode_allenhth(data: bytes) -> str | None:
    """If `data` contains the AllenHth vendor id, describe what follows it using
    dt.rs's rules. Heuristic: the real ConMon envelope between the id and our
    payload is still unknown, so we scan a short window after the id for a byte
    that looks like one of our message types and try to parse from there."""
    idx = data.find(VENDOR_ID)
    if idx < 0:
        return None
    after = idx + len(VENDOR_ID)
    lines = [f"  >> AllenHth vendor id at offset {idx}"]
    # dump the 16 bytes after the id — this is where the type/len envelope lives
    lines.append(f"     next16: {data[after:after+16].hex(' ')}")

    for probe in range(after, min(after + 24, len(data))):
        t = data[probe]
        if t == MSG_STATUS_FULL and len(data) - (probe + 1) >= PREAMP_COUNT * 3:
            body = data[probe + 1 :]
            pres = []
            for i in range(PREAMP_COUNT):
                g = int.from_bytes(body[i * 3 : i * 3 + 2], "big")
                f = body[i * 3 + 2]
                pres.append(
                    f"ch{i:02d} gain=0x{g:04x} pad={'Y' if f & FLAG_PAD else '.'} "
                    f"48v={'Y' if f & FLAG_PHANTOM else '.'}"
                )
            lines.append(f"     looks like STATUS_FULL at +{probe-after}:")
            lines += ["       " + p for p in pres]
            break
        if t == MSG_SET and len(data) - probe >= 5:
            ch = data[probe + 1]
            g = int.from_bytes(data[probe + 2 : probe + 4], "big")
            f = data[probe + 4]
            lines.append(
                f"     looks like SET_MIC_PRE at +{probe-after}: ch={ch} "
                f"gain=0x{g:04x} pad={'Y' if f & FLAG_PAD else '.'} "
                f"48v={'Y' if f & FLAG_PHANTOM else '.'}"
            )
            break
    else:
        lines.append("     (no recognisable message type in the window — dump above)")
    return "\n".join(lines)


# --- mDNS advertisement -----------------------------------------------------

def start_advertisement(ip: str) -> list[subprocess.Popen]:
    """Register the fake device over Bonjour, PINNED to one IP.

    Uses `dns-sd -P` (proxy) so the SRV target and its A record resolve ONLY to
    `ip` (the wifi), never to a Tailscale/Parallels/VPN address the machine's
    `.local` hostname would otherwise also carry. `dns-sd -R` was advertising on
    every interface, which is why the app — bound to its Dante interface — could
    pick the wrong address or ignore us.

    TXT keys are best-guess from the app's strings (it requires a non-empty
    `id` with "sufficient data", and a non-empty `process`); adjust and re-run
    if the app still won't list it."""
    procs = []
    host = "dt168-fake.local"
    txt = [
        "id=0102030405060708090a0b0c0d0e0f10",  # conmon instance id (16 bytes hex)
        "process=1",                            # conmon process field — non-empty
        f"dante={DEVICE_NAME}",
        "mf=Allen & Heath",
        "model=DT168",
    ]
    services = [
        ("_netaudio-cmc._udp", CMC_PORT),   # ConMon — the one the app connects on
        ("_netaudio-arc._udp", 4440),       # audio routing control — present on real units
    ]
    for stype, port in services:
        # dns-sd -P <name> <type> <domain> <port> <host> <ip> [TXT...]
        cmd = ["dns-sd", "-P", DEVICE_NAME, stype, "local", str(port),
               host, ip, *txt]
        print(f"[{now()}] advertising {DEVICE_NAME} {stype} :{port} "
              f"pinned to {host} -> {ip}")
        procs.append(subprocess.Popen(cmd, stdout=subprocess.DEVNULL,
                                      stderr=subprocess.DEVNULL))
    return procs


# --- listener ---------------------------------------------------------------

def open_listeners() -> dict[int, socket.socket]:
    socks: dict[int, socket.socket] = {}
    for port in [CMC_PORT, *EXTRA_LISTEN_PORTS]:
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
            socks[port] = s
        except OSError as e:
            print(f"[{now()}] port {port} unavailable ({e}); skipping")
    print(f"[{now()}] listening on: {sorted(socks)}")
    return socks


def main() -> int:
    ap = argparse.ArgumentParser(description="Fake DT168 capture harness")
    ap.add_argument("--listen-only", action="store_true",
                    help="don't advertise over mDNS, just capture")
    ap.add_argument("--iface", default="en0",
                    help="interface to pin the advert to (default en0 = wifi)")
    ap.add_argument("--ip", default=None,
                    help="override the pinned IP (default: the --iface address)")
    ap.add_argument("--respond", default="none",
                    choices=["none", "echo", "connect"],
                    help="none=capture only; echo=ack CMC probes; "
                         "connect=echo + announce back to the controller's channel")
    args = ap.parse_args()

    ip = args.ip or iface_ip(args.iface) or primary_ip()
    print(f"=== pinning fake DT168 to {ip} (iface {args.iface}) ===")
    print(f"=== fake DT168 bring-up harness — local IP {ip} ===")
    print("Run DT Preamp Control now. Watch for the fake in Detected Devices,")
    print("and for packets below. Ctrl-C to stop.\n")

    adverts: list[subprocess.Popen] = []
    if not args.listen_only:
        adverts = start_advertisement(ip)

    socks = open_listeners()
    if not socks:
        print("No listen ports available — is another Dante app holding them all?")
        return 1

    import select

    try:
        while True:
            ready, _, _ = select.select(list(socks.values()), [], [], 1.0)
            for s in ready:
                try:
                    data, addr = s.recvfrom(65535)
                except OSError:
                    continue
                port = s.getsockname()[1]
                head = parse_cmc_head(data)
                tag = ""
                if head:
                    tag = (f"  [CMC seq=0x{head['seq']:04x} type=0x{head['type']:04x} "
                           f"sub=0x{head['sub']:04x}]")
                print(f"\n[{now()}] {addr[0]}:{addr[1]} -> :{port}  ({len(data)} bytes){tag}")
                print(hexdump(data))
                decoded = try_decode_allenhth(data)
                if decoded:
                    print(decoded)

                if args.respond == "none" or head is None:
                    continue

                if args.respond == "echo":
                    s.sendto(data, addr)
                    print(f"     <- echo ({len(data)} b) to {addr[0]}:{addr[1]}")
                    continue

                # "connect": reply on the CMC socket to the controller with OUR
                # id and OUR channel endpoint (rather than parroting DC's).
                if head["sub"] == 0:
                    reply = bytearray(data)
                    reply[10:18] = OUR_ID            # answer as ourselves
                    s.sendto(bytes(reply), addr)
                    print(f"     <- probe-reply ({len(reply)} b, our id) to "
                          f"{addr[0]}:{addr[1]}")
                elif head["sub"] == 1:
                    reply = build_device_announce(head["seq"], ip, CMC_PORT)
                    s.sendto(reply, addr)
                    print(f"     <- announce-reply ({len(reply)} b, our id+chan) "
                          f"to {addr[0]}:{addr[1]}")
    except KeyboardInterrupt:
        print("\nstopping.")
    finally:
        for p in adverts:
            p.terminate()
        for s in socks.values():
            s.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
