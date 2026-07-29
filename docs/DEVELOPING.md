# Developing Dante-BabelBox

Build, test and extension guide.

**Related docs, so this one doesn't duplicate them:**
- [`../USAGE.md`](../USAGE.md) — the user guide: CLI commands, config, troubleshooting
- [`plugin-development-guide.md`](plugin-development-guide.md) — writing a device plugin
- [`API.md`](API.md) — the patch-bay web API
- [`mic-telemetry-architecture.md`](mic-telemetry-architecture.md) — the mic side's design
- [`../AGENTS.md`](../AGENTS.md) — orientation and invariants

---

## Build and run

```bash
cargo build
cargo test
cargo clippy --all-targets --all-features

cargo run -p preamp-cli      # preamp-bridge
cargo run -p mic-cli         # mic-monitor
cargo run -p preamp-web
```

---

## Layout

```
crates/
  core/          Preamp traits and models. VENDOR-NEUTRAL.
  mic-core/      Radio-mic traits and models. VENDOR-NEUTRAL.
  discovery/     Dante mDNS discovery
  preamp-adapter-{osc,ah,yamaha}/     preamp vendor adapters
  mic-adapter-{shure,sennheiser,lectrosonics}/   mic vendor adapters
  plugin-osc-x32/    An adapter shipped as a loadable plugin
  preamp-cli/ mic-cli/ preamp-web/
```

---

## The rule the project exists to enforce

**Vendor specifics never enter `core` or `mic-core`.**

`core` describes what a preamp *is*; each adapter translates one manufacturer's protocol into
that. The moment a vendor quirk lands in `core`, the abstraction stops paying for itself and
every other adapter inherits someone else's oddity.

Concretely, the differences that must stay adapter-side: dLive addressing physical **sockets**
1–128 rather than console strips; Yamaha DM3 using **Local Input Num** 1–16 with coarse
integer-dB gain; Wing exposing only its 8 built-in LCL preamps.

---

## Adding a vendor

**Prefer a plugin over a new built-in crate.** `kind` is an open string and
`--plugins-dir` loads `.so`/`.dylib`/`.dll` at runtime, so a new vendor needs no change to the
config format or to this project. Every real vendor adapter already ships this way —
`plugin-osc-x32` is the worked example. Read
[`plugin-development-guide.md`](plugin-development-guide.md).

### Cite your protocol source — this is not optional here

**Each adapter's module doc comment states where its protocol came from**, and whether that
source is official, community-authoritative, or unverified.

That convention is what makes the project's one weak spot *visible*: the **Lectrosonics mic
adapter's wire format is an unverified placeholder**, and it's flagged in the README's status
table because of this discipline. Without it, an unverified adapter is indistinguishable from
a documented one.

If you work on Lectrosonics: either verify the format against real gear or documentation and
update the flag, or leave the flag exactly where it is. Don't quietly promote it.

---

## Testing

```bash
cargo test
```

`preamp-web`'s tests are the pattern to follow — they pin behaviour, not just happy paths:

- `add_virtual_device_needs_no_adapter`
- `add_real_device_connects_via_registry` — adding a real device really connects
- `add_real_device_surfaces_adapter_failure_as_bad_request` — a failed connection is a `400`,
  not an accepted-but-broken device
- `add_device_rejects_kind_with_no_channel_count_and_no_override`
- `add_device_duplicate_id_is_a_conflict`
- `remove_virtual_device_cascades_its_mappings`

Note how each error path has its **own** test and its **own** status code. Preserve that —
collapsing them into a generic 400 loses the operator's ability to tell "wrong address" from
"already exists".

### The honest limit

**No adapter has ever been validated against real hardware** — only against mocks in the test
suite. That's the single most valuable thing anyone could change, and the
[capture guides](capture-guide-macos.md) exist to support exactly that work: they explain how
to capture a real console-to-stagebox conversation with Wireshark, per OS.

The separate **`Dante-BabelBox-notes`** repo (private) holds further research material.

---

## Security posture

The patch-bay web UI has **no auth and no TLS**, and binds all interfaces by default. That is
a deliberate choice with a stated model — *a hardware router's control port on a trusted
operations network*. `--bind 127.0.0.1:PORT` restricts it to the local machine.

If you change the default, change the README and `USAGE.md` too; operators rely on knowing
which it is.

---

## Conventions

- Multi-platform release CI; cross-compile macOS x86_64 on **`macos-14`**, never `macos-13`.
- Public repo, ships a user-facing AI-assisted disclaimer.
- "Commit" means commit **and** push.
