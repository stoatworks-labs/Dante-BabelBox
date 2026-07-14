# Usage Guide

This guide covers what the bridge does, how it works internally, and how
to configure and run it. For the current implementation status by vendor,
see the table in [README.md](README.md).

## What it does

Dante carries audio between devices on the same network and handles basic
device discovery (via mDNS), but it says nothing about preamp gain or
phantom power — each console vendor layers its own proprietary control
protocol on top of the same Dante network. If you mix gear from two
vendors on one Dante network (say, an Allen & Heath stagebox feeding a
Behringer X32 for monitors), turning a gain knob on one console does
**not** update the matching channel on the other — they simply don't
speak the same control language.

`preamp-bridge` sits on the network as a translator: it connects to each
console/processor using its native control protocol, listens for gain and
phantom-power changes (from a physical knob, an on-screen UI, or another
app), and replays that same change on whichever device(s) you've mapped
it to — regardless of vendor.

It is **not** a device emulator. It doesn't make itself appear as a
native device inside another console's own UI — you configure mappings
by IP address and channel number in a config file, and it acts as a
router in the middle. See the README's "Not yet built" section for what
true device emulation would take.

## How it works

```
crates/
├── core/                    # shared AdapterError/DeviceInfo + preamp Router/types
├── discovery/                # mDNS-based Dante device discovery
├── preamp-adapter-osc/        # X32-family + Wing (Behringer/Midas OSC dialects)
├── preamp-adapter-ah/         # AHM TCP/IP + dLive MIDI-over-TCP (Allen & Heath)
├── preamp-adapter-yamaha/     # DM3 OSC
└── preamp-cli/                # `preamp-bridge` binary: discover, run, config, hot-reload
```

1. **Adapters** — one per vendor protocol. Each implements a common
   `DeviceAdapter` trait (`connect`, `set_gain`, `set_phantom`,
   `get_state`, and a `subscribe` stream of state-change events observed
   on the wire). An adapter's job is purely protocol translation: turning
   generic gain/phantom values into that vendor's specific bytes, and
   back.
2. **Router** — holds your mapping table (from `bridge.toml`) and, when
   an adapter reports a state change on a device, pushes that change out
   to every device mapped to it. It tracks the last value it pushed to
   each address so a device's own confirmation of a command the bridge
   just sent isn't mistaken for a fresh independent change and echoed
   back and forth forever between two bidirectionally-mapped devices.
3. **Discovery** — a separate, optional step that browses Dante's own
   mDNS advertisements to show you what Dante devices exist on the LAN.
   Dante's discovery only confirms an address exists; it carries no
   vendor/model/protocol information, so you still tell the bridge what
   kind each device is in `bridge.toml`.
4. **CLI** — the `preamp-bridge` binary wraps all of this with two
   subcommands, `discover` and `run` (below).

## Building

```sh
cargo build --workspace
cargo test --workspace     # 27 tests, all against mock devices - no hardware required
```

## Commands

### `preamp-bridge discover`

Browses Dante's mDNS advertisements for a fixed window and prints every
device it finds (name, addresses, port). Useful for finding IPs before
writing your config.

```sh
cargo run --bin preamp-bridge -- discover
cargo run --bin preamp-bridge -- discover --timeout-secs 10   # default is 5
```

### `preamp-bridge run`

Loads a config file, connects to every configured device, wires them
through the router according to your mappings, and runs until you hit
Ctrl-C.

```sh
cp bridge.example.toml bridge.toml   # edit for your rig
cargo run --bin preamp-bridge -- run --config bridge.toml   # --config defaults to bridge.toml
```

On startup it prints every device and mapping it loaded, then connects to
each device in turn — if a device is unreachable, it fails fast with the
device id and address rather than starting a partially-connected bridge.

While running, editing `bridge.toml`'s `[[mapping]]` entries **hot-reloads
live** — no restart needed. Adding, removing, or editing `[[device]]`
entries does **not** hot-reload; restart the bridge for those. A config
edit that fails to parse is logged as a warning and the previous, still
valid mapping table keeps running rather than tearing the bridge down.

## Configuring `bridge.toml`

Copy [`bridge.example.toml`](bridge.example.toml) as a starting point.
The file has two kinds of blocks: `[[device]]` (what's on the network)
and `[[mapping]]` (what should track what).

### `[[device]]`

```toml
[[device]]
id = "ahm-rack"       # your own label, referenced by mappings below
kind = "ah-tcp"        # see the kind table below
address = "10.0.0.10"  # IP on the Dante/control network
port = 51325           # optional - each kind has a sensible default
```

Implemented `kind` values:

| `kind` | Vendor / device | Protocol | Default port |
|---|---|---|---|
| `osc-x32` | Behringer X32, Wing (X32 dialect), Midas M32/HD96 | OSC/UDP | 10023 |
| `osc-wing` | Behringer Wing, native dialect | OSC/UDP | 2223 |
| `ah-tcp` | Allen & Heath AHM-series processors | NRPN-over-TCP | 51325 |
| `dlive-tcp` | Allen & Heath dLive MixRack | MIDI-over-TCP | 51325 |
| `yamaha-dm3` | Yamaha DM3 / DM3S | OSC/UDP | 49900 |

`ah-midi` (Qu/SQ) and `yamaha` (CL/QL/DM7, Rio/Tio) are recognized by the
config parser but rejected at startup with an explanation — no public
protocol spec documents preamp control for those, so nothing is
implemented for them yet (see README's status table).

### `[[mapping]]`

```toml
[[mapping]]
from = { device = "ahm-rack", channel = 3 }
to   = { device = "x32-monitors", channel = 7 }
bidirectional = true    # optional, defaults to false (one-way: from -> to)
```

`device` must match a `[[device]] id`. `channel` means whatever that
protocol's native addressing unit is — **it is not always a mixing
channel number**:

- **X32-family (`osc-x32`)**: headamp index 1–24 (1–8 local XLR, 9–16
  AES50-A, 17–24 AES50-B).
- **Wing (`osc-wing`)**: 1–8, the console's own built-in LCL preamps
  only — AES50/StageConnect-attached stageboxes aren't covered yet.
- **AHM (`ah-tcp`)**: 1-based channel index as configured on the unit.
- **dLive (`dlive-tcp`)**: a physical preamp **Socket** number (1–128),
  *not* a processing channel — dLive keeps a socket's gain/phantom tied
  to the physical input regardless of what mixer channel it's patched
  to. Socket numbering: MixRack 1–64, DX1/2 65–96, DX3/4 97–128.
- **DM3 (`yamaha-dm3`)**: Local Input Num, 1–16. Gain on this protocol is
  coarse whole-dB steps only (0–64 dB) — a real limitation of Yamaha's
  spec, not something the bridge can improve on.

You can chain more than one mapping off the same device/channel to fan a
single preamp's changes out to several peers, and mark any mapping
`bidirectional = true` to let either side drive the other (with echo
suppression preventing feedback loops).

## Troubleshooting

- **Bridge exits immediately with a "connecting to device" error** — the
  configured `address`/`port` is unreachable. Confirm the device is
  actually on that network and IP (use `discover` if unsure), and that
  nothing else already holds an exclusive control connection (Wing in
  particular only allows one OSC subscriber at a time — its own
  official app will silently displace this bridge's subscription if
  both are connected).
- **`ah-midi` / `yamaha` device rejected at startup** — expected; those
  vendor lines have no public preamp-control spec to implement against.
  See README's status table for the exact gap per device.
- **Mapping edits don't take effect** — only `[[mapping]]` changes
  hot-reload; adding/removing/editing a `[[device]]` block needs a
  restart.
- **Gain values look rounded/coarser than expected on one side** — check
  whether either mapped device has a coarser native resolution than the
  other (e.g. DM3's integer-dB-only `HAGain`); the bridge relays the
  value as reported, it doesn't interpolate between differing
  resolutions.

## Contributing a new adapter

See the "Contributing a new adapter" section in [README.md](README.md) —
in short: implement strictly against an official or community-authoritative
protocol spec, not guessed wire framing, with unit tests against the
spec's own worked examples and an integration test through the `Router`.
