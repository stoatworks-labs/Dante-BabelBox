# Allen & Heath DT preamp control over Dante (`AllenHth` ConMon)

This documents the control protocol behind Allen & Heath's **DT Preamp Control**
application — the way an A&H **DT168** / **DT164-W** Dante expander's mic-preamp
gain, pad and +48V are read and set over the Dante network with no console
present. It is the source for [`preamp_adapter_ah::dt`](../crates/preamp-adapter-ah/src/dt.rs).

## Provenance and confidence

Everything here was recovered **statically** from two shipped artefacts:

1. **DT Preamp Control V1.21 (macOS)** — `com.yourcompany.DT-Preamp-Control`,
   a Qt/QML app, © 2020–2026 Allen & Heath. It **statically links the Audinate
   Dante API** (DAPI build `30089`) and reaches devices through Audinate
   **ConMon** (`conmon_audinate_controller` client). Its installer also bundles
   Audinate's `ConMon.pkg`.
2. **The SQ Dante-card firmware** — `AllenHeath_SQDante_1.0.6.17.dnt`,
   `AllenHeath_SQDanteV2_1.1.2.dnt` and `SQDante64-V3.s64`. These are Audinate
   `AUDI`-container images for A&H's **KLANTE** module ("KLANTE — Brooklyn II
   replacement module", a Xilinx Zynq running the Audinate IP core). Their
   headers register the **same ConMon vendor namespace** the app uses.

There is **no packet capture and no hardware test** behind any of this — unlike
the Yamaha MBC work in [`yamaha-ha-remote-over-dante.md`](yamaha-ha-remote-over-dante.md),
whose gain message was proven against a real Rio3224-D2. Treat this as a
**well-evidenced hypothesis to validate**, not a proven wire format. Each claim
below is tagged:

- **[HIGH]** — read directly out of the binary/firmware, corroborated in two places.
- **[MED]** — recovered from one disassembled function; plausible but unverified.
- **[OPEN]** — genuinely unknown from static analysis; needs a capture or hardware.

## Transport: ConMon vendor messages, vendor ID `AllenHth`

**[HIGH]** A&H's control rides on Audinate **ConMon** vendor messages, not MIDI
and not OSC. The app sends with `conmon_client_send_control_message` and receives
by registering with `conmon_client_register_monitoring_messages`. The device is
found through ConMon's own mDNS discovery (the app resolves `conmon` TXT records
with `id` / `process` fields and a `dante_device_name`).

**[HIGH]** A&H's ConMon **vendor ID is the 8-byte ASCII string `AllenHth`**
(`41 6C 6C 65 6E 48 74 68`). This is confirmed in **both** artefacts:

- In the app's send path (`_SendConmonMessage`), the 16-byte ConMon message-class
  constant is `01 00 01 01 | 41 6C 6C 65 6E 48 74 68 | 00 00 00 05` — i.e. a
  small prefix, `AllenHth`, then a message-type field (`05`).
- In every `.dnt` / `.s64` card image, the `AUDI` container header at offset
  0x1A reads `… 41 6C 6C 65 6E 48 74 68 | 00 00 00 40 | 00 00 00 05 | 00 00 00 10`.

A second, longer tag **`AllenHthZT`** also appears in the card firmware — a
distinct message class (purpose unknown, **[OPEN]**).

**Consequence for the SQ:** because the SQ's own Dante card (KLANTE) registers
the `AllenHth` ConMon namespace, the SQ is reachable by the *same* codec in
principle — see "The SQ question" below.

**[OPEN]** The exact ConMon envelope on the wire (head layout, whether messages
go to the control or monitoring channel, unicast vs. the `vendor_broadcast`
path the strings mention) is DAPI's internal business and was not reversed to
the byte. This is why the Rust side is a **codec only, with no ConMon transport**
— the same stopping point as the Aphex adapter, and for the same reason (the
transport needs Audinate DAPI, which this workspace does not link).

## Device model

**[HIGH]** A device exposes **16 mic preamps** (every status/loop in the app
iterates `0..16`). The app supports **up to 32 devices** on the network at once
(help text). Two hardware variants are named in the firmware payloads and help:

| App/firmware name | Product     | Firmware payloads                          |
|-------------------|-------------|--------------------------------------------|
| `DT Box 1608`     | **DT168**   | `Upd1608/{DT_Main.bin, DT_FPGA.bin, Update.dat}`, `Bootloader_1608.hex` |
| `DT Box 1604`     | **DT164-W** | `Upd1604/{DT_Main.bin, DT_FPGA.bin, Update.dat}`, `Bootloader_1604.hex` |

**[HIGH]** Per-preamp state, as carried in the app's `sDTPreampState` struct and
its C callback `micPreStatusCallback(name, u8 index, u16 gain, bool pad, bool v48)`:

| Field   | Type        | Notes |
|---------|-------------|-------|
| `gain`  | `u16` (UWORD) | Whole-dB control (`SquirrelEnums.ValueTypeEnum.GainWholedB`). Reference value `0x8000` (the app's own test path sets gain `0x8000`). Range is **device-reported** (`driver.min`/`driver.max` in the QML), not hard-coded. |
| `pad`   | `bool`      | Applies a **−20 dB** offset to the value the UI displays (help + QML `conversionOffset: pad ? -20 : 0`). |
| `v48`   | `bool`      | +48V phantom. |
| `name`  | `char[32]`  | Editable socket name (separate `SetChannelName` message). |

**[OPEN]** The **UWORD→dB scale** is not recovered. It is whole-dB and centred
near `0x8000`, and the min/max come from the device at runtime, but the exact
mapping lives in A&H's `AHDrivers`/`Squirrel` layer, which was not reversed. The
codec therefore treats gain as a raw `u16` and offers only a **provisional,
loudly-flagged** dB conversion.

## Messages

The app's message layer (`_Mess_SetMicPre`, `_ProcessConmonMessage`,
`_Mess_GetAllMicPre`) dispatches on a small message-type space (a 7-entry jump
table, types 1..7). Three are identified:

### Mic-pre status (device → controller) **[MED]**

Payload byte 0 selects a variant:

- **`0x01` — full status:** 16 back-to-back entries, **3 bytes each**:
  `[u16 gain][u8 flags]`. The `flags` byte packs the booleans in its low bits —
  `_SetStatusForMicPre` extracts **bit 0** and **bit 1** as the two flags
  (pad / +48V). Entry *i* is preamp *i* for `i` in `0..16`.
- **`0x00` — flags-only:** a shorter per-preamp form carrying just the boolean
  bytes (no gain), read at offset `4 + index*2`.

**[OPEN]** Byte order of the `u16` gain on the wire (the app loads it with a
native-endian `movzwl`; the device MCU could store it either way) and the exact
offset of the first entry within the ConMon payload.

### Firmware version (device → controller) **[MED]**

A message whose payload bytes 4/5/6 are **major / minor / patch** — surfaced as
`DT Box 1608, V%d.%d`.

### Set mic-pre (controller → device) **[MED]**

`_Mess_SetMicPre(channel, …, value)` builds a **16-byte** control message
(`_AddCmMessage(…, len=0x10, …)`, message-type `5`) carrying the channel plus the
`u16` gain and the two boolean flags. In the app's own self-test the value is
`0x8000` with flags clear. **[OPEN]** the precise field offsets inside those 16
bytes were not pinned to the byte (no capture to check against), so the codec's
`encode_set` documents its layout as provisional.

### Poll (controller → device) **[MED]**

`_Mess_GetAllMicPre` requests a full status dump; the app also calls
`UpdateLibRequestDevicePreampInfo` on connect and after any device change.

## Device side (context, not needed by the codec)

**[HIGH]** On the box, the preamps hang off an MCU that drives them over **SPI**
(`SPI_CMD_GET_MICPRE`, `SPI_IF_NewMicPreValue`). The unit is field-updatable via
the app: an Intel-HEX bootloader plus `DT_Main.bin` (control MCU) and
`DT_FPGA.bin`, with the **firmware-update transfer using Audinate DDP**
(big-endian offset/response messages), distinct from the ConMon control path
above.

## The SQ question

The user's working assumption was that the SQ shares the QU / Avantis / dLive
console protocol. For **preamp control over Dante specifically**, the evidence
points somewhere more useful:

- The SQ's **Dante option card is KLANTE**, and it registers the **`AllenHth`
  ConMon** namespace — the *same* vendor channel the DT expanders answer on.
  So the most promising SQ-over-Dante preamp path is **this codec**, not the
  console MIDI protocol. **[OPEN]** whether an SQ actually answers the mic-pre
  message set (and how it addresses its 48 local + SLink sockets) is untested.
- The **console MIDI-over-TCP** path (port 51325, the family
  [`ahm`](../crates/preamp-adapter-ah/src/ahm.rs) / [`dlive`](../crates/preamp-adapter-ah/src/dlive.rs)
  implement) is the fallback. But the published *SQ MIDI Protocol* documents
  **no** preamp messages, so that route is a hypothesis (reuse the AHM/dLive
  NRPN scheme) with even less backing than the ConMon route. It is **not**
  implemented as SQ code here to avoid fabricating a SysEx product byte we do
  not have.

Either way the decisive next step is the same: **capture DT Preamp Control (or a
console) talking to a real DT168 / SQ** and check the bytes against this codec.
See the capture guides in `docs/`.

## What would settle each [OPEN]

| Open question | How to close it |
|---|---|
| ConMon envelope / channel | One capture of the app setting a gain; read the ConMon head. |
| `u16` gain endianness | Same capture: set a known gain, see the two bytes. |
| UWORD→dB scale + range | Sweep the app's gain knob across its range while capturing. |
| Set-message field offsets | Same gain-set capture. |
| SQ answers `AllenHth` mic-pre? | Capture an SQ (or point the app at one). |
| `AllenHthZT` class | Capture around device discovery / naming / metering. |

## Live transport findings (2026-08-08, no hardware)

A fake DT168 was stood up on the bench (`tools/dt-fake/fake_dt168.py`): a Bonjour
beacon advertising `_netaudio-cmc._udp` pinned to one interface, plus a listener
that decodes anything the Audinate stack sends it. Running the real **Dante
Controller** against it exposed the **CMC (ConMon Management Channel)** connect
handshake — the transport layer that was previously `[OPEN]`:

- The app does **no network I/O itself**. It links DAPI but reaches the wire
  through the shared local Dante runtime over loopback UDP `127.0.0.1:8850`; that
  runtime (and Dante Controller) do the actual mDNS browse + ConMon on the
  configured Dante interface. So a device only appears in the app if the shared
  runtime accepts it.
- Discovery works: pin the beacon to the **Dante interface** (not a VPN/VM one)
  and DC finds it and tries to connect. **Confirmed the discovery model is right.**
- **CMC connect packet** (controller → device's advertised CMC port), 20 bytes:
  `[0:2]=0x1000 version  [2:4]=len  [4:6]=seqnum  [6:8]=0x1001 type
  [8:10]=phase(0)  [10:18]=controller 8-byte conmon id  [18:20]=0x0000`.
- Answering it elicits a **40-byte phase-1** packet that carries the controller's
  own **data-channel endpoint** (observed `192.168.1.90:9710`) twice, prefixed by
  a `0x0002` count.

**Where it stalls:** completing the handshake needs the *device-side* CMC
response byte-for-byte, and DC rejects approximations (it just re-probes with an
incrementing seqnum). That response was **not** recoverable from static analysis:
the DT box's `DT_Main.bin` is an STM32 with **no network stack** (USART to the
Audinate module + SPI/GPIO to the preamps), and the SQ mixer firmware carries the
**DX/SLink** stagebox line, not DT/Dante. The CMC responder lives in the DT box's
Audinate module (KLANTE-class), i.e. **generic Audinate ConMon**, identical on
DVS/Brooklyn/Ultimo. So the fastest way to finish an emulator is a capture of a
real device's CMC, not more firmware archaeology.

## The ConMon wire envelope (2026-08-08, from a real DVS capture)

A Wireshark capture of a real Dante device (a Dante Virtual Soundcard, vendor
`Audinate`) broadcasting on the ConMon multicast channels gave the **exact wire
envelope** that wraps every vendor payload — the frame an `AllenHth` mic-pre
message rides inside. This is real bytes, not inference (the only inference is
that A&H's payload sits in the identical envelope with its vendor id swapped in,
which matches the app-side RE):

```
offset  field
[0:2]   magic    0xFFFE on the status/monitoring channel, 0xFFFF on control
[2:4]   length   whole message, big-endian (== UDP payload length)
[4:6]   seqnum   increments per message
[6:8]   0x0000
[8:16]  device   EUI-64 device id (MAC with FF:FE inserted mid-6)
[16:24] vendor   8 ASCII bytes — "Audinate" here, "AllenHth" for A&H
[24:..] body     the vendor message (our mic-pre set/status payload)
```

Channel map (multicast, observed):

| Group / port          | magic  | carries |
|-----------------------|--------|---------|
| `224.0.0.233:8708`    | 0xFFFE | status / metering broadcasts (`vendor_broadcast`) |
| `224.0.0.231:8702`    | 0xFFFF | control / device-info |
| `224.0.0.230:8703`    | -      | (seen, sparse) |

Implication for the codec: [`encode_set`]/[`decode_status`] produce the **body**;
[`wrap_conmon`]/[`parse_conmon`] add/strip this envelope. A DT box's mic-pre
status is expected on `224.0.0.233:8708` with vendor `AllenHth`. The body's own
sub-framing (the Audinate bodies here open `08 00 01 10 00 00 00 00 …`) is
**vendor-specific**, so the A&H body layout still needs one DT-box capture to
confirm — the envelope does not.

## Capturing the reference (Wireshark)

Two captures are worth having; either unblocks real progress.

1. **Transport reference — any real Dante device.** In Dante Controller, connect
   to a real device and let the CMC handshake run; the device's responses are the
   template our fake must reproduce. A Dante Virtual Soundcard on the same host
   advertises as a normal cmc device and works as the reference (it may appear on
   `lo0` since it's same-host — capture `lo0` as well as the Dante NIC).
2. **The whole protocol — the app + a real DT168/DT164-W.** This gets the CMC
   transport *and* the `AllenHth` preamp bytes (gain UWORD scale, endianness, set
   layout) in one shot. Launch DT Preamp Control, then change a gain and toggle
   +48V while capturing.

Recipe:

- Capture interface: the **Dante NIC** for external hardware; add **`lo0`** when
  the target is a same-host DVS.
- Capture (BPF) filter to keep it small:
  `udp portrange 8700-8850 or udp portrange 9700-9900 or net 224.0.0.230/28`
- To catch the *connect* (not just keepalives), trigger a fresh session: restart
  Dante Controller, or power-cycle / re-select the device, while capturing.
- Save as `.pcapng`. The `AllenHth` id (`41 6c 6c 65 6e 48 74 68`) marks the A&H
  vendor payloads; the `0x1000`/`0x1001` heads mark the CMC transport.
