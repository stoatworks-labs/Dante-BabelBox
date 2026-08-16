# SQ capture playbook

A step-by-step for the session when a real **Allen & Heath SQ** is on the
network. Goal: grab, in one sitting, everything that unblocks the DT/SQ work
(see `allenheath-dt-preamp-over-dante.md` for why each matters).

Written for a bench with an **SQ + Dante card (KLANTE)**, **Dante Virtual
Soundcard**, and a **Dante AVIO USB** adapter. Each plays a different role — see
"What each box is for" below.

> If the bench also has **R Remote** and **DT Preamp Control** available, run
> [`bench-session-playbook.md`](bench-session-playbook.md) instead — it sequences
> this procedure alongside the emulator work those two apps make possible, and
> the SQ steps here are Phase 5 of it.

## What we're after

Ordered by value-per-minute on the bench.

1. **CMC device-side response** — the *one* thing blocking `tools/dt-fake`. Needs
   a real **remote** Dante device answering Dante Controller. Near-certain.
2. **`AllenHth` on the wire** — whether the SQ broadcasts or answers preamp
   messages (vendor id `41 6c 6c 65 6e 48 74 68`). The coin-flip; settles the open
   SQ-over-Dante question either way.
3. **Gain scale + endianness calibration** — if (2) is positive, this closes three
   `[OPEN]`s at once and de-provisionalises the codec's dB conversion. See the
   dedicated procedure below.
4. **SQ console preamp protocol** — SQ-MixPad ↔ SQ on TCP **51325** while a preamp
   gain moves. Independent of Dante; useful even if (2) is negative.

**Already closed, don't re-capture:** the **ConMon wire envelope** (magic/length/
seqnum/EUI-64/vendor-id layout and the multicast channel map) was captured from a
real DVS on 2026-08-08 and is implemented as `wrap_conmon`/`parse_conmon`. Only
the **body** sub-framing inside the envelope is still vendor-specific and unknown.

## What each box is for

| Box | Role | Why |
|---|---|---|
| **SQ + KLANTE card** | Primary target | The only device here that could answer `AllenHth`. Also a valid CMC reference (remote device). |
| **Dante AVIO USB** | CMC control sample | Ultimo silicon — a *second, independent* Audinate implementation. Cross-check against the SQ's KLANTE to test the "CMC response is generic Audinate" assumption the emulator rests on. |
| **DVS** | Local runtime, **not** a CMC target | Provides the Dante runtime + Dante Controller transport. A **same-host DVS is not usable as a CMC reference** — its control goes over loopback IPC, not network CMC. (It already gave us the envelope; that job's done.) |

## Setup

- Put the **SQ, the AVIO and this Mac on the same physical Dante network** (a
  switch, or direct cable for the SQ if capturing point-to-point). All will
  self-assign `169.254.x` with no DHCP — fine.
- In **Dante Controller** and **DVS**, set the Dante interface to that NIC.
  Confirm the SQ *and* the AVIO both appear in DC's device list.
- Note the Dante NIC's name (`en0`, or whatever the Thunderbolt/USB adapter
  enumerates as). You'll capture on it.
- Have ready: **Wireshark**, **Dante Controller**, **SQ-MixPad** pointed at the SQ,
  and optionally **DT Preamp Control**.

> **Don't do this on a show console.** Steps 5–7 change preamp gain and toggle
> phantom power. Pull the mic lines, or use a spare SQ.

## Wireshark

- **Interface:** the Dante NIC. (No `lo0` needed — both targets are remote. No
  mirror port needed either: DC runs on the capturing Mac, so the unicast CMC is
  to/from this host and is visible locally.)
- **Capture (BPF) filter:**
  ```
  udp portrange 8700-8850 or udp portrange 9700-9900 or net 224.0.0.224/27 or udp port 5353 or udp port 4440 or tcp port 51325
  ```
  This already excludes Dante audio (UDP 14336–14591), so the pcap stays small —
  don't skip it and plan to strip afterwards, that's how the QL1 capture reached
  1.7 GB.
- Start capturing **before** the actions below, and **jot the wall-clock time** of
  each action. Values + timestamps are what make the pcap sliceable afterwards.

## Capture procedure

Keep one capture running throughout if you can; the timestamps separate the phases.

### Phase A — CMC transport (near-certain win)

1. **Fresh CMC connect to the SQ.** With capture running, force DC to (re)connect:
   restart Dante Controller, or double-click the SQ to open **Device View**. →
   gives the unicast CMC handshake — the controller's `0x1001` connect *and the
   SQ's response*, which is the byte sequence `dt-fake` needs.
2. **Same again against the AVIO USB.** Open its Device View. → the cross-check
   sample.
3. **Idle observe (~30 s)** with DC open. → multicast ConMon status on
   `224.0.0.233:8708` / `224.0.0.231:8702` from both devices.

### Phase B — the `AllenHth` question

4. **Grep the idle traffic for the vendor id** before moving on — if `AllenHth`
   never appears in Phase A's 30 s, the SQ may only emit it *on change*, which
   Phase C will settle.

### Phase C — preamp moves + gain calibration

This is the money shot. Work on **Input 1** throughout and write down every value.

5. **Park at known gains.** On the SQ surface (or SQ-MixPad), set Input 1 gain to
   each of these in turn, pausing ~3 s at each so it's unambiguous in the timeline:

   - the console's **minimum** (note what it displays)
   - **0 dB** if the console offers it
   - **+10 dB**, **+20 dB**, **+30 dB**, **+40 dB**
   - the console's **maximum** (note what it displays)

   Two points solve scale + offset; the rest confirm it's linear and whole-dB.

6. **Toggle pad** on Input 1, then off. Note the times.
7. **Toggle +48V** on Input 1, then off. Note the times.
8. **One other channel.** Move Input 9 to +20 dB — confirms per-preamp indexing and
   tells us how the SQ addresses its 48 local + SLink sockets (the DT model is a
   flat 16, so the SQ's layout may well differ).

### Phase D — console-side protocol

9. **SQ-MixPad preamp move.** With MixPad connected, change Input 1 gain again. →
   TCP 51325 carries the console's own preamp protocol.
10. **DT Preamp Control against the SQ (optional).** Launch it; if it lists the SQ,
    poke a gain. Low odds, but free — and if it *does* work, it gives the
    controller→device set message directly.

11. Stop capture. **Save as `.pcapng`.**

## What the gain calibration should show

Predictions worth checking on the spot — a mismatch is itself a finding.

- Gain is a `u16` whole-dB control centred near `0x8000`. If the reference holds,
  **+30 dB ≈ `0x801E`**. That makes endianness trivially readable: `80 1E` on the
  wire is big-endian, `1E 80` is little-endian. This single value settles the
  `[OPEN]` on byte order.
- The `-20 dB` **pad is a display offset, not a gain change** (the app's QML applies
  `conversionOffset: pad ? -20 : 0`). So toggling pad should flip **flags bit 0**
  and leave the `u16` *unchanged*. If the `u16` moves by 20 instead, the codec's
  model is wrong and `dt.rs` needs a note.
- **+48V is flags bit 1.**
- Min/max are device-reported rather than fixed, so the two endpoint values give
  the SQ's actual range — which need not match a DT168's.

## Optional: does `AllenHth` need DAPI?

Only after the passive capture above is **saved to disk**.

The repo currently says the ConMon transport needs Audinate DAPI. The Yamaha MBC
work suggests otherwise: a plain laptop UDP socket to `224.0.0.232:8705` was
accepted by a real Rio3224-D2 (`ql1-rio3224d2-write-test-accepted.pcap`). If
`AllenHth` behaves the same way, `plugin-ah` can ship without linking Audinate's
SDK at all.

Cheapest test: send a **poll** (`_Mess_GetAllMicPre`, read-only) wrapped in the
known ConMon envelope with vendor `AllenHth`, from a plain socket to
`224.0.0.233:8708` and to `224.0.0.231:8702`, and see whether the SQ answers.
Read-only, so nothing is at risk. Only try a *set* after a poll round-trips.

## After

- Drop the pcap somewhere readable (e.g. `/Users/Shared/stoatworks-labs/captures/`)
  and share the action timestamps + the gain values you parked at.
- Markers to pull on:
  - `AllenHth` = `41 6c 6c 65 6e 48 74 68` — A&H vendor payloads (offset 16 in the
    ConMon envelope).
  - `10 00 … 10 01` heads — CMC transport.
  - ConMon envelope: `FF FE` (status/monitoring) / `FF FF` (control), then length,
    seqnum, EUI-64 device id, 8-byte vendor id.
  - TCP 51325: MIDI NRPN (`B0 63 … B0 62 … B0 06`) or A&H SysEx (`F0 00 00 1A 50 …`).
- Archive it in `stoatworks-labs/dante-captures` (private) with a README row
  giving packet count, duration and what it holds — and the action timestamps,
  since several of these are one-time physical events.

## Realistic expectations

- **CMC handshake: near-certain.** Any real remote Dante device connecting via DC
  gives it. Two independent samples (KLANTE + Ultimo) also test whether the
  response really is generic across Audinate implementations — the assumption
  `dt-fake` rests on. If they differ, the emulator needs the KLANTE variant.
- **`AllenHth` over Dante: genuinely unknown — that's the point.** The SQ's preamps
  are internal to the console, so it may not advertise them the way a standalone DT
  box does. A negative is a real result: it means SQ preamp control is console-side
  (MIDI 51325) and ConMon preamp control is DT-box-only.
- **Gain calibration: only if `AllenHth` shows up.** Conditional on the coin-flip,
  but if it lands it's the highest-value half hour available.
- **SQ MIDI 51325: near-certain to reveal the console preamp protocol**, which is
  independently useful for BabelBox whatever the Dante answer is.
- **What this session cannot settle:** the DT168/DT164-W itself. An SQ is not a DT
  box — the flat-16 model, the `DT Box 1608` naming and DT addressing stay
  inferred, and a positive SQ result may not transfer cleanly.
