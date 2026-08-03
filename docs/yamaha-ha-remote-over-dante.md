# Yamaha HA remote control over Dante — the `MBC` vendor block

> ## 🔬 Built from real hardware — and proven on it
>
> **This is the first protocol in Dante-BabelBox that is not derived from a
> vendor document.** It was captured from a real Yamaha QL1 paired to a real
> Rio3224-D2, decoded, written up as the spec below, then **rebuilt from that
> spec and transmitted back to the stagebox, which accepted it and changed its
> gain.** The reference code in §10 reproduces, byte for byte, a packet a real
> Rio3224-D2 acted on.
>
> **This part of the project is ready for actual hardware testing.** An adapter
> written against this document is implementing a verified wire format, not a
> guess. That is a materially different position from every other adapter in
> this workspace.
>
> **What that does *not* mean:** no Rust code in this repo has driven real
> hardware. The write was performed by a standalone Python script. There is
> still no Rio adapter. See §11 for the exact line between the two.

*Read the confidence markers throughout:* some findings are **confirmed** by
watching values change under a known physical action, some are **inferred**, and
the inferred ones are flagged as such rather than smoothed over.

---

## 1. The short version

Yamaha does **not** carry head-amp control over Dante's own routing protocol.
Gain, phantom power and metering travel as a **Yamaha-proprietary block
tunnelled inside Audinate ConMon packets**, identified by the three ASCII bytes
`MBC`. Dante carries it the way it would carry any other vendor's ConMon
payload — Audinate's own control ports (4440 ARCP) are used only for *patching*.

Two consequences for this project:

- An adapter that speaks only Audinate's documented control protocols will
  never see a gain change.
- The MBC block is addressed by **Yamaha device MAC**, not by Dante device MAC
  or IP. The two are different NICs on the same console.

## 2. Capture provenance

| | |
|---|---|
| Console | Yamaha QL1 — Dante MAC `00:1d:c1:17:ea:2c` (`169.254.177.13`), Yamaha MAC `00:a0:de:e0:ce:f6` |
| Stagebox | Yamaha Rio3224-D2 — Dante MAC `00:1d:c1:25:df:04` (`169.254.193.229`) |
| Dante names | `Y001-Yamaha-QL1-17ea2c`, `Y004-Yamaha-Rio3224-D2-25df04` |
| Capture | 11 871 211 frames / 1.37 GB, 183 s, port-mirrored |
| Actions performed | pairing, patching, gain sweeps on inputs 1–4, +48 V on/off on inputs 1–4 |

99.9 % of the capture is Dante audio (UDP 14337–14351). Everything described
here lives in the remaining ~5 000 frames. To reproduce, strip the audio first:

```bash
tcpdump -r capture.pcapng -w control.pcap 'not (udp portrange 14336-14591)'
```

## 3. Where MBC rides

| Port | Address | Carries |
|---|---|---|
| UDP 8705 | `224.0.0.232` | ConMon status multicast — **most MBC traffic**, including metering |
| UDP 8708 | `224.0.0.233` | ConMon multicast |
| UDP 8800 | unicast | ConMon unicast — device-name exchange during pairing |
| UDP 4440 | unicast | Audinate ARCP — **patching only**, no MBC |

### The ConMon envelope

Verified by building one from scratch that a real Rio accepted:

```
offset  size  field
     0     2  magic — 0xffff (8705/8800) or 0xfffe (8708)
     2     2  total UDP payload length
     4     2  sequence
     6     2  0000
     8     8  device EUI-64 of the sender
    16     8  "Audinate"
    24     4  message class (e.g. 072e1002 from the QL1, 07311002 from the Rio)
    28     4  00000000
    32     2  sequence, repeated
    34     2  0010
    36     2  0001
    38     2  0000
    40     2  vendor payload length: (len(MBC block) + 1) << 8 | 0xC0
    42     .  the MBC block
```

The length at offset 40 is the one non-obvious field — its high byte is the MBC
block length **plus one**, its low byte is always `0xC0`. Getting this wrong is
silent: the device simply ignores the packet.

Searching the UDP payload for the bytes `MBC` is a reliable shortcut when
*reading*. When *writing*, the whole envelope has to be right.

## 4. The MBC block

```
"MBC" | ver:u8 | len:u16 | src_mac[6] | dst_mac[6] | 0000 | len_lo:u8 | flags:u8 | opcode:u16 | body
```

- `ver` — always `0x01` in this capture.
- `len` — **counts from `len_lo` to the end of the block**, so
  `len(body) == len - 4`. This matters: several messages are shorter than
  their ConMon packet, and several are longer than the bytes actually
  present when the packet ends. Trust `len`, not the packet boundary.
- `src_mac` / `dst_mac` — **Yamaha** MACs. The console appears as
  `00:a0:de:e0:ce:f6`, which is *not* its Dante MAC. Broadcast-style messages
  use `ff:ff:ff:ff:ff:ff` as the destination.
- `flags` — **varies by message class; do not hard-code one value.** Bit `0x20`
  is the direction: clear from the console, set from the device. The low bits
  vary — short messages use `0x01`/`0x21`, but the 32-channel gain broadcast
  uses `0x00`/`0x20`. Getting this wrong produces a packet the device silently
  discards. Copy the value from the equivalent captured message.
- `len_lo` — the low byte of `len`, repeated. Purpose unknown (**inferred**:
  a framing sanity check).

### Body

```
subop:u8 | count:u16 | start_index:u16 | data[count × width] | checksum:u8
```

`count` is the number of *elements*, not bytes; `width` comes from the subop.
A body with `count = 1` and no data bytes is a **read request** — the console
uses this to poll.

## 5. Confirmed messages

These four were confirmed by correlating the wire values against physical
actions performed at a known time.

### `0x0722` / subop `0x16` — head-amp gain

32 × **int16 big-endian, centi-dB** (`0x0100` = +1.00 dB).

Confirmed by sweeping input 1, then 2, then 3, then 4 in isolation and watching
exactly one array slot move each time. Observed range −6.00 dB … +25.00 dB,
every value an exact whole dB. The floor of **−6.00 is the Rio3224-D2's
documented minimum gain**, and the sweep visibly clamped there — good evidence
the unit is native centi-dB with no scaling, and that the full range is
−6.00 … +66.00.

```
16 0020 0000 fda8 fda8 ... (32 × int16) ... <cksum>
   │    └── start index 0
   └── count = 32 channels
```

### `0x0722` / subop `0x17` — +48 V

32 × **uint8 boolean**. Confirmed by switching phantom on inputs 1→4 and then
off 1→4; the array tracked it exactly, one slot per press.

### `0x0742` / subop `0x00` — input metering

32 × **uint8**, broadcast by the stagebox at **31 Hz** (3 461 messages in
110 s). Observed value range 31 (floor, silence) to 64 (peak).

**Inferred, not confirmed:** the scale. No calibrated signal was injected, so
the mapping from byte value to dBFS is unknown. Do not present this as a
calibrated meter.

### `0x0731` / subop `0x01` — single-channel change

Body carries a real `start_index` and one value:

```
01 0001 0003 01 <cksum>     # index 3, value 1
```

Sent as a **1-then-0 pair** a few hundred ms apart. **Inferred:** this is the
console surface reporting a key down / key up, not two state changes — the
resulting state array only moves once.

## 6. Other opcodes seen

Present in the capture with a stable shape but no confirmed meaning.

| Opcode | Subops | Notes |
|---|---|---|
| `0x0711` | `01`–`09` | pairing handshake; subop `05` carries the Dante device name as ASCII |
| `0x0712`, `0x0713` | `0a` | single exchange each during pairing |
| `0x071f` | `06` | ~6 s heartbeat, both directions |
| `0x0722` | `10`,`12`,`13`,`15`,`18`,`19`,`1a`,`1b` | further per-channel HA arrays — see below |
| `0x0731` | `01` | 32 × uint8, all zero throughout |
| `0x0731` | `02` | 24 × uint8, all zero throughout — note **24**, not 32 |
| `0x0742` | `00` | metering (above) |

### Why the remaining `0x0722` subops stayed blank

Across 5 301 ConMon frames — including the 32 packets that carry more than one
MBC block — these subops appear **only in their query form**: `count = 32`,
`start_index = 0`, no data. The console asks for them once at pairing; nothing
ever populated them, because none of those parameters was touched while
capturing.

So their element count (32, one per input) is known and their addressing is
known, but their **element width and meaning are not**. Do not guess: a 32-slot
array is equally consistent with HPF frequency, HPF on/off, digital trim,
polarity, pad, insert state or metering mode.

Mapping them is a ten-minute job with hardware, and needs nothing clever — put a
capture on the control network and, pausing a few seconds between each so the
messages are unambiguous in time, change one parameter at a time on **input 1
only**: HPF on/off, then HPF frequency, then polarity, then digital trim, then
pad. Each will light up exactly one subop, and the changed slot identifies the
channel indexing at the same time. Repeating one of them on input 9 confirms
whether a single 32-slot array covers all inputs or whether they are banked.

## 7. Pairing: the console's state wins

At t = 65 s the stagebox announced all 32 inputs at **+36.00 dB**. 60 ms later
the console overwrote every channel with **−6.00 dB** — its own stored scene.
The stagebox did not push back.

For a bridge this is the important behaviour: **on pairing, the console is
authoritative and the stagebox's physical state is discarded.** Any adapter
that mirrors Yamaha state needs to expect a full-array overwrite immediately
after a console connects, and must not treat it as 32 individual user edits.

## 8. The checksum

The final body byte is an additive checksum over **the whole MBC block**,
starting at the `M` of `MBC` and ending just before the checksum itself:

```python
checksum = (0x3F - (sum(block[:checksum_index]) & 0xFF)) & 0xFF
```

Equivalently, the bytes of the block including the checksum sum to `0x3F` mod
256.

Verified against 3 830 of 3 907 complete MBC messages, across every opcode and
both directions. The 77 that don't fit are a parsing artefact, not an exception:
**every one of them has trailing bytes past the length-derived boundary**, so the
byte being tested isn't the real checksum. No message with an unambiguous block
boundary violates the rule.

Independently confirmed on hardware — the rule reproduces, byte for byte, the
checksum of a packet a real Rio3224-D2 accepted and acted on (§9).

### Why the earlier per-class table worked

An earlier pass established only that `K = (checksum + sum(body[:-1])) & 0xFF` is
constant within a (direction, opcode, subop, length) class, and used it as a
lookup table. That falls out of the real rule: the 24-byte block header is
identical within such a class, so

```
K = (0x3F - sum(header)) & 0xFF
```

For the gain class the header sums to `0x98`, giving `K = 0xa7` — exactly the
value the table held.

The term that had been missing was the **source and destination MAC addresses**
in the block header. That is why direction shifted `K` (different source MAC) and
why it stayed constant within a class. Two earlier hypotheses — the flags byte at
offset +21, and the repeated length byte at +20 — were tested and are wrong;
folding either in makes the residual *less* stable, not more.

## 9. Confirmed: writing gain to a real Rio3224-D2

A hand-built MBC gain message, sent from a laptop to `224.0.0.232:8705`, **was
accepted and applied by a real Rio3224-D2.**

Method: take a captured console gain broadcast, patch one channel's int16, and
recompute the trailing checksum as `(0xa7 - sum(body[:-1])) & 0xff`. Everything
else byte-identical, including the ConMon envelope, with both sequence fields
bumped.

Evidence — the Rio echoed the new state back on its own status broadcast:

| Sent | Rio's own broadcast |
|---|---|
| input 5 → `0x04b0` (+12.00 dB) | `…fda8 fda8 fda8 fda8 04b0 fda8…` |
| input 5 → `0x09c4` (+25.00 dB) | `…fda8 fda8 fda8 fda8 09c4 fda8…` |
| input 5 → `0xfda8` (−6.00 dB, restore) | restored, verified |

This confirms, end to end: the MBC block layout, the ConMon envelope including
the offset-40 vendor length, the gain encoding as big-endian centi-dB, and the
checksum constant for that message class.

Three failure modes cost real time, all of them silent — a wrong flags byte, a
truncated envelope missing the 18-byte sub-header, and a field offset slip. The
Rio never NAKs. **If a device ignores you, assume framing, not permissions.**

### The Rio accepts MBC but refuses Audinate ARCP

Worth knowing when writing discovery. The QL1 answers read-only ARCP queries on
UDP 4440 from an arbitrary host (`0x1003` returns its Dante name, Brooklyn-II
module and `Audinate DCM`). The Rio3224-D2 **answers nothing** on 4440, 4444,
4455 or 8800 — yet it accepts and acts on MBC control from the same host.

So Audinate-level queries are not a reliable way to enumerate Yamaha stageboxes.
mDNS works for both: they appear under `_netaudio-arc._udp` and
`_netaudio-cmc._udp` as `Y001-Yamaha-QL1-17ea2c` and
`Y004-Yamaha-Rio3224-D2-25df04`.

## 10. Reference: building a valid gain message

Everything needed to construct a packet a Rio3224-D2 will act on. This is the
exact shape of the code that was verified on hardware.

```python
import socket, struct

MBC_HEADER = (
    b"MBC" + b"\x01"                       # magic, version
    + struct.pack(">H", 74)                # len: counts from +20 to end of block
    + bytes.fromhex("00a0dee0cef6")        # src MAC  (the console's Yamaha MAC)
    + bytes.fromhex("001dc125df04")        # dst MAC  (the stagebox)
    + b"\x00\x00"
    + b"\x4a\x00"                          # len low byte, flags
    + b"\x07\x22"                          # opcode
)

def gain_block(gains_centi_db):            # 32 values, e.g. -600 == -6.00 dB
    body = b"\x16" + struct.pack(">HH", 32, 0)
    body += b"".join(struct.pack(">h", g) for g in gains_centi_db)
    block = MBC_HEADER + body
    return block + bytes([(0x3F - (sum(block) & 0xFF)) & 0xFF])

def conmon(block, seq, sender_eui64):
    env = (b"\xff\xff" + b"\x00\x00"       # magic, length patched below
           + struct.pack(">H", seq) + b"\x00\x00"
           + sender_eui64 + b"Audinate"
           + bytes.fromhex("072e1002") + b"\x00\x00\x00\x00"
           + struct.pack(">H", seq)
           + bytes.fromhex("001000010000")
           + struct.pack(">H", ((len(block) + 1) << 8 | 0xC0)))
    pkt = bytearray(env + block)
    struct.pack_into(">H", pkt, 2, len(pkt))
    return bytes(pkt)

pkt = conmon(gain_block([-600] * 4 + [1200] + [-600] * 27),
             0x0930, bytes.fromhex("001dc117ea2c0000"))

s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_TTL, 1)
s.sendto(pkt, ("224.0.0.232", 8705))
```

Three traps, all of which fail **silently** — the device never NAKs:

1. The vendor length at envelope offset 40 is `(len(block) + 1) << 8 | 0xC0`.
2. The flags byte differs per message class; the 32-channel gain broadcast uses
   `0x00`, not the `0x01` seen on short messages.
3. The checksum covers the whole MBC block including both MAC addresses, not
   just the body.

Note this addresses the stagebox as the console — it uses the console's MAC as
the source and its EUI-64 as the ConMon sender. On a network where the real
console is also connected, both will be writing the same parameters, and §7
applies: the console's state wins on any resync.

## 11. What this does and does not validate

**Validated against real hardware:** that Yamaha HA control is MBC-over-ConMon
and not Audinate ARCP; the gain encoding, unit and range; the phantom encoding;
the console-wins pairing behaviour; the ConMon envelope; and — by successful
transmission — the ability to *set* gain on a real Rio3224-D2.

**Not validated:** any of this from inside Dante-BabelBox. The write was
performed by a standalone Python script, not by `preamp-adapter-yamaha`. No
crate in this workspace has yet driven real hardware, so the README's honesty
warning stands. What has changed is that the protocol is no longer guesswork —
an adapter built to this document can be expected to work.
