# Dante-BabelBox API reference

Dante-BabelBox has three interfaces:

1. **The CLIs** — `preamp-bridge` and `mic-monitor`. Fully documented in
   [`../USAGE.md`](../USAGE.md); not duplicated here.
2. **The patch-bay web API** — documented below.
3. **The plugin ABI** — see
   [`plugin-development-guide.md`](plugin-development-guide.md).

---

# Patch-bay web API

Served by `preamp-web`, started by `preamp-bridge run`.

> **No authentication, no TLS.** The bind address defaults to all interfaces, deliberately —
> the intended trust model is *a hardware router's control port*: a trusted operations
> network. Use `127.0.0.1:PORT` to restrict it to the local machine. Don't expose it beyond a
> production LAN.

Errors are returned as `{"error": "..."}` with `400`, `404` or `409`.

---

## `GET /`
The patch-bay UI (a single embedded HTML page).

## `GET /api/state`

```json
{
  "devices":  [ /* DeviceView */ ],
  "mappings": [ /* ChannelMapping */ ]
}
```

## `POST /api/devices`

```json
{
  "id": "ahm-rack",
  "kind": "ah-tcp",
  "virtual": false,
  "address": "10.0.0.10",
  "port": 51325,
  "channels": 64
}
```

| Field | Required | Notes |
|---|---|---|
| `id` | yes | Must not be empty or already in use |
| `kind` | yes | **An open string, not a fixed list** — plugins register new kinds |
| `virtual` | no | Default `false`. A virtual device needs no adapter and never connects |
| `address` / `port` | for real devices | Where to reach it |
| `channels` | sometimes | Required when the kind has no known channel count |

Returns the created `DeviceView`.

**Error cases, each with its own status:**

| Status | Cause |
|---|---|
| `400` | Empty `id`; a `kind` with no known channel count and no `channels` override; **an adapter that failed to connect** |
| `409` | `id` already exists |

That third `400` matters: adding a *real* device **connects through the registry immediately**,
so a wrong address or an unreachable device fails the request rather than being accepted and
silently sitting broken. A **virtual** device needs no adapter and is accepted regardless.

## `DELETE /api/devices/:id`

**Removing a device cascades to its mappings.** They are deleted with it — you won't be left
with mappings pointing at a device that no longer exists.

`404` if unknown.

## `POST /api/mappings`

```json
{
  "from": { "device_id": "ahm-rack",      "channel": 1 },
  "to":   { "device_id": "x32-monitors",  "channel": 5 },
  "bidirectional": false
}
```

Both endpoints are `PreampAddress` (a device id plus a channel). `bidirectional` defaults to
`false` — a one-way mapping mirrors changes from `from` to `to` only.

`400` if either device id is unknown.

## `DELETE /api/mappings/:id`

## `GET /ws`
WebSocket. Pushes the same object as `GET /api/state` on connect and whenever it changes, so
the patch bay updates live without polling.

---

## Channel addressing differs per vendor — read this

A "channel" does not mean the same thing across device kinds, and getting it wrong silently
addresses the wrong preamp:

| Kind | What `channel` means |
|---|---|
| `ah-tcp` | Allen & Heath AHM-series processors |
| `dlive-tcp` | **Physical preamp *socket* number, 1–128** — not a console strip |
| `yamaha-dm3` | **Local Input Num, 1–16** |
| `osc-x32` | X32/M32/HD96 OSC dialect |
| `osc-wing` | Wing's own dialect — **the console's 8 built-in LCL preamps only** |

`yamaha-dm3` gain is **coarse integer dB, 0–64 only**. A fractional gain request cannot be
represented.

Not implemented: `ah-midi` (Qu/SQ) and `yamaha` (CL/QL/DM7, Rio/Tio).

---

## `kind` is deliberately open

`kind` is an **open string**, not an enum. Dropping a plugin `.so`/`.dylib`/`.dll` into
`--plugins-dir` registers new kinds **without changing the config format or this project**.

Every real vendor adapter ships as a plugin — only a couple of explanatory-error placeholder
kinds are built in. Even `osc-x32` is a loaded plugin (`crates/plugin-osc-x32`), not compiled
in directly.

See [`plugin-development-guide.md`](plugin-development-guide.md).

---

## Configuration

`bridge.toml` (preamps) and `mics.toml` (radio mics) — both fully documented in
[`../USAGE.md`](../USAGE.md), with `bridge.example.toml` as a template.

`preamp-bridge init --infer-mappings` can draft `[[mapping]]` entries by observing live Dante
audio routing. **Treat the result as a draft** — the README explains the caveat that comes
with trusting inferred mappings.
