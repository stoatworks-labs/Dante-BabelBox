# `dt-fake` — DT168 bring-up / capture harness

A bench tool for reverse-engineering and (eventually) emulating the Allen & Heath
**DT168 / DT164-W** control protocol so the real *DT Preamp Control* app can be
tested against it. **Not a finished emulator** — see status below.

It pairs with the codec in `crates/preamp-adapter-ah/src/dt.rs` and the protocol
write-up in [`docs/allenheath-dt-preamp-over-dante.md`](../../docs/allenheath-dt-preamp-over-dante.md).

## Run

```
python3 tools/dt-fake/fake_dt168.py --iface en0            # advertise + capture + respond
python3 tools/dt-fake/fake_dt168.py --iface en0 --listen-only   # capture only, no mDNS
python3 tools/dt-fake/fake_dt168.py --iface en0 --respond connect
```

Pure stdlib + macOS `dns-sd`. `--iface` **must** be the machine's real Dante
interface (not a VPN/VM one) — pick the NIC that Dante Controller / DVS is bound
to, or nothing will see the fake.

## What works / what's blocked

- **Discovery: works.** Pinned to the Dante NIC, the fake appears to the Audinate
  runtime and Dante Controller tries to connect.
- **ConMon envelope: known** (from a real DVS capture) — `wrap_conmon`/
  `parse_conmon` in the codec.
- **CMC connect handshake: blocked.** Completing it needs the *device-side* CMC
  response byte-for-byte, which is generic Audinate (not in any A&H firmware we
  can read). Until we capture a real device doing it, DC keeps re-probing and the
  fake never fully connects. This is the one missing piece.

## To unblock it — capture a real device

The fastest path is a packet capture; two options, either works:

- **A real DT168 + the app** — gets the CMC transport *and* the `AllenHth` preamp
  bytes at once. Best.
- **Any remote Dante device** (a second computer running DVS, an SQ, a stagebox) —
  gets the CMC handshake reference alone, which is enough to finish the emulator.
  A **same-host DVS does not work** (its control goes over loopback IPC, not the
  network CMC).

Step-by-step recipes:
- General: the "Capturing the reference" section of the protocol doc.
- For an SQ specifically: [`docs/sq-capture-playbook.md`](../../docs/sq-capture-playbook.md).

Once you have a `.pcapng`, drop it somewhere readable and share the action
timestamps; the `AllenHth` id and the `10 00 … 10 01` CMC heads are the anchors.
