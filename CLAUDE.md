# Dante-BabelBox

Cross-vendor Dante preamp + mic bridge (Rust). Normalizes preamp control and wireless-mic monitoring across vendors behind common core traits, exposed via CLIs and a web UI.

## Commands
- Build: `cargo build` (workspace)
- Test: `cargo test`
- Lint: `cargo clippy --all-targets --all-features`
- Run preamp CLI: `cargo run -p preamp-cli`
- Run mic CLI: `cargo run -p mic-cli`
- Preamp web UI: `cargo run -p preamp-web`

## Layout (crates/)
- `core` / `mic-core` — shared traits & models
- `discovery` — Dante device discovery
- `oca` — internal object model; `oca-plugin-abi` — FFI host/plugin contract
- `ocp1` — a *real* AES70-3 (OCP.1) controller. Not the same thing as `oca`
- `plugin-*` — dynamically-loaded device plugins (cdylib), one per vendor
- `plugin-aes70` — any AES70/OCA device (written against Focusrite RedNet);
  built from the published standard, enumerates the device's objects at
  runtime. Kind is `aes70`, with `rednet-aes70` kept as an alias. See
  `docs/rednet-mp8r-capture-request.md` for why the Yamaha route wasn't used
- `preamp-adapter-aphex` — Aphex 1788A SysEx **codec only**; nothing delivers
  the bytes yet, because the transport (MIDI vs the unit's Ethernet port) is
  undecided. Don't wire a transport without reading the module comment first
- `preamp-adapter-{osc,ah,yamaha}` — preamp vendor adapters
- `preamp-adapter-yamaha` also carries `mbc` — Rio/Tio + QL/CL head amps over Audinate ConMon
- `mic-adapter-{shure,sennheiser,lectrosonics}` — wireless-mic vendor adapters (lectrosonics = placeholder wire format); `mic-adapter-shure-acn` = QLX-D on a console, read-only
- `preamp-cli` / `mic-cli` — command-line entrypoints
- `preamp-web` — web UI

## Notes
- Adding a vendor = new `*-adapter-*` crate implementing the core trait; don't leak vendor specifics into `core`.
- Multi-platform release CI; cross-compile macOS x86_64 on macos-14 (never macos-13).
- Public repo. Ships user-facing AI disclaimer. "Commit" = commit **and** push.

## Diagnostics

Log via `tracing` as usual; `crates/diag` adds a rotating file, an in-memory ring and a
panic hook that writes a JSON crash report. Wire it as the **first** thing in `main`, and
**hold the returned guard** — dropping it (`let _ = diag::init(..)`) silently stops the log
file being written. Console output goes to stderr; stdout is reserved for program output.
See [docs/diagnostics.md](docs/diagnostics.md).
