# Capture request: a Focusrite RedNet MP8R driven by a Yamaha CL/QL

**Status: open.** This is a request for evidence, not a specification. If you
have a RedNet MP8R and a CL or QL console on the same Dante network, three
minutes of packet capture would let this project support the MP8R over Yamaha's
head-amp protocol as well as over AES70.

Nothing here is needed for the **AES70/OCA** path, which is already implemented
in [`crates/plugin-rednet-aes70`](../crates/plugin-rednet-aes70) and needs no
Yamaha console at all. This document is about the *other* route.

---

## 1. Why the existing captures aren't enough

The MP8R really does speak Yamaha. Its manual has a **`Yamaha ID` setting
(`Off`, `Y000`–`Y00F`)** — "the Yamaha ID to which the unit will respond" — and
that is the same ID space this project's own capture sits in: the QL1 appears as
`Y001-Yamaha-QL1-17ea2c`, the Rio3224-D2 as `Y004-Yamaha-Rio3224-D2-25df04`.
Focusrite advertise remote gain, phantom power and HPF from CL/QL consoles. It
is a reasonable working assumption that the MP8R implements the same R-series
head-amp path documented in
[`yamaha-ha-remote-over-dante.md`](yamaha-ha-remote-over-dante.md).

What the existing captures give that document is the **transport**, and all of
that would carry over unchanged:

- the ConMon envelope, including the offset-40 vendor length
- the `MBC` block framing and the §4.1 block-boundary rule
- the §8 checksum
- gain as big-endian centi-dB, and the phantom encoding
- §7's console-wins pairing behaviour

What they cannot give is anything about the MP8R, because **no RedNet device was
on that network**. The capture is a QL1 and a Rio3224-D2, nothing else.

## 2. The five unknowns

| # | Unknown | Why it can't be guessed |
|---|---|---|
| 1 | **Gain range and encoding** | The MP8R is **10–65 dB in 1 dB steps**; the Rio is −6…+66. The codec's clamp constants are the Rio's. What an MP8R does with a −6.00 dB centi-dB value is undefined — it may clamp, ignore the message, or wrap. |
| 2 | **Channel mapping** | The captured gain broadcast is always **32 slots**. The MP8R has 8 inputs and presents 16 network channels (9–16 being the gain-compensated split). Which slots it reads, and whether it accepts a 32-element array at all, is unobserved. |
| 3 | **Identity and addressing** | `MBC` addresses a device by MAC — the Rio's Dante MAC, and the console's *second*, non-Dante NIC. The MP8R has primary and secondary Dante ports; which MAC it answers on is unknown. Nor does the spec have a field that carries the `Y00x` ID, so how the ID reaches the wire is an open question. `0x0722`/subop `0x12` = 13 is the only unexplained per-device constant and the obvious model-code candidate. |
| 4 | **Pad, HPF, impedance, polarity** | The MP8R's HPF is a fixed 65 Hz on/off; the Rio's looks frequency-variable. These live in the `0x0722` subops that §6 records with a known *shape* and an unknown *meaning*. |
| 5 | **Whether a cold write works at all** | §9's hardware proof was a Rio **already paired with a live QL1**. The `0x0711` pairing handshake (subops `01`–`09`) has never been implemented. Whether any device accepts `MBC` with no console session established is untested, and it is the single most likely thing to make a from-the-spec MP8R adapter silently do nothing. |

Building an adapter from the existing captures alone would produce something in
the same class as `mic-adapter-lectrosonics` — a plausible guess flagged as a
placeholder. That is not a trade this project makes when there is a documented
standards-based alternative for the same device.

## 3. What a useful capture contains

**Gear:** a CL or QL console, a RedNet MP8R with its `Yamaha ID` set to
something other than `Off`, and a mirrored port. §3 of the
[capture guides](capture-guide-macos.md) has a step-by-step for a
USW-Flex-Mini if you don't already have a mirroring switch.

Perform these with a few seconds of silence between each, so every event is
unambiguous in time:

1. **Power-up / pairing.** Start the capture *before* the console sees the
   MP8R, so the whole `0x0711` handshake is recorded. This is the single most
   valuable part — it is the thing the existing captures are missing entirely.
2. **Gain sweep on input 1 only**, slowly, bottom of range to top and back.
   One input at a time is what identifies the channel indexing.
3. **Gain on input 8**, so it's clear whether all 8 inputs live in one array
   and at which offsets.
4. **+48 V on and off, input 1**, then input 8.
5. **HPF on and off, input 1.**
6. **Pad (−20 dB) in and out, input 1**, then the **2.4 kΩ impedance**
   switch, then **polarity**. Each should light up exactly one of the
   currently-unmapped `0x0722` subops.
7. If you can, **change gain at the MP8R's own front panel** and let the
   console resync — that shows whether §7's console-wins rule holds for a
   third-party head amp too.

Three minutes of this is enough. Strip the audio before sending anything: it
will otherwise be ~99.9 % of the file.

```bash
tcpdump -r raw.pcapng -w control.pcap 'not (udp portrange 14336-14591)'
```

Please also note the MP8R's `Yamaha ID`, its Dante device name, and its firmware
version — the ID in particular is what ties the on-wire identity to the setting.

## 4. What would happen with one

Items 1–5 in §2 all become answerable from a single capture, and the encoder in
`preamp-adapter-yamaha::mbc` — which already rebuilds a hardware-accepted packet
byte for byte — would need a per-device profile (range, channel count, identity)
rather than new protocol work. That is a small change sitting behind a missing
three-minute recording.

Captures land in the private `dante-captures` repo, audio-stripped, with
provenance recorded per file. Open an issue on
[Dante-BabelBox](https://github.com/stoatworks-labs/Dante-BabelBox/issues) and
we'll sort out how to get the file across.
