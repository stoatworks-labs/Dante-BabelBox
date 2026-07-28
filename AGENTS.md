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
  preamp-adapter-osc/     \
  preamp-adapter-ah/       > preamp vendor adapters
  preamp-adapter-yamaha/  /
  mic-adapter-shure/         \
  mic-adapter-sennheiser/     > radio-mic vendor adapters
  mic-adapter-lectrosonics/  /  <- SEE WARNING BELOW
  preamp-cli/    Preamp command-line entry point
  mic-cli/       Mic command-line entry point
  preamp-web/    Web UI
```

## 4. Two honesty requirements

These are in the README as user-facing warnings and must not be quietly softened:

**Nothing has been validated against real hardware.** Every adapter is tested against mock
devices in the test suite only.

**The Lectrosonics mic adapter's wire format is an unverified placeholder.** Every other
adapter is built against official or community-authoritative vendor protocol specs — and each
adapter's module doc comment cites its source. Lectrosonics is the flagged exception. If you
work on it, either verify the format against real gear/documentation and update the flag, or
leave the flag exactly where it is.

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
