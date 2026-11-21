#!/usr/bin/env python3
"""ewdx_probe - bench client for Sennheiser EW-DX EM receivers over SSC.

Why this exists
---------------
`crates/mic-adapter-sennheiser` was written from Sennheiser's "SSC Developer's
Guide for EW-DX EM" (firmware 3.0.x, Publ. 03/2024) and has never met a
receiver. This tool runs the bench sequence in
`docs/sennheiser-ewdx-bench-plan.md`: it reads, writes and subscribes the way
the guide says, logs every datagram both ways, and puts back what it changed.
What the receiver sends back is the evidence; nothing this tool sends is.

SSC in one paragraph: one JSON object per UDP datagram, default port 45. An
address such as /rx1/gain is the JSON path {"rx1": {"gain": ...}}. A null
value is a read; any other value is a write, and the reply carries the value
the receiver actually settled on (it may adapt an out-of-range value instead
of refusing it). Subscriptions go to /osc/state/subscribe.

Run (every command takes --host; see --help for the rest)
---------------------------------------------------------
    ewdx_probe.py --host 10.0.0.32 get /device/identity/product /rx1/gain
    ewdx_probe.py --host 10.0.0.32 snapshot -o before.json
    ewdx_probe.py --host 10.0.0.32 set /rx1/gain 12
    ewdx_probe.py --host 10.0.0.32 sweep gain --channel 1 --pause
    ewdx_probe.py --host 10.0.0.32 watch adapter --channel 1 --duration 600
    ewdx_probe.py --host 10.0.0.32 errors
    ewdx_probe.py --host 10.0.0.32 clients --n 9
    ewdx_probe.py --host 10.0.0.32 restore before.json
    ewdx_probe.py diff before.json after.json

Safety
------
Writes to the paths in DENY are refused before anything is sent: factory and
audio restore, restart, link-density mode (the guide says it reboots the
receiver), encryption, every network setting, the clock, the booster supply
(DC up the antenna cable), presets and TX sync settings. RF frequency writes
need --allow-rf. To send one of the refused paths, use a different tool on
purpose; this one will not do it.

Pure stdlib, Python 3.9+. Every datagram is appended to a JSONL log
(--log, default ewdx-<timestamp>.jsonl in the current directory).
"""

from __future__ import annotations

import argparse
import datetime as _dt
import json
import re
import socket
import sys
import time
from typing import Any, Iterable, Iterator

DEFAULT_PORT = 45

# Paths a write may never touch. Checked as prefixes against every non-null
# leaf of an outgoing message (the /osc/ control tree is exempt: subscribe,
# close, xid and ping carry no settings).
DENY = [
    re.compile(r"^/device/restore\b"),
    re.compile(r"^/device/restart\b"),
    re.compile(r"^/device/link_density_mode\b"),
    re.compile(r"^/device/encryption\b"),
    re.compile(r"^/device/network/"),
    re.compile(r"^/device/time\b"),
    re.compile(r"^/device/booster\b"),
    re.compile(r"^/device/remote_access\b"),
    re.compile(r"^/rx\d+/restore\b"),
    re.compile(r"^/rx\d+/presets/"),
    re.compile(r"^/rx\d+/sync_settings/"),
]
RF = re.compile(r"^/rx\d+/frequency$")

# What `restore` may put back from a snapshot. Frequency only with --allow-rf.
RESTORABLE = [
    re.compile(r"^/device/(name|location|lock|brightness)$"),
    re.compile(r"^/rx\d+/(name|mute|gain)$"),
    re.compile(r"^/audio1/out\d+/level$"),
]

# Leaves that move on their own; `diff` ignores them.
VOLATILE = [
    re.compile(r"^/m/"),
    re.compile(r"^/device/time$"),
    re.compile(r"^/mates/tx\d+/battery/(gauge|lifetime)$"),
    re.compile(r"^/rx\d+/warnings$"),
    re.compile(r"^/mates/tx\d+/warnings$"),
]


# ---------------------------------------------------------------- JSON paths

def tree(path: str, value: Any) -> dict:
    """'/rx1/gain', 12 -> {'rx1': {'gain': 12}}."""
    parts = [p for p in path.split("/") if p]
    if not parts:
        raise ValueError(f"empty SSC path: {path!r}")
    node: Any = value
    for part in reversed(parts):
        node = {part: node}
    return node


def merge(a: dict, b: dict) -> dict:
    out = dict(a)
    for k, v in b.items():
        if k in out and isinstance(out[k], dict) and isinstance(v, dict):
            out[k] = merge(out[k], v)
        else:
            out[k] = v
    return out


def leaves(node: Any, prefix: str = "") -> Iterator[tuple[str, Any]]:
    if isinstance(node, dict) and node:
        for k, v in node.items():
            yield from leaves(v, f"{prefix}/{k}")
    else:
        yield prefix or "/", node


def lookup(node: Any, path: str) -> Any:
    for part in [p for p in path.split("/") if p]:
        if not isinstance(node, dict) or part not in node:
            return KeyError
        node = node[part]
    return node


def parse_value(text: str) -> Any:
    """Command-line value: JSON if it parses (12, true, null, "x", [1]), else a string."""
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return text


def check_safe(msg: dict, allow_rf: bool) -> None:
    for path, value in leaves(msg):
        if path.startswith("/osc/") or value is None:
            continue
        if any(rx.search(path) for rx in DENY):
            raise SystemExit(f"refused: {path} is on the deny list (see the module docstring)")
        if RF.search(path) and not allow_rf:
            raise SystemExit(f"refused: {path} changes the RF frequency; pass --allow-rf on purpose")


def osc_error(msg: Any) -> Any:
    """The /osc/error part of a reply, if any. Its shape is not in the EW-DX
    guide, so this returns whatever is there for the caller to print raw."""
    if isinstance(msg, dict) and isinstance(msg.get("osc"), dict) and "error" in msg["osc"]:
        return msg["osc"]["error"]
    return None


# ---------------------------------------------------------------- transport

class Log:
    def __init__(self, path: str | None):
        if path is None:
            stamp = _dt.datetime.now().strftime("%Y%m%d-%H%M%S")
            path = f"ewdx-{stamp}.jsonl"
        self.path = path
        self.fh = open(path, "a", encoding="utf-8")

    def write(self, **rec: Any) -> None:
        rec.setdefault("ts", time.time())
        self.fh.write(json.dumps(rec, ensure_ascii=False) + "\n")
        self.fh.flush()

    def note(self, text: str) -> None:
        self.write(dir="note", text=text)


class Ssc:
    """An unconnected UDP socket, so a reply from any source port is still seen
    (and reported: the Rust adapter's socket is connect()ed and would drop it)."""

    def __init__(self, host: str, port: int, log: Log, bind_port: int = 0):
        self.remote = (socket.gethostbyname(host), port)
        self.log = log
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.bind(("0.0.0.0", bind_port))
        self.local = self.sock.getsockname()
        self.warned_ports: set[int] = set()

    def send_raw(self, text: str) -> None:
        self.sock.sendto(text.encode("utf-8"), self.remote)
        self.log.write(dir="tx", local=list(self.local), remote=list(self.remote), raw=text)

    def send(self, msg: dict) -> None:
        self.send_raw(json.dumps(msg, separators=(",", ":")))

    def recv(self, timeout: float) -> list[tuple[float, Any, str]]:
        """Everything that arrives within `timeout` s (the window restarts on
        each datagram for at most 4x timeout, so a burst is not cut short).
        Each item is (time.monotonic() at arrival, parsed JSON or None, raw)."""
        out = []
        end = time.monotonic() + timeout
        hard_end = time.monotonic() + timeout * 4
        while True:
            left = min(end, hard_end) - time.monotonic()
            if left <= 0:
                return out
            self.sock.settimeout(left)
            try:
                data, src = self.sock.recvfrom(65535)
            except socket.timeout:
                return out
            arrived = time.monotonic()
            raw = data.decode("utf-8", errors="replace")
            self.log.write(dir="rx", src=list(src), local=list(self.local), raw=raw)
            if src[1] != self.remote[1] and src[1] not in self.warned_ports:
                self.warned_ports.add(src[1])
                print(f"!! reply came from port {src[1]}, not {self.remote[1]}: the Rust adapter's "
                      f"connected socket would drop it (bench plan P2)", file=sys.stderr)
            try:
                parsed = json.loads(raw)
            except json.JSONDecodeError:
                parsed = None
            out.append((arrived, parsed, raw))
            end = time.monotonic() + timeout

    def call(self, msg: dict, timeout: float = 1.0) -> list[tuple[float, Any, str]]:
        self.send(msg)
        return self.recv(timeout)

    def close(self) -> None:
        self.sock.close()


def print_replies(replies: list[tuple[float, Any, str]], raw: bool = False) -> None:
    if not replies:
        print("   (no reply)")
    for _, parsed, text in replies:
        if raw or parsed is None:
            print(f"   RX {text}")
            continue
        err = osc_error(parsed)
        for path, value in leaves(parsed):
            if path.startswith("/osc/error"):
                continue
            print(f"   {path} = {json.dumps(value)}")
        if err is not None:
            print(f"   !! /osc/error = {json.dumps(err)}")


def fmt(value: Any) -> str:
    """JSON for printing; KeyError (no value came back for the path) reads as such."""
    return "<no value>" if value is KeyError else json.dumps(value)


def reply_value(replies: list[tuple[float, Any, str]], path: str) -> Any:
    for _, parsed, _ in replies:
        if parsed is None:
            continue
        v = lookup(parsed, path)
        if v is not KeyError:
            return v
    return KeyError


# ---------------------------------------------------------------- read lists

DEVICE_GROUPS = [
    ["/device/identity/vendor", "/device/identity/product", "/device/identity/version",
     "/device/identity/serial"],
    ["/device/name", "/device/location", "/device/lock", "/device/brightness",
     "/device/identification/visual", "/device/language"],
    ["/device/frequency_code", "/device/frequency_ranges", "/device/preset_spacing",
     "/device/link_density_mode", "/device/encryption"],
    ["/device/network/ipv4/auto", "/device/network/ipv4/ipaddr", "/device/network/ipv4/netmask",
     "/device/network/ipv4/gateway", "/device/network/ether/macs", "/device/network/mdns"],
    ["/interface/version", "/osc/version", "/osc/state/auth/access"],
    ["/osc/feature/subscription", "/osc/feature/pattern", "/osc/feature/timetag",
     "/osc/feature/baseaddr"],
]
DANTE_GROUP = [
    "/device/network/dante/identity/version", "/device/network/dante/ipv4/auto",
    "/device/network/dante/ipv4/ipaddr", "/device/network/dante/ipv4/netmask",
    "/device/network/dante/macs", "/device/network/dante/interfaces",
    "/device/network/dante/interface_mapping",
]
SYNC_SETTINGS = ["trim", "trim_ignore", "name_ignore", "mute_config", "mute_config_ignore",
                 "mute_config_ts", "lowcut", "lowcut_ignore", "lock", "lock_ignore", "led",
                 "led_ignore", "frequency_ignore", "cable_emulation", "cable_emulation_ignore"]
TX_FIELDS = ["battery/type", "battery/gauge", "battery/lifetime", "warnings", "version", "type",
             "trim", "name", "mute", "mute_config", "mute_config_ts", "lowcut", "lock", "led",
             "identification", "capsule", "cable_emulation"]


def channel_groups(ch: int) -> list[list[str]]:
    rx = f"/rx{ch}"
    return [
        [f"{rx}/name", f"{rx}/mute", f"{rx}/gain", f"{rx}/frequency", f"{rx}/mates",
         f"{rx}/audio", f"{rx}/warnings", f"{rx}/identification/visual", f"{rx}/presets/active"],
        [f"{rx}/sync_settings/{f}" for f in SYNC_SETTINGS],
        [f"/audio1/out{ch}/level"] + [f"/m/rx{ch}/{f}" for f in ("rssi", "rsqi", "divi", "af")],
        [f"/mates/tx{ch}/{f}" for f in TX_FIELDS],
    ]


def null_tree(paths: Iterable[str]) -> dict:
    msg: dict = {}
    for p in paths:
        msg = merge(msg, tree(p, None))
    return msg


def detect_channels(ssc: Ssc, timeout: float) -> int:
    product = reply_value(ssc.call(tree("/device/identity/product", None), timeout),
                          "/device/identity/product")
    if isinstance(product, str) and "EM 4" in product:
        return 4
    if isinstance(product, str):
        return 2
    print("!! no product string; assuming 2 channels (pass --channels)", file=sys.stderr)
    return 2


# ---------------------------------------------------------------- commands

def cmd_send(ssc: Ssc, a: argparse.Namespace) -> None:
    text = a.json
    try:
        msg = json.loads(text)
    except json.JSONDecodeError:
        if not a.malformed:
            raise SystemExit("not valid JSON; pass --malformed to send it anyway (error test)")
        ssc.send_raw(text)
    else:
        check_safe(msg, a.allow_rf)
        ssc.send_raw(text)
    print_replies(ssc.recv(a.wait), raw=a.raw)


def cmd_get(ssc: Ssc, a: argparse.Namespace) -> None:
    msg = null_tree(a.paths) if not a.separate else None
    if msg is not None:
        print_replies(ssc.call(msg, a.wait), raw=a.raw)
        return
    for p in a.paths:
        print(f"-- {p}")
        print_replies(ssc.call(tree(p, None), a.wait), raw=a.raw)


def cmd_set(ssc: Ssc, a: argparse.Namespace) -> None:
    value = parse_value(a.value)
    msg = tree(a.path, value)
    check_safe(msg, a.allow_rf)
    before = reply_value(ssc.call(tree(a.path, None), a.wait), a.path)
    replies = ssc.call(msg, a.wait)
    got = reply_value(replies, a.path)
    after = reply_value(ssc.call(tree(a.path, None), a.wait), a.path)
    adapted = "" if got == value or got is KeyError else "   <- ADAPTED by the receiver"
    print(f"{a.path}: was {fmt(before)}, sent {fmt(value)}, "
          f"reply {fmt(got)}, read back {fmt(after)}{adapted}")
    for _, parsed, raw in replies:
        if osc_error(parsed) is not None or parsed is None:
            print(f"   RX {raw}")
    if not replies:
        print("   (no reply to the write)")
    if before is not KeyError:
        print(f"   restore with: probe set {a.path} '{json.dumps(before)}'")


def take_snapshot(ssc: Ssc, channels: int, timeout: float, dante: bool) -> dict:
    groups = list(DEVICE_GROUPS) + ([DANTE_GROUP] if dante else [])
    for ch in range(1, channels + 1):
        groups += channel_groups(ch)
    values: dict = {}
    errors = []
    for group in groups:
        replies = ssc.call(null_tree(group), timeout)
        if not replies:
            errors.append({"asked": group, "reply": None})
        for _, parsed, raw in replies:
            if parsed is None:
                errors.append({"asked": group, "raw": raw})
                continue
            if osc_error(parsed) is not None:
                errors.append({"asked": group, "raw": raw})
            body = {k: v for k, v in parsed.items() if k != "osc"}
            if isinstance(parsed.get("osc"), dict):
                rest = {k: v for k, v in parsed["osc"].items() if k != "error"}
                if rest:
                    body["osc"] = rest
            values = merge(values, body)
    return {"meta": {"host": ssc.remote[0], "port": ssc.remote[1], "channels": channels,
                     "taken": _dt.datetime.now().isoformat(timespec="seconds")},
            "values": values, "errors": errors}


def cmd_snapshot(ssc: Ssc, a: argparse.Namespace) -> None:
    channels = a.channels or detect_channels(ssc, a.wait)
    snap = take_snapshot(ssc, channels, a.wait, dante=not a.no_dante)
    with open(a.output, "w", encoding="utf-8") as fh:
        json.dump(snap, fh, indent=2, ensure_ascii=False)
    n = sum(1 for _ in leaves(snap["values"]))
    print(f"{n} values from {channels} channel(s) -> {a.output}; {len(snap['errors'])} error replies")
    for e in snap["errors"]:
        text = e.get("raw", "NO REPLY")
        print(f"   asked {e['asked'][0]}..: {text[:240]}{'...' if len(text) > 240 else ''}")
    if snap["errors"]:
        print(f"   (full error replies are in {a.output} under \"errors\")")


def cmd_diff(a: argparse.Namespace) -> None:
    with open(a.a, encoding="utf-8") as fh:
        va = dict(leaves(json.load(fh)["values"]))
    with open(a.b, encoding="utf-8") as fh:
        vb = dict(leaves(json.load(fh)["values"]))
    changed = 0
    for path in sorted(set(va) | set(vb)):
        if not a.all and any(rx.search(path) for rx in VOLATILE):
            continue
        x, y = va.get(path, "<absent>"), vb.get(path, "<absent>")
        if x != y:
            changed += 1
            print(f"{path}: {json.dumps(x)} -> {json.dumps(y)}")
    print(f"{changed} difference(s)" + ("" if a.all else " (meters, battery and warnings ignored)"))
    if changed:
        sys.exit(1)


def cmd_restore(ssc: Ssc, a: argparse.Namespace) -> None:
    with open(a.snapshot, encoding="utf-8") as fh:
        snap = json.load(fh)
    pending = []
    for path, value in leaves(snap["values"]):
        if any(rx.search(path) for rx in RESTORABLE) or (a.allow_rf and RF.search(path)):
            pending.append((path, value))
    for path, value in pending:
        now = reply_value(ssc.call(tree(path, None), a.wait), path)
        if now == value:
            continue
        if a.dry_run:
            print(f"would set {path}: {fmt(now)} -> {fmt(value)}")
            continue
        msg = tree(path, value)
        check_safe(msg, a.allow_rf)
        got = reply_value(ssc.call(msg, a.wait), path)
        ok = "ok" if got == value else "!! DID NOT TAKE"
        print(f"set {path}: {fmt(now)} -> {fmt(value)} (reply {fmt(got)}) {ok}")
    print(f"checked {len(pending)} restorable values")


SWEEPS = {
    # (path template, in-range steps, odd values: off-step, out of range, wrong type)
    "gain": ("/rx{ch}/gain", list(range(-3, 43, 3)), [10, 11, 1.5, -10, 50, "12"]),
    "level": ("/audio1/out{ch}/level", list(range(-24, 19, 6)), [-20, 1, 3, -40, 30, "0"]),
    "mute": ("/rx{ch}/mute", [True, False], [1, 0, "true"]),
}


def cmd_sweep(ssc: Ssc, a: argparse.Namespace) -> None:
    template, steps, odd = SWEEPS[a.what]
    path = template.format(ch=a.channel)
    values = steps + (odd if a.odd else [])
    original = reply_value(ssc.call(tree(path, None), a.wait), path)
    if original is KeyError:
        raise SystemExit(f"{path} did not answer a read; not sweeping it")
    print(f"{path} starts at {fmt(original)}; {len(values)} steps; restores at the end")
    ssc.log.note(f"sweep {path} from {original!r}")
    try:
        for v in values:
            got = reply_value(ssc.call(tree(path, v), a.wait), path)
            back = reply_value(ssc.call(tree(path, None), a.wait), path)
            flag = "" if got == v else ("   <- no value in reply" if got is KeyError else "   <- adapted")
            print(f"  sent {fmt(v):>6}  reply {fmt(got):>6}  read {fmt(back):>6}{flag}")
            if a.pause:
                input("     note the meter / output, then Enter ")
            else:
                time.sleep(a.dwell)
    finally:
        got = reply_value(ssc.call(tree(path, original), a.wait), path)
        print(f"restored {path} -> {fmt(got)}" + ("" if got == original else "  !! CHECK BY HAND"))


def watch_message(a: argparse.Namespace) -> dict:
    opts = {k: getattr(a, k) for k in ("lifetime", "count", "min", "max") if getattr(a, k) is not None}
    ch = a.channel
    if a.preset == "adapter":
        # The tree SennheiserAdapter's subscribe_request() sends, including
        # its "#" (lifetime 60, count 1000) unless overridden. The adapter
        # also renews it every 20 s and on a 310; pass --renew 20 to match.
        opts = opts or {"lifetime": 60, "count": 1000}
        body = {f"rx{ch}": {"mute": None, "frequency": None},
                "m": {f"rx{ch}": {"rssi": None, "rsqi": None, "divi": None, "af": None}},
                "mates": {f"tx{ch}": {"battery": {"gauge": None, "lifetime": None}}}}
    elif a.preset == "meters":
        body = null_tree(f"/m/rx{ch}/{f}" for f in ("rssi", "rsqi", "divi", "af"))
    elif a.preset == "status":
        body = null_tree([f"/rx{ch}/warnings", f"/rx{ch}/mute", f"/rx{ch}/gain",
                          f"/mates/tx{ch}/warnings", f"/mates/tx{ch}/mute",
                          f"/mates/tx{ch}/battery/gauge", f"/mates/tx{ch}/battery/lifetime",
                          f"/mates/tx{ch}/battery/type", f"/mates/tx{ch}/type",
                          f"/mates/tx{ch}/name", f"/mates/tx{ch}/capsule"])
    else:
        if not a.paths:
            raise SystemExit("watch paths needs one or more SSC paths")
        body = null_tree(a.paths)
    item = dict(body)
    if opts:
        item = {"#": opts, **body}
    return {"osc": {"state": {"subscribe": [item]}}}


def cmd_watch(ssc: Ssc, a: argparse.Namespace) -> None:
    msg = watch_message(a)
    print(f"TX {json.dumps(msg, separators=(',', ':'))}")
    ssc.send(msg)
    start = time.monotonic()
    last_renew = start
    n = 0
    per_path: dict[str, int] = {}
    terminated = None
    try:
        while time.monotonic() - start < a.duration:
            if a.renew and time.monotonic() - last_renew >= a.renew:
                ssc.send(msg)
                last_renew = time.monotonic()
                ssc.log.note("renewed subscription")
            for arrived, parsed, raw in ssc.recv(0.25):
                t = arrived - start
                n += 1
                err = osc_error(parsed)
                if parsed is None:
                    print(f"{t:8.2f}s  RX (not JSON) {raw}")
                    continue
                if err is not None:
                    print(f"{t:8.2f}s  !! /osc/error {json.dumps(err)}")
                    if "310" in json.dumps(err):
                        terminated = (n, t)
                for path, value in leaves(parsed):
                    if path.startswith("/osc/error"):
                        continue
                    per_path[path] = per_path.get(path, 0) + 1
                    if not a.quiet:
                        print(f"{t:8.2f}s  {path} = {json.dumps(value)}")
            if terminated and not a.renew:
                break
    except KeyboardInterrupt:
        pass
    finally:
        took = time.monotonic() - start
        if not a.leave:
            cancel = json.loads(json.dumps(msg))
            cancel["osc"]["state"]["subscribe"][0]["#"] = {"cancel": True}
            ssc.send(cancel)
            print("\ncancel sent; reply:")
            print_replies(ssc.recv(0.5), raw=True)
            # Subscriptions are per client (per source port), so only this
            # socket can ask what it still holds.
            print("still held by this client:")
            print_replies(ssc.call({"osc": {"state": {"subscribe": None}}}, 0.5), raw=True)
    print(f"\n{n} datagrams in {took:.1f} s")
    for path, count in sorted(per_path.items()):
        print(f"   {path}: {count} ({count / max(took, 0.001):.2f}/s)")
    if terminated:
        print(f"subscription terminated (310) at datagram {terminated[0]}, {terminated[1]:.1f} s in")


ERROR_PROBES: list[tuple[str, str]] = [
    ("unknown leaf", '{"rx1":{"no_such_leaf":null}}'),
    ("unknown container", '{"no_such_container":{"x":null}}'),
    ("channel past the last", '{"rx9":{"mute":null}}'),
    ("mixed good and bad", '{"rx1":{"mute":null,"no_such_leaf":null}}'),
    ("malformed JSON", '{"rx1":{"mute":null}'),
    ("not an object", '[1,2,3]'),
    ("write to read-only vendor", '{"device":{"identity":{"vendor":"probe"}}}'),
    ("write to read-only rssi", '{"m":{"rx1":{"rssi":0}}}'),
    ("pattern, rx?", '{"rx?":{"name":null}}'),
    ("pattern, rx*", '{"rx*":{"gain":null}}'),
    ("subscribe, doc example form", '{"osc":{"state":{"subscribe":{["#":{"lifetime":5},"device":{"name":null}}]}}}'),
    ("subscribe, object not array", '{"osc":{"state":{"subscribe":{"#":{"lifetime":5},"device":{"name":null}}}}}'),
    ("subscriptions held", '{"osc":{"state":{"subscribe":null}}}'),
    ("ping", '{"osc":{"ping":null}}'),
    ("xid echo", '{"osc":{"xid":4242},"device":{"name":null}}'),
    ("schema of /rx1", '{"osc":{"schema":{"rx1":{"gain":null}}}}'),
    ("limits of /rx1/gain", '{"osc":{"limits":{"rx1":{"gain":null}}}}'),
    ("error state", '{"osc":{"error":null}}'),
]


def cmd_errors(ssc: Ssc, a: argparse.Namespace) -> None:
    for label, text in ERROR_PROBES:
        print(f"-- {label}\n   TX {text}")
        ssc.send_raw(text)
        for _, _, raw in ssc.recv(a.wait) or [(0, None, "(no reply)")]:
            print(f"   RX {raw}")


def cmd_clients(ssc: Ssc, a: argparse.Namespace) -> None:
    socks = []
    msg = {"osc": {"state": {"subscribe": [{"#": {"lifetime": a.lifetime}, "device": {"name": None}}]}}}
    for i in range(a.n):
        s = Ssc(a.host, a.port, ssc.log)
        socks.append(s)
        replies = s.call(msg, a.wait)
        verdict = "no reply"
        if replies:
            err = osc_error(replies[0][1])
            verdict = f"error {json.dumps(err)}" if err is not None else "accepted"
        print(f"client {i + 1} (local port {s.local[1]}): {verdict}")
        for _, _, raw in replies:
            print(f"   RX {raw}")
    if a.close:
        for i, s in enumerate(socks):
            s.send({"osc": {"state": {"close": None}}})
            print(f"closed client {i + 1}:")
            print_replies(s.recv(0.5), raw=True)
    for s in socks:
        s.close()


# ---------------------------------------------------------------- main

def main(argv: list[str] | None = None) -> None:
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0],
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--host", help="receiver IP (its control interface)")
    p.add_argument("--port", type=int, default=DEFAULT_PORT, help="SSC UDP port (default 45)")
    p.add_argument("--bind-port", type=int, default=0, help="local UDP port (default: any)")
    p.add_argument("--wait", type=float, default=1.0, help="seconds to wait for replies")
    p.add_argument("--log", help="JSONL log of every datagram (default ewdx-<time>.jsonl)")
    p.add_argument("--allow-rf", action="store_true", help="allow /rxN/frequency writes")
    sub = p.add_subparsers(dest="cmd", required=True)

    s = sub.add_parser("send", help="send one raw JSON message")
    s.add_argument("json")
    s.add_argument("--malformed", action="store_true", help="allow invalid JSON (error tests)")
    s.add_argument("--raw", action="store_true", help="print replies verbatim")

    s = sub.add_parser("get", help="read one or more paths")
    s.add_argument("paths", nargs="+")
    s.add_argument("--separate", action="store_true", help="one message per path")
    s.add_argument("--raw", action="store_true")

    s = sub.add_parser("set", help="write one path, show the reply and read it back")
    s.add_argument("path")
    s.add_argument("value", help="JSON (12, true, \"name\") or a bare string")

    s = sub.add_parser("snapshot", help="read every documented readable leaf to a JSON file")
    s.add_argument("-o", "--output", required=True)
    s.add_argument("--channels", type=int, help="receiver channels (default: from the product name)")
    s.add_argument("--no-dante", action="store_true", help="skip the Dante network leaves (EM 2)")

    s = sub.add_parser("diff", help="compare two snapshots (offline)")
    s.add_argument("a")
    s.add_argument("b")
    s.add_argument("--all", action="store_true", help="include meters, battery and warnings")

    s = sub.add_parser("restore", help="write back the restorable values in a snapshot")
    s.add_argument("snapshot")
    s.add_argument("--dry-run", action="store_true")

    s = sub.add_parser("sweep", help="step a control through its range, read back, restore")
    s.add_argument("what", choices=sorted(SWEEPS))
    s.add_argument("--channel", type=int, default=1)
    s.add_argument("--odd", action="store_true", help="add off-step, out-of-range and wrong-type values")
    s.add_argument("--pause", action="store_true", help="wait for Enter after each step")
    s.add_argument("--dwell", type=float, default=2.0, help="seconds per step without --pause")

    s = sub.add_parser("watch", help="subscribe and print notifications")
    s.add_argument("preset", choices=["adapter", "meters", "status", "paths"])
    s.add_argument("paths", nargs="*", help="SSC paths, with the 'paths' preset")
    s.add_argument("--channel", type=int, default=1)
    s.add_argument("--duration", type=float, default=60.0)
    s.add_argument("--lifetime", type=int)
    s.add_argument("--count", type=int)
    s.add_argument("--min", type=int, help="ms; see the plan's rate test for what it really does")
    s.add_argument("--max", type=int, help="ms; see the plan's rate test for what it really does")
    s.add_argument("--renew", type=float, help="re-send the subscription every N s")
    s.add_argument("--leave", action="store_true",
                   help="do not cancel the subscription on exit (the plan's leaked-slot test)")
    s.add_argument("--quiet", action="store_true", help="only the summary")

    sub.add_parser("errors", help="send the read-only error and edge-case probes")

    s = sub.add_parser("clients", help="open N clients, each subscribing, to find the client limit")
    s.add_argument("--n", type=int, default=9)
    s.add_argument("--lifetime", type=int, default=60)
    s.add_argument("--close", action="store_true", help="send /osc/state/close from each afterwards")

    a = p.parse_args(argv)
    if a.cmd == "diff":
        cmd_diff(a)
        return
    if not a.host:
        p.error("--host is required")
    log = Log(a.log)
    log.note("argv " + " ".join(sys.argv[1:] if argv is None else argv))
    print(f"logging to {log.path}", file=sys.stderr)
    ssc = Ssc(a.host, a.port, log, a.bind_port)
    try:
        {"send": cmd_send, "get": cmd_get, "set": cmd_set, "snapshot": cmd_snapshot,
         "restore": cmd_restore, "sweep": cmd_sweep, "watch": cmd_watch,
         "errors": cmd_errors, "clients": cmd_clients}[a.cmd](ssc, a)
    finally:
        ssc.close()


if __name__ == "__main__":
    main()
