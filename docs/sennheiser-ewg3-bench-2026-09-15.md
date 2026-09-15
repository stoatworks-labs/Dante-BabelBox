# Sennheiser evolution-wireless G3 — bench session, 2026-09-15

Reverse-engineered the G3 receiver's network control protocol from
**Wireless Systems Manager (WSM) 4.9.0** talking to a **real EM 500 G3**
(MAC `00:1B:66:7A:DE:44`, RF band 606–648 MHz), then built it into the
bridge as the `sennheiser-ewg3` kind and drove the receiver's AF-out from a
DM3's remote head-amp gain — **confirmed end-to-end on the hardware.**

Full protocol write-up, reference client and packet captures live in a
separate research repo: `~/reverse-engineering/audio/sennheiser-ewg3-re`.
This doc is the bridge-facing summary and the record of the live test.

## The protocol, in one paragraph

The G3 rack receivers (EM 300/500 G3, SR 300 IEM G3) are controlled by WSM
over an **undocumented binary protocol on UDP 8133** — *not* the documented
ASCII "Media Control Protocol" (TI 1254, UDP 53212, `Push`/`RF1`/`RF2`…).
That ASCII protocol needs receiver firmware ≥ 1.7.0 and **this EM 500 G3
does not answer port 53212 at all**; the 8133 binary protocol is what WSM
actually uses and works on the same unit. This also corrects a fleet-wide
assumption: the G3/G4/2000 family is neither SSC/JSON (that's EW-DX and
Digital 6000/9000) nor reliably the ASCII 53212 protocol.

Key wire facts (see the codec crate for byte-level detail):

- **Discovery**: a fixed 1035-byte payload multicast to `224.0.0.251:8133`
  (the mDNS group, but not mDNS). The receiver replies `Model=EM500G3
  ID=… IPA=…` and also self-announces it every ~5 s.
- **All control on UDP 8133**, both directions using port 8133 — the
  receiver replies to port 8133 regardless of the request's source port, so
  a client must bind 8133.
- **Single-master write lock**: the receiver honours writes only from the
  one client holding its master slot. A client keeps the slot by sending a
  keepalive every < 5 s. **WSM, if running against the same receiver, holds
  the slot and any other client's writes are silently dropped** — so WSM
  must be closed before the bridge can drive the receiver.
- **AF-out level** (the receiver's analog audio output, what we expose as
  "gain") is a config field stored as an **index**, `dB = 3·index − 24`,
  index `0..14` = `−24..+18` dB in 3 dB steps. Confirmed against WSM: config
  byte `0x0c` displays "+12", `0x06` displays "−6". A write sets the field
  plus a per-field dirty flag; verified by writing every step and reading
  back.

## What's writable (proven on the EM 500 G3)

Frequency (kHz, little-endian), equalizer, RX-mute and **AF-out** all
round-tripped cleanly with read-back. Only AF-out is wired into the bridge
(it is the useful "gain" for console control). One hazard found and
documented: a write that sets stray flags near the name field blanks the
device name — so the adapter never writes the name, and the codec exposes
only the specific fields proven safe.

## The bridge adapter

- `crates/preamp-adapter-sennheiser-ewg3` — pure codec (packet build/parse,
  AF-out index math) with the exact captured bytes as test fixtures, plus an
  async `DeviceAdapter` (UDP 8133, a 2 s keepalive to hold the master slot,
  and a receive loop that turns the receiver's config-push into a gain
  event for best-effort bidirectional behaviour).
- `crates/plugin-sennheiser-ewg3` — the loadable `sennheiser-ewg3` plugin.
- One channel; "gain" is AF-out, clamped to −24…+18 dB. The G3 has no remote
  phantom, so `set_phantom` is accepted and ignored. `port` is ignored (the
  protocol is fixed to 8133); `address` is the receiver's IP.

## Live test — DM3 remote gain → G3 AF-out

Rig: this Mac on the bench LAN, the EM 500 G3 at `192.168.0.101`, a DM3
(the same desk from the DM3 bench) reachable at `169.254.214.203`. WSM
closed. `bridge.toml`:

```toml
[[device]]
id = "dm3-bench"
kind = "yamaha-dm3-scp"
address = "169.254.214.203"

[[device]]
id = "g3-em500"
kind = "sennheiser-ewg3"
address = "192.168.0.101"

[[mapping]]
from = { device = "dm3-bench", channel = 2 }
to = { device = "g3-em500", channel = 1 }
bidirectional = false
```

Method: start `preamp-bridge run` (both plugins in `--plugins-dir`), then
change the DM3's Local Input 2 head-amp gain over SCP (as any controller or
the surface itself would). The DM3 broadcasts `NOTIFY set …HAGain 1 0 <v>`;
the `yamaha-dm3-scp` adapter emits a gain event; the Router maps
`dm3-bench:2 → g3-em500:1`; the G3 adapter writes AF-out. The receiver's
AF-out was then read back independently (with the bridge stopped, via the
research-repo client).

Result — the G3 AF-out **tracked the DM3 gain**:

| DM3 Local Input 2 head-amp gain | G3 AF-out read back |
|---|---|
| baseline before test | +12 dB |
| set to 0 dB | **0 dB** |
| set to +15 dB | **+15 dB** |

The Router logged its echo-suppression on `g3-em500` `Ono(1)` each time,
i.e. it wrote the value and then correctly swallowed the receiver's own
confirmation config-push. (The adapter's own `debug!` line does not surface
through the loaded cdylib's tracing boundary — the device state is the
proof, not the log.)

Afterwards both devices were restored: the G3 AF-out back to +12 dB (its
original), name/frequency/EQ/RX-mute untouched, and the DM3's Local Input 2
gain back to its baseline 15 dB.

## Open gap

The receiver's meter/telemetry packet (RF, AF, battery, pilot) was not
decoded: the transmitter sat unchanged during the session so the meters
were static. Mapping those bytes — for a future mic-telemetry adapter —
needs a session that varies the transmitter (walk it away, speak into it,
mute it, run the battery down).
