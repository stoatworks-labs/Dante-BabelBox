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
TCP to real devices. Only `plugin-rednet-aes70` uses it.

## 4. Two honesty requirements

These are in the README as user-facing warnings and must not be quietly softened:

**No adapter has been validated against real hardware.** Every adapter is tested against mock
devices in the test suite only. Say "no adapter has been validated", not "nothing has been
validated" — the distinction now matters, see below.

**The Yamaha R-series HA protocol *has* been proven on real hardware**, and this is the one
place the honesty warning got *stronger* rather than weaker. It was captured from a real
QL1 + Rio3224-D2, decoded, written up as
[`docs/yamaha-ha-remote-over-dante.md`](docs/yamaha-ha-remote-over-dante.md), then rebuilt
from that document and transmitted — the stagebox accepted it and changed its gain. Keep the
line sharp when editing: **the protocol is verified, the code is not.** The write came from a
standalone Python script; `preamp-adapter-yamaha` still only covers DM3 over OSC and has no
Rio support at all. Anyone implementing Rio HA is working from evidence, not guesswork — that
is the claim, and it should not be inflated into "the Yamaha adapter works".

**The Lectrosonics mic adapter's wire format is an unverified placeholder.** Every other
adapter is built against official or community-authoritative vendor protocol specs — and each
adapter's module doc comment cites its source. Lectrosonics is the flagged exception. If you
work on it, either verify the format against real gear/documentation and update the flag, or
leave the flag exactly where it is.

**The Focusrite RedNet plugin is built against a published standard, not a
capture.** RedNet units carry an AES70 endpoint in firmware (RedNet Control's
per-device `AES70 Enable/Disable`), so `plugin-rednet-aes70` implements AES70-1/-3
and enumerates the device's objects at runtime rather than shipping a vendor ONo
map. An MP8R can *also* be driven as a Yamaha head amp, and that route is
deliberately not taken: see [`docs/rednet-mp8r-capture-request.md`](docs/rednet-mp8r-capture-request.md)
for the five things the QL1/Rio captures can't answer about a device that wasn't
on that network. Don't implement it from the `MBC` spec alone.

When adding an adapter, follow the existing convention: **cite the protocol source in the
module doc comment.** That's what makes the Lectrosonics exception visible rather than
invisible.

## 5. Commands

```bash
cargo build
cargo test
cargo clippy --all-targets --all-features
cargo run -p preamp-cli
cargo run -p mic-cli
cargo run -p preamp-web
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
