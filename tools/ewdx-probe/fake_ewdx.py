#!/usr/bin/env python3
"""fake_ewdx - a spec-shaped stand-in for an EW-DX EM, for dry-running ewdx_probe.

**Nothing this fake says is evidence.** It exists so the bench sequence in
`docs/sennheiser-ewdx-bench-plan.md` can be rehearsed, and `ewdx_probe.py` and
`mic-monitor watch` exercised, before a receiver is on the bench. Where the
EW-DX guide gives a value, a range or a rule, the fake follows it. Where the
guide is silent, the fake guesses, and every guess is marked GUESS below:

  * GUESS: the shape of an error reply (the guide lists codes, not the JSON).
  * GUESS: an adapted off-step value rounds to the nearest step.
  * GUESS: a subscription is acknowledged by echoing the request, and the
    initial notification follows as its own datagram.
  * GUESS: meters change every 50 ms while the transmitter is on.
  * GUESS: a client is one source (IP, port) pair.

Followed from the guide: UDP, one JSON object per datagram; null reads;
read-only leaves answer with their value; /rxN/gain -3..+42 dB in 3 dB steps;
/audio1/outN/level -24..+18 dB in 6 dB steps; names 8 (rx) and 18 (device)
characters; mates paths answer 424 while the transmitter is off; subscription
lifetime default 10 s, count default 1000, 310 when either runs out; at most
eight subscribing clients; min/max rate options.

Run:
    python3 tools/ewdx-probe/fake_ewdx.py                  # 127.0.0.1:4545, EM 2 Dante
    python3 tools/ewdx-probe/fake_ewdx.py --channels 4 --product "EW-DX EM 4 Dante"
    kill -USR1 <pid>                                       # transmitter 1 on/off
"""

from __future__ import annotations

import argparse
import json
import math
import random
import signal
import socket
import threading
import time
from typing import Any

MAX_CLIENTS = 8


def leaves(node: Any, prefix: str = ""):
    if isinstance(node, dict) and node:
        for k, v in node.items():
            yield from leaves(v, f"{prefix}/{k}")
    else:
        yield prefix or "/", node


def tree(path: str, value: Any) -> dict:
    node: Any = value
    for part in reversed([p for p in path.split("/") if p]):
        node = {part: node}
    return node


def merge(a: dict, b: dict) -> dict:
    out = dict(a)
    for k, v in b.items():
        out[k] = merge(out[k], v) if isinstance(out.get(k), dict) and isinstance(v, dict) else v
    return out


class Leaf:
    def __init__(self, value, writable=False, lo=None, hi=None, step=None, length=None, mate=False):
        self.value, self.writable = value, writable
        self.lo, self.hi, self.step, self.length, self.mate = lo, hi, step, length, mate

    def adapt(self, v):
        if isinstance(self.value, bool):
            return v if isinstance(v, bool) else self.value
        if isinstance(self.value, (int, float)) and not isinstance(self.value, bool):
            if not isinstance(v, (int, float)) or isinstance(v, bool):
                return self.value
            if self.step:
                v = self.lo + round((v - self.lo) / self.step) * self.step  # GUESS: nearest step
            return max(self.lo, min(self.hi, v)) if self.lo is not None else v
        if isinstance(self.value, str):
            return str(v)[: self.length] if self.length else str(v)
        return self.value


class Fake:
    def __init__(self, product: str, channels: int):
        self.lock = threading.Lock()
        self.tx_on = {ch: True for ch in range(1, channels + 1)}
        self.leaves: dict[str, Leaf] = {}
        d = self._add
        d("/device/identity/vendor", Leaf("Sennheiser electronic SE & CO KG"))
        d("/device/identity/product", Leaf(product))
        d("/device/identity/version", Leaf("3.0.0-fake"))
        d("/device/identity/serial", Leaf(""))
        d("/device/name", Leaf("FAKE-EWDX", True, length=18))
        d("/device/location", Leaf("", True, length=400))
        d("/device/lock", Leaf(False, True))
        d("/device/brightness", Leaf(5, True, 1, 5, 1))
        d("/device/identification/visual", Leaf(False, True))
        d("/device/frequency_code", Leaf("Q1-9"))
        d("/device/frequency_ranges", Leaf(["470000000:25000:550000000"]))
        d("/device/preset_spacing", Leaf(300))
        d("/device/link_density_mode", Leaf(False, True))
        d("/device/encryption", Leaf(False, True))
        d("/device/network/ipv4/ipaddr", Leaf(["127.0.0.1"]))
        d("/device/network/dante/ipv4/ipaddr", Leaf(["169.254.1.2", "0.0.0.0"]))
        d("/device/network/dante/interface_mapping", Leaf("SINGLE_CABLE", True))
        d("/osc/version", Leaf("1.0"))
        d("/osc/feature/subscription", Leaf(True))
        d("/osc/feature/pattern", Leaf("*?"))
        for ch in range(1, channels + 1):
            rx, tx = f"/rx{ch}", f"/mates/tx{ch}"
            d(f"{rx}/name", Leaf(f"CH {ch}", True, length=8))
            d(f"{rx}/mute", Leaf(False, True))
            d(f"{rx}/gain", Leaf(24, True, -3, 42, 3))
            d(f"{rx}/frequency", Leaf(520000 + 300 * ch, True, 470200, 1999000, 25))
            d(f"{rx}/warnings", Leaf([]))
            d(f"{rx}/mates", Leaf([f"mates/tx{ch}"]))
            d(f"{rx}/audio", Leaf([f"audio1/out{ch}"]))
            d(f"/audio1/out{ch}/level", Leaf(0, True, -24, 18, 6))
            d(f"/m/rx{ch}/rssi", Leaf(-55.0))
            d(f"/m/rx{ch}/rsqi", Leaf(95))
            d(f"/m/rx{ch}/divi", Leaf(1))
            d(f"/m/rx{ch}/af", Leaf(-40.0))
            d(f"{tx}/battery/gauge", Leaf(80, mate=True))
            d(f"{tx}/battery/lifetime", Leaf(420, mate=True))
            d(f"{tx}/battery/type", Leaf("Battery", mate=True))
            d(f"{tx}/type", Leaf("SKM", mate=True))
            d(f"{tx}/name", Leaf(f"CH {ch}", mate=True))
            d(f"{tx}/mute", Leaf(False, mate=True))
            d(f"{tx}/warnings", Leaf([], mate=True))
        # client (ip, port) -> {path: sub}; sub = dict(min,max,count,lifetime,start,sent,last,lastval)
        self.clients: dict[tuple, dict[str, dict]] = {}

    def _add(self, path, leaf):
        self.leaves[path] = leaf

    def read(self, path):
        leaf = self.leaves.get(path)
        if leaf is None:
            return None, 404
        ch = path.split("/")[2][2:] if path.startswith("/mates/tx") else None
        if leaf.mate and ch and not self.tx_on.get(int(ch), False):
            return None, 424
        return leaf.value, None

    def handle(self, raw: bytes, client: tuple) -> list[dict]:
        try:
            msg = json.loads(raw)
        except json.JSONDecodeError:
            return [self.error_reply({}, 400)]
        if not isinstance(msg, dict):
            return [self.error_reply({}, 400)]
        out: dict = {}
        errors: dict = {}
        extra: list[dict] = []
        with self.lock:
            sub = msg.get("osc", {}).get("state", {}).get("subscribe", KeyError) if isinstance(msg.get("osc"), dict) else KeyError
            if sub is not KeyError:
                return self.subscribe(sub, client)
            for path, value in leaves(msg):
                if path == "/osc/ping":
                    out = merge(out, tree(path, None))
                    continue
                if path == "/osc/xid":
                    out = merge(out, tree(path, value))
                    continue
                if path == "/osc/state/close":
                    self.clients.pop(client, None)
                    out = merge(out, tree(path, None))
                    continue
                leaf = self.leaves.get(path)
                if leaf is None:
                    errors = merge(errors, tree(path, [404, {"desc": "address not found"}]))
                    continue
                if value is not None and leaf.writable:
                    leaf.value = leaf.adapt(value)
                current, err = self.read(path)
                if err:
                    errors = merge(errors, tree(path, [err, {"desc": "failed dependency"}]))
                else:
                    out = merge(out, tree(path, current))
        if errors:
            out = merge(out, self.error_reply(errors, 210 if out else None))
        return [out] + extra

    @staticmethod
    def error_reply(addresses: dict, code: int | None) -> dict:
        # GUESS: the EW-DX guide lists error codes but never shows the JSON.
        if not addresses:
            return {"osc": {"error": [code, {"desc": "message not understood"}]}}
        body: list = [addresses]
        if code:
            body = [code, {"failed_addresses": [addresses]}]
        return {"osc": {"error": body}}

    def subscribe(self, sub, client) -> list[dict]:
        if sub is None:
            held: dict = {}
            for p in self.clients.get(client, {}):
                held = merge(held, tree(p, None))
            return [{"osc": {"state": {"subscribe": [held]}}}]
        if not isinstance(sub, list) or len(sub) != 1 or not isinstance(sub[0], dict):
            return [self.error_reply({}, 400 if not isinstance(sub, list) else 414)]
        item = dict(sub[0])
        opts = item.pop("#", {}) or {}
        active = [c for c, held in self.clients.items() if held]
        if client not in active and len(active) >= MAX_CLIENTS:
            return [{"osc": {"error": [503, {"desc": "too many subscribing clients"}]}}]  # GUESS: code
        subs = self.clients.setdefault(client, {})
        now = time.monotonic()
        initial: dict = {}
        errors: dict = {}
        for path, _ in leaves(item):
            if path not in self.leaves:
                errors = merge(errors, tree(path, [404, {"desc": "address not found"}]))
                continue
            if opts.get("cancel"):
                subs.pop(path, None)
                continue
            value, err = self.read(path)
            subs[path] = {"min": opts.get("min", 0), "max": opts.get("max", 0),
                          "count": opts.get("count", 1000), "lifetime": opts.get("lifetime", 10),
                          "start": now, "sent": 1, "last": now, "lastval": value}
            if err:
                errors = merge(errors, tree(path, [err, {"desc": "failed dependency"}]))
            else:
                initial = merge(initial, tree(path, value))
        ack = {"osc": {"state": {"subscribe": [dict({"#": opts} if opts else {}, **item)]}}}
        if errors:
            ack = merge(ack, self.error_reply(errors, 210))
        return [ack] + ([initial] if initial else [])

    def tick(self) -> list[tuple[tuple, dict]]:
        """Move the meters, then build the notifications that are due."""
        out = []
        now = time.monotonic()
        with self.lock:
            for ch, on in self.tx_on.items():
                m = f"/m/rx{ch}"
                if on:
                    self.leaves[f"{m}/rssi"].value = round(-55 + 8 * math.sin(now / 3 + ch) + random.uniform(-1, 1), 1)
                    self.leaves[f"{m}/af"].value = round(-30 + 15 * math.sin(now * 2 + ch) + random.uniform(-3, 3), 1)
                    self.leaves[f"{m}/rsqi"].value = random.choice([93, 94, 95, 96])
                    self.leaves[f"{m}/divi"].value = 1 if math.sin(now / 5 + ch) > 0 else 2
                else:
                    self.leaves[f"{m}/rssi"].value = -107.0
                    self.leaves[f"{m}/af"].value = -138.5
                    self.leaves[f"{m}/rsqi"].value = 0
                    self.leaves[f"{m}/divi"].value = 0
                self.leaves[f"/rx{ch}/warnings"].value = [] if on else ["NoLink"]
            for client, subs in list(self.clients.items()):
                bundle: dict = {}
                errs: dict = {}
                ended: dict = {}
                for path, s in list(subs.items()):
                    value, err = self.read(path)
                    changed = value != s["lastval"]
                    due = False
                    if s["max"] and now - s["last"] < s["max"] / 1000:
                        due = False
                    elif changed:
                        due = True
                    elif s["min"] and now - s["last"] >= s["min"] / 1000:
                        due = True
                    if not due:
                        continue
                    s["last"], s["lastval"], s["sent"] = now, value, s["sent"] + 1
                    if err:
                        errs = merge(errs, tree(path, [err, {"desc": "failed dependency"}]))
                    else:
                        bundle = merge(bundle, tree(path, value))
                    if s["sent"] >= s["count"] or now - s["start"] >= s["lifetime"]:
                        ended = merge(ended, tree(path, [310, {"desc": "subscription terminated"}]))
                        del subs[path]
                for path, s in list(subs.items()):
                    if now - s["start"] >= s["lifetime"]:
                        ended = merge(ended, tree(path, [310, {"desc": "subscription terminated"}]))
                        del subs[path]
                if not subs:
                    del self.clients[client]
                if bundle:
                    out.append((client, bundle))
                if errs:
                    out.append((client, self.error_reply(errs, None)))
                if ended:
                    out.append((client, {"osc": {"error": [ended]}}))
        return out


def main() -> None:
    p = argparse.ArgumentParser(description="spec-shaped EW-DX EM stand-in (not evidence)")
    p.add_argument("--bind", default="127.0.0.1")
    p.add_argument("--port", type=int, default=4545)
    p.add_argument("--product", default="EW-DX EM 2 Dante")
    p.add_argument("--channels", type=int, default=2)
    p.add_argument("--quiet", action="store_true")
    a = p.parse_args()
    fake = Fake(a.product, a.channels)
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind((a.bind, a.port))
    print(f"fake {a.product} on udp {a.bind}:{a.port} - NOT EVIDENCE; kill -USR1 toggles tx1", flush=True)

    def toggle(*_):
        with fake.lock:
            fake.tx_on[1] = not fake.tx_on[1]
        print(f"tx1 {'on' if fake.tx_on[1] else 'OFF'}", flush=True)

    signal.signal(signal.SIGUSR1, toggle)

    def ticker():
        while True:
            time.sleep(0.05)
            for client, msg in fake.tick():
                sock.sendto(json.dumps(msg, separators=(",", ":")).encode(), client)

    threading.Thread(target=ticker, daemon=True).start()
    while True:
        data, client = sock.recvfrom(65535)
        if not a.quiet:
            print(f"<- {client[1]} {data.decode(errors='replace')[:200]}", flush=True)
        for reply in fake.handle(data, client):
            sock.sendto(json.dumps(reply, separators=(",", ":")).encode(), client)


if __name__ == "__main__":
    main()
