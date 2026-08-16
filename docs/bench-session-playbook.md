# Bench session playbook

A runbook for the session with an SQ, both vendor control apps, and a live
assistant on the wire. Supersedes nothing — it *sequences* the existing
procedures and adds the ones that only became possible once R Remote and DT
Preamp Control were both available. Per-parameter detail for the SQ stays in
[`sq-capture-playbook.md`](sq-capture-playbook.md); don't duplicate it here.

## What's on the bench

| Present | Role |
|---|---|
| Allen & Heath **SQ** + Dante card (KLANTE) | Only device that might answer `AllenHth`; also a real remote CMC target |
| **Dante Virtual Soundcard** | The local Dante runtime both apps reach the wire through |
| **Dante AVIO USB** | Second, independent Audinate implementation (Ultimo) for CMC cross-check |
| **R Remote** 6.0.0 | Yamaha controller — [SCP over TCP](yamaha-scp-r-remote.md) |
| **DT Preamp Control** | A&H controller — [`AllenHth` over ConMon](allenheath-dt-preamp-over-dante.md) |
| 2× UniFi **USW-Flex-Mini** | Port mirroring, Dante and control networks |
| Assistant live on both networks | Can run captures, decode, and iterate emulator responses in the loop |

**Not present: no Rio, no DT168, no QL/CL.** That shapes everything below.

## The through-line

Both vendors' control apps are on the bench, and neither vendor's *device* is. So
the session's centre of gravity is **pointing real controller software at fake
devices** and reading what it sends. That direction — controller→device — is
exactly where the open questions live in both protocols, and it needs no target
hardware at all.

The corollary, stated up front so it isn't discovered late: a fake device
validates what the **real app sends**, which is genuine evidence. It cannot
validate our **decode of device→controller status**, because we'd be decoding a
fake we wrote from our own model. Status parsing stays inferred until real
hardware emits it.

## Before the session — build tasks

- [x] **`tools/rio-fake` is written** — see its
      [README](../tools/rio-fake/README.md). Smoke-tested locally over loopback;
      never yet run against R Remote.
- [ ] Confirm `tools/dt-fake/fake_dt168.py` still runs on this machine.
- [ ] Have Wireshark/tshark, Dante Controller, SQ-MixPad, R Remote and DT Preamp
      Control all installed and launchable.
- [ ] Read [`reference_unifi_port_mirroring`] equivalent: §3 of any of the three
      `capture-guide-*.md` for the USW-Flex-Mini mirror recipe. Don't re-derive it.

## Network layout

Two networks, one switch each:

- **Dante network** — SQ's Dante card, AVIO USB, the Mac's Dante NIC. Carries
  mDNS, ConMon, CMC, and (if it exists) SCP.
- **Control network** — SQ's control port for SQ-MixPad / TCP 51325.

**Mirror only where you actually need it.** Both control apps run *on the Mac*, so
app↔device unicast is already visible locally. Mirrors earn their keep for
device↔device traffic the Mac isn't an endpoint of — SQ↔AVIO, or anything you
want to observe without being party to it. Set them up, but don't block on them.

Capture `lo0` as well on both apps: the DT investigation established that these
apps reach the wire through the shared local Dante runtime over loopback
(`127.0.0.1:8850`), so the app↔runtime IPC is only visible there. R Remote's SCP
is a direct socket (`VCOM::VComTcpClientSocket`) and will be on the real NIC.

### Pre-flight traps

- [ ] **Disable VPN and VM interfaces** (Parallels included). Beacons pinned to
      the wrong NIC are invisible to the Dante runtime, and this has already cost
      a session once.
- [ ] Confirm DVS and Dante Controller are bound to the **real Dante NIC**, and
      note its name — every `--iface` below means that one.
- [ ] Apply the audio-excluding BPF filter from the start. Dante audio is UDP
      14336–14591 and will otherwise dominate the file; the QL1 capture reached
      1.7 GB before filtering.
- [ ] **Not a show console.** Phases 5–6 toggle phantom power. Pull mic lines.
- [ ] Note that UniFi mirror config persists — plan to revert it afterwards.

Baseline filter for the Dante NIC:

```
udp portrange 8700-8850 or udp portrange 9700-9900 or net 224.0.0.224/27 or udp port 5353 or udp port 4440
```

Add `or tcp port 51325` on the control network, and widen to `or (tcp and host <fake-ip>)` during Phase 3.

---

## Phase 0 — setup verification (~15 min)

- [ ] SQ and AVIO both visible in Dante Controller.
- [ ] One capture running per network, filter applied, writing to disk.
- [ ] Wall-clock noted. **Log the time of every action from here on** — it's what
      makes the pcap sliceable afterwards.

## Phase 1 — passive baseline (~10 min)

Everything idle, nothing launched.

- [ ] 5 minutes of quiet capture.
- [ ] Grep for `AllenHth` (`41 6c 6c 65 6e 48 74 68`). If the SQ emits it while
      idle, Phase 5's central question is already answered.

## Phase 2 — R Remote discovery, nothing to find (~10 min)

Launch R Remote with no Yamaha device present.

- [ ] What service types does it browse for? (mDNS, `udp port 5353`.)
- [ ] Does it probe DVS or the AVIO — any TCP SYN to either?
- [ ] Does it reach the runtime over `lo0:8850`?

**Why first:** this tells us what a fake Rio must advertise before we build one,
and a stray SYN would hand us the SCP port for free.

## Phase 3 — fake Rio + R Remote ★ highest value (~60–90 min)

The two biggest open items in [`yamaha-scp-r-remote.md`](yamaha-scp-r-remote.md)
are the **TCP port** and the **wire framing**. Both fall out here, with no Rio.

### 3a — the port, from a failed connection

The fake does **not** need to work. Advertise something R-series-shaped and let
R Remote try to connect: **the SYN alone reveals the port.** If discovery is
enough to trigger a connection attempt, this is a five-minute result.

```bash
python3 tools/rio-fake/fake_rio.py --iface <dante-nic> --log phase3.log
```

- [ ] Watch for the `*** TCP CONNECT ... SCP PORT = N ***` line.
- [ ] If nothing appears, the port is outside the candidate list — run the
      `tcpdump` line the tool prints at startup in a second terminal to catch
      SYNs to ports it never bound.

### 3b — the framing, by iteration

With the port known, have the fake accept and log.

- [ ] Log the first bytes R Remote sends on connect. Expect `DEVINFO` /
      `SCPMODE` / negotiation.
- [ ] Craft a plausible reply from the `DevInfo` schema (`protocolver`,
      `productname`, `manufacturer`, `deviceid`, `devicename`, `inputport`,
      `outputport`) — it's in
      [`evidence/rremote-600-scp-schemas.xml`](evidence/rremote-600-scp-schemas.xml).
- [ ] Iterate: reply, watch what it asks next, reply again. I can turn these
      around live.

**Success looks like:** R Remote proceeding into the association sequence — the
359 commands decoded in the SCP doc — which confirms both the sequence and the
framing in one go.

### 3c — the payoff, if it associates

- [ ] Move **gain** in the UI. Capture the `SET`. Confirms the write path and
      settles whether `HAGain` goes out as whole dB or centi-dB.
- [ ] Move **HPF frequency** across its range → recovers the 4–63 index→Hz
      mapping without lifting `ParamTableForHpfFreq` out of the data segment.
- [ ] Toggle **48V**, **HPFOn**, **GainCompOn**; move **CompGain**.
- [ ] Move **OutCh Polarity** and **GainLevel** — the parameters the MBC spec
      mis-attributed to the input side.

## Phase 4 — CMC capture → dt-fake → DT Preamp Control (~45 min)

`tools/dt-fake` is blocked on the **device-side CMC response**, which needs a real
**remote** Dante device. There are two on the bench.

- [ ] **4a.** Force Dante Controller to connect to the **SQ** (restart DC, or open
      Device View). Capture the handshake.
- [ ] **4b.** Same against the **AVIO USB**.
- [ ] **4c.** Compare the two byte-for-byte. They should differ only in device id
      and seqnum. If they do, "the CMC response is generic Audinate" is confirmed
      and `dt-fake` can be completed from either. If they don't, the emulator
      needs the KLANTE variant specifically — which is itself worth knowing.
- [ ] **4d.** Feed the response into `dt-fake`, run it, and point **DT Preamp
      Control** at it. If it connects, move a gain → **the `AllenHth` body
      sub-framing**, the last unknown in that protocol.

Note a same-host DVS is *not* usable as a CMC reference — its control goes over
loopback IPC, not network CMC.

## Phase 5 — the SQ itself (~45 min)

Follow [`sq-capture-playbook.md`](sq-capture-playbook.md) for the detail. In short:

- [ ] Does the SQ emit `AllenHth` on a preamp change? The coin-flip.
- [ ] If yes: gain calibration — park Input 1 at known dB values. `+30 dB` should
      read `80 1E` big-endian / `1E 80` little, settling byte order from one value.
      Pad should flip flags bit 0 and leave the `u16` unchanged.
- [ ] SQ-MixPad ↔ SQ on **TCP 51325** while moving a preamp → the console's own
      preamp protocol, useful regardless of the Dante answer.
- [ ] Input 9 as well as Input 1 → how the SQ addresses 48 local + SLink sockets.

## Phase 6 — opportunistic, if time allows

- [ ] **DT Preamp Control pointed at the SQ.** Shared `AllenHth` namespace; low
      odds, free to try.
- [ ] **R Remote with the SQ on the network** — does it probe a non-Yamaha Dante
      device? Tells us how it filters candidates.
- [ ] Read-only `AllenHth` poll from a plain socket to `224.0.0.233:8708` and
      `224.0.0.231:8702`, **only after the passive captures are saved**. If the SQ
      answers, the ConMon transport doesn't need Audinate DAPI — the Yamaha MBC
      work already showed a plain socket accepted by a real Rio.

---

## If time is short

Ordered by value per minute:

1. **Phase 3a** — the SCP port. Minutes, and it unblocks an entire plugin.
2. **Phase 4a/4b** — CMC handshakes. Near-certain, ~10 minutes, unblocks `dt-fake`.
3. **Phase 5's TCP 51325 capture** — near-certain, and a new endpoint on its own.
4. **Phase 3b/3c** — SCP framing. Highest ceiling, least predictable duration.
5. Everything else.

## Artefacts

- Save as `.pcapng`, one file per phase, named for the phase.
- Keep the **action log** — timestamps and values — alongside. Several of these
  are one-time physical events.
- Archive into `stoatworks-labs/dante-captures` (private) with a README row per
  file: packet count, duration, what it holds. Follow the audio-stripping rule
  there before committing.
- Revert the UniFi mirror configuration.

## What success would change

| Phase | Unblocks |
|---|---|
| 3a+3b | An SCP plugin — a Rio path with no MAC impersonation, no multicast, no DAPI question |
| 3c | The `HAGain` scale question, the HPF table, and the parameters the MBC spec got wrong |
| 4a–4c | `tools/dt-fake` completes; the "generic Audinate" assumption is tested rather than assumed |
| 4d | The `AllenHth` body sub-framing — the last unknown in the A&H protocol |
| 5 | Either an SQ-over-Dante preamp path, or a definitive negative; plus an SQ console endpoint |
| 6 | Possibly removes the DAPI dependency from `plugin-ah` entirely |
