# Notes

Working notes for this repo: status, decisions, and the traps that have actually bitten.
Migrated out of Claude Code's memory on 2026-08-24, so they are written in the first
person and dated by when each thing was learned — that date is usually the useful part.

Cross-cutting notes that are not specific to this repo live in
[fleet-notes](https://github.com/stoatworks-labs/fleet-notes).

*Dante-BabelBox — cross-vendor Dante preamp control bridge (Rust), local repo and GitHub state.*

Dante-BabelBox (repo name on GitHub; local dir/crate name `dante-preamp-bridge`) lives at
`~/Projects/Dante-BabelBox`, GitHub `allansargeant/Dante-BabelBox` (public). It's a Rust
workspace that bridges preamp gain/phantom-power control across different Dante-networked
mixing console vendors (Behringer/Midas X32 family + Wing, Allen & Heath AHM/dLive, Yamaha
DM3) that otherwise don't speak each other's proprietary control protocols.

Working adapters (X32-family, Wing LCL-only, AHM, dLive, DM3) are implemented against
official/community specs and unit/integration tested against mock devices — **not yet
validated against real hardware**. Qu/SQ and most Yamaha lines (CL/QL/DM7, Rio/Tio) are
unimplemented because no public spec documents their preamp control.

Full "device emulation" (impersonating a native device so a console's own on-screen UI
controls a foreign-vendor device directly) is a discussed future direction, not yet built.
It would require packet captures of a real console paired with its own *native* device
(e.g. a real Yamaha QL1 talking to a real Rio/Tio, not the foreign device itself) to learn
the Dante-layer mDNS identity and vendor handshake/session semantics — none of that is in
any public spec.

**Both reverse-engineered protocols now have adapters (2026-08-03).**
`preamp-adapter-yamaha::mbc` implements Yamaha R-series HA — a proprietary `MBC` block
inside Audinate **ConMon** packets (UDP 8705/8708/8800), **not** AD8HR MIDI SysEx. Its
`gain_broadcast()` rebuilds byte-for-byte the packet a real Rio3224-D2 accepted, so the
*encoder* is anchored to hardware-accepted bytes; the adapter itself has still never run
against a device. `mic-adapter-shure-acn` is a new crate: SLPv2 discovery + DMP property
decode for a QLX-D mounted on a console, **read-only** (the receiver unicasts events to the
console, so telemetry needs a **mirrored port**) and with no SDT session client. `MicState`
gained `battery_bars` so ACN's 0–5 segment count isn't faked as a percentage.

**Video published 2026-08-03**: YouTube `Tx7hntcaOTw`, Reel
`https://www.instagram.com/reel/Dblguz2FZFF/`. It is a *render*, not a screen recording —
the footage is the repo's own `decode_mbc_capture` / `decode_acn_capture` examples run for
real against the captures, with `render.py` pulling line ranges out of their genuine stdout
so a decoder regression breaks the render instead of quietly producing a video that lies.
Status framing on camera: Yamaha QL/Rio **beta**, Shure QLX-D **alpha**, plus a call for
users to send captures of unsupported gear.

Captures are archived in **`stoatworks-labs/dante-captures` (PRIVATE)** — see
[dante captures](https://github.com/stoatworks-labs/dante-captures/blob/main/docs/NOTES.md) (`dante-captures`). Two artefacts are **permanently lost**: the ACN capture holding
the JOIN/CONNECT/SUBSCRIBE handshake (so an active ACN client can't be built from evidence)
and the Python that wrote gain to the Rio.

A second domain, **radio-mic telemetry** (`mic-core` trait + `mic-cli` `mic-monitor` binary),
monitors battery/RF/audio/mute from wireless receivers over each vendor's IP control port (Dante
audio never touched). Adapters: `mic-adapter-shure` (ULX-D/Axient, ASCII TCP 2202),
`mic-adapter-sennheiser` (EW-DX, SSC/UDP 45), and **`mic-adapter-lectrosonics`** (DSQD/Duet, added
2026-07-26). **Lectrosonics is the one adapter NOT built from an authoritative spec** — its wire
format is an unverified PLACEHOLDER (framing/port/fields guessed, default port 4992), mirroring the
RFutils adapter of the same name; flagged as such in the README top disclaimer + radio-mic status
table + `mics.example.toml`. It keeps the no-fabrication rule (audio dBFS + RF dBm stay `None`; RF
maps to an assumed `rf_quality_percent`). Config kind `lectrosonics-dsqd`. See [rfutils](https://github.com/stoatworks-labs/RFutils/blob/main/docs/NOTES.md) (`RFutils`).
NB: the workspace grew an OCA-plugin architecture (crates `oca`, `oca-plugin-abi`, `plugin-*`) —
preamp vendors are now dynamically-loaded abi_stable plugins; re-read the README before assuming
crate layout.

README carries a disclaimer noting the project was built with Claude and hasn't been
hardware-validated yet.

**2026-08-07 — Focusrite RedNet added over AES70, deliberately NOT over Yamaha.** A RedNet
MP8R *can* be driven as a Yamaha head amp (it has a `Yamaha ID` setting, `Y000`–`Y00F`, the
same ID space the QL1/Rio capture sits in), but the captures contain **no RedNet traffic**,
so five things can't be derived: MP8R gain range is **10–65 dB** vs the Rio's −6…+66, the
8-input-into-32-slot channel mapping, which MAC/ID it answers on, the unmapped pad/HPF
subops, and — the big one — **whether any device accepts MBC cold**, since §9's hardware
proof was a Rio already paired with a live QL1. Written up as `docs/rednet-mp8r-capture-request.md`.
Instead: RedNet units carry an **AES70 endpoint in firmware** (RedNet Control's per-device
`AES70 Enable/Disable`, listed alongside Clock Source), so RedNet Control need NOT be running
— unlike Focusrite's MIDI/SysEx path, whose guide says it must be. Two new crates:
`crates/ocp1` (a real AES70-3 controller, ported from **db-remote's `aes70`** crate minus the
d&b ONo packing) and `crates/plugin-aes70` (kind `aes70`; `rednet-aes70` kept as an alias).
It **enumerates the device's object tree at runtime** rather than shipping an ONo map, so the
only guess is role-string naming (`roles.rs`). NB `oca` (internal model) and `ocp1` (real wire
protocol) are different crates. The plugin is **vendor-neutral** — Bosch/Dynacord OMNEO (IPX,
IX:4, MXE5) documents OCA/AES70 as open to third parties and should need no new code.

**Better MBC capture target than the MP8R: the Rupert Neve RMP-D8** — Rupert Neve state it
appears to a CL/QL/PM/DM7 **as a native Yamaha Rio**, i.e. the exact device already captured,
so the 32-slot arrays and −6…+66 range likely match as-is.

**`crates/preamp-adapter-aphex` (2026-08-07) is a CODEC WITH NO TRANSPORT, deliberately.**
Aphex 1788A SysEx from the published command table (`F0 | 00 00 38 | MIDI Ch, MIDI Device,
Net Number | [Cmd, MicCh, Value] ×1..64 | F7`). It covers gain (1Ah–41h = **26–65 dB
literal, unscaled**), phantom, pad, polarity, mute, low cut, limiter — more of the model than
any other device here. Not wired up because the transport is undecided: the table is the MIDI
layer (a MIDI backend would add ALSA headers to Linux CI and be the only non-IP-socket device
here), and the 1788A's Ethernet SysEx framing is undocumented. Also undocumented: whether
"Mic Channel" is 0- or 1-based, and the `20h`/`56h` dump REPLY format. NB the 1788A table is
often called "the Avid/Pro Tools protocol" — it is **Aphex's**; Pro Tools just speaks it.

**Why:** Tracking this so future sessions don't need to re-derive the project's shape,
implementation status, or the emulation-capture plan from scratch.

**How to apply:** When picking up this project again, treat the status table in the repo's
README.md as the source of truth for current adapter coverage (it may have changed since
this memory was written) — this memory is for orientation, not a live status feed. See also
**commit means push** (working-practice note, kept in Claude memory) for this user's git workflow preference, which applies here
too.

**2026-08-06 — the three capture guides now carry a UniFi mirror recipe.** §3 of each
edition (`docs/capture-guide-{macos,windows,linux}.md`, plus the hand-authored `.html`
and the Chrome-printed `.pdf`) was rewritten from "if you already own a mirroring switch"
to a step-by-step for a **USW-Flex-Mini** — see [unifi port mirroring](https://github.com/stoatworks-labs/fleet-notes/blob/main/notes/reference_unifi_port_mirroring.md) for the
technical facts. The §3 block is byte-identical across the three files (mirroring happens
on the switch, which doesn't care what the laptop runs), so one replacement patches all
three; only the name of the bridge the reader is *not* creating varies.

**The `.html` is hand-authored with base64-embedded fonts and inline SVG diagrams — there
is no generator.** Edit the markup directly, reusing the document's own components
(`.chain`/`.step`, `.callout.warn`, `.alt-grid`, `.diagram-frame`). The `.pdf` regenerates
with `Google Chrome --headless --no-pdf-header-footer --print-to-pdf`, which reproduces the
committed PDFs exactly (7 pages, Skia/PDF producer) — verified against the originals before
touching them. The private `Dante-BabelBox-notes` repo holds the *original* HTML and is now
behind; its AGENTS.md says not to move content between the two, so it was left alone.

**2026-08-25 — tagging alone does NOT cut a release here, and v0.2.0 proves it.**
There are two release paths and only one of them produces signable macOS artefacts.
`scripts/release-local.sh --upload` (a thin wrapper over the vendored
`scripts/release-rust.sh`) is the real one: it builds the versioned artefacts including
`dante-babelbox-<ver>-macos-{aarch64,x86_64}-cli.pkg` and `.dmg` — a `.pkg` to put the
binaries on PATH and a `.dmg` for people who would rather mount and copy, since these are
console tools with no `.app` — plus the Windows NSIS `setup.exe`.
`.github/workflows/release.yml` is the other, fires on any `v*` tag push, and emits only
unversioned `dante-babelbox-<platform>.tar.gz` / `.zip` plus the Linux `.deb`/`.rpm`.

v0.1.1 got both passes, so its macOS `.pkg`/`.dmg` were signed and notarised. **v0.2.0 was
cut by pushing the tag and nothing else**, so it carries only CI's output. Consequences,
all still live at the time of writing: the macOS downloads are unsigned and un-notarised;
the Windows `setup.exe` and every versioned artefact are missing; `README.md`'s download
block — regenerated by `gen-downloads.py` from the *published* assets — now points macOS
users at the unsigned `.tar.gz` and nothing else; and the fleet autosign agent
(`com.stoatworks.autosign`) failed verification on it every fifteen minutes for a week,
because `posthoc-sign.sh` handles `.dmg`, `.pkg` and `.zip` only and a `.tar.gz` gives it
nothing to unpack. `Dante-BabelBox v0.2.0` is now a **tag-scoped** entry in
`~/.local/state/stoatworks-autosign/skip` to stop the loop — tag-scoped and not repo-level
on purpose, so the same mistake on a later tag is not swallowed silently.

**Decided not to re-cut v0.2.0** — the fix folds into v0.2.1.

**How to apply:** cut v0.2.1 (and every release after it) with
`scripts/release-local.sh --version <ver> --upload`, not by pushing the tag on its own.
Afterwards, confirm `gh release view <tag> --json assets` lists the two `-cli.pkg` and two
`-cli.dmg` files, regenerate the README download block, and remove the
`Dante-BabelBox v0.2.0` line from the autosign skip file. Note that the CI workflow still
fires on the tag push and its unversioned `.tar.gz` assets will sit alongside the real
ones, exactly as they did on v0.1.1 — that is expected, not a second failure. Also worth
knowing: `verify-signing.sh` passes a release as soon as *one* asset is checkable, so the
unsigned macOS `.tar.gz` is not caught by the gate even when everything else is correct.
