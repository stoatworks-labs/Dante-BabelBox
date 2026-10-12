# AGENTS.md — bringing an LLM up to speed on Dante-BabelBox

Orientation for an AI assistant (or a new human) picking this project up cold. `CLAUDE.md`
holds the short command reference; this file explains the model and the traps.

---

## 1. What this is

A **cross-vendor Dante control bridge** in Rust, covering two domains:

1. **Preamp control** — bridges gain and phantom-power control across manufacturers whose
   remote-control protocols don't otherwise interoperate.
2. **Radio-mic telemetry** — normalizes wireless-mic monitoring (audio, battery, RF) across
   vendors.

Both work the same way: vendor-specific adapters behind a common core trait, exposed through
CLIs and a web UI.

Public repo, ships a user-facing AI-assisted disclaimer.

## 2. The architectural rule

**Adding a vendor means adding a new `*-adapter-*` crate that implements the core trait. Do
not leak vendor specifics into `core`.**

This is the entire value of the project — `core` describes what a preamp or a mic receiver
*is*, and each adapter translates one manufacturer's protocol into that. The moment a vendor
quirk lands in `core`, the abstraction stops paying for itself.

## 3. Layout

```
crates/
  core/          Shared traits and models for preamps
  mic-core/      Shared traits and models for radio mics
  discovery/     Dante device discovery
  oca/           The internal object model (Ono/OcaClass/OcaValue/OcaObject)
  oca-plugin-abi/ The FFI-safe host<->plugin contract (abi_stable)
  ocp1/          A real AES70-3 (OCP.1) controller - see the note below
  preamp-adapter-osc/     \
  preamp-adapter-ah/       > preamp vendor adapters
  preamp-adapter-yamaha/  /
  preamp-adapter-aphex/   <- CODEC ONLY, no transport - see below
  plugin-*/      Dynamically-loaded device plugins (cdylib), one per vendor
  mic-adapter-shure/         \
  mic-adapter-sennheiser/     > radio-mic vendor adapters
  mic-adapter-lectrosonics/  /  <- SEE WARNING BELOW
  preamp-cli/    Preamp command-line entry point
  mic-cli/       Mic command-line entry point
  preamp-web/    Web UI
```

**`oca` and `ocp1` are different things and the names invite confusion.** `oca`
is the *internal* model — it borrows AES70's class taxonomy because it fits, and
speaks no protocol. `ocp1` is an actual AES70 controller that talks OCP.1 over
TCP to real devices. Only `plugin-aes70` uses it.

## 4. Two honesty requirements

These are in the README as user-facing warnings and must not be quietly softened:

**Almost no adapter has been validated against real hardware** — every adapter is tested
against mock devices in the test suite, and only the DM3 (both transports, below) and the
Sennheiser G3 have also been run against real devices
([`docs/sennheiser-ewg3-bench-2026-09-15.md`](docs/sennheiser-ewg3-bench-2026-09-15.md)). Say "almost no adapter has been validated", not "nothing has been
validated" — the distinction now matters twice over, see below.

**The Yamaha DM3 is a fully hardware-validated target** (V3.00, 2026-09-15) — the first and
so far only adapter with *every method* proven on a real console. The validated path is the
**SCP adapter** `Dm3ScpAdapter` (`crates/preamp-adapter-yamaha/src/dm3_scp.rs`, kind
`yamaha-dm3-scp`, TCP 49280): identify, get_state, set_gain, set_phantom and a live
`subscribe()` change feed (driven by the DM3's `NOTIFY` push) all ran against the desk,
through the full plugin stack. The older **OSC** adapter `Dm3Adapter` (kind `yamaha-dm3`,
UDP 49900) also ran but is weaker — OSC `set` is unacknowledged and the DM3 pushes no
unsolicited OSC, so read-after-write and `subscribe()` don't work as well. Keep the two
straight when editing: **SCP is the validated, recommended transport; OSC is the legacy one.**
Both are real, hardware-run adapters; neither is a mock. See
[`docs/yamaha-dm3-bench-2026-09-15.md`](docs/yamaha-dm3-bench-2026-09-15.md).

**The Yamaha R-series HA protocol *has* been proven on real hardware**, and this is the one
place the honesty warning got *stronger* rather than weaker. It was captured from a real
QL1 + Rio3224-D2, decoded, written up as
[`docs/yamaha-ha-remote-over-dante.md`](docs/yamaha-ha-remote-over-dante.md), then rebuilt
from that document and transmitted — the stagebox accepted it and changed its gain. Keep the
line sharp when editing: **the protocol is verified, the code is not.** The write came from a
standalone Python script; `preamp-adapter-yamaha` covers the DM3 (OSC `Dm3Adapter` and the
hardware-validated SCP `Dm3ScpAdapter`) and has the Rio's `MbcAdapter` (`src/mbc/`), which
rebuilds the accepted frame byte for byte but has never run against a device, and no plugin
loads it. Anyone implementing Rio HA is working from evidence, not guesswork — that
is the claim, and it should not be inflated into "the Yamaha adapter works".

**The Lectrosonics mic adapter's wire format is an unverified placeholder.** Every other
adapter is built against official or community-authoritative vendor protocol specs — and each
adapter's module doc comment cites its source. Lectrosonics is the flagged exception. If you
work on it, either verify the format against real gear/documentation and update the flag, or
leave the flag exactly where it is.

**The Focusrite RedNet plugin is built against a published standard, not a
capture.** RedNet units carry an AES70 endpoint in firmware (RedNet Control's
per-device `AES70 Enable/Disable`), so `plugin-aes70` implements AES70-1/-3
and enumerates the device's objects at runtime rather than shipping a vendor ONo
map. An MP8R can *also* be driven as a Yamaha head amp, and that route is
deliberately not taken: see [`docs/rednet-mp8r-capture-request.md`](docs/rednet-mp8r-capture-request.md)
for the five things the QL1/Rio captures can't answer about a device that wasn't
on that network. Don't implement it from the `MBC` spec alone.

**`preamp-adapter-aphex` is a codec with no transport, on purpose.** It builds and
parses the Aphex 1788A's SysEx correctly, from Aphex's own published command table,
but nothing sends it — the table documents the MIDI layer, and how SysEx is framed
over the unit's Ethernet port isn't published. Picking MIDI would make it the only
device here not on an IP socket and would add a MIDI backend plus ALSA headers to
Linux release CI, so that is a decision to take deliberately, not by drifting into
it. Three things the source document doesn't answer are listed in the crate's module
comment; don't paper over them.

When adding an adapter, follow the existing convention: **cite the protocol source in the
module doc comment.** That's what makes the Lectrosonics exception visible rather than
invisible.

## 4a. The gain lock's invariants

`crates/core/src/gain_lock.rs` compensates follower desks' trims for a leader's preamp changes.
See [`docs/GAIN-LOCK.md`](docs/GAIN-LOCK.md). Four rules hold it together; break one and it
misbehaves mid-show rather than in a test:

- **Deltas, never targets.** A follower is only ever moved *by* an amount, from what it holds at
  that moment (`nudge_object`). Never compute an absolute trim from lock-time state: that would
  overwrite a trim the follower's engineer changed.
- **Read live before a relative write.** An adapter with no protocol increment reads the desk's
  trim immediately before writing it, never from cache. A missed push, or another app taking
  Wing's single subscription, leaves a cache stale without warning.
- **Owed is never dropped.** What a follower couldn't take (range, step, error) is kept and
  offered again, so `applied + owed == -(leader drift)` always holds.
- **A dead desk is held, not waited on.** After a failed move its desk goes in `Inner::down`.
  Its followers accrue without I/O until the retry tick's probe (a read, made outside the lock)
  answers. Follower moves run in parallel across desks. Waiting out one timeout per follower,
  under the engine's lock, would freeze every healthy desk behind one dead one.
- **Only reported leader values drive it, and its own echoes don't.** The engine swallows one
  report of a value it read or set, but only as the next report for that signal (see
  `Live::expected`).

Trim objects are role `"In N Trim"` at `channel_scheme::trim_ono(N)`. That's an input-channel
space, separate from the preamp `channel` the other objects use.

## 5. Commands

```bash
cargo build
cargo test
cargo clippy --all-targets --all-features
cargo run --bin preamp-bridge   # crates/preamp-cli; `run` also serves the preamp-web UI
cargo run --bin mic-monitor     # crates/mic-cli
```

`bridge.example.toml` is the configuration template.

## 6. Conventions

- Multi-platform release CI; cross-compile macOS x86_64 on `macos-14` — never `macos-13`.
- Public repo. "Commit" means commit **and** push.

## 7. Related work

There is a separate `Dante-BabelBox-notes` repo holding research notes. Check there before
re-deriving protocol details.

## Diagnostics

Log via `tracing` as usual; `crates/diag` adds a rotating file, an in-memory ring and a
panic hook that writes a JSON crash report. Wire it as the **first** thing in `main`, and
**hold the returned guard** — dropping it (`let _ = diag::init(..)`) silently stops the log
file being written. Console output goes to stderr; stdout is reserved for program output.
See [docs/diagnostics.md](docs/diagnostics.md).

## Notes

`docs/NOTES.md` carries this repo's working notes — current status, decisions
already made, and the traps that have actually bitten. Read it before changing
anything non-obvious. Cross-cutting fleet knowledge lives in
[fleet-notes](https://github.com/stoatworks-labs/fleet-notes).
