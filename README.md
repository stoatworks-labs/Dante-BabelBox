# Dante-BabelBox

> **AI-assisted project.** This codebase was created with [Claude](https://claude.com/claude-code)
> (Anthropic). Adapters are built against official/community-authoritative
> vendor protocol specs (see each adapter's module doc comment for its
> source), but this has **not been validated against real hardware** —
> only against mock devices in the test suite. Review before use on live
> gear.

Cross-vendor Dante control bridge, currently covering two domains:

1. **Preamp control** — bridges gain/phantom-power control across
   Dante-networked mixing consoles and stageboxes from different vendors.
2. **Radio-mic telemetry** — monitors battery, RF signal, and audio level
   from wireless mic receivers, across vendors, whether or not the
   hardware even has a Dante audio option installed (see the "Radio Mic
   Telemetry" sections below for why that doesn't matter here).

Dante carries audio and basic mDNS-based device discovery, but nothing
about preamp gain, phantom power, or wireless-mic status — each vendor
layers its own proprietary control protocol on top of the same network.
This project translates those protocols so state on one vendor's device
is usable from outside its own ecosystem.

## Preamp Control — Status

| Vendor | Device | Protocol | Status |
|---|---|---|---|
| Behringer / Midas | X32, Wing, M32, HD96 | OSC | X32 family done; Wing done (8 built-in preamps only) |
| Allen & Heath | AHM-series processors | NRPN-over-TCP | Done |
| Allen & Heath | dLive | NRPN-over-TCP (Socket addressing) | Done |
| Allen & Heath | Qu, SQ | — | Not implemented — no public preamp-control spec exists |
| Yamaha | DM3 / DM3S | OSC | Done |
| Yamaha | DM7, CL, QL | — | Not implemented — no public spec (see below) |
| Yamaha | Rio, Tio | Legacy AD8HR (MIDI SysEx) | Not implemented — setup docs exist, wire format doesn't |

Every "Done" adapter is built from an official vendor spec (or, for the
X32 family, the long-established community reference), not guesswork, and
is unit- and integration-tested against mock devices standing in for the
real protocol. See each adapter's module doc comment
(`crates/preamp-adapter-*/src/*.rs`) for the exact spec it's built from and any
open gaps.

**Not yet built:** device emulation (making the bridge answer as if it
*is* a native device of a foreign brand, so a console's own on-screen
preamp UI can control it directly). Today the bridge is a translating
router you configure by IP/channel — genuinely useful, but not yet
"invisible" to the consoles. This needs real hardware to build safely,
since it means impersonating a device's discovery/pairing handshake
closely enough that real gear accepts it.

Building that requires packet captures of a real console paired with its
own native device (e.g. a real Yamaha QL1 talking to a real Rio/Tio, not
a foreign-vendor box) — none of that handshake is in any public spec.
[`docs/`](docs/) has a non-technical field guide, one edition per OS, for
capturing that traffic with Wireshark using nothing but a laptop as an
inline bridge:

- [Windows](docs/capture-guide-windows.md) ([PDF](docs/capture-guide-windows.pdf), [HTML](docs/capture-guide-windows.html))
- [macOS](docs/capture-guide-macos.md) ([PDF](docs/capture-guide-macos.pdf), [HTML](docs/capture-guide-macos.html))
- [Linux](docs/capture-guide-linux.md) ([PDF](docs/capture-guide-linux.pdf), [HTML](docs/capture-guide-linux.html))

## Preamp Control — Architecture

```
crates/
├── core/                    # shared AdapterError/DeviceInfo + preamp Router/types
├── discovery/                # mDNS-based Dante device discovery
├── preamp-adapter-osc/        # X32-family + Wing (Behringer/Midas OSC dialects)
├── preamp-adapter-ah/         # AHM TCP/IP + dLive MIDI-over-TCP (Allen & Heath)
├── preamp-adapter-yamaha/     # DM3 OSC
└── preamp-cli/                # `preamp-bridge` binary: discover, run, config, hot-reload
```

Each adapter implements `core::DeviceAdapter` (connect, set_gain,
set_phantom, get_state, subscribe to state-change events). The `Router`
holds a mapping table (`bridge.toml`) and fans state-change events from
one device out to its mapped peer(s), with echo suppression so a device's
own confirmation of a command doesn't bounce back and forth forever
between bidirectionally-mapped devices.

## Preamp Control — Building and running

```sh
cargo build --workspace
cargo test --workspace     # 45 tests (both domains), all against mock devices - no hardware required

# Browse Dante's mDNS advertisements for devices on the LAN
cargo run --bin preamp-bridge -- discover

# Run the bridge daemon
cp bridge.example.toml bridge.toml   # edit for your rig
cargo run --bin preamp-bridge -- run --config bridge.toml
```

Editing `bridge.toml` while the bridge is running hot-reloads the mapping
table (not the device list — adding or removing a device still needs a
restart).

## Preamp Control — Config format

See [`bridge.example.toml`](bridge.example.toml) for a worked example
covering all four implemented device kinds. Shape:

```toml
[[device]]
id = "ahm-rack"
kind = "ah-tcp"        # osc-x32 | osc-wing | ah-tcp | dlive-tcp | yamaha-dm3
address = "10.0.0.10"
port = 51325            # optional, defaults to the protocol's standard port

[[mapping]]
from = { device = "ahm-rack", channel = 3 }
to   = { device = "x32-monitors", channel = 7 }
bidirectional = true
```

`channel` means whatever the underlying protocol's native addressing
unit is — an X32 headamp number, a dLive physical preamp Socket number
(distinct from processing channel), a DM3 Local Input number, etc. See
the relevant adapter's module doc comment for exact ranges.

## Radio Mic Telemetry — Status

| Vendor | Device | Protocol | Status |
|---|---|---|---|
| Shure | ULX-D | ASCII command strings (TCP 2202) | Done |
| Shure | Axient Digital | ASCII command strings (TCP 2202) | Wire framing done; field-level behavior only spot-checked against the doc, see adapter's module comment |
| Sennheiser | EW-DX EM 2 / EM 2 Dante / EM 4 Dante | Sound Control Protocol, SSC (JSON over UDP) | Done |

Both adapters are built from official vendor specs (see each adapter's
module doc comment for the exact document and URL) and unit/integration
tested against mocked sockets — same "no guessed wire framing" discipline
as the preamp adapters, and likewise **not yet validated against real
hardware**.

This domain is a different shape from preamp control: telemetry is
read-heavy (battery, RF level, audio level are monitoring-only; mute is
the only realistic write), so it has its own `MicAdapter` trait and
`MicState` type (`crates/mic-core`) rather than extending the preamp
`DeviceAdapter`/`Router` — see that crate's module doc comment for why.

Because every adapter only ever talks to a device's IP control channel,
support here is **Dante-optional by construction** — it works the same
whether or not the specific unit has a Dante audio card installed, since
Dante audio is never touched at all.

## Radio Mic Telemetry — Architecture

```
crates/
├── mic-core/                   # MicAdapter trait, MicState/MicEvent types
├── mic-adapter-shure/           # ULX-D + Axient Digital (ASCII over TCP)
├── mic-adapter-sennheiser/      # EW-DX EM (JSON/SSC over UDP)
└── mic-cli/                     # `mic-monitor` binary: discover, watch
```

## Radio Mic Telemetry — Building and running

```sh
# Connect to the mics in mics.toml and print live telemetry
cp mics.example.toml mics.toml   # edit for your rig
cargo run --bin mic-monitor -- watch --config mics.toml
```

`mic-monitor discover` reuses the same Dante mDNS browse as
`preamp-bridge discover` — a convenience for Dante-enabled units, not a
requirement; anything else (including hardware with no Dante card at
all) is configured directly by IP in `mics.toml`.

## Radio Mic Telemetry — Config format

```toml
[[mic]]
id = "ulxd-1"
kind = "shure-ulxd"       # shure-ulxd | shure-axient | sennheiser-ewdx
address = "10.0.0.30"
port = 2202                 # optional, defaults to the protocol's standard port (2202 Shure, 45 Sennheiser)
```

No `[[mapping]]` section — this domain is pure monitoring, nothing to
route between devices (yet; see below).

**Not yet built:** emulating a supported vendor's telemetry protocol on a
host console (e.g. making a Yamaha QL's Wireless Monitor screen show
synthesized ULX-D-shaped data for a mic it doesn't natively support) —
the same class of problem as preamp device emulation above, needing
packet captures of a real console+device pairing to learn the display's
own query/identity handshake. Deferred until there's real hardware
access.

## Contributing a new adapter

Every adapter so far follows the same shape: read the official (or
community-authoritative) protocol doc, implement the relevant trait
(`DeviceAdapter` for preamp control, `MicAdapter` for radio-mic
telemetry) against it exactly, write unit tests against the documented
byte/message examples, and add an integration test through a mock
socket. Don't implement against guessed wire framing — several vendors
here (preamp: Qu/SQ, CL/QL, Rio/Tio) are deliberately left unimplemented
because no public spec covers the relevant control; closing those gaps
needs either a real spec or packet captures from real hardware, not
assumptions.
