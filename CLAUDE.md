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
- `preamp-adapter-{osc,ah,yamaha}` — preamp vendor adapters
- `mic-adapter-{shure,sennheiser,lectrosonics}` — wireless-mic vendor adapters (lectrosonics = placeholder wire format)
- `preamp-cli` / `mic-cli` — command-line entrypoints
- `preamp-web` — web UI

## Notes
- Adding a vendor = new `*-adapter-*` crate implementing the core trait; don't leak vendor specifics into `core`.
- Multi-platform release CI; cross-compile macOS x86_64 on macos-14 (never macos-13).
- Public repo. Ships user-facing AI disclaimer. "Commit" = commit **and** push.
