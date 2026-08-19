# Dante-BabelBox

> **AI-assisted project.** This codebase was created with [Claude](https://claude.com/claude-code)
> (Anthropic), directed and reviewed by a human author. Adapters are built
> against official/community-authoritative vendor protocol specs (see each
> adapter's module doc comment for its source) — with one clearly-flagged
> exception, the Lectrosonics mic adapter, whose wire format is an unverified
> placeholder (see the Radio Mic Telemetry status table). No adapter in this
> workspace has been **validated against real hardware** — only against mock
> devices in the test suite. Review before use on live gear.
>
> **One exception, and it's a big one.** The Yamaha R-series HA protocol has
> been captured from a real QL1 + Rio3224-D2, decoded, documented, and then
> **rebuilt from that documentation and transmitted back — the stagebox
> accepted it and changed its gain.** See
> [`docs/yamaha-ha-remote-over-dante.md`](docs/yamaha-ha-remote-over-dante.md).
> That is a *protocol* proven on hardware, not *code* proven on hardware. The
> adapter now exists (`MbcAdapter`) and its codec reproduces, byte for byte,
> the frame the Rio accepted — but the adapter itself has still never been run
> against a real device. The wire format is verified; the code around it is
> tested against captured frames, like everything else here.

Cross-vendor Dante control bridge, currently covering two domains:

1. **Preamp control** — bridges gain/phantom-power control across
   Dante-networked mixing consoles and stageboxes from different vendors.
2. **Radio-mic telemetry** — monitors battery, RF signal, and audio level
   from wireless mic receivers, across vendors, whether or not the
   hardware even has a Dante audio option installed (see the "Radio Mic
   Telemetry" sections below for why that doesn't matter here).

[![Dante-BabelBox — two vendor protocols recovered from packet captures](docs/video-thumb.png)](https://www.youtube.com/watch?v=Tx7hntcaOTw)

*56 seconds. Every line of terminal output is the real decoder — the two
capture-replay tools in this repo, run against real recordings of a Yamaha QL1
talking to a Rio3224-D2 and a Shure QLXD4 mounted on a console.*

Dante carries audio and basic mDNS-based device discovery, but nothing
about preamp gain, phantom power, or wireless-mic status — each vendor
layers its own proprietary control protocol on top of the same network.
This project translates those protocols so state on one vendor's device
is usable from outside its own ecosystem.

```mermaid
flowchart LR
    X32["Behringer / Midas<br/>(OSC)"] <--> BB
    AH["Allen & Heath<br/>(NRPN over TCP)"] <--> BB
    YAM["Yamaha DM3<br/>(OSC)"] <--> BB
    BB["Dante-BabelBox<br/>translating router"]
    SHURE["Shure ULX-D / Axient<br/>(ASCII/TCP)"] --> BB
    SENN["Sennheiser EW-DX<br/>(SSC/JSON over UDP)"] --> BB
    BB --> STATE["Unified gain / phantom state<br/>+ radio-mic telemetry"]
```

<!-- downloads:start -->

## Download

**[v0.2.0](https://github.com/stoatworks-labs/Dante-BabelBox/releases/tag/v0.2.0)** — prebuilt for macOS, Windows and Linux. Pick your platform:

<details>
<summary><b>macOS</b> — Apple Silicon, Intel</summary>

| Build | Download | Size |
| --- | --- | --- |
| Apple Silicon · .tar.gz archive | [`dante-babelbox-macos-aarch64.tar.gz`](https://github.com/stoatworks-labs/Dante-BabelBox/releases/latest/download/dante-babelbox-macos-aarch64.tar.gz) | 4.3 MB |
| Intel · .tar.gz archive | [`dante-babelbox-macos-x86_64.tar.gz`](https://github.com/stoatworks-labs/Dante-BabelBox/releases/latest/download/dante-babelbox-macos-x86_64.tar.gz) | 4.4 MB |

</details>

<details>
<summary><b>Windows</b> — x64, ARM64</summary>

| Build | Download | Size |
| --- | --- | --- |
| x64 · .zip archive | [`dante-babelbox-windows-x86_64.zip`](https://github.com/stoatworks-labs/Dante-BabelBox/releases/latest/download/dante-babelbox-windows-x86_64.zip) | 3.8 MB |
| ARM64 · .zip archive | [`dante-babelbox-windows-aarch64.zip`](https://github.com/stoatworks-labs/Dante-BabelBox/releases/latest/download/dante-babelbox-windows-aarch64.zip) | 3.6 MB |

</details>

<details>
<summary><b>Linux</b> — x64, ARM64</summary>

| Build | Download | Size |
| --- | --- | --- |
| x64 · .deb package (Debian/Ubuntu) | [`dante-babelbox_0.2.0_amd64.deb`](https://github.com/stoatworks-labs/Dante-BabelBox/releases/download/v0.2.0/dante-babelbox_0.2.0_amd64.deb) | 4.8 MB |
| ARM64 · .deb package (Debian/Ubuntu) | [`dante-babelbox_0.2.0_arm64.deb`](https://github.com/stoatworks-labs/Dante-BabelBox/releases/download/v0.2.0/dante-babelbox_0.2.0_arm64.deb) | 4.9 MB |
| x64 · .rpm package (Fedora/RHEL) | [`dante-babelbox-0.2.0-1.x86_64.rpm`](https://github.com/stoatworks-labs/Dante-BabelBox/releases/download/v0.2.0/dante-babelbox-0.2.0-1.x86_64.rpm) | 5.0 MB |
| ARM64 · .rpm package (Fedora/RHEL) | [`dante-babelbox-0.2.0-1.aarch64.rpm`](https://github.com/stoatworks-labs/Dante-BabelBox/releases/download/v0.2.0/dante-babelbox-0.2.0-1.aarch64.rpm) | 5.0 MB |
| x64 · .tar.gz archive | [`dante-babelbox-linux-x86_64.tar.gz`](https://github.com/stoatworks-labs/Dante-BabelBox/releases/latest/download/dante-babelbox-linux-x86_64.tar.gz) | 4.7 MB |
| ARM64 · .tar.gz archive | [`dante-babelbox-linux-aarch64.tar.gz`](https://github.com/stoatworks-labs/Dante-BabelBox/releases/latest/download/dante-babelbox-linux-aarch64.tar.gz) | 4.8 MB |

</details>

All builds, checksums and release notes: [github.com/stoatworks-labs/Dante-BabelBox/releases](https://github.com/stoatworks-labs/Dante-BabelBox/releases).

macOS builds are signed and notarised and open normally. The Windows builds are unsigned, so SmartScreen warns once — see [Windows SmartScreen & Defender Firewall](#windows-smartscreen--defender-firewall) for the one-time click-through.

<!-- downloads:end -->

## Preamp Control — Status

| Vendor | Device | Protocol | Status |
|---|---|---|---|
| Behringer / Midas | X32, Wing, M32, HD96 | OSC | X32 family done; Wing done (8 built-in preamps only) |
| Allen & Heath | AHM-series processors | NRPN-over-TCP | Done |
| Allen & Heath | dLive | NRPN-over-TCP (Socket addressing) | Done |
| Allen & Heath | DT168, DT164-W (Dante expanders) | `AllenHth` vendor messages over Audinate ConMon | **Codec only, no transport.** Built/parsed from static reverse-engineering of A&H's *DT Preamp Control* app + SQ Dante-card firmware — gain, pad, +48 V for 16 preamps. No capture, no hardware: a well-evidenced hypothesis, not proven bytes. Transport (ConMon) needs Audinate DAPI, as with Aphex. See [docs/allenheath-dt-preamp-over-dante.md](docs/allenheath-dt-preamp-over-dante.md) |
| Allen & Heath | Qu, SQ | (likely `AllenHth` ConMon — see DT168) | Not implemented, but no longer a blank: the SQ's Dante card (KLANTE) registers the same `AllenHth` ConMon namespace as the DT expanders, so the DT codec is the most promising SQ-over-Dante path pending a capture. The published SQ MIDI protocol documents no preamp control |
| Yamaha | DM3 / DM3S | OSC | Done |
| Yamaha | CL, QL | `MBC` over Audinate ConMon | Partial — a QL/CL is the *controlling* side of the captured protocol, and the same adapter addresses it. Driving a console's own **local** head amps was never observed on the wire, so it is untested and may not be supported at all |
| Yamaha | DM7 | — | Not implemented — no public spec, and nothing confirms it shares DM3's OSC dialect or the R-series `MBC` block |
| Yamaha | Rio, Tio (R-series) | `MBC` over Audinate ConMon | Implemented — gain, +48 V and metering, from a wire format [captured and proven on real hardware](docs/yamaha-ha-remote-over-dante.md). Never run against a device. |
| *Any AES70 vendor* | Focusrite RedNet (MP8R and any unit with AES70 enabled); expected to reach Bosch/Dynacord OMNEO and d&b without new code | AES70/OCA (OCP.1 over TCP) | Implemented from the **published standard** — no reverse engineering, and not vendor-specific. The device's objects, classes and names are discovered at runtime, so there is no vendor ONo map to be wrong. Never run against any AES70 device; see the plugin's module comment for the two things to check first. |
| Aphex | 1788A | Parametric MIDI SysEx | **Codec only, no transport.** The byte string is built and parsed from Aphex's published command table (gain, phantom, pad, polarity, mute, low cut, limiter — more of this project's model than any other device here). Nothing sends it yet: the table is the MIDI layer, and how SysEx reaches the unit's Ethernet port isn't documented. See the crate's module comment |

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
closely enough that real gear accepts it. The patch-bay web UI (below)
already lets you declare **virtual** devices — placeholders for this
emulation layer — and map them against real devices now, so the intended
topology can be designed ahead of the emulation itself existing.

Building that requires packet captures of a real console paired with its
own native device (e.g. a real Yamaha QL1 talking to a real Rio/Tio, not
a foreign-vendor box) — none of that handshake is in any public spec.
[`docs/`](docs/) has a non-technical field guide, one edition per OS, for
capturing that traffic with Wireshark using nothing but a laptop as an
inline bridge:

- [Windows](docs/capture-guide-windows.md) ([PDF](docs/capture-guide-windows.pdf), [HTML](docs/capture-guide-windows.html))
- [macOS](docs/capture-guide-macos.md) ([PDF](docs/capture-guide-macos.pdf), [HTML](docs/capture-guide-macos.html))
- [Linux](docs/capture-guide-linux.md) ([PDF](docs/capture-guide-linux.pdf), [HTML](docs/capture-guide-linux.html))

[![Send us a capture — the packets two open-source audio tools are waiting on](docs/video-capture-callout-thumb.png)](https://www.youtube.com/watch?v=E-gjYZoHVAw)

*57 seconds, covering this project and [RFutils](https://github.com/stoatworks-labs/RFutils)
together. The Wireshark section is the real QL1/Rio recording the Yamaha decoder
was built from — 4052 packets of Audinate ConMon that Wireshark renders as
`Data`, because nothing knew what they were.*

Each guide also carries a step-by-step recipe for the other route — putting a
port on a **UniFi USW-Flex-Mini** into mirroring mode, which is a ~£25 switch
rather than the borrowed-from-IT hardware a mirror port used to mean. That
matters beyond convenience: a receiver mounted on a console *unicasts* its
telemetry to that console, so an ordinary switch port sees none of it and there
may be no cable to bridge into. Mirroring is the only way to capture that at
all — it is how the [Shure ACN work](docs/SHURE-ACN.md) was done.

## Preamp Control — Architecture

```
crates/
├── oca/                      # internal object model (Ono/OcaClass/OcaValue/OcaObject/OcaEvent) - see "Plugin Architecture" below
├── oca-plugin-abi/            # abi_stable FFI contract for dynamically-loaded device plugins
├── core/                      # PluginRegistry, LocalAdapter, LegacyPluginBridge, Router
├── discovery/                 # mDNS-based Dante device discovery + Dante's own routing-observation protocol
├── plugin-osc-x32/             # X32-family plugin (hand-written FFI translation - the original reference implementation)
├── plugin-osc-wing/             # Wing plugin (thin wrapper over LegacyPluginBridge)
├── plugin-ah-tcp/                # AHM plugin (thin wrapper over LegacyPluginBridge)
├── plugin-dlive-tcp/             # dLive plugin (thin wrapper over LegacyPluginBridge)
├── plugin-yamaha-dm3/            # DM3 plugin (thin wrapper over LegacyPluginBridge)
├── preamp-adapter-osc/         # X32/Wing OSC wire-protocol logic, reused by the two plugins above
├── preamp-adapter-ah/          # AHM/dLive wire-protocol logic, reused by the two plugins above
├── preamp-adapter-yamaha/      # DM3 wire-protocol logic, reused by the plugin above
├── preamp-web/                 # Patch-bay web UI + device/mapping management API (axum)
└── preamp-cli/                 # `preamp-bridge` binary: discover, init, run, config, hot-reload
```

Every device, whatever vendor and whatever wire protocol, is represented
internally as a flat list of OCA objects (a gain knob, a mute switch, a
battery-percent reading — each one an object with an `Ono` identity, a
class, a role label, and a value). The `Router` holds a mapping table
(`bridge.toml`) and fans OCA events from one device's objects out to
their mapped peers, with echo suppression so a device's own confirmation
of a command doesn't bounce back and forth forever between
bidirectionally-mapped devices. See "Plugin Architecture" below for the
full picture, and
[`docs/plugin-development-guide.md`](docs/plugin-development-guide.md)
for how to add a new device as a loadable plugin.

All five preamp vendors ship as real dynamically-loaded plugins - X32's
FFI translation is hand-written (it was this project's original
proof-of-concept, predating the generic wrapper below); Wing, AHM,
dLive, and DM3 are thin crates built on `core::LegacyPluginBridge`, which
generically wraps the older in-process `DeviceAdapter` trait (connect,
set_gain, set_phantom, get_state, subscribe) so their existing,
already-tested wire-protocol code didn't need rewriting to become a real
plugin - only a `create_adapter`/`plugin_info` pair per vendor.

## Plugin Architecture

Device support can be loaded two ways, and both end up looking identical
to the `Router` and the web UI:

- **Statically registered** — compiled into the `preamp-bridge` binary at
  build time (today: only the two known-but-unimplemented placeholder
  kinds, `ah-midi`/`yamaha`, which exist purely to explain why they're
  not supported rather than to back a real device).
- **Dynamically loaded** — a separate `.so`/`.dylib`/`.dll` file, scanned
  from a directory (`--plugins-dir`, default `plugins`) at startup and
  loaded at runtime via [`abi_stable`](https://docs.rs/abi_stable). Adding
  a new vendor this way needs **no recompile of this project at all** —
  build your plugin, drop the file in the plugins directory, restart the
  bridge. All six real preamp vendors work this way today.

```mermaid
flowchart LR
    subgraph Host["preamp-bridge (compiled)"]
        Reg["PluginRegistry"]
        Reg --> Router
    end
    X32["libplugin_osc_x32"] -. abi_stable FFI .-> Reg
    Wing["libplugin_osc_wing"] -. abi_stable FFI .-> Reg
    Ahm["libplugin_ah_tcp"] -. abi_stable FFI .-> Reg
    Dlive["libplugin_dlive_tcp"] -. abi_stable FFI .-> Reg
    Dm3["libplugin_yamaha_dm3"] -. abi_stable FFI .-> Reg
    Aes70["libplugin_aes70"] -. abi_stable FFI .-> Reg
    Router["Router (OCA objects)"] --> Web["Patch-bay web UI"]
```

Both registration paths converge on the same internal shapes:
`LocalAdapter` (the in-process trait the `Router` actually talks to) and
the OCA object model (`Ono`/`OcaClass`/`OcaValue`/`OcaObject`) described
above. A dynamically-loaded plugin talks to the host through a narrower,
FFI-safe mirror of that same model (`dante-babelbox-oca-plugin-abi`'s
`PluginAdapter` trait) — see
[`docs/plugin-development-guide.md`](docs/plugin-development-guide.md) for
the full contract, worked examples (`crates/plugin-osc-x32` for a
hand-written translation, `crates/plugin-osc-wing` for the generic
`LegacyPluginBridge` path most vendors use, and
`crates/plugin-aes70` for one that implements `PluginAdapter`
directly because its object set is only known once the device has been
asked), and the real
pitfalls hit building it (async-to-sync bridging, why a plugin's Tokio
runtime has to be multi-threaded, why loading more than one plugin file
sharing the same root-module type needs `abi_stable`'s uncached
per-file API rather than `RootModule::load_from_file`, how automatic
device+channel mapping resolution actually matches objects by role
name).

That guide is written for anyone adding support for a new device without
needing this project to accept a PR first — implement your protocol,
compile it as a `cdylib`, and it loads into any `preamp-bridge` build
without touching this repo's own crates.

## Preamp Control — Building and running

```sh
cargo build --workspace
cargo test --workspace     # 136 tests (both domains), all against mock devices - no hardware required

# Browse Dante's mDNS advertisements for devices on the LAN
cargo run --bin preamp-bridge -- discover

# Auto-generate the [[device]] blocks of a bridge.toml by discovering
# devices and probing each one against every implemented adapter's
# identify() until one claims it. [[mapping]] entries are NOT generated
# by default - add those by hand afterwards, or see --infer-mappings below.
cargo run --bin preamp-bridge -- init --output bridge.toml

# Run the bridge daemon
cp bridge.example.toml bridge.toml   # edit for your rig, or use the init command above
cargo run --bin preamp-bridge -- run --config bridge.toml
```

`init`'s device-identification confidence varies by protocol: X32-family
and Wing both confirm vendor *and* model (from documented `/info`/`/?`
replies); AHM and dLive confirm protocol family only (their specs have no
model-string query); DM3 is weakest-signal, since its spec documents no
identify query at all and this reuses a scene-status request as a
presence probe. See each adapter's `identify()` for details.

### Auto-generating `[[mapping]]` entries (`--infer-mappings`)

```sh
cargo run --bin preamp-bridge -- init --output bridge.toml --infer-mappings
```

Dante carries its own audio-routing/subscription protocol, separate from
every vendor's preamp-control protocol this bridge speaks elsewhere (see
`crates/discovery/src/dante_control.rs`). With this flag, `init` also
queries each identified device's current RX channel subscriptions and,
for every subscription pointing at another device already in the
generated `bridge.toml`, writes a `[[mapping]]` entry for it.

This is a real signal — it comes from watching live patching, not a
guess — but it comes with one real caveat: the channel numbers it writes
are *Dante audio channel numbers*. Whether that's the same as the
preamp/headamp channel number a vendor adapter addresses is a
default-configuration convention on most gear (Dante channel order
mirrors physical I/O order 1:1 out of the box), not a protocol
guarantee — a console with customized Dante patching can break this
assumption silently. That's why it's opt-in rather than the default, and
why the written file gets a header comment calling this out explicitly.
Treat inferred mappings as a first draft to verify against each
adapter's channel numbering, not a final answer.

Editing `bridge.toml` while the bridge is running hot-reloads the mapping
table. The device list itself is no longer restart-only either: the
patch-bay web UI (below) can add and remove devices (real or virtual) and
mappings, all live.

## Preamp Control — Patch-bay web UI

![Patch-bay web UI: the Patch tab, with source devices on the left and destination devices on the right, each a rack strip of numbered channel jacks, and green patch cables joining mapped channels](docs/screenshots/patch-bay.png)

*Real screenshot of the running web UI (`preamp-bridge run`). The devices shown are **virtual** placeholders so the UI can be demoed without any hardware on the network — see "Device management" below.*

`run` also serves a web UI, bound by default to `0.0.0.0:8080` so anyone
on the LAN can reach it at `http://<this-machine's-IP>:8080` - no
separate app to install, just a browser, from any device on the network.
Change the bind with `--web-bind` (e.g. `--web-bind 127.0.0.1:8080` to
restrict it to this machine only), or turn it off entirely with
`--no-web`. Like the rest of this bridge, there's no auth or TLS - same
trust model as a hardware router's control port, meant for a trusted
operations network, not the open internet.

`run` also takes `--plugins-dir <path>` (default `plugins`) - scanned at
startup for dynamically-loadable device plugin `.so`/`.dylib`/`.dll`
files, alongside the compiled-in vendors. See "Plugin Architecture" above.

- **Patch tab** — every device drawn as a line-art rack strip with
  numbered channel jacks, sources on the left and destinations on the
  right (like a hardware patch-bay screen). Click a source channel then a
  destination channel to connect them; click the patch cable to
  disconnect.
- **Crosspoint tab** — the same mappings as a matrix, scoped to channels
  that are actually mapped (a full all-channels grid across even two
  devices would be tens of thousands of cells). Pin a device to bring all
  its channels into the grid on demand.
- **Device management** — add real devices (connects immediately, same
  as a config-declared one) or **virtual** devices: placeholders for the
  not-yet-built emulation layer, with a chosen channel count and no live
  connection, so a mapping topology can be designed before the emulation
  exists to back it.
- **Export as TOML** — since devices/mappings added through the UI are
  in-memory only (they don't survive a restart, matching how this
  project's config hot-reload has always worked), a button exports the
  current state so it can be pasted into `bridge.toml` to keep it.

Removing a real device calls its adapter's `disconnect()` (cancellation
token torn down through the background socket task, port actually
freed), not just a drop from a list - every adapter implements this, so
add/remove works the same way whether a device is real or virtual.

## Preamp Control — Config format

See [`bridge.example.toml`](bridge.example.toml) for a worked example
covering all four built-in device kinds. `kind` is an open string, not a
fixed list — a loaded plugin (see "Plugin Architecture" above) can
register additional kinds beyond the ones this repo ships. Shape:

```toml
[[device]]
id = "ahm-rack"
kind = "ah-tcp"        # osc-x32 | osc-wing | ah-tcp | dlive-tcp | yamaha-dm3
address = "10.0.0.10"
port = 51325            # optional, defaults to the protocol's standard port

[[device]]
id = "future-x32"
kind = "osc-x32"        # the protocol this virtual device will eventually emulate
virtual = true          # placeholder for the not-yet-built emulation layer - no address needed
channels = 8            # required when there's no documented default for `kind` (or to override it)

[[mapping]]
from = { device = "ahm-rack", channel = 3 }
to   = { device = "x32-monitors", channel = 7 }
bidirectional = true
```

`channel` means whatever the underlying protocol's native addressing
unit is — an X32 headamp number, a dLive physical preamp Socket number
(distinct from processing channel), a DM3 Local Input number, etc. See
the relevant adapter's module doc comment for exact ranges. `address` is
required for every non-virtual device; `channels` is optional for real
devices (defaults to the kind's documented channel count) and required
for kinds with none documented (`ah-midi`, `yamaha`).

## Radio Mic Telemetry — Status

| Vendor | Device | Protocol | Status |
|---|---|---|---|
| Shure | ULX-D | ASCII command strings (TCP 2202) | Done |
| Shure | Axient Digital | ASCII command strings (TCP 2202) | Wire framing done; field-level behavior only spot-checked against the doc, see adapter's module comment |
| Sennheiser | EW-DX EM 2 / EM 2 Dante / EM 4 Dante | Sound Control Protocol, SSC (JSON over UDP) | Done |
| Lectrosonics | DSQD / D Squared, Duet | Ethernet control port | ⚠️ **Placeholder wire format** — scaffold only, NOT built from an authoritative spec; correct the bytes + set the real port before use. See adapter's module comment |
| Shure | QLX-D **mounted on a console** | ANSI E1.17 ACN (SLPv2 + SDT/DMP over UDP) | Discovery + passive telemetry, reverse-engineered from real hardware. **Read-only, and needs a mirrored port** — the receiver unicasts its events to the console. Cannot open its own session; see the adapter's module comment |

The Shure and Sennheiser adapters are built from official vendor specs
(see each adapter's module doc comment for the exact document and URL)
and unit/integration tested against mocked sockets — same "no guessed
wire framing" discipline as the preamp adapters, and likewise **not yet
validated against real hardware**.

The **Lectrosonics** adapter is the one exception to that discipline and
is flagged as such: no equivalent open protocol document was available,
so its framing, port and field mappings are an unverified **placeholder**
(mirroring the RFutils adapter of the same name), isolated so a packet
capture or the official IP-control spec can correct them in one file. It
keeps the same no-fabrication rule for what it *does* report — audio dBFS
and RF dBm stay `None` rather than being invented — but treat it as a
scaffold, not a working adapter, until the wire format is confirmed.

[`docs/mic-telemetry-architecture`](docs/mic-telemetry-architecture.md)
([PDF](docs/mic-telemetry-architecture.pdf), [HTML](docs/mic-telemetry-architecture.html))
diagrams why this domain has its own trait, both vendors' wire protocols
sequence-by-sequence, and exactly what ends up in `MicState` per vendor.

[`docs/yamaha-ha-remote-over-dante.md`](docs/yamaha-ha-remote-over-dante.md) is a
different kind of document: a protocol note reverse-engineered from a capture of a
real QL1 and Rio3224-D2, showing that Yamaha head-amp control is a proprietary
block tunnelled inside Dante ConMon rather than anything in Audinate's own control
protocols. It is observation, not validation — see its closing section.

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
├── mic-adapter-shure-acn/       # QLX-D on a console (ACN: SLPv2 + SDT/DMP)
├── mic-adapter-sennheiser/      # EW-DX EM (JSON/SSC over UDP)
├── mic-adapter-lectrosonics/    # DSQD / Duet (placeholder wire format)
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
kind = "shure-ulxd"       # shure-ulxd | shure-axient | sennheiser-ewdx | lectrosonics-dsqd
address = "10.0.0.30"
port = 2202                 # optional, defaults to the protocol's standard port (2202 Shure, 45 Sennheiser, 4992 Lectrosonics placeholder)
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

## Windows SmartScreen & Defender Firewall

macOS builds are **Developer ID-signed and notarised by Apple** — they open
normally, with no Gatekeeper warning and no quarantine step. The Windows
binaries are **not** code-signed, so Windows still warns you the first time.

- **Windows** — SmartScreen shows *"Windows protected your PC"* →
  **More info** → **Run anyway**.
- **Windows Defender Firewall** — first launch pops *"Allow Dante-BabelBox to
  communicate on these networks"*. Tick **Private** (and **Domain** on a managed
  network) — Dante-BabelBox needs it to serve the patch-bay UI and reach preamps and
  radio-mic receivers on the audio LAN. Deny it and no devices will be discovered and
  gain/phantom changes won't reach the hardware.
- **Linux** — no signing gate.

Per-artifact steps, self-signing, checksum verification and the Defender Firewall reset
procedure: **[docs/UNSIGNED.md](docs/UNSIGNED.md)**.

## Roadmap / TODO

- [ ] **Implement the Yamaha R-series adapter** — **ready to build now.** The wire format is captured, decoded and verified on a real Rio3224-D2 in [`docs/yamaha-ha-remote-over-dante.md`](docs/yamaha-ha-remote-over-dante.md), including a reference implementation that reproduces a packet the stagebox accepted. Gain (int16 centi-dB) and phantom (uint8) map straight onto `PreampState`. This is the most shovel-ready item on the list.
- [ ] **Map the remaining Yamaha subops** — eight 32-element arrays (`0x0722` subops `10`,`12`,`13`,`15`,`18`–`1b`) that almost certainly include pad, HPF and polarity, but were never observed populated. A ten-minute capture with hardware resolves them; the procedure is written up in the doc.
- [ ] **Capture DT Preamp Control driving a real DT168 / DT164-W** — `crates/preamp-adapter-ah::dt` builds and parses the `AllenHth` ConMon vendor payload (gain/pad/+48 V for 16 preamps), but from static RE only, with no capture to anchor it. One capture of the app setting a gain settles every [OPEN] in [`docs/allenheath-dt-preamp-over-dante.md`](docs/allenheath-dt-preamp-over-dante.md): the ConMon envelope, the `u16` gain byte order, the UWORD→dB scale, the set-message field offsets. The same capture pointed at an **SQ** would confirm whether the console answers the same messages (its Dante card registers the same namespace).
- [ ] **Give the Aphex 1788A codec a transport** — `crates/preamp-adapter-aphex` builds and parses the SysEx correctly but nothing delivers it. Two candidate paths, and the choice decides whether this becomes an ordinary socket adapter or the first device here needing a MIDI backend (plus ALSA headers in Linux release CI): the documented MIDI layer, or the unit's Ethernet port, whose SysEx framing isn't published. A capture, or ten minutes with a 1788A, settles it — along with whether "Mic Channel" counts from 0 or 1 and what a `20h` dump reply looks like.
- [ ] **Validate the adapters against real hardware** — every adapter is currently tested only against mock devices; no adapter has been run on live gear.
- [ ] **Migrate the radio-mic domain onto plugins + OCA** — give it a `Router` in the process (it has none today, since it's pure monitoring), so telemetry mapping/web-UI parity comes for free.
- [ ] **Ship plugin binaries in release CI** — `.github/workflows/release.yml` builds only the two host binaries today; building/packaging all six `crates/plugin-*` `cdylib`s per platform is a real CI expansion, not yet done.
- [ ] **Get a third-party head-amp capture** — a Focusrite MP8R or, better, a **Rupert Neve RMP-D8**, driven from a CL/QL/PM/DM7. Both can act as Yamaha head amps, which would reuse the `MBC` work wholesale, and Rupert Neve state the RMP-D8 appears to the console *as a native Rio* — the exact device already captured. Nothing about either can be derived from the QL1/Rio captures, since neither was on that network; [`docs/rednet-mp8r-capture-request.md`](docs/rednet-mp8r-capture-request.md) says exactly what a three-minute capture would need to contain.
- [ ] **Preamp device emulation** — make the bridge answer as a native device of a foreign brand so a console's own preamp UI controls it directly (needs packet captures of a real console+device pairing).
- [ ] **Telemetry emulation on a host console** — e.g. surface ULX-D-shaped data on a Yamaha QL Wireless Monitor screen (same capture-dependent problem).
- [ ] **More preamp vendors** — Yamaha CL/QL/DM7, Yamaha Rio/Tio; blocked on missing public control specs or wire-format captures. (Allen & Heath Qu/SQ is now scaffolded via the DT `AllenHth` ConMon codec — see the capture item above.) The plugin path (see below) means these no longer need to land in this repo to exist.
- [ ] **Wing** — currently only its 8 built-in preamps; extend to remote stagebox preamps.

## Contributing a new adapter

Two ways to add device support, both expecting the same underlying
discipline: read the official (or community-authoritative) protocol doc
and implement against it exactly — never guess wire framing — with unit
tests against the spec's own worked examples and an integration test
through a mock socket. Several vendors here (preamp: Qu/SQ, CL/QL,
Rio/Tio) are deliberately left unimplemented because no public spec
covers the relevant control; closing those gaps needs a real spec or
packet captures from real hardware, not assumptions.

**Rio/Tio is no longer in that category.** The packet captures exist, the
protocol is decoded, and a message built from the write-up was accepted by a
real Rio3224-D2 — see
[`docs/yamaha-ha-remote-over-dante.md`](docs/yamaha-ha-remote-over-dante.md).
It stays on the TODO list as unimplemented work, not as a blocked unknown.

- **In-process** (lands in this repo) — implement the relevant trait
  (`DeviceAdapter` for preamp control, `MicAdapter` for radio-mic
  telemetry) directly, compiled into the workspace. This is how the
  radio-mic adapters (Shure, Sennheiser) work today - the preamp domain
  has fully moved to the plugin path below, though a not-yet-bespoke
  vendor can still start here and move to `LegacyPluginBridge` later
  (see `crates/plugin-osc-wing` for what that migration looks like).
- **Plugin** (doesn't need to touch this repo at all) — implement
  `dante-babelbox-oca-plugin-abi`'s `PluginAdapter` trait, compile as a
  `cdylib`, and it loads into any `preamp-bridge` build via
  `--plugins-dir`. See
  [`docs/plugin-development-guide.md`](docs/plugin-development-guide.md)
  for the full contract and a worked reference
  (`crates/plugin-osc-x32`). This is the path for shipping support for a
  device without waiting on (or needing) a PR here.

<!-- attributions:start -->
This project is built on other people's work — see [ATTRIBUTIONS.md](ATTRIBUTIONS.md).
<!-- attributions:end -->
