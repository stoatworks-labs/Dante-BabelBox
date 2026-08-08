# SQ capture playbook

A step-by-step for the session when a real **Allen & Heath SQ** is on the
network. Goal: grab, in one sitting, the three things that unblock the DT/SQ
work (see `allenheath-dt-preamp-over-dante.md` for why each matters).

## What we're after

1. **ConMon CMC handshake** — Dante Controller connecting to the SQ over the wire.
   This is the *device-side* CMC response we could not fabricate; it unblocks the
   `tools/dt-fake` emulator.
2. **`AllenHth` over Dante?** — whether the SQ broadcasts / answers preamp
   messages on the ConMon channels (vendor id `AllenHth`, `41 6c 6c 65 6e 48 74
   68`). Settles the open SQ-over-Dante question either way.
3. **SQ console preamp protocol** — SQ-MixPad ↔ SQ on TCP **51325** while a preamp
   gain moves, to see the console's own preamp MIDI (validates BabelBox's A&H
   adapter for SQ).

## Setup

- Put the **SQ and this Mac on the same physical Dante network** (a switch, or a
  direct cable). Both will self-assign `169.254.x` if there's no DHCP — fine.
- In **Dante Controller** and **Dante Virtual Soundcard**, set the Dante interface
  to that NIC (the one the SQ is on). Confirm the SQ appears in DC's device list.
- Note the Mac's Dante interface name (e.g. `en0`, or whatever the USB/Thunderbolt
  Dante adapter enumerates as). You'll capture on it.
- Have ready: **Wireshark**, **Dante Controller**, and **SQ-MixPad** (already
  installed) pointed at the SQ.

## Wireshark

- **Interface:** the Dante NIC (the SQ is remote, so the network CMC is on the
  wire — no need for `lo0` this time).
- **Capture (BPF) filter** — keep it lean:
  ```
  udp portrange 8700-8850 or udp portrange 9700-9900 or net 224.0.0.224/27 or udp port 5353 or udp port 4440 or tcp port 51325
  ```
- Start capturing **before** the actions below, and **jot the wall-clock time** of
  each action (makes the pcap trivial to slice afterwards).

## Capture procedure (do these in order, keep capturing throughout)

1. **Fresh CMC connect.** With capture already running, force Dante Controller to
   (re)connect to the SQ: either **restart Dante Controller**, or in DC
   double-click the SQ to open **Device View**. → gives the unicast CMC handshake
   (the `0x1001` connect + the SQ's response).
2. **Idle observe (~30 s).** Just let it sit with DC open on the SQ. → captures the
   SQ's multicast ConMon status on `224.0.0.233:8708` etc. We'll grep these for
   `AllenHth`.
3. **Move a preamp from the SQ side.** On the SQ surface (or in SQ-MixPad), change
   **Input 1 gain** by a known amount (e.g. set it to +30 dB), toggle **+48V** on
   Input 1, and toggle the **pad**. Note each value/time. → this is the money shot:
   if the SQ emits `AllenHth` ConMon on any change, it's here; and the SQ-MixPad
   ↔ SQ TCP 51325 stream carries the console preamp protocol.
4. **Try the DT app against the SQ (optional).** Launch **DT Preamp Control**. It's
   built for DT boxes, but the SQ shares the `AllenHth` namespace — if it lists the
   SQ, poke a gain and note it. Low odds, but free.
5. Stop capture. **Save as `.pcapng`.**

## After

- Drop the pcap somewhere I can read it (e.g. `/Users/Shared/stoatworks-labs/captures/`)
  and tell me the action timestamps.
- Markers I'll pull on: `AllenHth` (`41 6c 6c 65 6e 48 74 68`) for A&H vendor
  payloads; `10 00 … 10 01` heads for CMC transport; the ConMon envelope
  (`FF FE`/`FF FF … <vendor>`); and on TCP 51325, MIDI (`B0 63 … B0 62 … B0 06`
  NRPN or `F0 00 00 1A 50 …` SysEx) for the console preamp control.

## Realistic expectations

- **CMC handshake: near-certain.** Any real Dante device connecting via DC gives it.
- **`AllenHth` over Dante: unknown — that's the point.** The SQ's preamps are
  internal to the console, so it may *not* advertise them over Dante ConMon the way
  a standalone DT box does. If it doesn't, that's a real answer: SQ preamp control
  is console-side (MIDI 51325), and Dante ConMon control is DT-box-only.
- **SQ MIDI 51325: near-certain to reveal the console preamp protocol**, which is
  independently useful for BabelBox even if the Dante answer is negative.
