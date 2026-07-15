# Usage Guide

This guide covers what's in this workspace, how it works internally, and
how to configure and run each part. For the current implementation status
by vendor, see the tables in [README.md](README.md).

There are two independent tools here, sharing the same workspace and
`dante-babelbox-discovery` crate but otherwise unrelated: `preamp-bridge`
(preamp gain/phantom control) below, and `mic-monitor` (radio-mic
telemetry) in the "mic-monitor — Radio Mic Telemetry" section further
down.

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
├── discovery/                # mDNS-based Dante device discovery + Dante's own routing-observation protocol
├── preamp-adapter-osc/        # X32-family + Wing (Behringer/Midas OSC dialects)
├── preamp-adapter-ah/         # AHM TCP/IP + dLive MIDI-over-TCP (Allen & Heath)
├── preamp-adapter-yamaha/     # DM3 OSC
├── preamp-web/                # Patch-bay web UI + device/mapping management API (axum)
└── preamp-cli/                # `preamp-bridge` binary: discover, init, run, config, hot-reload
```

1. **Adapters** — one per vendor protocol. Each implements a common
   `DeviceAdapter` trait (`connect`, `disconnect`, `set_gain`,
   `set_phantom`, `get_state`, and a `subscribe` stream of state-change
   events observed on the wire). An adapter's job is purely protocol
   translation: turning generic gain/phantom values into that vendor's
   specific bytes, and back. `disconnect()` tears down the adapter's
   background socket task via a cancellation token, so removing a live
   device (via the web UI) actually frees its port rather than just
   dropping it from a list.
2. **Router** — holds your mapping table (from `bridge.toml`, or added
   live via the web UI) and, when an adapter reports a state change on a
   device, pushes that change out to every device mapped to it. It
   tracks the last value it pushed to each address so a device's own
   confirmation of a command the bridge just sent isn't mistaken for a
   fresh independent change and echoed back and forth forever between
   two bidirectionally-mapped devices. Devices and mappings can be
   registered/deregistered while the Router is already running.
3. **Discovery** — a separate, optional step that browses Dante's own
   mDNS advertisements to show you what Dante devices exist on the LAN.
   Dante's discovery only confirms an address exists; it carries no
   vendor/model/protocol information, so `init` (below) probes each
   found address against every implemented adapter's `identify()` to
   figure out what it actually is. A second, distinct protocol
   (`crates/discovery/src/dante_control.rs`) can also observe Dante's
   own audio-routing/subscription state, which `init --infer-mappings`
   uses to guess `[[mapping]]` entries from live patching.
4. **CLI** — the `preamp-bridge` binary wraps all of this with three
   subcommands, `discover`, `init`, and `run` (below). `run` also serves
   the patch-bay web UI alongside the bridge.

## Building

```sh
cargo build --workspace
cargo test --workspace     # 100 tests (both preamp-bridge and mic-monitor), no hardware required
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

### `preamp-bridge init`

Discovers devices via mDNS and probes each address against every
implemented adapter's `identify()` until one claims it, then writes the
`[[device]]` blocks of a `bridge.toml` for you.

```sh
cargo run --bin preamp-bridge -- init --output bridge.toml
cargo run --bin preamp-bridge -- init --output bridge.toml --force   # overwrite an existing file
```

`[[mapping]]` entries are **not** generated by default — add those by
hand, or pass `--infer-mappings` to have `init` observe live Dante audio
routing (which RX channel is subscribed to which TX channel/device) and
write a first draft of the mapping table from it:

```sh
cargo run --bin preamp-bridge -- init --output bridge.toml --infer-mappings
```

This is a real signal (it comes from watching live patching), not a
guess, but the channel numbers it writes are *Dante audio channel
numbers* — whether that's the same as the preamp/headamp channel number
a vendor adapter addresses is a default-configuration convention on most
gear (1:1 local I/O order), not a protocol guarantee. The written file's
header comment calls this out; treat inferred mappings as a first draft
to verify, not a final answer.

### `preamp-bridge run`

Loads a config file, connects to every configured device, wires them
through the router according to your mappings, and runs until you hit
Ctrl-C.

```sh
cp bridge.example.toml bridge.toml   # edit for your rig, or use `init` above
cargo run --bin preamp-bridge -- run --config bridge.toml   # --config defaults to bridge.toml
```

On startup it prints every device and mapping it loaded, then connects to
each device in turn — if a device is unreachable, it fails fast with the
device id and address rather than starting a partially-connected bridge.

While running, editing `bridge.toml`'s `[[mapping]]` entries **hot-reloads
live** from the file — no restart needed. Adding, removing, or editing
`[[device]]` entries in the *file* does **not** hot-reload; restart the
bridge for those. A config edit that fails to parse is logged as a
warning and the previous, still valid mapping table keeps running rather
than tearing the bridge down.

Separately from file-based hot-reload, `run` also serves a **patch-bay
web UI** at `http://0.0.0.0:8080` by default (`--web-bind` to change,
`--no-web` to disable) where devices (real or virtual placeholders for
the not-yet-built emulation layer) and mappings can be added and removed
live, with no restart at all — a line-art rack-strip "Patch" view
(sources left, destinations right, click two channels to connect them)
plus a mapped-only "Crosspoint" grid as a secondary tab. No auth or TLS —
same trust model as a hardware router's control port, meant for a
trusted operations network. State added through the UI is in-memory only
(an "export as TOML" button lets you paste it into `bridge.toml` to keep
it across a restart).

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

A device can also be declared `virtual = true` — a placeholder for the
not-yet-built device-emulation layer, with no `address` (there's nothing
real to dial yet) and a `channels` count instead:

```toml
[[device]]
id = "future-x32"
kind = "osc-x32"    # the protocol this virtual device will eventually emulate
virtual = true
channels = 8         # required when `kind` has no documented default channel count
```

Virtual devices can be mapped against real ones like any other device -
useful for designing the intended topology ahead of the emulation itself
existing (or via the patch-bay web UI's "virtual device" option).

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
- **Editing `bridge.toml`'s `[[device]]` blocks doesn't take effect** —
  expected; only `[[mapping]]` changes in the *file* hot-reload. Use the
  patch-bay web UI to add/remove devices live instead, or restart for a
  file-based `[[device]]` edit to take effect.
- **Can't remove a physical device from the web UI** — real device
  removal now works (it calls the adapter's `disconnect()`, which
  releases its port) - if it's failing, check the bridge's logs for the
  actual error rather than assuming it's unsupported.
- **Gain values look rounded/coarser than expected on one side** — check
  whether either mapped device has a coarser native resolution than the
  other (e.g. DM3's integer-dB-only `HAGain`); the bridge relays the
  value as reported, it doesn't interpolate between differing
  resolutions.

## `mic-monitor` — Radio Mic Telemetry

### What it does

`mic-monitor` connects to wireless-mic receivers over their native IP
control channel and prints live telemetry — battery level, RF signal,
audio level, mute state, frequency — regardless of vendor. Unlike
`preamp-bridge`, it doesn't route anything between devices; it's pure
monitoring. It also doesn't care whether the receiver has a Dante audio
card installed, since it never touches the audio path, only the control
channel.

### How it works

```
crates/
├── mic-core/                   # MicAdapter trait, MicState/MicEvent types
├── mic-adapter-shure/           # ULX-D + Axient Digital (ASCII over TCP 2202)
├── mic-adapter-sennheiser/      # EW-DX EM (JSON/SSC over UDP 45)
└── mic-cli/                     # `mic-monitor` binary: discover, watch
```

`MicAdapter` is a separate trait from `DeviceAdapter` (see
`crates/mic-core/src/adapter.rs`'s doc comment for why) — telemetry is
read-heavy, with `mute` as the only realistic write, so there's no
`Router`/mapping concept here, just a per-channel `get_state` and a
`subscribe` stream of updates.

### Commands

#### `mic-monitor discover`

Same Dante mDNS browse as `preamp-bridge discover` (they share the
`dante-babelbox-discovery` crate) — a convenience for finding Dante-
advertised devices, not a requirement. Non-Dante hardware, or hardware
discovery hasn't found yet, is configured directly by IP in `mics.toml`.

```sh
cargo run --bin mic-monitor -- discover
```

#### `mic-monitor watch`

Connects every configured mic, then prints a line per telemetry update
as it arrives:

```sh
cp mics.example.toml mics.toml   # edit for your rig
cargo run --bin mic-monitor -- watch --config mics.toml   # --config defaults to mics.toml
```

```
ulxd-foh ch1: battery=82% runtime=300min rf=-45dBm quality=n/a af=n/a antenna=A freq=614.125MHz mute=false
ewdx-1 ch1: battery=72% runtime=245min rf=-50dBm quality=88% af=-18dBFS antenna=A freq=614.125MHz mute=false
```

Fields a vendor doesn't report show as `n/a` rather than a fabricated
value — e.g. Shure's `quality`/`af` are always `n/a` since Shure's
protocol doesn't document a signal-quality indicator or a calibrated
dBFS conversion for its raw audio meter (see `mic-adapter-shure`'s module
doc comment).

On startup, channels 1-4 are proactively probed for each device so
Sennheiser receivers (which need a per-channel subscribe to start
sending telemetry) begin flowing data; a channel a smaller unit doesn't
have just logs a debug-level "not available" and is otherwise ignored.

### Configuring `mics.toml`

Copy [`mics.example.toml`](mics.example.toml) as a starting point:

```toml
[[mic]]
id = "ulxd-1"          # your own label
kind = "shure-ulxd"     # shure-ulxd | shure-axient | sennheiser-ewdx
address = "10.0.0.30"   # IP on the control network
port = 2202              # optional - defaults to 2202 (Shure) or 45 (Sennheiser)
```

No `[[mapping]]` section — nothing to route between mics.

### Troubleshooting

- **`quality`/`af` always show `n/a` for a Shure mic** — expected; Shure's
  protocol doesn't document those fields the way Sennheiser's does. See
  `mic-adapter-shure`'s module doc comment.
- **A Sennheiser channel never reports anything** — subscriptions there
  default to a 1-hour lifetime (requested explicitly in `connect()`); a
  `watch` session running longer than that would need re-subscription,
  which isn't implemented yet.
- **`watch` connects but nothing ever prints** — confirm the configured
  `address`/`port` actually matches the device's control port (2202 for
  Shure, 45 for Sennheiser unless changed), and that nothing else already
  holds an exclusive connection to it.

## Contributing a new adapter

See the "Contributing a new adapter" section in [README.md](README.md) —
in short: implement strictly against an official or community-authoritative
protocol spec, not guessed wire framing, with unit tests against the
spec's own worked examples and an integration test through a mock socket.
