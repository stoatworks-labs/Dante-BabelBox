# dante-preamp-bridge

Cross-vendor preamp control bridge for Dante-networked mixing consoles and
stageboxes.

Dante carries audio and basic mDNS-based device discovery, but nothing
about preamp gain or phantom power — each console vendor layers its own
proprietary control protocol on top of the same network. This bridge
translates those protocols so gain/phantom changes on one vendor's device
propagate to another vendor's device over the same LAN.

## Status

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
(`crates/adapter-*/src/*.rs`) for the exact spec it's built from and any
open gaps.

**Not yet built:** device emulation (making the bridge answer as if it
*is* a native device of a foreign brand, so a console's own on-screen
preamp UI can control it directly). Today the bridge is a translating
router you configure by IP/channel — genuinely useful, but not yet
"invisible" to the consoles. This needs real hardware to build safely,
since it means impersonating a device's discovery/pairing handshake
closely enough that real gear accepts it.

## Architecture

```
crates/
├── core/            # PreampAddress/State/Event types, DeviceAdapter trait, Router
├── discovery/        # mDNS-based Dante device discovery
├── adapter-osc/       # X32-family + Wing (Behringer/Midas OSC dialects)
├── adapter-ah/        # AHM TCP/IP + dLive MIDI-over-TCP (Allen & Heath)
├── adapter-yamaha/    # DM3 OSC
└── cli/               # `preamp-bridge` binary: discover, run, config, hot-reload
```

Each adapter implements `core::DeviceAdapter` (connect, set_gain,
set_phantom, get_state, subscribe to state-change events). The `Router`
holds a mapping table (`bridge.toml`) and fans state-change events from
one device out to its mapped peer(s), with echo suppression so a device's
own confirmation of a command doesn't bounce back and forth forever
between bidirectionally-mapped devices.

## Building and running

```sh
cargo build --workspace
cargo test --workspace     # 27 tests, all against mock devices - no hardware required

# Browse Dante's mDNS advertisements for devices on the LAN
cargo run -p preamp-bridge -- discover

# Run the bridge daemon
cp bridge.example.toml bridge.toml   # edit for your rig
cargo run -p preamp-bridge -- run --config bridge.toml
```

Editing `bridge.toml` while the bridge is running hot-reloads the mapping
table (not the device list — adding or removing a device still needs a
restart).

## Config format

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

## Contributing a new adapter

Every adapter so far follows the same shape: read the official (or
community-authoritative) protocol doc, implement `DeviceAdapter` against
it exactly, write unit tests against the documented byte/message
examples, and add an integration test bridging it through the `Router`
against a mock socket. Don't implement against guessed wire framing —
several vendors here (Qu/SQ, CL/QL, Rio/Tio) are deliberately left
unimplemented because no public spec covers their preamp control; closing
those gaps needs either a real spec or packet captures from real
hardware, not assumptions.
