# Sennheiser EW-DX EM (Dante): bench plan

The test sequence for the first session with a real **EW-DX EM 2 Dante or EM 4 Dante**.
Prepared 2026-11-21. **Nothing in this document has been run against a receiver yet.**

The `sennheiser-ewdx` mic kind (`crates/mic-adapter-sennheiser`) was written from Sennheiser's
*SSC Developer's Guide for EW-DX EM* (firmware 3.0.x, Publ. 03/2024, 327 pages; the URL in the
adapter's module comment still resolves, through two redirects) and tested only against mocked
sockets. This session is meant to put it in front of the real thing, and to collect what the
guide leaves out.

The tool for the session is [`tools/ewdx-probe`](../tools/ewdx-probe/README.md). It reads,
writes and subscribes over SSC, logs every datagram both ways, refuses the dangerous writes,
and puts back what it changed. A guide-shaped fake receiver comes with it, so the whole
sequence can be rehearsed beforehand (see [Rehearsal](#rehearsal)).

## What the session has to settle

1. **Does `mic-monitor` work on a real EW-DX, and for how long?** Reading the adapter against
   the guide gives seven predicted problems ([below](#predicted-problems)). Each has a step
   that confirms or clears it.
2. **The shapes the guide never shows.** It lists error codes but not the JSON of an error
   reply. Also missing: whether a subscription's first values come bundled with its
   acknowledgement, what an adapted value looks like, and what the `min`/`max` rate options
   really do (its own prose contradicts itself on that).
3. **Control: what a console's gain should drive.** There are two candidates: `/rxN/gain`
   (−3…+42 dB in 3 dB steps) and `/audio1/outN/level` (−24…+18 dB in 6 dB steps). We need to
   know what each one does to the **Dante** output and to the analog output, how off-step
   values get rounded, and whether a change made elsewhere is pushed to subscribers. That is
   the input to a `sennheiser-ewdx` *preamp* kind, which doesn't exist yet. The G3 equivalent
   is AF-out (`sennheiser-ewg3`).
4. **Telemetry with a transmitter that moves.** The G3 session couldn't map its meters because
   the transmitter never changed. This one moves it on purpose: distance, shielding, mute
   switch, peaks, switching it off, and battery type.
5. **The Dante side:** what the receiver puts on the Dante network, and whether SSC answers on
   the Dante interface as well as the control one.

## What to bring

| Item | Why |
|---|---|
| EW-DX EM 2 Dante or EM 4 Dante, firmware noted from the front panel | the target |
| At least one paired transmitter (SKM handheld or SK bodypack) | telemetry is about the transmitter |
| Both battery types, if possible (rechargeable pack and AA cells) | `battery/type`, `gauge`, `lifetime` per type |
| A steady audio source: tone into a bodypack's input, or a speaker on pink noise at a fixed distance from a handheld | gain steps have to be readable on a meter |
| A Dante receiver with a meter: Dante Virtual Soundcard plus any meter app, or the DM3 | to see what gain, level and mute do to the Dante output |
| Something to read the analog output (any desk input or interface) | to compare analog with Dante |
| A metal box (a cake tin will do) and room to walk 20 m or more | RF level, quality and diversity changes |
| This Mac on the receiver's network, the `Dante-BabelBox` checkout built (`cargo build -p dante-babelbox-mic-cli`) | probe and `mic-monitor` |
| Sennheiser's own control software, if it's to hand | the multi-client test (5.8) |

**Not a show rig.** Phase 5 mutes, re-levels and renames channels, and locks the front panel.
Everything gets put back, but plan as if the audio could jump.

## Safety

- **Snapshot first, restore last.** Step 2.1 saves every readable value, and step 9.1 writes the
  restorable ones back and diffs the result.
- **Never written** (the probe refuses these before anything is sent):
  - `/device/restore` and `/rxN/restore`: factory or audio defaults;
  - `/device/restart`;
  - `/device/link_density_mode`: the guide says this reboots the receiver;
  - `/device/encryption`;
  - everything under `/device/network/`, where one write can lose the unit;
  - `/device/time`;
  - `/device/booster`: on an EM 4 Dante this puts DC up the antenna cable;
  - `/device/remote_access`, `/rxN/presets/*` and `/rxN/sync_settings/*`.
- **Written and restored in this session:** identification blink, mute, gain, AF-out level, the
  channel name, location, front-panel lock and brightness. Device name and RF frequency are
  optional steps that need deciding on the day (5.9, 5.10). Frequency needs `--allow-rf`.
- **If the front panel stays locked** after a crash mid-step, `probe set /device/lock false`
  unlocks it.

## Predicted problems

These come from reading `crates/mic-adapter-sennheiser/src/sennheiser.rs` against the guide.
They are predictions, not findings. Each one names the step that settles it.

P1, P3 and P4 were fixed in the adapter on 2026-11-21, from the guide alone, so for those three
the session checks the fix rather than looking for the bug. The same change stopped the
adapter's receive loop giving up for good on the first read error (a connected UDP socket
reports an ICMP port-unreachable as one, which is what a rebooting receiver would cause). P2
and P5–P7 still stand as written.

| # | Prediction | Why | Settled by |
|---|---|---|---|
| P1 | **Fixed, to confirm: `mic-monitor` stopped updating after about 1000 notifications, and might not look like it.** | The guide's subscription defaults are `count` 1000 and `lifetime` 10 s. The adapter used to ask for a `lifetime` of 3600 and no `count`, and never renewed. In the rehearsal against the fake (which counts per address, a guess), `af` and `rf` froze after about 50 s and `quality` carried on. It now asks for the guide's own example values (`lifetime` 60, `count` 1000), re-sends every subscription every 20 s, and re-sends at once on any `310`. To confirm: no field ever freezes, and how often `310`s arrive between renewals. | 4.6, 7.2 |
| P2 | **`mic-monitor` shows nothing if replies come from a port other than 45.** | The adapter `connect()`s its UDP socket to `ip:45`, so the OS drops a datagram from any other source port. The probe uses an unconnected socket and warns when this happens. | 1.2 |
| P3 | **Fixed, to confirm: the battery reading went stale when the transmitter went away.** | The guide says the `mates` paths answer **424** while the transmitter is absent. The adapter only cleared battery on an empty `battery` object and ignored `/osc/error`. It now clears the reading for any `mates.txN.battery` address it finds inside an `/osc/error`, whether that's a bare address tree or nested under other keys. The guide doesn't print the error's shape, so the real 424 might still slip past. | 6.4 |
| P4 | **Fixed, to confirm: restarting `mic-monitor` used up the receiver's client slots.** | The guide allows eight subscribing clients. Every run bound a new port, asked for an hour and never cancelled or closed. Now dropping the adapter (Ctrl-C) cancels each subscription and sends `/osc/state/close`, and the 60 s lifetime means a run that is killed outright frees its slot within a minute. To confirm: nine clean restarts in a row are never refused, and after a `kill -9` the slot comes back within a minute. | 4.5, 7.3 |
| P5 | **Nothing subscribes at all, if the receiver wants a different form.** | The guide's prose says the argument is an *array* of address trees. Every example it prints is invalid JSON (`"subscribe":{["#":…`). The adapter sends the array form. | 1.4, 3.1 |
| P6 | **The lifetime gets adapted down.** | The guide lets the server adapt subscription parameters and send back the adapted values. The adapter renews every 20 s, so a 60 s lifetime adapted below 20 s would lapse between renewals; the adapter still catches the `310`, but each one is a gap. | 4.1 |
| P7 | **EM 2: the rx3/rx4 priming gets 404s.** | `watch` primes channels 1–4 on every receiver. It's probably harmless: `watch` logs the failure at debug level and moves on after about a second per channel. It should still be seen once. | 7.1 |

Not a prediction, but something the session should record: `MicState` has nowhere to put
warnings (`LowSignal`, `NoLink`, `RfPeak`, `AfPeak`, `Aes256Error`, `LowBattery`), the
transmitter's own mute switch, or its type, capsule and name. Record their real shapes so
whether to add them can be decided on evidence.

## Setup (Phase 0, ~20 min)

**0.1** Note from the front panel and the label: model, firmware, frequency range code,
transmitter model and firmware, and battery type fitted. Note the receiver's control IP and,
from Dante Controller, its Dante IP. If the front panel shows the network port mapping
(single cable, split, redundant), note that too.

**0.2** Set the shell up in the `Dante-BabelBox` checkout. Logs and snapshots go in one session
folder:

```bash
export EWDX=10.0.0.32 EWDX_DANTE=169.254.x.y SESSION=~/ewdx-bench-$(date +%Y%m%d)
```

```bash
mkdir -p "$SESSION" && probe() { python3 tools/ewdx-probe/ewdx_probe.py --host "$EWDX" --log "$SESSION/probe.jsonl" "$@"; }
```

**0.3** Start a capture on the interface that reaches the receiver, before anything is sent. It
excludes Dante audio, which would otherwise swamp the file:

```bash
sudo tcpdump -i en0 -w "$SESSION/ewdx.pcap" "(host $EWDX or host $EWDX_DANTE or udp port 5353) and not udp portrange 14336-14591"
```

**0.4** Log the wall-clock time of every action from here on. It is what makes the pcap and the
JSONL log sliceable afterwards. The probe's log carries a timestamp on every line.

**0.5** Patch the receiver's Dante Tx 1 to the metering receiver. Start the steady source on
transmitter 1 and check it shows on the meter.

## Phase 1: reach and identity (read-only, ~15 min)

**1.1 Discovery.** Leave each running a few seconds, then Ctrl-C:

```bash
dns-sd -B _ssc._udp
```

```bash
dns-sd -L "<instance name from -B>" _ssc._udp local.
```

Record the instance name (the guide says it must equal `/device/name`), the port and the TXT
record (`txtvers`, `version`). Then look at the Dante side of the same box:

```bash
dns-sd -B _netaudio-arc._udp
```

```bash
cargo run --bin mic-monitor -- discover
```

**1.2 First contact.**

```bash
probe get /osc/ping /osc/version /device/identity/vendor /device/identity/product /device/identity/version
```

Record the exact product string; the adapter reports it as the model. **Look for a `!! reply
came from port …` warning on stderr: that line is P2.** If nothing answers at all, try once with
`--bind-port 45` (`sudo` may be needed). A receiver that replies only to port 45 is a finding
in itself.

**1.3 Features and access.**

```bash
probe get /osc/feature/subscription /osc/feature/pattern /osc/feature/timetag /osc/feature/baseaddr /osc/state/auth/access /interface/version
```

The guide expects `true`, `"*?"`, `false` and `false`. `auth/access` is undocumented beyond "an
array of strings": record what it says.

**1.4 The subscribe form (P5),** with a 5-second lifetime so it expires on its own:

```bash
probe watch paths /device/name --lifetime 5 --duration 8
```

Record whether there's an acknowledgement, whether it echoes the request, whether the initial
value comes in the same datagram or the next one, and whether a 310 arrives at about 5 s.
**If this fails, stop and work it out before going further:** every telemetry step depends on
it.

## Phase 2: snapshot (read-only, ~5 min)

**2.1** Save every documented readable leaf. This is also the restore point:

```bash
probe snapshot -o "$SESSION/before.json"
```

It reads in small groups, so one unknown path doesn't sink the rest. The `errors` list in the
file holds every error reply verbatim. That list is the first real sample of the error format.
Expect 404s for anything the firmware doesn't implement (`/device/identity/serial` is "not yet
implemented" according to the guide).

**2.2** Ask for one channel's whole picture in one datagram, as a monitoring client would.
Record whether it answers in one piece or refuses with 413 (too long), 414 (too complex) or 450
(answer too long):

```bash
probe get /rx1/name /rx1/mute /rx1/gain /rx1/frequency /rx1/warnings /m/rx1/rssi /m/rx1/rsqi /m/rx1/divi /m/rx1/af /mates/tx1/battery/gauge /mates/tx1/battery/lifetime /mates/tx1/battery/type /mates/tx1/type /mates/tx1/name /mates/tx1/capsule --raw
```

## Phase 3: errors and edges (read-only, ~10 min)

**3.1** Run the canned probes:

```bash
probe errors | tee "$SESSION/errors.txt"
```

The set covers:

- an unknown leaf, an unknown container, and a channel past the last;
- a good and a bad leaf in one message (the guide expects 210, partial success);
- malformed JSON, and an array instead of an object;
- writes to two read-only leaves (vendor and RSSI: the guide says the reply carries the real
  value);
- OSC patterns (`rx?`, `rx*`);
- the shape of the guide's invalid subscribe examples, and the object form instead of the array;
- the subscription list (`subscribe: null`), `ping`, an `xid` echo;
- schema and limits reflection for `/rx1/gain`, and `/osc/error`.

Record each reply verbatim. **The shape of the 404 and 210 replies is what the adapter will
have to parse.**

## Phase 4: subscriptions (read-only, ~40 min)

**4.1 The adapter's own request (P6).** This sends exactly what `subscribe_channel(1)` sends:

```bash
probe watch adapter --channel 1 --duration 20
```

Record the acknowledgement, and **whether `lifetime: 60, count: 1000` come back unchanged**.
Record how the initial values arrive (one datagram or several), and the notification rate for
each path in the summary. The adapter re-sends this same request every 20 s, so also run it
with `--renew 20 --duration 70` and check the re-sends are acknowledged the same way and
nothing arrives twice.

**4.2 What `min` and `max` do.** The guide's prose contradicts itself, so measure it. Keep the
steady source running and use 20 s each. Each asks for a 30 s lifetime, because the guide's
default of 10 s would end it halfway; any watch longer than that needs `--lifetime` and, past
the lifetime, `--renew`:

```bash
probe watch meters --duration 20 --lifetime 30 --quiet
```

```bash
probe watch meters --duration 20 --lifetime 30 --quiet --max 500
```

```bash
probe watch meters --duration 20 --lifetime 30 --quiet --min 1000
```

```bash
probe watch meters --duration 20 --lifetime 30 --quiet --min 200 --max 200
```

Record the rates. Then repeat the first one with the transmitter **muted at the source**, so
the level is steady. Does an unchanged value still notify?

**4.3 Lifetime and renewal.**

```bash
probe watch paths /rx1/mute --lifetime 15 --duration 25
```

```bash
probe watch paths /rx1/mute --lifetime 15 --duration 40 --renew 10
```

The first should end in a 310 at about 15 s, and the second should not.

**4.4 Cancel.** Every `watch` sends `cancel: true` when it exits (unless `--leave`). It then asks
the receiver, on the same socket, what that client still holds. Subscriptions belong to one
client, which is one source port, so a separate `probe` command can't see them. Record both
replies from the end of 4.3's runs: the cancel's acknowledgement, and an empty list.

**4.5 The client limit (P4, first half).** Start with nothing else subscribed (every earlier
`watch` cancelled its own on exit). Then:

```bash
probe clients --n 9 --lifetime 60
```

The guide says eight. Record the ninth client's reply verbatim. Run the same command again
straight away. The first run's clients exited without closing, so if the receiver still counts
them, every client is now refused until their 60 s run out. That is P4 in miniature. Wait
60 s and check that all eight come back.

Then check whether `/osc/state/close` frees slots without waiting. Run these back to back: if
it does, the second run gets eight acceptances again at once.

```bash
probe clients --n 9 --lifetime 60 --close
```

```bash
probe clients --n 9 --lifetime 60 --close
```

**4.6 How fast the count runs out (P1).** The adapter renews every 20 s, which keeps a
subscription alive only if its 1000 notifications last longer than that. Measure it with a
lifetime long enough that only the count can end it. Start this last, in a **second terminal**,
and let it run through Phases 5 and 6. Define `probe` there too.

```bash
probe watch adapter --channel 1 --lifetime 3600 --duration 1800 --quiet
```

Record when, and after how many datagrams, the 310 arrives. The tool prints both. If it's
under 20 s, the adapter will be renewing on each 310 rather than ahead of it, with a short gap
each time; 7.2 shows whether that gap is visible. Then try the same with `--count 100000`, to
see whether a larger count is accepted or adapted. If it's accepted, a bigger count is the
cheaper fix.

## Phase 5: control (writes, all restored, ~45 min)

Keep the 4.6 watch running in the second terminal. Add a status watch in a third. It subscribes
to gain, mute and warnings, and to the transmitter's own fields, so **it shows whether a change
made by one client (the probe's writes below) is pushed to another.** A bidirectional mapping
depends on that:

```bash
probe watch status --channel 1 --duration 3600 --lifetime 60 --renew 20
```

**5.1 Identify.** Blink the receiver, then channel 1, and check the front panel:

```bash
probe set /device/identification/visual true
```

```bash
probe set /device/identification/visual false
```

```bash
probe set /rx1/identification/visual true
```

```bash
probe set /rx1/identification/visual false
```

**5.2 Gain sweep, the main control result.** Steady source on, meter on Dante Tx 1, and the
analog output in view:

```bash
probe sweep gain --channel 1 --odd --pause
```

For every step, record the requested value, the reply, the read-back, and **the Dante meter
and the analog meter**. Each in-range step should move by 3 dB. The odd values (10, 11, 1.5,
−10, 50, `"12"`) show the rounding rule and the out-of-range and wrong-type behaviour. The
sweep restores the starting gain when it ends, even on Ctrl-C.

What to conclude: does `/rx1/gain` move Dante, analog, or both? Does `/m/rx1/af` move with it
(post-gain metering) or stay put (pre-gain)?

**5.3 AF-out level sweep.** Same procedure:

```bash
probe sweep level --channel 1 --odd --pause
```

The expectation is 6 dB steps on the **analog** output only. Record whether Dante moves at
all.

**5.4 Mute.**

```bash
probe sweep mute --channel 1 --odd --pause
```

Record whether it mutes Dante, analog or both, and what `/m/rx1/af` reads while muted. Then
check how `/rx1/mute` and the transmitter's own switch (`/mates/tx1/mute`) relate: toggle the
**transmitter's** mute switch by hand and watch the status terminal.

**5.5 Front-panel changes.** On the receiver itself, change channel 1's gain up one step and
back. Does the status watch see it, and how quickly? A bidirectional mapping needs this.

**5.6 Channel name.** Eight characters per the guide. The second command tests what happens
past that limit:

```bash
probe set /rx1/name BBTEST1
```

```bash
probe set /rx1/name ABCDEFGHIJK
```

Check the front panel, and **Dante Controller's Tx channel label**: does a channel name rename
the Dante channel? Restore with the command the *first* `set` printed. The second one's "was" is
the test name.

**5.7 Location, lock and brightness.**

```bash
probe set /device/location "babelbox bench"
```

```bash
probe set /device/brightness 2
```

```bash
probe set /device/lock true
```

With the lock on, try a front-panel key, then unlock:

```bash
probe set /device/lock false
```

Restore the location and brightness with the commands the probe printed.

**5.8 Two controllers at once (optional).** If Sennheiser's own software is on the bench,
connect it to the receiver. Repeat one gain step from the probe and one from the software, and
check each side sees the other's change. Unlike the G3, the guide describes no single-master
lock, so writes from both should be honoured. Confirm that.

**5.9 Device name (optional, decide on the day).** The guide says the `_ssc._udp` instance
name must follow `/device/name`. Whether the **Dante** device name follows too is undocumented,
and it matters to BabelBox's Dante side. It's low risk, but it renames the box on the network:

```bash
probe set /device/name BBTEST-EWDX
```

Check `dns-sd -B _ssc._udp` and Dante Controller, then restore with the printed command.

**5.10 RF frequency (optional, only on a frequency known to be clear).** This drops the link,
and it stays down until the transmitter is re-synced. BabelBox doesn't need it, so skip it
unless RF control is wanted:

```bash
probe --allow-rf set /rx1/frequency <clear kHz>
```

Re-sync the transmitter, then restore the frequency and re-sync again.

## Phase 6: telemetry with a moving transmitter (~40 min)

Start `mic-monitor` now (step 7.1), so it sees everything below. Keep the status watch
running and add a meters watch. Narrate each action with the time:

```bash
probe watch meters --channel 1 --duration 3600 --max 250 --lifetime 60 --renew 20
```

**6.1 Audio level.** Speak, shout and stay silent. With the steady source, compare `/m/rx1/af`
(dBFS according to the guide) with the Dante meter's reading. Push hard enough to see `AfPeak`
in `/rx1/warnings` and `/mates/tx1/warnings`.

**6.2 RF.** Walk away slowly until the link drops, then come back. Put the transmitter in the
metal box. Watch `rssi` (−107…0 dBm), `rsqi` (0–100 %) and `LowSignal`. Disconnect one antenna,
then the other, and watch `divi` (0 none, 1 A, 2 B).

**6.3 Battery.** Record `gauge`, `lifetime` and `type` with each battery type. The guide gives
`"Battery"` and `"Primary Cell"` as the types. Note which fields a primary cell reports at all.

**6.4 Transmitter off (P3).** Switch the transmitter off and watch every terminal. Record what
the status watch receives: a 424 (in what shape), `NoLink`, the meters' floor values. Also
record **what `mic-monitor` shows**: battery and runtime should go to `n/a`. If they stay on
screen, find in the probe log what the receiver sent instead. A 424 the adapter missed means
its shape is different from what the adapter looks for. No 424 at all means the adapter needs
another cue, such as `NoLink` in the warnings. Switch the transmitter back on and check the
subscription carries on by itself and the battery comes back.

**6.5 Second channel.** Sync the transmitter to channel 2. Check `/rx2/mates` reads
`["mates/tx2"]` and that the battery shows under `tx2`. The adapter assumes channel N's
transmitter is always `txN`. Then sync it back.

## Phase 7: BabelBox's own adapter (~30 min; 7.1 starts at the beginning of Phase 6)

**7.1** Start `mic-monitor` on the receiver in its own terminal:

```bash
printf '[[mic]]\nid = "ewdx"\nkind = "sennheiser-ewdx"\naddress = "%s"\nport = 45\n' "$EWDX" > "$SESSION/mics.toml"
```

```bash
RUST_LOG=mic_monitor=debug,dante_babelbox_mic_adapter_sennheiser=debug,info cargo run --bin mic-monitor -- watch --config "$SESSION/mics.toml" 2>&1 | tee "$SESSION/mic-monitor.log"
```

Compare its readings with the probe's for the same moment: battery, RF, quality, AF, antenna,
frequency and mute. On an EM 2, look for the `channel not available` or `timed out priming
channel` lines for rx3/rx4 (P7), and check the receiver's reply to those subscriptions in the
pcap.

**7.2 How long it lasts (P1).** Leave it running for at least ten minutes, with the
transmitter moving and the source varying. Nothing should stop changing: check that `af` and
`rf` still move well past 4.6's time. **Watch the `af` and `rf` columns, not just whether lines
keep coming:** a line still prints whenever any one field changes, so a frozen meter can sit
inside output that looks live. Count the `SSC subscription terminated (310); renewing` lines in
the log against the minutes run. A few means the count runs out between renewals (4.6). None
means the 20 s timer is keeping ahead.

**7.3 Client slots (P4, second half).** This is the last step that subscribes, because if the
fix doesn't hold it can leave the receiver refusing subscriptions for a minute or more. Phases
8 and 9 only read and write. First stop every probe `watch` (each cancels its own on exit), so
`mic-monitor` is the only client. Then stop it with Ctrl-C and restart it nine times within a
few minutes. After each restart, check that the probe can still subscribe:

```bash
probe watch paths /device/name --lifetime 5 --duration 2
```

None should be refused: each Ctrl-C cancels and closes. Then imitate a crash: start
`mic-monitor`, `kill -9` it (`pkill -9 mic-monitor`), and run the probe line above every
10 s. Record how long until the slot came back; the 60 s lifetime says within a minute.
Record whether Sennheiser's software (if connected) lost its updates at any point, and whether
anything needed a power cycle.

## Phase 8: the Dante side (read-only, ~15 min)

**8.1** In Dante Controller, record the device name, Tx channel names, sample rate, latency, and
whether the names track `/rxN/name` (from 5.6).

**8.2** Read what SSC itself says about the Dante interfaces:

```bash
probe get /device/network/dante/identity/version /device/network/dante/ipv4/ipaddr /device/network/dante/ipv4/auto /device/network/dante/macs /device/network/dante/interfaces /device/network/dante/interface_mapping
```

**8.3** Does SSC answer on the Dante IP as well as the control IP? It's read-only:

```bash
python3 tools/ewdx-probe/ewdx_probe.py --host "$EWDX_DANTE" --log "$SESSION/probe.jsonl" get /device/name
```

**8.4** Later, from the pcap: does the receiver's Dante module carry anything besides standard
Audinate traffic, such as ConMon vendor messages (UDP 8700–8800) when settings change in
Phase 5? This is what decides whether a console could see the EW-DX over Dante itself, the way
a QL sees a Rio, or only over SSC on the network.

## Phase 9: restore and close (~10 min)

**9.1** Put everything back and prove it:

```bash
probe restore "$SESSION/before.json" --dry-run
```

```bash
probe restore "$SESSION/before.json"
```

```bash
probe snapshot -o "$SESSION/after.json" && python3 tools/ewdx-probe/ewdx_probe.py diff "$SESSION/before.json" "$SESSION/after.json"
```

The diff ignores meters, battery and warnings. **It must report 0 differences.** Anything left
over gets fixed by hand and noted. `restore` leaves the RF frequency alone unless it's run with
`--allow-rf`, so if 5.10 was done, the diff is where a missed frequency shows up.

**9.2** Stop the capture, every watch and `mic-monitor`. Check the front panel is unlocked,
names are as found, and the transmitter is synced and passing audio.

## Results sheet

Fill this in during the session. The evidence column is a time in the JSONL log or pcap.

| Step | Question | Result | Evidence |
|---|---|---|---|
| 1.1 | `_ssc._udp` port, TXT, instance = device name? | | |
| 1.2 | Product string; replies from port 45? (P2) | | |
| 1.3 | `auth/access` value | | |
| 1.4 | Array subscribe form accepted? Ack and initial value bundled? (P5) | | |
| 2.1 | Leaves that 404 on this firmware | | |
| 2.2 | One-datagram limit (413/414/450?) | | |
| 3.1 | Error reply JSON: 404, 210, 400 | | |
| 3.1 | Write to a read-only leaf: error or real value? | | |
| 4.1 | `lifetime` 60 / `count` 1000 adapted to? (P6) Rate per meter; re-sends clean? | | |
| 4.2 | Rates for each `min`/`max` setting; does an unchanged value notify? | | |
| 4.3 | 310 at the lifetime; renewal keeps it alive? | | |
| 4.4 | Cancel acknowledged? Anything still held afterwards? | | |
| 4.5 | Client limit, the refusal's JSON, does `close` free a slot? | | |
| 4.6 | Time and datagram count to 310 with default count (P1): under 20 s? Larger count accepted? | | |
| 5.2 | Gain: Dante / analog / both? Rounding of 10, 11, 1.5; clamp; `"12"` | | |
| 5.2 | `/m/rxN/af` pre- or post-gain? | | |
| 5.3 | AF-out level: analog only? Rounding | | |
| 5.4 | Mute: Dante / analog / both? AF while muted; TX switch vs `/rxN/mute` | | |
| 5.4/5.5 | Change from another client / the front panel pushed to subscribers? Delay | | |
| 5.6 | Name over 8 chars: truncated or refused? Dante channel label follows? | | |
| 5.7 | Lock blocks the keys? | | |
| 5.8 | Two controllers both honoured? | | |
| 5.9 | Dante device name follows `/device/name`? | | |
| 6.1 | `af` against the Dante meter; `AfPeak` threshold | | |
| 6.2 | `rssi`/`rsqi` range seen; `divi` with each antenna removed; `LowSignal` | | |
| 6.3 | Battery fields per type | | |
| 6.4 | Transmitter off: 424 shape, `NoLink`, meter floor; battery to `n/a` in `mic-monitor`? (P3) | | |
| 6.5 | Channel 2 ↔ `tx2` | | |
| 7.1 | `mic-monitor` agrees with the probe? rx3/rx4 on an EM 2 (P7) | | |
| 7.2 | Any field freeze in 10+ min? 310-renewals per minute (P1) | | |
| 7.3 | Nine Ctrl-C restarts: any refusal? After `kill -9`, slot back in (P4) | | |
| 8.3 | SSC on the Dante IP? | | |
| 8.4 | Anything Sennheiser-specific on the Dante control ports? | | |
| 9.1 | Diff after restore = 0? | | |

## After the session

- Write it up as `docs/sennheiser-ewdx-bench-<date>.md`, the way
  [`sennheiser-ewg3-bench-2026-09-15.md`](sennheiser-ewg3-bench-2026-09-15.md) records the G3.
  The pcap and the probe log go in the private `dante-captures` repo, not this public one.
- Correct `crates/mic-adapter-sennheiser`: its module comment where the guide was wrong, unit
  tests rebuilt on real replies as fixtures, and fixes for whichever of P2 and P5–P7 were
  confirmed. Where the receiver disagrees with the guide-based fixes for P1, P3 and P4 (the
  renewal interval, the 424's shape, whether `close` frees a slot), correct those too.
- With 5.2–5.5 answered, decide what a `sennheiser-ewdx` preamp kind's "gain" is. The
  rounding rule matters: the router's echo check treats values within 0.001 dB as equal, so a
  receiver that answers 9 when asked for 10 looks like a new value and could bounce back
  along a bidirectional mapping.
- Keep [AGENTS.md](../AGENTS.md) §4 honest. Until this has run, the EW-DX adapter is "built
  from the official spec, tested against mocks". After it has run, say exactly which steps
  passed.

## Rehearsal

`tools/ewdx-probe/fake_ewdx.py` answers on `127.0.0.1:4545` and behaves the way the guide
describes. Where the guide is silent it guesses, and every guess is marked in its source.
**Nothing it says is evidence.** It's there to rehearse the commands above and check the
probe still works:

```bash
python3 tools/ewdx-probe/fake_ewdx.py
```

Then, in another terminal, set `EWDX=127.0.0.1` and define `probe` with `--port 4545` added.
Every step above runs against it except the ones that need RF, audio or a front panel.
`kill -USR1 <pid>` switches its transmitter 1 off and on (step 6.4).
