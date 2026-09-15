# Yamaha DM3 on the bench — 2026-09-15

The first session with a real Yamaha console on the wire since the QL1/Rio
captures of 2026-08-01, and the first time any adapter in this workspace ran
against hardware. One DM3 (Dante model, firmware **V3.00**), its NETWORK port and
Dante PRIMARY on one unmanaged switch with the Mac's Dante NIC (`en19`, DVS
bound). No Rio, no Tio, no QL. Captures and the full action log are archived in
`stoatworks-labs/dante-captures` under `yamaha-dm3/`.

Everything below was observed, not inferred, unless marked **[OPEN]**. Where a
statement corrects an existing document or a module comment, the correction is
called out — those are the reason this file exists.

## 1. What a DM3 looks like on the network

Three Yamaha-OUI MACs appear for one console:

| MAC | Address on the bench | What it is |
|---|---|---|
| `ac:44:f2:a4:36:af` | 169.254.237.144, `Broadway-a436af.local` | The Dante module. PTP leader, ConMon, ARC `4440`, CMC `8800`, dbc `4455` |
| `ac:44:f2:a4:36:ae` | 169.254.157.235 | The **NETWORK** jack — the console's Linux host. Sends the YSDP beacon (§2) |
| `ac:44:f2:a4:36:ad` | 169.254.214.203, `dm3.local` | The same Linux host's **second** interface, hanging on the Dante module's internal switch, so it reaches the wire through the Dante PRIMARY jack |

The Dante module is a **Broadway** (`router_vers=4.3`, `arcp_vers=2.8.8`,
`mf=Yamaha model=000946`), matching the `DM3_BWY` device-tree variant in
`yamaha-ql-re`'s firmware analysis — this unit is not the Brooklyn 3 build.

Both host interfaces query `_dante-safe._udp` over mDNS; the Dante module
queries `default._dante-ddm-d._udp` (looking for a Dante Domain Manager).

### The flat-network trap

With NETWORK and Dante PRIMARY on the **same** segment, the NETWORK address
(169.254.157.235) answered nothing — no ARP, no ping, no TCP — while the same
host served everything on `dm3.local`. That is ordinary Linux dual-homed
behaviour (one route back, reverse-path filtering on the other interface), but
it matters here because the YSDP beacon advertises the *unreachable* address.
Anyone running a DM3 on one small switch — common — and pointing a controller at
"the IP the console announces" can hit this. Every control test in this session
went to `dm3.local`.

Open TCP ports on the console host: **5355** (LLMNR/systemd-resolved), **49280**
(SCP), 49312, 50001, 50368 (unidentified **[OPEN]**). Full 1–65535 scan.

## 2. YSDP — how Yamaha controllers find consoles

The NETWORK port broadcasts a beacon on **UDP 54330** every ~5 s. R Remote 6.0.0
sends the same beacon every ~3 s. R Remote's binary carries the symbols
`YSDP`, `YSDPSUBSCRIBE`, `YsdpRDiscovery`, `onYsdpFoundDevice`, `_ypa-scp`.

**Correction to `yamaha-scp-r-remote.md` and `tools/rio-fake`:** R-series
discovery is YSDP, not Bonjour. `rio-fake` advertises `_netaudio-*` records and
waits; R Remote never browses for those.

### Wire format

```
"YSDP"                      4 bytes, magic
u16   len                   = payload length − 6 (excludes magic and this field)
u16   type                  0x0004 = announce / query; 0x8004 = response (bit 15 set)
u32   ipv4                  the sender's address for the service
12 × 0x00                   [OPEN] — zero on every frame seen (mask/gateway? IPv6?)
6 bytes                     sender MAC
u8 n, n bytes               service name: "_ypa-scp"
0x00
u8 block_len                length of the label block that follows
labels                      each u8 len + bytes: manufacturer, model, unit-ID, [name]
```

Frames captured:

| From | Type | Labels |
|---|---|---|
| DM3 NETWORK port, broadcast | `0x0004` | `Yamaha` · `DM3` · `Y001` · `` (empty 4th label) |
| R Remote, broadcast | `0x0004` | `Yamaha` · `R Remote` · `Y000` |
| DM3 host (`dm3.local`), **unicast to R Remote** | `0x8004` | `Yamaha` · `DM3` · `Y001` · `Yamaha DM3` |

So the protocol is: everyone announces; a device that hears a controller's
announce answers it unicast with the response type, adding its device name. The
DM3 answered from whichever interface had the route back — `dm3.local`, with
*that* address in the `ipv4` field — which is how a controller on a flat network
ends up with a reachable address after all.

### What R Remote does with it

R Remote's device slots are bound to a **unit ID** (`Y000`–`Y07F`, the R-series
rotary-switch space), not to a device list. After the DM3's beacons, `Y001` was
greyed out in the picker — R Remote had registered a Yamaha device at that ID
and marked it unusable (not an R-series product). It opened no TCP connection to
the DM3.

A fake advertising `Yamaha / Rio3224-D2 / Y002 / Y002-Yamaha-Rio3224-D2-face01`
from the Mac's own address — answering R Remote's beacons with `0x8004` and
broadcasting announces, with a matching `_netaudio-arc`/`_netaudio-cmc` mDNS
record — was **not** picked up. After the operator opened the slot picker and
assigned IDs, R Remote showed `Y000` on slot 1 and `Y001` on slot 2, both
**BLANK**, and opened **no TCP** to any address (confirmed: zero SYNs to the
fake's 49280–49283/49900 in the capture). So R Remote binds a slot to a unit ID
but only *connects* when a genuine R-series unit answers at that ID; `Y001` (the
DM3) shows as present-but-blank because it is a console, not an R-series I/O
rack, and `Y002` (the fake) is ignored.

**[OPEN]** why the fake isn't accepted as connectable. Candidates, in order:
(a) R Remote drops YSDP from its own host address — untested, needs an alias IP
(`sudo ifconfig en19 alias …`, unavailable to the assistant); (b) the YSDP
`0x8004` response needs a field the fake leaves zero (the 12-byte block, or a
capability/port field this capture hasn't isolated); (c) it cross-checks YSDP
against the Dante ARC/CMC identity and the fake's mismatched MACs fail it. This
path is now **lower priority**: the DM3 proves the SCP framing directly (§3), so
the only remaining R-series-specific unknowns are the association *sequence* and
cold-write acceptance, and neither is answerable without a real Rio.

## 3. SCP on the DM3 — TCP 49280

Live, plain ASCII, one command per `\n`-terminated line, replies likewise. This
is the console's documented "Remote Control Protocol"; it is also, verbatim, the
command set the static analysis pulled out of R Remote (`DEVINFO`, `DEVSTATUS`,
`SCPMODE`, `GET/GETN/GETT`, `SET/SETN/SETT`, `PRMINFO/PRMNUM`, `LISTITEM*`,
`MTR*`, `SS*`). The framing question that document left **[OPEN]** is answered
for consoles, and there is no reason left to expect R-series units to differ.

```
devinfo productname            -> OK devinfo productname "DM3"
devinfo devicename             -> OK devinfo devicename "Y001-Yamaha-DM3-a436af"
devinfo protocolver            -> OK devinfo protocolver "1.3.0"
devinfo version                -> OK devinfo version "V3.00"
devinfo serialno               -> OK devinfo serialno ""
devinfo inputport / outputport -> "16" / "8"
devstatus runmode              -> OK devstatus runmode "normal"
scpmode sstype "text"          -> OK scpmode sstype text
scpmode keepalive 10000        -> OK scpmode keepalive 10000
get IO:Current/InCh/HAGain 0 0 -> OK get IO:Current/InCh/HAGain 0 0 23
getn IO:Current/InCh/HAGain 0 0-> OK getn IO:Current/InCh/HAGain 0 0 359      (0..1000 normalised)
gett IO:Current/InCh/HAGain 0 0-> OK gett IO:Current/InCh/HAGain 0 0 "+23"    (display string)
set  IO:Current/InCh/HAGain 0 0 23 -> OK set IO:Current/InCh/HAGain 0 0 23 "+23"
get MIXER:Current/InCh/HAGain 0 0  -> ERROR get UnknownAddress
help                           -> ERROR unknown UnknownCommand
```

- Indices are **0-based** (`0 0` is input 1); `15 0` is the last local input,
  `16 0` → `ERROR get InvalidArgument`.
- Error grammar: `ERROR <command> <Reason>` — seen: `WrongFormat`,
  `InvalidArgument`, `UnknownAddress`, `UnknownCommand`, `InternalError`.
- `set` on an unchanged value is acknowledged with `OK` but produces no `NOTIFY`.
- **`NOTIFY` is pushed to every connected client, unsolicited, no subscription
  step**: a front-panel press produced
  `NOTIFY set MIXER:Current/St/Fader/On 0 0 0 "OFF"` on a listener that had only
  ever sent `scpmode keepalive` and heartbeats. This is the `subscribe()`
  mechanism the DM3 adapter has been guessing about.
- The console idles happily with a client that sends nothing for >20 s once
  `scpmode keepalive 60000` is set; default keepalive behaviour **[OPEN]**.

### The console enumerates itself

`prmnum` → **177**; `prminfo <i>` returns
`"address" xcount ycount min max default "unit" type ? access step`;
`mtrnum` → **17**; `mtrinfo <i>` → `"address" count level`. The whole
dictionary, as the console returned it, is archived as
`dante-captures/yamaha-dm3/dm3-scp-dictionary.txt`. The part this project cares
about:

```
IO:Current/InCh/HAGain              16×1  0..64   "dB"  integer rw step 1
IO:Current/InCh/48VOn               16×1  0..1          integer rw
IO:Current/PortToPort/HAGain        16×1  0..64   "dB"  integer rw
IO:Current/PortToPort/48VOn         16×1  0..1          integer rw
IO:Current/PortToPort/HAAvailability 16×1 0..1          integer r
IO:Current/Dev/48VActiveOn          1×1                 integer r
IO:Current/Dev/SystemStatus         1×1   0..11         integer r
IO:Current/Dev/SyncStatus           1×1   0..12         integer r
```

Note the shape: `IO:Current/InCh` + `IO:Current/Dev/{SystemStatus,SyncStatus}`
is the same family as the Rio's `InCh[32]` + `Current/Dev` set recovered from
R Remote's schemas. `HPFOn`, `HPFFreq`, `GainCompOn` are **not** under
`IO:Current/InCh` on a DM3 (`UnknownAddress`) — a console's local head amp has
gain and phantom only; HPF is a channel-strip parameter under `MIXER:`.

`listitemnum <addr> 0 0` returned 0 for every parameter; `listitem`'s argument
form was not found **[OPEN]**.

## 4. OSC on the DM3 — UDP 49900

Works, on `dm3.local` (not on the Dante module's address — see §6). Findings
against the "DM3 Series OSC Specifications V1.0.0" reading in
`crates/preamp-adapter-yamaha/src/dm3.rs`:

| Adapter assumption | Observed |
|---|---|
| Replies arrive as `/yosc:rpl/…` (tests are written against this) | Replies are **`/yosc:ok/…`**. The address-agnostic `parse_headamp_addr` copes; the tests document the wrong prefix |
| `get` is "best effort, unconfirmed" | `get` works, with or without the spurious `int 0` argument the adapter sends |
| Indices in the OSC address | **1-based**: `/1/1` = input 1, `/16/1` = input 16, `/0/0` and `/17/1` get no reply |
| All values are `i` | `HAGain` and `Fader/Level` are int32 (`i`); **`48VOn`, `SystemStatus`, `SyncStatus`, `HAAvailability` are int64 (`h`)**. `last_int_value` accepts only `Int`/`Float`, so phantom is never parsed and `PreampState::phantom` silently stays `false` |
| `set` may be echoed | A `set` gets **no reply at all**, even when acknowledged (compare SCP's `OK set …`) |
| Errors | Silent — an invalid address, index, or command gets nothing back, ever |
| `sscurrent_ex "scene_a"` is the only identify probe available | It works: `/yosc:ok/sscurrent_ex ,sis "scene_a" 19 "modified"` (scene A number 19). But **`/yosc:req/devinfo ,s "productname"` → `/yosc:ok/devinfo ,ss "productname" "DM3"`** — the SCP `devinfo` keys are reachable over OSC as a string argument (not as a path segment), and give a real model string |
| Unsolicited pushes | None seen over OSC in 20 s of listening; SCP is the push channel |

Raw frames for test vectors (from `osc-probe-session.txt` in the captures repo):

```
/yosc:req/get/IO:Current/InCh/48VOn/1/1 ,   -> /yosc:ok/get/IO:Current/InCh/48VOn/1/1 ,h 0x0000000000000000
/yosc:req/get/IO:Current/InCh/HAGain/1/1 ,i 0 -> /yosc:ok/get/IO:Current/InCh/HAGain/1/1 ,i 23
/yosc:req/get/MIXER:Current/InCh/Fader/Level/1/1 -> ,i -240        (centi-dB)
/yosc:req/get/IO:Current/Dev/SystemStatus/1/1    -> ,h 2
/yosc:req/get/IO:Current/Dev/SyncStatus/1/1      -> ,h 5
```

## 4b. SCP writes and scene memory — proven on hardware

With the operator's consent (gain/phantom on ch 9+, presets above slot 25,
stereo output kept muted), the write and scene paths were exercised over SCP.
The full transcripts are `scp-write-test-ch9.txt` and `scp-scene-test.txt` in
the captures repo; a second SCP connection logged the `NOTIFY` echoes.

- **Gain write**, ch 9 (`InCh` index 8): `set IO:Current/InCh/HAGain 8 0 12` →
  `OK … 12 "+12"`, read back 12, restore to 0. The desk acted on it and pushed
  `NOTIFY set IO:Current/InCh/HAGain 8 0 12 "+12"` to the other connection.
- **Phantom write**, ch 9: `set IO:Current/InCh/48VOn 8 0 1` → `OK … "ON"`;
  `IO:Current/Dev/48VActiveOn` went to 1 while it was live; restored to 0.
- **Scene store**: `ssupdate_ex scene_a 30` on an empty slot → `OK`, plus
  `NOTIFY sscurrent_ex scene_a 30 unmodified` and `NOTIFY ssupdate_ex scene_a 30`.
  `ssinfo_ex scene_a 30` then reported the slot populated (`… "ins on" … user`).
- **Scene recall**: `ssrecall_ex scene_a 30` → `OK` + `NOTIFY sscurrent_ex
  scene_a 30`. Stereo master stayed muted across recall (`MIXER:Current/St/
  Fader/On 0 0` = 0 before and after), and the show channels were unchanged
  (ch1 = 23 dB, ch2 = 15 dB) — slot 30 had captured the live "19 modified"
  state, so recalling it was a no-op on the audio.
- **Scene enumeration**: `ssnum_ex scene_a` = 100 slots; `ssinfo_ex scene_a <n>`
  → `n "nn" "title" "comment" (user|empty)`. `sscurrent_ex scene_a` →
  `n (modified|unmodified)`.
- **No scene-clear verb.** `sstitle_ex`, `ssclear_ex`, `sscut_ex`, `sserase_ex`,
  `ssdelete_ex` all returned `UnknownCommand` — matching R Remote's recovered
  set, which has `SSRECALL`/`SSUPDATE` but no delete. Clearing a slot or editing
  a title is a front-panel / Editor operation, not in SCP.

**Residual state left on the desk** (both within what the operator authorised):
the current-scene pointer now reads `30` rather than `19 modified`, and slot 30
holds a test scene named "ins on". The live mix is byte-for-byte what it was.

## 5. The code on hardware

### Read path

`examples/dm3_live.rs` (added this session) drives `Dm3Adapter` read-only:

```
identify: OK vendor=Yamaha model=DM3 series (unconfirmed sub-model)  (11 ms)
get_state(1):  OK PreampState { gain_db: 23.0, phantom: false, pad: None }  (52 ms)
get_state(2):  OK PreampState { gain_db: 15.0, phantom: false, pad: None }
get_state(16): OK PreampState { gain_db: 0.0,  phantom: false, pad: None }
get_state(17): ERR no reply for channel 17 …  (1.03 s)
```

The `phantom: false` values are accidentally right (48 V was off on every
channel read): the int64 reply is dropped, so the field would read `false` with
phantom on. **Not fixed in this session** — the fix is a one-liner in
`last_int_value` (`OscType::Long(l) => Some(*l as i32)`), plus a test vector
from the frame above, plus updating the `rpl` prefixes in the existing tests.

### Write path — first WRITE-proven adapter in the workspace

`examples/dm3_write.rs` (added this session) drove `Dm3Adapter::set_gain` and
`set_phantom` against ch 9, with a second adapter subscribed. **The console
acted on both**: the SCP `NOTIFY` listener recorded `HAGain 8 0 12` then the
restore, and `48VOn` on then off — all originating from the adapter's *own* OSC
output, not hand-built frames. This is the first time any adapter's write path
has run against hardware (the Rio MBC write of 2026-08-03 was a standalone
Python script, not adapter code).

Two real adapter limitations fell out of the same run, and both argue for SCP:

1. **`get_state` returns a stale value right after a `set`.** The OSC read-back
   reported 0.0 while the desk was at 12 dB, because an OSC `set` gets no reply
   (§4) so the adapter can't confirm it, and `set_gain` doesn't write-through
   the state cache — `get_state` then returns the cached pre-set value. Over SCP
   the `OK set … "+12"` reply carries the confirmed value; over OSC there is
   nothing to key on.
2. **OSC `subscribe()` never fires.** The watcher adapter, listening on 49900,
   saw nothing while the desk changed under it — the DM3 pushes `NOTIFY` only on
   SCP 49280. The adapter's whole `subscribe()` contract is unimplementable on
   the OSC transport; it needs the SCP connection.

`preamp-bridge discover` was correct: the DM3's `_netaudio-arc` record at the
Dante module's address, port 4440, plus all sixteen `NN@…_netaudio-chan` TX
records.

`preamp-bridge init --infer-mappings` produced `device = []`, for two reasons
worth fixing:

1. **It probes the mDNS-discovered address.** For a DM3 that is the Dante
   module (169.254.237.144); the OSC/SCP endpoint is the console host
   (169.254.214.203). The DM3 is not an outlier — any console with a Dante
   card/module rather than a Dante-native control plane looks like this. The
   YSDP response (§2) carries the right address for Yamaha gear.
2. **It ran for 4½ minutes**, because every adapter's `identify()` was tried
   against every address DVS advertises for the Mac itself (VPN, Parallels,
   loopback, link-local — twelve of them), serially, each with its own timeout.
   The playbook's "disable VPN and VM interfaces" advice exists because of this;
   `init` should filter out local addresses and probe concurrently.

`preamp-bridge run` with a hand-written `[[device]]` at the right address
connected and served the web UI, but with no mapping it never sends a byte to
the console — the legacy bridge only queries state when something asks.

## 6. The Dante layer

- **Dante Controller → Broadway CMC handshake** (bench playbook Phase 4a) is in
  the capture: `169.254.15.36 → :8800  1200 0014 0001 1001 0000 385f <mac> 0000`
  answered `1200 0020 0001 1001 0001 0000 ac44f2a436af 0000 0001 0000 a9feed90 21fc 0000`,
  followed by ConMon setup on 8700 and a fourteen-exchange ARC session on 4440
  (opcodes `1000 1102 1003 2400 2032 3400 2010 2600 2204 3600 1100 3300 2320`).
  `tools/dt-fake` can be completed from this.
- **No `MBC` from an unmounted DM3** — zero frames containing `MBC` in over an
  hour. Its ConMon output is generic Audinate status on `224.0.0.233:8708`
  (~2/s) and device info on `224.0.0.231:8702` (`Yamaha Corporation`,
  `Broadway`, `Switched`/`Redundant`). The QL1 spoke MBC because a Rio was
  mounted; a console with no R-series device mounted has nothing to say. Whether
  a DM3 speaks MBC at all needs a mountable device — real or faked at the mDNS
  + YSDP level — on the DM3's I/O device screen **[OPEN]**.
- DVS on the bench still carries a stale RX subscription to
  `29@Y001-Yamaha-QL1-11a690` from August and queries for it constantly;
  harmless, but it is in every capture.

## 7. What this changes for the bridge

1. **SCP over TCP is the right transport for Yamaha, not OSC.** It has `NOTIFY`
   push, `devinfo` identify, `prminfo` enumeration (so no hard-coded parameter
   table), and the same grammar the R-series units are built on. A `yamaha-scp`
   adapter would cover the DM3 today and be the template for the Rio path once
   the R Remote discovery question is closed. The existing OSC adapter can stay
   as-is with the int64 fix, but nothing should be built on OSC's silent errors
   and absent acknowledgements.
2. **Identify Yamaha gear with `devinfo productname`** — over SCP (TCP 49280) or
   OSC (`/yosc:req/devinfo ,s "productname"`) — instead of the scene probe.
3. **YSDP belongs in `discovery`**: listen on 54330, answer nothing, and map
   `Y0xx` / model / name to the address in the response. That is also what
   `rio-fake` needs to send.
4. `init` needs the address-selection and concurrency fixes in §5.

## 7b. The DM3 Editor sync protocol — MMS over TCP 50368 (added later 2026-09-15)

Taking the macOS **DM3 Editor** online with this console exposed a **third**
Yamaha control protocol, distinct from SCP (§3) and OSC (§4). Capture:
`stoatworks-labs/dante-captures` `yamaha-dm3-editor/`.

- **Transport: one TCP connection to port 50368.** The Editor connects, and the
  console's state is transferred as the **MMS parameter framework** (`MMS::` in
  the firmware; the `mms_*.xml` descriptors) serialized on the wire. Message
  starts carry 4-char tags: `EEVT` (events / keepalive) and the MMS categories
  `MSTS` (Status), `MPRC`/`MPRO` (Processing), `MMIX` (Mixing), `MVOL`
  (Volume/fader), `MSCL`/`MSCS` (Scene), `MSUP` (Setup), `MCST`. A transport
  framing tagged `d000`/`d010`/`d020`/… wraps the payloads across TCP segments,
  and the payloads carry the same parameter-path strings as SCP (`Gain`,
  `Level`, `HPFOn`, `Pan`, `Scene`, …).
- **A successful sync** (Direct IP, Data Sync DM3→PC): the console pushed
  **3.30 MB** to the Editor (5175 frames > 56 B), the Editor sent 309 KB of
  requests, and the Editor went ONLINE mirroring the desk exactly — scene
  A30 "ins on", channel names, IN 1 = +23 dB — all cross-checking the §3 SCP
  reads.
- **Online steady state** is bidirectional `EEVT` KeepAlive (56 B, ~1/s).
- **Live push**: an SCP `set` on ch 9 from a *separate* connection, made while
  the Editor was online, arrived at the Editor as `MSTS`/`MPRC` frames (106/122
  B) — 50368 carries external changes live, the analogue of SCP's `NOTIFY`.
- **The operational trap, worth an hour:** the Editor will **not** sync while
  **R Remote is running.** R Remote holds the shared Yamaha discovery port
  **UDP 54330** (YSDP, §2); with it held, Direct-IP connect established the 50368
  event channel and received KeepAlive but never began the bulk transfer
  (spinner forever), and the interface path showed an empty device list. Quitting
  R Remote freed 54330 and the next connect synced fully. **Only one Yamaha
  control app can be online per host.** This is not a subnet problem — all
  addresses are `169.254.0.0/16` and the console answered SCP/OSC/50368
  throughout; only the sync *handshake* was gated on the discovery port.
- Not relevant to the bridge directly, but noted: the Editor keeps its scene
  library as `.dm3s` files under `~/Library/Application Support/Yamaha/DM3
  Editor/SceneList/Bank{A,B}/` — PatchFerret's format, a ready cross-check.

For the bridge, this reinforces §7: Yamaha's real control surfaces are
TCP-based (SCP 49280 for command/notify, 50368 MMS for full editor sync), not
the OSC port the current adapter uses. A `yamaha-scp` adapter remains the right
next step; the 50368 MMS protocol is a heavier lift (full binary framework) and
only worth it for whole-console mirroring, which is out of scope for a preamp
bridge.

## 7c. The Router validated on hardware (added 2026-09-15, later session)

With open bench access, the **bridge's actual function** — the `Router` reading a
change on one device and applying it to a mapped peer — was exercised against the
real DM3 for the first time (previously only mock-tested). Two `yamaha-dm3-scp`
devices were pointed at the same console (`dm3-a`, `dm3-b`) so a mapping could be
driven with one physical desk, all on spare channels (9–16, no source, show
channels and the stereo master untouched).

- **Unidirectional map `dm3-a:9 → dm3-b:10`:** setting ch 9 to 18 (from a separate
  SCP client) propagated to ch 10 within ~1 s — the Router consumed the `NOTIFY`
  on dm3-a, mapped it, and wrote dm3-b. Clean: one resulting `set`. This is the
  first time the Router (not just an adapter method) moved a value across a
  mapping on real hardware.
- **Bidirectional map `ch11 ↔ ch12`:** both directions propagate (ch11→22 pulled
  ch12 to 22; ch12→33 pulled ch11 to 33) and the values **converge and settle**
  (stable across repeated reads; steady state is 0 further `set`s — the storm
  self-terminates).
- **But** that bidirectional case showed **echo amplification**: ~42 `set`
  commands for 2 user actions, bouncing ~10 rounds before settling. The cause is
  the test topology, not deployment — both adapters connect to the **same**
  physical console, so each hears the other's `NOTIFY`s, which is what closes the
  loop. Two *distinct* devices (the real use case) can't form it: a device's
  `NOTIFY` only reaches its own adapter, and that adapter's echo suppression drops
  its own confirmation. Still worth tightening the suppression window so the
  adversarial case converges in one round rather than ten — logged as a follow-up.
- **Gain clamping matches:** the DM3 clamps `HAGain` to 0–64 itself (65/70/100 →
  64, −5 → 0), exactly the adapter's clamp, and signals a clamped write with an
  **`OKm`** reply prefix (vs plain `OK`) — "value modified." The adapter parses
  `OKm` fine (it keys on the address token) and clamps before sending regardless.
- All 16 inputs read correctly; spare channels 9–16 were write-proven and
  restored to baseline. No show channel or the stereo master was touched.

## 8. Still to do with this console

- The fake Rio did not get R Remote to connect (§2). If revisited: alias IP on
  the Dante NIC first (rules out the self-address theory), then compare the
  fake's `0x8004` bytes against a *real* Rio's response field by field — which
  needs a Rio, so this is properly blocked on hardware, not on more guessing.
- ~~DM3 Editor online: capture the Editor's sync protocol~~ **done** (§7b): MMS
  over TCP 50368; blocked only by R Remote holding UDP 54330, not by the network.
- ~~A `yamaha-scp` adapter (TCP 49280)~~ **shipped** (2026-09-15): `Dm3ScpAdapter`
  + `plugin-yamaha-dm3-scp` (kind `yamaha-dm3-scp`). identify / get_state /
  set_gain / set_phantom / `subscribe` all validated on the real DM3 through the
  full plugin stack — the first fully hardware-proven adapter in the project.
  (Scene recall/store over SCP is proven on the wire but not yet a bridge feature;
  the bridge's model is preamp gain/phantom, not scene control.)
- Mount a faked Rio on the DM3's I/O device screen: does the console emit MBC
  pairing queries (§6)?
- A write test with the user's consent: `set IO:Current/InCh/HAGain` on a spare
  channel over SCP, watching for the `NOTIFY` echo on a second connection.
- AES67 mode on the Broadway (a Dante-module reboot) for `squawk`: a real PTPv2
  grandmaster and real RTP flows.
- PatchFerret: save a scene to USB from this console, and load one PatchFerret
  wrote — nothing PatchFerret emits has ever been loaded into a console.
