# `ewdx-probe`: SSC bench client for Sennheiser EW-DX EM

A bench tool for putting `crates/mic-adapter-sennheiser` (kind `sennheiser-ewdx`) in front of
a real **EW-DX EM 2 / EM 2 Dante / EM 4 Dante** and recording what the receiver really says.
It runs the session in [`docs/sennheiser-ewdx-bench-plan.md`](../../docs/sennheiser-ewdx-bench-plan.md).
It has not been run against a receiver yet.

Both scripts are pure stdlib Python 3.9+, with nothing to install.

| File | What it is |
|---|---|
| `ewdx_probe.py` | The client: read, write, sweep, subscribe, snapshot, restore and diff, plus canned error and client-limit probes. Every datagram both ways goes to a JSONL log. |
| `fake_ewdx.py` | A stand-in receiver shaped on Sennheiser's guide, for rehearsing. **Nothing it says is evidence.** |

## Commands

```
ewdx_probe.py --host IP [--port 45] [--log FILE] [--wait S] [--allow-rf] COMMAND
  get PATH...                     read; one message unless --separate
  set PATH VALUE                  read, write, show the reply (marks an adapted value), read back
  send JSON                       one raw message; --malformed allows invalid JSON
  snapshot -o FILE                every documented readable leaf, plus error replies verbatim
  restore FILE [--dry-run]        write back name, mute, gain, AF-out level, location, lock, brightness
  sweep gain|level|mute           step through the range with read-back, then restore; --odd, --pause
  watch adapter|meters|status|paths [PATH...]
                                  subscribe and print; --lifetime --count --min --max --renew
                                  --duration; cancels on exit unless --leave
  errors                          the read-only error and edge-case probes
  clients --n 9                   open N clients, each subscribing, to find the limit; --close
ewdx_probe.py diff A.json B.json  offline; ignores meters, battery, warnings unless --all
```

`watch adapter` sends exactly the tree `SennheiserAdapter::subscribe_channel` sends, with the
same `"#": {"lifetime": 3600}`, so the receiver's answer to it is the answer the Rust adapter
will get.

## What it won't send

Writes to these paths are refused before anything goes on the wire:

- `/device/restore`, `/rxN/restore`, `/device/restart`;
- `/device/link_density_mode`, which reboots the receiver;
- `/device/encryption`, everything under `/device/network/`, `/device/time`;
- `/device/booster`, which puts DC up the antenna cable on an EM 4 Dante;
- `/device/remote_access`, `/rxN/presets/*`, `/rxN/sync_settings/*`.

`/rxN/frequency` needs `--allow-rf`. There is no override for the rest. To send one of them,
use a different tool on purpose.

## Two things it does differently from the Rust adapter, on purpose

- **Its socket isn't connected.** The adapter's UDP socket is `connect()`ed to `ip:45`, so a
  reply from any other source port is dropped by the OS. The probe sees every reply and
  prints a warning the first time one comes from somewhere unexpected (bench plan, P2).
- **It cancels its subscriptions on exit.** The adapter never does. The plan's leaked-slot
  test (P4) uses `--leave` to behave like the adapter.

## Rehearse against the fake

```bash
python3 tools/ewdx-probe/fake_ewdx.py
```

```bash
python3 tools/ewdx-probe/ewdx_probe.py --host 127.0.0.1 --port 4545 snapshot -o /tmp/before.json
```

The fake follows the guide's ranges and steps, read-only leaves, 424 for an absent
transmitter, subscription lifetime and count with 310 on expiry, the eight-client limit, and
`min`/`max`. Where the guide is silent it guesses: the error JSON, rounding to the nearest
step, ack-then-initial-notification, a 50 ms meter tick, and one client per source port. Each
guess is marked `GUESS` in its source. `kill -USR1 <pid>` switches transmitter 1 off and on.
`mic-monitor watch` runs against it with `port = 4545` in `mics.toml`.
