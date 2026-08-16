# Yamaha SCP, from R Remote 6.0.0

Static reverse-engineering of Yamaha's **R Remote** (`/Applications/R Remote.app`,
v6.0.0, 2025-03-04, Qt, x86_64, **not stripped**) — the console-less controller for
R-series head amps.

**The headline: R Remote does not use the `MBC` block.** It controls Rio preamps
over **SCP, an ASCII-addressed command protocol carried on plain TCP**. There are
two independent control paths into a Rio, and this project has so far only known
about the harder one.

| Path | Controller | Transport | Evidence |
|---|---|---|---|
| `MBC` block in ConMon vendor messages | a QL/CL console | UDP multicast, `224.0.0.232:8705` | [captured, write-proven on a Rio](yamaha-ha-remote-over-dante.md) |
| **SCP** | **R Remote (software, no console)** | **TCP** (`VCOM::VComTcpClientSocket`) | this document |

For BabelBox's purposes the SCP path is dramatically better, for reasons in
"What this means for the bridge" below.

## Provenance

Static analysis only — the app was never run, and no Rio was present. Recovered
from the Mach-O symbol table (11 673 symbols, C++ names intact) and from XML
parameter schemas embedded as string literals. The schemas are archived verbatim
at [`evidence/rremote-600-scp-schemas.xml`](evidence/rremote-600-scp-schemas.xml)
(18 blocks, three device families).

Confidence is **[HIGH]** for anything read directly out of the embedded schemas —
they are the app's own machine-readable contract, not inference. Wire framing is
**[OPEN]**: no capture was taken, so how a `SET` is serialised on the socket is
not established here.

## The protocol

`SCP::ScpBaseClient` / `SCP::ScpClient`, with a command set built by
`ScpClientCommandFactory*`:

```
DEVINFO DEVMODE DEVSTATUS EVENT
GET GETN GETT  SET SETN SETR SETT
PRMINFO PRMNUM  LISTITEM LISTITEMNUM
MTR MTRINFO MTRNUM MTRSTART MTRSTOP
SCPMODE  SSCURRENT SSINFO SSNUM SSRECALL SSUPDATE
```

Addresses are strings — `ScpBaseClient::getIndexFromAddress(unsigned int&,
YString const&)` resolves an address to an index — of the shape seen in the
binary's own format strings, e.g. `/Current/Dev:%d/MasterWCLK_Dante:%d/Fs:%d`.

**The parameter space is discovered at runtime**, not hardcoded: `PRMINFO` /
`PRMNUM` / `LISTITEM` / `LISTITEMNUM` exist precisely so the controller can ask
the device what it has. Same architectural pattern as this project's `plugin-aes70`,
which enumerates the device's object tree rather than shipping a fixed map — and
the same benefit: there is no vendor table that can be wrong.

`scpmode` carries a `keepalive` parameter (uint32, 1 000–600 000 ms, default
10 000), so the session expects a heartbeat.

### Association

`SCP::ScpBaseClient::associate(bool, SCP::IScpConnectSequence const*)`, with
`isAssociated()` / `isNegotiated()`, and per-family sequences `RioAssoSeq`,
`RSioAssoSeq`, `RMioAssoSeq` over a base `RRmt::AssoSeq`. The sequence is a
**scripted list of SCP commands** — `AssoSeq::getCommand(SCP::ScpSequenceItem&,
unsigned int)` and `getCommandNum()` — plus address info via `getCommAddrInfo()` /
`getCommAddrNum()` and `addOffsetInfo()`.

**This bears directly on the "does a Rio accept control cold?" question.** There
*is* an association step, so control is not simply fire-and-forget. But it is an
**SCP-level association performed by a software controller**, not a console
pairing — which is the encouraging reading: Yamaha ships a supported way for a
non-console client to take control of a Rio. Recovering the actual command list
from `RioAssoSeq`'s static data is the obvious next step.

## The Rio parameter set — **[HIGH]**

From the `Backup` schema (7 collections, 19 parameters). Array sizes are the
device's own.

### `InCh[32]` — the head amps

| Parameter | Type | Range | Default |
|---|---|---|---|
| `HAGain` | `int16_t` | **−6 … 66** | −6 |
| `48VOn` | `int8_t` | 0…1 | 0 |
| `HPFOn` | `int8_t` | 0…1 | 0 |
| `HPFFreq` | `int16_t` | 4…63 | 28 |
| `GainCompOn` | `int8_t` | 0…1 | 0 |
| `CompGain` | `int16_t` | −6…66 | −6 |

`HPFFreq` is a **table index, not a frequency** — the binary carries
`ParamTableForHpfFreq` with `m_values` / `m_floatValues` and
`paramToDispStr(int)`, so 4…63 indexes a frequency table that can be lifted from
the data segment.

### `OutCh[24]` and `OutChAES[8]`

| Parameter | Type | Range |
|---|---|---|
| `DelayOn` | `int8_t` | 0…1 |
| `DelayTime` | `int32_t` | 0…1 000 000 |
| `Polarity` | `int8_t` | 0…1 |
| `GainLevel` | `int32_t` | −9600…2400 (centi-dB, −96.00…+24.00 dB) |

Also `Headphone[2]` (`Mono`, `Assign` 0…58), `MeterSetup` (`PeakHold`),
`OutputSetup` (`DelayScale`, `DelayFrame`).

### Device level — `Current` / `Dev[1]`

`48VMasterOn`, `48VLeaderOn`, `48VActiveOn`, `MuteOn`, `WordclockFs`,
`OutputImpedance`, the Curr/Next IP quartets, `ExecMode`, `SystemStatus`,
`SyncStatus` (both 0…13).

### Change notification

A separate `Event` schema scoped `Dev[1]`, `InCh[32]`, `OutCh[24]` — the
subscription channel a bridge would consume to implement
`DeviceAdapter::subscribe()`.

## Three corrections to the MBC write-up

[`yamaha-ha-remote-over-dante.md`](yamaha-ha-remote-over-dante.md) lists as
`[OPEN]` that "pad, HPF, polarity and digital trim each correspond to one of
eight arrays under opcode `0x0722`", their widths known but their identities not,
because every observed value sat at a resting default. The schema names the real
per-channel set, and the guess list was wrong in three ways:

1. **There is no `Pad` parameter.** The R-series per-channel set has no pad at
   all. What exists instead is **gain compensation** — `GainCompOn` + `CompGain`.
2. **`Polarity` is an output parameter**, present on `OutCh` / `OutChAES` and
   absent from `InCh`. Looking for it among the input-side arrays will not find it.
3. **HPF is two parameters, not one** — `HPFOn` (`int8`) and `HPFFreq` (`int16`,
   a table index). A single array will not account for it.

The real `InCh` set is six parameters — three `int8` and three `int16` — which is
a much better fit for the observed array shapes than the guessed list, and worth
re-checking the capture against.

**One discrepancy to resolve:** the capture established `HAGain` on the wire as
**centi-dB** (`-600` == −6.00 dB); the SCP schema declares `int16_t` with range
**−6 … 66**, i.e. whole dB over the identical endpoints. Both agree on the range.
Either SCP exposes whole-dB and scales at the device, or the schema's bounds are
in display units. The capture is proven bytes for the MBC path and stays
authoritative there; this only matters when writing the SCP client.

## What this means for the bridge

The [`plugin-yamaha-mbc` sketch](plugin-yamaha-mbc-sketch.md) is blocked on a
config-shape problem: `MbcIdentity` needs `src_mac`, `dst_mac`, `sender_eui64`
and a message class, none of which fit `RDeviceConfig { id, address, port,
channels }`, plus `address` would have to mean the *local* interface.

**An SCP plugin has none of those problems.** It is a TCP client to an address and
port — exactly the shape every existing plugin already uses. Specifically it
avoids:

- impersonating a real console's MAC (and baking a captured QL1's identity into a
  public binary)
- the multicast interface-binding special case
- the "does this need Audinate DAPI?" question entirely — SCP is a plain socket
- a hardcoded parameter map, since `PRMINFO`/`PRMNUM` enumerate at runtime

It also reaches **RSio and RMio**, not just Rio: the binary carries separate
schemas and `ScpDeviceReporter_{Rio,RSio,RMio,RRmt}` classes, and `ProjSetup`
allows `Rio[24]` units in one project.

Recommendation: **prefer SCP over MBC as the Rio control path**, and treat the
MBC adapter as the console-emulation route it always was. This does not waste the
MBC work — it stays the only evidence of what a console actually puts on the wire,
which is what any future device-emulation work needs.

## Open questions

- **The TCP port.** Not recoverable from strings; it is an integer constant. Byte
  searching was inconclusive (a 16-bit pattern is noise at this binary size).
  Settle it by running R Remote against anything and watching, or by
  disassembling around `VComTcpClientSocket::connect`.
- **Wire framing of `SET`/`GET`.** The command names and the parameter model are
  solid; the serialisation is not established.
- **The `RioAssoSeq` command list** — static data, extractable, and the thing that
  answers what a controller must do before it may write.
- **Whether Yamaha publishes an SCP spec covering R-series.** Yamaha documents SCP
  for other product lines; if R-series is covered, much of this becomes
  "implemented from a published spec" rather than reverse-engineered, which
  changes how the README can describe it.

## Negative result: this does not unblock `dt-fake`

Worth recording so nobody repeats the search. R Remote statically links Audinate's
ConMon **client** — `conmon_client_*`, `conmon_cb_*`, and a large
`conmon_audinate_*` device-management surface (clocking, config, naming), all with
symbols. It does **not** contain a network-side CMC responder; the only
response-writing functions are `conmon_ipc_write_{connect,subscribe,
register_messages}_response`, which are the **local app ↔ Dante runtime IPC**, not
the on-network CMC handshake. The device side still lives in device firmware or the
runtime, so [the A&H emulator](../tools/dt-fake/README.md) still needs a capture of
a real remote device.

One speculative lead from it: those `conmon_ipc_*` symbols document the loopback
IPC protocol that the DT investigation found apps use to reach the wire
(`127.0.0.1:8850`). If that channel is usable directly, a plugin could send ConMon
vendor messages via the installed Dante runtime without linking DAPI. Unverified,
but it is a concrete alternative to "DAPI or nothing".
